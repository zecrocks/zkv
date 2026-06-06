use clap::{Args, Subcommand};

use crate::{commands::connection_args::ConnectionCliArgs, data::resolve_db, db::Database, ui};

/// `zkv admin <action>`: owner-only database-administration actions.
///
/// `finalize` permanently seals the database; `sign` and `verify` are the
/// offline memo-signing and memo-verification tools.
#[derive(Debug, Args)]
pub(crate) struct Command {
    #[command(subcommand)]
    action: Action,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Permanently seal the database against all future writes. Owner-only.
    Finalize(FinalizeArgs),

    /// Sign a memo for any write opcode and print it without broadcasting.
    Sign(crate::commands::sign::Command),

    /// Verify the signature on a raw zkv memo (full check against an imported db).
    Verify(crate::commands::verify::Command),
}

#[derive(Debug, Args)]
struct FinalizeArgs {
    /// Skip the confirmation prompt.
    #[arg(short, long)]
    yes: bool,

    #[command(flatten)]
    connection: ConnectionCliArgs,
}

impl Command {
    pub(crate) async fn run(self, db: Option<String>) -> anyhow::Result<()> {
        match self.action {
            Action::Finalize(a) => a.run(db).await,
            Action::Sign(c) => c.run(db),
            Action::Verify(c) => c.run(db).await,
        }
    }
}

impl FinalizeArgs {
    async fn run(self, db: Option<String>) -> anyhow::Result<()> {
        let name = resolve_db(db.as_deref())?;
        let connection = self.connection.into_inner();
        let database = Database::open(&name, connection)?;

        if !self.yes {
            ui::warn(format!(
                "Finalize database {name:?}? This permanently seals it, no further \
                 writes will EVER be possible, by anyone. This cannot be undone."
            ));
            eprint!("{}", ui::bold("Type 'y' to confirm [y/N]: "));
            use std::io::{BufRead, Write};
            let _ = std::io::stderr().flush();
            let mut line = String::new();
            std::io::stdin().lock().read_line(&mut line)?;
            if line.trim().to_lowercase() != "y" {
                ui::failure("Aborted.");
                return Ok(());
            }
        }

        ui::arrow(format!(
            "{}  {}",
            ui::bold("FINALIZE"),
            ui::dim("broadcasting…")
        ));
        let txid = database.finalize().await?;
        ui::success(format!("broadcast tx {}", ui::short_hash(&txid)));
        println!("{txid}");
        Ok(())
    }
}
