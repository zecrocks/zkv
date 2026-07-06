use clap::Args;
use zcash_client_backend::data_api::{AccountPurpose, WalletWrite};

use crate::{
    commands::connection_args::ConnectionCliArgs,
    config::WalletConfig,
    data::{db_dir, init_dbs, set_current_db},
    internal::{
        protocol::{encode_ufvk_for_pool, network_from_type, parse_zkv_addr},
        sync::run_sync_read,
    },
    ui,
};

#[derive(Debug, Args)]
pub(crate) struct Command {
    /// The zkv address to watch (a `zkv1…` token).
    pub(crate) zkv_addr: String,

    /// Local name for this database. Defaults to a slug derived from the UFVK.
    pub(crate) name: Option<String>,

    #[command(flatten)]
    pub(crate) connection: ConnectionCliArgs,
}

impl Command {
    pub(crate) async fn run(self, _db: Option<String>) -> anyhow::Result<()> {
        let parsed = parse_zkv_addr(&self.zkv_addr)?;
        let network = network_from_type(parsed.network)?;

        // Refuse to import the same database twice under a different name.
        if let Some(existing) = zkv::db::find_duplicate_watch_database(&self.zkv_addr)? {
            anyhow::bail!(
                "this database is already imported as {existing:?}; \
                 switch to it with `zkv use {existing}` instead of importing it again"
            );
        }

        let name = self.name.clone().unwrap_or_else(|| {
            // Derive a short name from the first ~10 chars of the UFVK string.
            let ufvk_str = encode_ufvk_for_pool(&parsed.ufvk, &network, parsed.pool);
            let suffix: String = ufvk_str.chars().skip(7).take(8).collect();
            format!("watch-{suffix}")
        });

        let dir = db_dir(&name)?;
        if dir.join("keys.toml").exists() {
            anyhow::bail!("database {name:?} already exists at {}", dir.display());
        }

        let connection = self.connection.into_inner();

        // The birthday is carried by the address, so pin it verbatim (no
        // buffer). Refuses a stale/unreachable tip before building the db.
        let mut client = connection.connect(network).await?;
        let birthday =
            crate::internal::sync::pinned_birthday(&mut client, network, parsed.birthday).await?;

        WalletConfig::init_watch(
            &name,
            birthday.height(),
            network,
            &self.zkv_addr,
            parsed.pool,
        )?;

        let mut db_data = init_dbs(network, &name)?;
        db_data.import_account_ufvk(
            &name,
            &parsed.ufvk,
            &birthday,
            AccountPurpose::ViewOnly,
            None,
        )?;

        // Switch to the newly-watched database so follow-up commands target it.
        set_current_db(&name)?;

        ui::success(format!("Watching database {:?} (now current)", name));

        // Sync from the birthday now so the first `zkv get` is instant instead
        // of blocking on a cold scan.
        ui::hint(format!("Syncing from birthday {}…", parsed.birthday));
        run_sync_read(&name, &connection, false).await?;
        ui::hint("Run `zkv get` to fetch state.");
        Ok(())
    }
}
