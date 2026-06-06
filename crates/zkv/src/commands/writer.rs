use clap::{Args, Subcommand};

use crate::{
    commands::connection_args::ConnectionCliArgs,
    data::resolve_db,
    db::{Database, Scope},
    ui,
};

/// `zkv roles writer <add|remove>`: manage scoped writers.
///
/// Owner-only. A writer may write only within its scope (`CREATE`, `UPDATE`,
/// `DESTROY`; "CRUD minus R", since reads are public). `add` sets the scope
/// wholesale; a later `add` for the same key replaces it. `remove` revokes
/// the writer entirely.
#[derive(Debug, Args)]
pub(crate) struct Command {
    #[command(subcommand)]
    action: Action,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Grant (or overwrite) a writer with the given capability scope.
    Add(AddArgs),
    /// Revoke a writer entirely.
    Remove(RemoveArgs),
}

#[derive(Debug, Args)]
struct AddArgs {
    /// The writer's public key: a zkvid1… key (canonical) or raw compressed hex.
    pubkey: String,

    /// Capability scope: a comma-separated subset of CREATE,UPDATE,DESTROY.
    /// CREATE = set a new key; UPDATE = overwrite an existing key;
    /// DESTROY = delete a key.
    scope: String,

    #[command(flatten)]
    connection: ConnectionCliArgs,
}

#[derive(Debug, Args)]
struct RemoveArgs {
    /// The writer's public key (a zkvid1… key or raw compressed hex).
    pubkey: String,

    #[command(flatten)]
    connection: ConnectionCliArgs,
}

impl Command {
    pub(crate) async fn run(self, db: Option<String>) -> anyhow::Result<()> {
        let name = resolve_db(db.as_deref())?;
        match self.action {
            Action::Add(a) => {
                let scope = Scope::parse(&a.scope).ok_or_else(|| {
                    anyhow::anyhow!(
                        "invalid scope {:?}: expected a comma-separated subset of \
                         CREATE,UPDATE,DESTROY",
                        a.scope,
                    )
                })?;
                let database = Database::open(&name, a.connection.into_inner())?;
                ui::arrow(format!(
                    "{} {} [{}]  {}",
                    ui::bold("WRITERSET"),
                    a.pubkey,
                    scope.to_wire(),
                    ui::dim("broadcasting…"),
                ));
                let txid = database.grant_writer(&a.pubkey, &scope).await?;
                ui::success(format!("broadcast tx {}", ui::short_hash(&txid)));
                println!("{txid}");
            }
            Action::Remove(a) => {
                let database = Database::open(&name, a.connection.into_inner())?;
                ui::arrow(format!(
                    "{} {}  {}",
                    ui::bold("WRITERDEL"),
                    a.pubkey,
                    ui::dim("broadcasting…"),
                ));
                let txid = database.revoke_writer(&a.pubkey).await?;
                ui::success(format!("broadcast tx {}", ui::short_hash(&txid)));
                println!("{txid}");
            }
        }
        Ok(())
    }
}
