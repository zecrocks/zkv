//! lightwalletd server selection + connection.
//!
//! Supports direct TCP (TLS) and external SOCKS5 (so you can route through your own
//! Tor if you want). The built-in `arti` Tor client is intentionally not included;
//! it drops a large dependency tree for a hackathon-grade tool.

use std::{borrow::Cow, fmt, net::SocketAddr, time::Duration};

use anyhow::{anyhow, bail};
use tonic::transport::{Channel, ClientTlsConfig, Endpoint, Uri};
use tracing::info;
use zcash_client_backend::proto::service::{
    self, compact_tx_streamer_client::CompactTxStreamerClient,
};

use crate::network::Network;
use crate::socks::SocksConnector;

/// Bound the TCP+TLS handshake. A fresh connect right after a laptop wakes
/// from sleep (network not yet back, DNS stalled) fails fast and the caller
/// reconnects on the next cycle, instead of hanging on a handshake that may
/// never complete.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// HTTP/2 PING keepalive interval. With [`KEEPALIVE_TIMEOUT`], a connection
/// whose peer has silently gone away (laptop slept mid-sync, NAT rebind, a
/// server that idle-closed the socket) is detected within roughly the sum of
/// the two, so a long-lived streaming RPC (block download) fails with an error
/// instead of blocking forever on a socket that will never produce another
/// byte. This is the core fix for "the GUI stops syncing after the laptop has
/// been asleep and never recovers until restarted": the dead-connection RPC now
/// errors out, the auto-sync worker returns, and the loop reconnects fresh.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);
/// How long to wait for a keepalive PING ack before declaring the connection
/// dead. See [`KEEPALIVE_INTERVAL`].
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(20);

/// Apply the shared connect-timeout + keepalive settings to an endpoint, so
/// both the direct and SOCKS paths detect a dead connection (rather than hang)
/// and bound the handshake. Keepalives are sent even while idle so a connection
/// that died between requests is noticed too.
fn tune_endpoint(endpoint: Endpoint) -> Endpoint {
    endpoint
        .connect_timeout(CONNECT_TIMEOUT)
        .tcp_keepalive(Some(KEEPALIVE_INTERVAL))
        .http2_keep_alive_interval(KEEPALIVE_INTERVAL)
        .keep_alive_timeout(KEEPALIVE_TIMEOUT)
        .keep_alive_while_idle(true)
}

const ECC_TESTNET: &[Server<'_>] = &[Server::fixed("lightwalletd.testnet.electriccoin.co", 9067)];

const YWALLET_MAINNET: &[Server<'_>] = &[
    Server::fixed("lwd1.zcash-infra.com", 9067),
    Server::fixed("lwd2.zcash-infra.com", 9067),
    Server::fixed("lwd3.zcash-infra.com", 9067),
    Server::fixed("lwd4.zcash-infra.com", 9067),
];

const ZEC_ROCKS_MAINNET: &[Server<'_>] = &[
    Server::fixed("zec.rocks", 443),
    Server::fixed("ap.zec.rocks", 443),
    Server::fixed("eu.zec.rocks", 443),
    Server::fixed("na.zec.rocks", 443),
    Server::fixed("sa.zec.rocks", 443),
];
const ZEC_ROCKS_TESTNET: &[Server<'_>] = &[Server::fixed("testnet.zec.rocks", 443)];

#[derive(Clone, Debug)]
pub enum ServerOperator {
    Ecc,
    YWallet,
    ZecRocks,
}

impl ServerOperator {
    fn servers(&self, network: Network) -> &[Server<'_>] {
        match (self, network) {
            (ServerOperator::Ecc, Network::Main) => &[],
            (ServerOperator::Ecc, Network::Test) => ECC_TESTNET,
            (ServerOperator::YWallet, Network::Main) => YWALLET_MAINNET,
            (ServerOperator::YWallet, Network::Test) => &[],
            (ServerOperator::ZecRocks, Network::Main) => ZEC_ROCKS_MAINNET,
            (ServerOperator::ZecRocks, Network::Test) => ZEC_ROCKS_TESTNET,
            // No operator hosts a public regtest chain; a regtest database
            // needs an explicit `--server host:port` pointing at the local
            // lightwalletd (see `Servers::pick`'s error).
            (_, Network::Regtest) => &[],
        }
    }
}

#[derive(Clone, Debug)]
pub enum Servers {
    Hosted(ServerOperator),
    Custom(Vec<Server<'static>>),
}

impl Servers {
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s {
            "ecc" => Ok(Self::Hosted(ServerOperator::Ecc)),
            "ywallet" => Ok(Self::Hosted(ServerOperator::YWallet)),
            "zecrocks" => Ok(Self::Hosted(ServerOperator::ZecRocks)),
            _ => s
                .split(',')
                .map(|sub| {
                    sub.rsplit_once(':').and_then(|(host, port_str)| {
                        port_str
                            .parse()
                            .ok()
                            .map(|port| Server::custom(host.into(), port))
                    })
                })
                .collect::<Option<_>>()
                .map(Self::Custom)
                .ok_or(anyhow!(
                    "'{s}' must be one of ['ecc', 'ywallet', 'zecrocks'], or a comma-separated list of host:port"
                )),
        }
    }

    pub fn pick(&self, network: Network) -> anyhow::Result<&Server<'_>> {
        match self {
            Servers::Hosted(server_operator) => {
                server_operator.servers(network).first().ok_or_else(|| {
                    if network == Network::Regtest {
                        anyhow!(
                            "no hosted lightwalletd serves regtest; pass \
                             `--server <host>:<port>` pointing at your local one"
                        )
                    } else {
                        anyhow!("{:?} doesn't serve {:?}", server_operator, network)
                    }
                })
            }
            Servers::Custom(servers) => Ok(servers.first().expect("not empty")),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Server<'a> {
    host: Cow<'a, str>,
    port: u16,
}

impl fmt::Display for Server<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

impl Server<'static> {
    const fn fixed(host: &'static str, port: u16) -> Self {
        Self {
            host: Cow::Borrowed(host),
            port,
        }
    }
}

impl Server<'_> {
    fn custom(host: String, port: u16) -> Self {
        Self {
            host: Cow::Owned(host),
            port,
        }
    }

    fn use_tls(&self) -> bool {
        !matches!(self.host.as_ref(), "localhost" | "127.0.0.1" | "::1")
            && !self.host.ends_with(".onion")
    }

    fn endpoint(&self) -> String {
        format!(
            "{}://{}:{}",
            if self.use_tls() { "https" } else { "http" },
            self.host,
            self.port
        )
    }

    pub async fn connect_direct(&self) -> anyhow::Result<CompactTxStreamerClient<Channel>> {
        info!("Connecting to {}", self);

        let endpoint = Channel::from_shared(self.endpoint())?;
        let endpoint = if self.use_tls() {
            endpoint.tls_config(
                ClientTlsConfig::new()
                    .domain_name(self.host.to_string())
                    .assume_http2(true)
                    .with_webpki_roots(),
            )?
        } else {
            endpoint
        };
        let endpoint = tune_endpoint(endpoint);

        Ok(CompactTxStreamerClient::new(endpoint.connect().await?))
    }

    pub async fn connect_over_socks(
        &self,
        proxy_addr: SocketAddr,
    ) -> anyhow::Result<CompactTxStreamerClient<Channel>> {
        info!("Connecting to {} via SOCKS proxy {}", self, proxy_addr);

        let connector = SocksConnector::new(proxy_addr);
        let uri: Uri = self.endpoint().parse()?;

        let mut endpoint =
            tune_endpoint(Endpoint::from(uri.clone())).timeout(Duration::from_secs(30));

        if self.use_tls() {
            endpoint = endpoint.tls_config(
                ClientTlsConfig::new()
                    .domain_name(self.host.to_string())
                    .assume_http2(true)
                    .with_webpki_roots(),
            )?;
        }

        let channel = endpoint.connect_with_connector(connector).await?;
        Ok(CompactTxStreamerClient::with_origin(channel, uri))
    }
}

/// Determines how to connect to the lightwalletd server.
#[derive(Clone, Debug)]
pub enum ConnectionMode {
    Direct,
    SocksProxy(SocketAddr),
}

pub fn parse_connection_mode(s: &str) -> Result<ConnectionMode, String> {
    match s {
        "direct" => Ok(ConnectionMode::Direct),
        s if s.starts_with("socks5://") => {
            let addr: SocketAddr = s
                .strip_prefix("socks5://")
                .unwrap()
                .parse()
                .map_err(|_| format!("Invalid SOCKS5 proxy address: {s}"))?;
            Ok(ConnectionMode::SocksProxy(addr))
        }
        _ => Err("Invalid connection mode. Use 'direct' or 'socks5://<host>:<port>'".to_string()),
    }
}

/// How to reach a `lightwalletd` server: which operator (or custom host list),
/// and whether to dial directly or through a SOCKS5 proxy.
///
/// Library-friendly: this type is plain data with no `clap` dependency, so
/// it composes into HTTP services, libraries, and other consumers without
/// dragging in a CLI parser. The in-tree `zkv` and `zkv-faucet` binaries
/// each wrap this in their own `clap::Args`-derived type at the CLI seam.
///
/// `Default` picks direct TCP to ZecRocks, which serves both mainnet and
/// testnet.
#[derive(Debug, Clone)]
pub struct ConnectionArgs {
    /// Which lightwalletd server(s) to try, for any network without a more
    /// specific override below.
    pub server: Servers,

    /// Optional per-network server overrides (`--mainnet-server` /
    /// `--testnet-server`). When set, the matching one wins over `server` for
    /// that network; when `None`, `server` is used.
    pub mainnet_server: Option<Servers>,
    pub testnet_server: Option<Servers>,

    /// Direct TCP, or a SOCKS5 proxy.
    pub connection: ConnectionMode,
}

impl Default for ConnectionArgs {
    fn default() -> Self {
        Self {
            server: Servers::Hosted(ServerOperator::ZecRocks),
            mainnet_server: None,
            testnet_server: None,
            connection: ConnectionMode::Direct,
        }
    }
}

impl ConnectionArgs {
    /// The server list to dial for `network`: its per-network override if one
    /// was given, otherwise the default `server`.
    fn servers_for(&self, network: Network) -> &Servers {
        match network {
            Network::Main => self.mainnet_server.as_ref().unwrap_or(&self.server),
            Network::Test => self.testnet_server.as_ref().unwrap_or(&self.server),
            // Regtest has no per-network override flag; the explicit
            // `--server host:port` is the only sensible configuration.
            Network::Regtest => &self.server,
        }
    }

    pub async fn connect(
        &self,
        network: Network,
    ) -> anyhow::Result<CompactTxStreamerClient<Channel>> {
        let server = self.servers_for(network).pick(network)?;
        let mut client = match &self.connection {
            ConnectionMode::Direct => server.connect_direct().await?,
            ConnectionMode::SocksProxy(addr) => server.connect_over_socks(*addr).await?,
        };
        verify_server_network(&mut client, network).await?;
        Ok(client)
    }

    /// Probe the lightwalletd server for `network`: dial it and pull a single
    /// `GetLightdInfo`. Surfaces the dialed endpoint, the server's tip height,
    /// and the backend node implementation + version (parsed from the
    /// `zcashd_subversion` token, e.g. `zcashd 6.20.0` / `Zebra 2.1.0`). Used
    /// by the GUI Settings screen, which shows one row per network. Errors (no
    /// such server for the network, connection failure) propagate so the caller
    /// can render the row as offline.
    pub async fn server_info(&self, network: Network) -> anyhow::Result<ServerInfo> {
        let server = self.servers_for(network).pick(network)?;
        let endpoint = server.to_string();
        let mut client = match &self.connection {
            ConnectionMode::Direct => server.connect_direct().await?,
            ConnectionMode::SocksProxy(addr) => server.connect_over_socks(*addr).await?,
        };
        let info = client
            .get_lightd_info(service::Empty {})
            .await
            .map_err(|e| anyhow!("GetLightdInfo failed: {e}"))?
            .into_inner();
        Ok(ServerInfo {
            endpoint,
            block_height: info.block_height,
            backend: backend_label(&info.zcashd_subversion, &info.vendor),
        })
    }
}

/// A lightwalletd server's self-reported identity, for the GUI Settings screen.
#[derive(Clone, Debug)]
pub struct ServerInfo {
    /// `host:port` we dialed.
    pub endpoint: String,
    /// Best-chain tip height the server reports.
    pub block_height: u64,
    /// Backend node implementation + version, e.g. `"zcashd 6.20.0"` or
    /// `"Zebra 2.1.0"`. Falls back to the raw subversion / vendor string when
    /// the conventional `/Name:version/` shape isn't present.
    pub backend: String,
}

/// Translate lightwalletd's `zcashd_subversion` (e.g. `/MagicBean:6.20.0/`,
/// `/Zebra:2.1.0/`) into a human backend label. `MagicBean` is `zcashd`'s
/// historical user-agent name, so we surface it as `zcashd`; `Zebra` stays
/// `Zebra`. The version number is kept (`zcashd 6.20.0`). Unrecognized or empty
/// strings fall back to the trimmed subversion, then the vendor.
fn backend_label(subversion: &str, vendor: &str) -> String {
    // Subversion is conventionally `/Name:version/` (BIP-14 user agent).
    let trimmed = subversion.trim_matches('/');
    if let Some((name, version)) = trimmed.split_once(':') {
        let pretty = match name {
            "MagicBean" => "zcashd",
            other => other,
        };
        let version = version.trim();
        if version.is_empty() {
            return pretty.to_owned();
        }
        return format!("{pretty} {version}");
    }
    if !trimmed.is_empty() {
        return trimmed.to_owned();
    }
    if !vendor.is_empty() {
        return vendor.to_owned();
    }
    "unknown".to_owned()
}

/// Human label for a network, for error messages.
fn network_label(n: Network) -> &'static str {
    n.name()
}

/// Map a lightwalletd `chain_name` (`"main"` / `"test"` / `"regtest"`) to a
/// [`Network`].
fn network_from_chain_name(chain_name: &str) -> Option<Network> {
    match chain_name {
        "main" => Some(Network::Main),
        "test" => Some(Network::Test),
        "regtest" => Some(Network::Regtest),
        _ => None,
    }
}

/// Confirm the connected lightwalletd is serving the chain we expect.
///
/// The network is part of a zkv address (the UFVK's HRP), and a database
/// adopts that network. If `--server` points at the *other* chain, we would
/// scan it and silently surface another chain's memos (and, in the worst case,
/// honor an INIT crafted for that chain). One `GetLightdInfo` call closes that
/// hole: a definitive `chain_name` mismatch is a hard error. An unrecognized
/// `chain_name` is only a warning; some servers don't report a standard name,
/// and we'd rather not break against them.
async fn verify_server_network(
    client: &mut CompactTxStreamerClient<Channel>,
    expected: Network,
) -> anyhow::Result<()> {
    let info = client
        .get_lightd_info(service::Empty {})
        .await
        .map_err(|e| anyhow!("could not fetch lightwalletd info to verify network: {e}"))?
        .into_inner();
    match network_from_chain_name(&info.chain_name) {
        Some(actual) if actual == expected => Ok(()),
        // zebra implements regtest as a configured testnet, so its
        // getblockchaininfo (and hence lightwalletd's chain_name) reports
        // "test" for a regtest chain. A regtest database therefore accepts a
        // "test" server; the mainnet refusal (the guard that protects real
        // funds) still holds, and pointing a regtest database at the public
        // testnet is harmless (different receiver domain, no memos decrypt).
        Some(Network::Test) if expected == Network::Regtest => Ok(()),
        Some(actual) => bail!(
            "lightwalletd is serving {} but this database is {}, refusing to \
             scan the wrong chain (server chain_name={:?})",
            network_label(actual),
            network_label(expected),
            info.chain_name,
        ),
        None => {
            tracing::warn!(
                chain_name = %info.chain_name,
                "lightwalletd reported an unrecognized chain_name; skipping network verification",
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod backend_label_tests {
    use super::backend_label;

    #[test]
    fn magicbean_becomes_zcashd_with_version() {
        assert_eq!(backend_label("/MagicBean:6.20.0/", ""), "zcashd 6.20.0");
    }

    #[test]
    fn zebra_kept_with_version() {
        assert_eq!(backend_label("/Zebra:2.1.0/", ""), "Zebra 2.1.0");
    }

    #[test]
    fn unknown_shape_falls_back_to_subversion_then_vendor() {
        assert_eq!(backend_label("weird", ""), "weird");
        assert_eq!(backend_label("", "ECC LightWalletD"), "ECC LightWalletD");
        assert_eq!(backend_label("", ""), "unknown");
    }

    #[test]
    fn name_without_version() {
        assert_eq!(backend_label("/Zebra:/", ""), "Zebra");
    }
}
