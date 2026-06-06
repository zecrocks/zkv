//! `zkv gui-browser`: serve the localhost web database browser in the
//! system browser.
//!
//! Thin CLI wrapper around [`zkv::gui::serve`]. The server binds the
//! loopback interface only and manages every local database, so the
//! global `--db` flag doesn't apply here. (`zkv gui` launches the native
//! desktop window instead; see [`crate::commands::gui`].)

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use clap::Args;

use crate::commands::connection_args::ConnectionCliArgs;
use zkv::gui::{serve, GuiConfig};

#[derive(Debug, Args)]
pub(crate) struct Command {
    /// Port to listen on (loopback only). Use 0 to pick a free port.
    #[arg(long, default_value_t = 8088)]
    port: u16,

    /// Don't try to open the system browser automatically.
    #[arg(long)]
    no_open: bool,

    #[command(flatten)]
    connection: ConnectionCliArgs,
}

impl Command {
    pub(crate) async fn run(self, _db: Option<String>) -> anyhow::Result<()> {
        let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), self.port);
        serve(GuiConfig {
            bind,
            conn: self.connection.into_inner(),
            open_browser: !self.no_open,
        })
        .await
    }
}
