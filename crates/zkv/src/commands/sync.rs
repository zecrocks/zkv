use clap::Args;

use crate::{
    commands::connection_args::ConnectionCliArgs,
    config::WalletConfig,
    data::resolve_db,
    internal::{
        state::{load_state, INIT_CONFIRMATIONS},
        sync::read_sync_with_status,
    },
    remote::ConnectionMode,
    ui,
};

#[derive(Debug, Args)]
pub(crate) struct Command {
    #[command(flatten)]
    connection: ConnectionCliArgs,

    /// Delete the local wallet cache and re-scan from the birthday.
    ///
    /// The chain is the source of truth, so nothing is lost: every byte
    /// deleted is re-derived by the scan that follows. Reach for this when a
    /// database will not sync (a wallet cache left unmigratable by an upgrade,
    /// or corrupted by an interrupted write). It costs a full re-scan.
    #[arg(long)]
    rebuild: bool,
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

        // A rebuild has to happen with no node running: it deletes the very
        // files the node holds open, and re-creates the account the node then
        // checks its viewing-key pin against. Take the datadir lock to enforce
        // that rather than merely assume it: on Unix an unlinked file keeps
        // being written, so a GUI or second zkv syncing through this would
        // flush a second wallet into the directory after the wipe.
        //
        // The guard is scoped tightly and dropped before the engine opens
        // below: `flock` is not reentrant across handles, so the node could not
        // take a lock this process still held.
        if self.rebuild {
            // A member of the shared scan has no wallet files of its own to
            // wipe, and the shard it reads is shared with other databases, so
            // deleting it here would rescan everybody. `zkv fleet rebuild` is
            // the operation that means this for a fleet.
            if cfg.engine.is_fleet_member() {
                anyhow::bail!(
                    "{name:?} is served by the shared scan, so it has no wallet cache of its \
                     own to rebuild. Use `zkv fleet rebuild` to re-import the whole shared \
                     scan, or `zkv fleet leave {name}` to give this database files of its own."
                );
            }
            {
                let _lock = crate::engine::lock_datadir(&crate::data::db_dir(&name)?, &name)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                ui::warn("Deleting the local wallet cache and re-scanning from the birthday.");
                crate::internal::recover::wipe_sidecars(&name)?;
                crate::internal::recover::rebootstrap(&name, &connection).await?;
            }
        }

        // No tolerance: this command exists to catch up, so it does the work
        // even when a read at the same moment would have been entitled to skip.
        let engine = crate::engine::EngineRef::open(&name, &connection)?;
        let height = read_sync_with_status(&engine, &name, &connection, cfg.network, None).await?;

        // Report where we synced from: network, the server we picked, and the
        // transport. `pick` can't fail here (the sync above already connected
        // through it), but fall back to a bare line if it somehow does.
        let network = cfg.network.name().to_owned();
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
