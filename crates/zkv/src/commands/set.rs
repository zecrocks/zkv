use clap::Args;

use crate::{
    commands::connection_args::ConnectionCliArgs,
    config::Role,
    data::resolve_db,
    db::{Database, ZkvError},
    ui,
};

#[derive(Debug, Args)]
pub(crate) struct Command {
    /// The key to set.
    key: String,

    /// The value to set.
    value: String,

    #[command(flatten)]
    connection: ConnectionCliArgs,

    /// Skip the pre-broadcast sync (still broadcasts immediately, just
    /// doesn't refresh the wallet first). Use when you control sync timing.
    #[arg(long = "no-sync", alias = "offline")]
    no_sync: bool,
}

impl Command {
    pub(crate) async fn run(self, db: Option<String>) -> anyhow::Result<()> {
        let name = resolve_db(db.as_deref())?;
        let connection = self.connection.into_inner();
        let database = Database::open(&name, connection)?;
        // Fail fast on watch-only before printing the broadcast status line, so
        // the only output is the error (not a stray "broadcasting…" line).
        if database.role() != Role::Admin {
            return Err(ZkvError::WatchOnly.into());
        }
        ui::arrow(format!(
            "{} {}  {}",
            ui::bold("SET"),
            self.key,
            ui::dim("broadcasting…"),
        ));
        let txid = if self.no_sync {
            database.set_no_sync(&self.key, &self.value).await?
        } else {
            database.set(&self.key, &self.value).await?
        };
        ui::success(format!("broadcast tx {}", ui::short_hash(&txid)));
        println!("{txid}");
        Ok(())
    }
}
