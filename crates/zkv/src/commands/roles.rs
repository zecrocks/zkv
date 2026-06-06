use std::io::IsTerminal;

use clap::{Args, Subcommand};

use crate::{
    commands::connection_args::ConnectionCliArgs,
    data::resolve_db,
    db::{Database, RevokedRole},
};

/// `zkv roles [owner|writer] …`: inspect and manage the authorization registry.
///
/// With no subcommand, lists the confirmed owners and scoped writers (the
/// default, auto-syncing first unless `--offline`). The `owner` and `writer`
/// subcommands grant/revoke authority, both owner-only.
#[derive(Debug, Args)]
pub(crate) struct Command {
    #[command(subcommand)]
    action: Option<Action>,

    /// Listing options (used when no subcommand is given).
    #[command(flatten)]
    list: ListArgs,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Manage database owners (add/remove). Owner-only.
    Owner(super::owner::Command),
    /// Manage scoped writers (add/remove). Owner-only.
    Writer(super::writer::Command),
}

#[derive(Debug, Args)]
struct ListArgs {
    #[command(flatten)]
    connection: ConnectionCliArgs,

    /// Don't sync first; report over last-known state.
    #[arg(long)]
    offline: bool,

    /// Minimum confirmations for a management memo to count.
    #[arg(short = 'c', long, default_value_t = 3)]
    confirmations: u32,
}

impl Command {
    pub(crate) async fn run(self, db: Option<String>) -> anyhow::Result<()> {
        match self.action {
            Some(Action::Owner(c)) => c.run(db).await,
            Some(Action::Writer(c)) => c.run(db).await,
            None => self.list.run(db).await,
        }
    }
}

impl ListArgs {
    async fn run(self, db: Option<String>) -> anyhow::Result<()> {
        let name = resolve_db(db.as_deref())?;
        let connection = self.connection.into_inner();
        let database = Database::open(&name, connection)?;

        if !self.offline {
            // Facade sync() honors `blocksync` on its own (skips the scan).
            database.sync().await?;
        }

        // Gate the listing if the database requires a newer client (blockread),
        // and surface any upgrade warning. A cheap extra replay; `roles` isn't
        // a hot path.
        crate::commands::gate_read(&database.version(self.confirmations)?, &name)?;

        let auth = database.roles(self.confirmations)?;
        let revoked = database.revoked_roles(self.confirmations)?;
        if auth.is_empty() && revoked.is_empty() {
            eprintln!(
                "(no roles, database not initialized at confirmations={})",
                self.confirmations
            );
            return Ok(());
        }

        let creator = database.signer().ok();
        let owners: Vec<&str> = auth.owners().collect();
        let writers: Vec<(&str, String)> = auth.writers().map(|(w, s)| (w, s.to_wire())).collect();
        let finalized = database.is_finalized(self.confirmations)?;

        // Interactive terminals get a grouped, coloured layout; piped/redirected
        // stdout keeps the stable one-record-per-line machine format so scripts
        // (and `--output`-free consumers) don't break.
        if std::io::stdout().is_terminal() {
            print_pretty(creator.as_deref(), &owners, &writers, &revoked, finalized);
        } else {
            print_raw(creator.as_deref(), &owners, &writers, &revoked, finalized);
        }
        Ok(())
    }
}

/// The stable machine format: `creator`/`owner`/`writer`/`revoked-*`/`finalized`,
/// one record per line, `<pubkey>` in canonical `zkvid1…` form.
fn print_raw(
    creator: Option<&str>,
    owners: &[&str],
    writers: &[(&str, String)],
    revoked: &[RevokedRole],
    finalized: bool,
) {
    if let Some(creator) = creator {
        println!("creator {creator}");
    }
    for owner in owners {
        println!("owner {owner}");
    }
    for (writer, scope) in writers {
        println!("writer {writer} {scope}");
    }
    for r in revoked {
        let height = r
            .height
            .map(|h| h.to_string())
            .unwrap_or_else(|| "-".to_owned());
        let by = r.revoked_by.as_deref().unwrap_or("-");
        if r.was_owner {
            println!("revoked-owner {} {height} {by}", r.pubkey);
        } else {
            let scope = if r.capabilities.is_empty() {
                "-".to_owned()
            } else {
                r.capabilities.join(",")
            };
            println!("revoked-writer {} {scope} {height} {by}", r.pubkey);
        }
    }
    if finalized {
        println!("finalized");
    }
}

/// Human-facing grouped layout for an interactive terminal.
fn print_pretty(
    creator: Option<&str>,
    owners: &[&str],
    writers: &[(&str, String)],
    revoked: &[RevokedRole],
    finalized: bool,
) {
    use crate::ui::out;

    println!("{} ({})", out::header("Owners"), owners.len());
    for owner in owners {
        if Some(*owner) == creator {
            println!("  {}  {}", owner, out::cyan("· creator"));
        } else {
            println!("  {owner}");
        }
    }
    // The creator is a permanent trait; if its owner authority was revoked it
    // won't appear above, so note it separately rather than dropping it.
    if let Some(creator) = creator {
        if !owners.contains(&creator) {
            println!(
                "  {}  {}",
                out::dim(creator),
                out::dim("· creator (owner authority revoked)"),
            );
        }
    }

    println!();
    println!("{} ({})", out::header("Writers"), writers.len());
    if writers.is_empty() {
        println!("  {}", out::dim("(none)"));
    } else {
        for (writer, scope) in writers {
            println!("  {}  {}", writer, out::cyan(scope));
        }
    }

    if !revoked.is_empty() {
        println!();
        println!("{} ({})", out::header("Revoked"), revoked.len());
        for r in revoked {
            let kind = if r.was_owner { "owner " } else { "writer" };
            let scope = if r.was_owner || r.capabilities.is_empty() {
                String::new()
            } else {
                format!("  [{}]", r.capabilities.join(","))
            };
            let mut detail = match r.height {
                Some(h) => format!("· revoked at {h}"),
                None => "· revoked".to_owned(),
            };
            if let Some(by) = &r.revoked_by {
                detail.push_str(&format!(" by {by}"));
            }
            println!(
                "  {} {}{}  {}",
                out::dim(kind),
                r.pubkey,
                out::dim(&scope),
                out::dim(&detail),
            );
        }
    }

    if finalized {
        println!();
        println!(
            "{}",
            out::yellow("● Finalized, permanently sealed against all future writes."),
        );
    }
}
