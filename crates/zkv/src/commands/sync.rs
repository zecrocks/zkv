use clap::Args;

use crate::{
    commands::connection_args::ConnectionCliArgs,
    config::WalletConfig,
    data::resolve_db,
    internal::{
        state::{load_state, INIT_CONFIRMATIONS},
        sync::run_sync_with_status,
    },
    remote::ConnectionMode,
    ui,
};

#[derive(Debug, Args)]
pub(crate) struct Command {
    #[command(flatten)]
    connection: ConnectionCliArgs,
}

impl Command {
    pub(crate) async fn run(self, db: Option<String>) -> anyhow::Result<()> {
        let name = resolve_db(db.as_deref())?;
        let connection = self.connection.into_inner();
        // If the database disabled syncing for this client version, skip the
        // scan.
        if crate::commands::blocksync_skip(&name)? {
            return Ok(());
        }
        let cfg = WalletConfig::read(&name)?;
        let height = run_sync_with_status(&name, &connection, false).await?;

        // Report where we synced from: network, the server we picked, and the
        // transport. `pick` can't fail here (the sync above already connected
        // through it), but fall back to a bare line if it somehow does.
        let network = crate::data::Network::from(cfg.network).name().to_owned();
        match connection.server.pick(cfg.network) {
            Ok(server) => {
                let via = match &connection.connection {
                    ConnectionMode::Direct => "a direct connection".to_owned(),
                    ConnectionMode::SocksProxy(addr) => format!("a SOCKS5 proxy ({addr})"),
                };
                ui::success(format!(
                    "Synced to {network} height {height} {}",
                    ui::dim(&format!("using {server} via {via}")),
                ));
            }
            Err(_) => ui::success(format!("Synced to {network} height {height}")),
        }

        // `sync` is purely a read: it scans the chain and never broadcasts.
        // INIT broadcasting lives in `zkv init` (re-run it on a funded but
        // uninitialized database to finalize). The only side output here is an
        // advisory version-upgrade warning if the database now requires a newer
        // client epoch.
        let result = load_state(&name, INIT_CONFIRMATIONS, false)?;
        if let Some(warning) = result.version.upgrade_warning() {
            ui::warn(warning);
        }
        Ok(())
    }
}
