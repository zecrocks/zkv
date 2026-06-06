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
use zcash_protocol::consensus::Network;

use crate::socks::SocksConnector;

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
            (ServerOperator::Ecc, Network::MainNetwork) => &[],
            (ServerOperator::Ecc, Network::TestNetwork) => ECC_TESTNET,
            (ServerOperator::YWallet, Network::MainNetwork) => YWALLET_MAINNET,
            (ServerOperator::YWallet, Network::TestNetwork) => &[],
            (ServerOperator::ZecRocks, Network::MainNetwork) => ZEC_ROCKS_MAINNET,
            (ServerOperator::ZecRocks, Network::TestNetwork) => ZEC_ROCKS_TESTNET,
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
            Servers::Hosted(server_operator) => server_operator
                .servers(network)
                .first()
                .ok_or(anyhow!("{:?} doesn't serve {:?}", server_operator, network)),
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

        let channel = Channel::from_shared(self.endpoint())?;
        let channel = if self.use_tls() {
            channel.tls_config(
                ClientTlsConfig::new()
                    .domain_name(self.host.to_string())
                    .assume_http2(true)
                    .with_webpki_roots(),
            )?
        } else {
            channel
        };

        Ok(CompactTxStreamerClient::new(channel.connect().await?))
    }

    pub async fn connect_over_socks(
        &self,
        proxy_addr: SocketAddr,
    ) -> anyhow::Result<CompactTxStreamerClient<Channel>> {
        info!("Connecting to {} via SOCKS proxy {}", self, proxy_addr);

        let connector = SocksConnector::new(proxy_addr);
        let uri: Uri = self.endpoint().parse()?;

        let mut endpoint = Endpoint::from(uri.clone())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30));

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
            Network::MainNetwork => self.mainnet_server.as_ref().unwrap_or(&self.server),
            Network::TestNetwork => self.testnet_server.as_ref().unwrap_or(&self.server),
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
    match n {
        Network::MainNetwork => "mainnet",
        Network::TestNetwork => "testnet",
    }
}

/// Map a lightwalletd `chain_name` (`"main"` / `"test"`) to a [`Network`].
fn network_from_chain_name(chain_name: &str) -> Option<Network> {
    match chain_name {
        "main" => Some(Network::MainNetwork),
        "test" => Some(Network::TestNetwork),
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
