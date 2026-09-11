use clap::Args;

use crate::{
    commands::connection_args::ConnectionCliArgs,
    data::{db_dir, set_current_db},
    internal::protocol::{encode_ufvk_for_pool, network_from_type, parse_zkv_addr},
    ui,
};

#[derive(Debug, Args)]
pub(crate) struct Command {
    /// The zkv address to watch (a `zkv1…` token).
    pub(crate) zkv_addr: String,

    /// Local name for this database. Defaults to a slug derived from the UFVK.
    pub(crate) name: Option<String>,

    /// Give this database a wallet engine of its own instead of joining the
    /// shared scan: its own node, its own connection, its own pass over the
    /// blocks. Slower and heavier, and the only mode before the shared scan
    /// existed.
    #[arg(long)]
    pub(crate) standalone: bool,

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
        // `default` is the wallet name the engine always has, so a manifest by
        // that name collides with it and stops the fleet node starting at all.
        // Refuse early, with the fix, rather than at the next sync.
        if !self.standalone && name == "default" {
            anyhow::bail!(
                "the shared scan cannot serve a database named \"default\" (the wallet engine \
                 reserves that name). Pick another name, or pass --standalone."
            );
        }

        let connection = self.connection.into_inner();

        // The facade owns creation for both modes: it pins the birthday
        // against a fresh tip, writes keys.toml, and then either lays down this
        // database's own wallet files or enrols it in the shared scan.
        let db = if self.standalone {
            zkv::db::Database::init_watch_standalone(&name, &self.zkv_addr, connection.clone())
                .await?
        } else {
            zkv::db::Database::init_watch(&name, &self.zkv_addr, connection.clone()).await?
        };

        // Switch to the newly-watched database so follow-up commands target it.
        set_current_db(&name)?;
        ui::success(format!("Watching database {:?} (now current)", name));

        // Sync from the birthday now so the first `zkv get` is instant instead
        // of blocking on a cold scan.
        ui::hint(format!("Syncing from birthday {}…", parsed.birthday));
        match db.sync().await {
            Ok(_) => ui::hint("Run `zkv get` to fetch state."),
            // A member whose account the fleet node has not imported yet is the
            // ordinary first-run state, not a failure: the manifest is on disk
            // and the next sync pass picks it up.
            Err(zkv::db::ZkvError::Importing) => ui::hint(
                "Enrolled in the shared scan. It starts on the next sync pass; \
                 run `zkv fleet status` to watch it.",
            ),
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }
}
