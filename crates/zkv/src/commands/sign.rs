use anyhow::anyhow;
use clap::{Args, Subcommand};

use crate::{
    data::resolve_db,
    db::Scope,
    internal::{
        protocol::Op,
        write::{manage_and_print, write_and_print},
    },
};

/// `zkv sign <op> …`: sign a memo and print it without broadcasting.
///
/// Produces the exact signed memo text for any write opcode so it can be
/// relayed through a separately-funded wallet (or the faucet). Authorization
/// is enforced at sign time exactly as it would be on broadcast, so an
/// unauthorized signer fails fast instead of emitting a memo readers drop.
#[derive(Debug, Args)]
pub(crate) struct Command {
    #[command(subcommand)]
    action: Action,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Sign a SET memo (auto-promotes to SETL for empty/newline values).
    Set(SetArgs),
    /// Sign a DEL memo.
    Del(DelArgs),
    /// Sign an owner-management memo (add/remove). Owner-only.
    Owner(OwnerArgs),
    /// Sign a scoped-writer memo (add/remove). Owner-only.
    Writer(WriterArgs),
    /// Sign a FINALIZE memo (permanently seal the database). Owner-only.
    Finalize,
}

#[derive(Debug, Args)]
struct SetArgs {
    /// The key to set.
    key: String,
    /// The value to set.
    value: String,
}

#[derive(Debug, Args)]
struct DelArgs {
    /// The key to delete.
    key: String,
}

#[derive(Debug, Args)]
struct OwnerArgs {
    #[command(subcommand)]
    action: OwnerAction,
}

#[derive(Debug, Subcommand)]
enum OwnerAction {
    /// Sign an OWNERADD (grant/re-affirm owner authority).
    Add { pubkey: String },
    /// Sign an OWNERDEL (revoke owner authority).
    Remove { pubkey: String },
}

#[derive(Debug, Args)]
struct WriterArgs {
    #[command(subcommand)]
    action: WriterAction,
}

#[derive(Debug, Subcommand)]
enum WriterAction {
    /// Sign a WRITERADD (grant/overwrite a scoped writer).
    Add {
        pubkey: String,
        /// Capability scope: a comma-separated subset of CREATE,UPDATE,DESTROY.
        scope: String,
    },
    /// Sign a WRITERDEL (revoke a writer entirely).
    Remove { pubkey: String },
}

impl Command {
    pub(crate) fn run(self, db: Option<String>) -> anyhow::Result<()> {
        let name = resolve_db(db.as_deref())?;
        match self.action {
            Action::Set(a) => {
                // Auto-promote to SETL for empty / newline-containing values;
                // see `commands/set.rs` for the rationale.
                let op = Op::set_for_value(&a.value);
                write_and_print(&name, op, &a.key, Some(&a.value))
            }
            Action::Del(a) => write_and_print(&name, Op::Del, &a.key, None),
            Action::Owner(o) => match o.action {
                OwnerAction::Add { pubkey } => manage_and_print(&name, Op::OwnerAdd, &pubkey, None),
                OwnerAction::Remove { pubkey } => {
                    manage_and_print(&name, Op::OwnerDel, &pubkey, None)
                }
            },
            Action::Writer(w) => match w.action {
                WriterAction::Add { pubkey, scope } => {
                    let scope = Scope::parse(&scope).ok_or_else(|| {
                        anyhow!(
                            "invalid scope {scope:?}: expected a comma-separated subset of \
                             CREATE,UPDATE,DESTROY"
                        )
                    })?;
                    manage_and_print(&name, Op::WriterAdd, &pubkey, Some(&scope.to_wire()))
                }
                WriterAction::Remove { pubkey } => {
                    manage_and_print(&name, Op::WriterDel, &pubkey, None)
                }
            },
            Action::Finalize => manage_and_print(&name, Op::Finalize, "", None),
        }
    }
}
