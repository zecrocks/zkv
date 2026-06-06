//! CLI-side wrapper around [`zkv::remote::ConnectionArgs`].
//!
//! The library type is clap-free so external crates can construct connections
//! without inheriting a clap dependency. This wrapper re-adds the `clap::Args`
//! derive for the in-tree `zkv` binary.

use clap::Args;

use crate::remote::{parse_connection_mode, ConnectionArgs, ConnectionMode, Servers};

#[derive(Debug, Clone, Args)]
pub(crate) struct ConnectionCliArgs {
    /// The lightwalletd server to use. One of "ecc", "ywallet", "zecrocks", or a
    /// comma-separated list of `host:port`. Used for any network without a more
    /// specific override below.
    #[arg(short, long, default_value = "zecrocks", value_parser = Servers::parse)]
    pub server: Servers,

    /// Override the lightwalletd server for mainnet only (same format as
    /// `--server`); falls back to `--server` when unset.
    #[arg(long, value_parser = Servers::parse)]
    pub mainnet_server: Option<Servers>,

    /// Override the lightwalletd server for testnet only (same format as
    /// `--server`); falls back to `--server` when unset.
    #[arg(long, value_parser = Servers::parse)]
    pub testnet_server: Option<Servers>,

    /// Connection mode: "direct" (default) or "socks5://<host>:<port>".
    #[arg(long, default_value = "direct", value_parser = parse_connection_mode)]
    pub connection: ConnectionMode,
}

impl ConnectionCliArgs {
    pub(crate) fn into_inner(self) -> ConnectionArgs {
        ConnectionArgs {
            server: self.server,
            mainnet_server: self.mainnet_server,
            testnet_server: self.testnet_server,
            connection: self.connection,
        }
    }
}
