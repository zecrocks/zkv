use clap::{Args, Subcommand};

use crate::{commands::connection_args::ConnectionCliArgs, data::resolve_db, db::Database, ui};

/// `zkv roles owner <add|remove> <pubkey>`: manage database owners.
///
/// Owner-only. An owner can write any key, add/remove owners, and add/remove
/// scoped writers. The root (UFVK-derived) signer is owner #1 from INIT. The
/// last remaining owner cannot be removed.
#[derive(Debug, Args)]
pub(crate) struct Command {
    #[command(subcommand)]
    action: Action,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Grant owner authority to a secp256k1 public key.
    Add(OwnerArgs),
    /// Revoke owner authority from a public key.
    Remove(OwnerArgs),
}

#[derive(Debug, Args)]
struct OwnerArgs {
    /// The owner's public key: a zkvid1… key (canonical) or raw compressed hex.
    pubkey: String,

    #[command(flatten)]
    connection: ConnectionCliArgs,
}

impl Command {
    pub(crate) async fn run(self, db: Option<String>) -> anyhow::Result<()> {
        let (verb, args, grant) = match self.action {
            Action::Add(a) => ("OWNERADD", a, true),
            Action::Remove(a) => ("OWNERDEL", a, false),
        };
        let name = resolve_db(db.as_deref())?;
        let connection = args.connection.into_inner();
        let database = Database::open(&name, connection)?;
        ui::arrow(format!(
            "{} {}  {}",
            ui::bold(verb),
            args.pubkey,
            ui::dim("broadcasting…"),
        ));
        let txid = if grant {
            database.grant_owner(&args.pubkey).await?
        } else {
            database.revoke_owner(&args.pubkey).await?
        };
        ui::success(format!("broadcast tx {}", ui::short_hash(&txid)));
        println!("{txid}");
        Ok(())
    }
}
