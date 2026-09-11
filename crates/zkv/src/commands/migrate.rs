//! `zkv migrate`: adopt a database into the wallet engine, or report whether
//! it has been.
//!
//! Adoption is automatic: every path that opens the engine performs it, so
//! this command exists to make it explicit and inspectable rather than to
//! enable it. `--status` answers "has this database been adopted?" without
//! writing anything, which is the form scripts and the end-to-end tests want.
//!
//! See [`crate::engine::migrate`] for what adoption actually does. The short
//! version: it derives this database's viewing key and records it in
//! `keys.toml`. No wallet data is moved, nothing is deleted, and no network
//! is needed.
//!
//! Adoption is therefore reversible, but the upgrade as a whole is not, and
//! the boundary is worth saying out loud because it is not where you would
//! guess. The one-way step is the **first sync**, not this command: the node
//! relocates `data.sqlite`, `blockmeta.sqlite` and `blocks/` into
//! `<db>/zec/lrz/` when it starts, so from then on a pre-engine zkv build
//! looks at the database root, finds nothing, and reports the database as
//! having no key imported. Adopting says so once, so a user who wants a copy
//! of the old layout takes it before syncing rather than after.

use clap::Args;

use crate::{
    config::WalletConfig,
    data,
    engine::{migrate, Adoption},
    ui,
};

#[derive(Debug, Args)]
pub(crate) struct Command {
    /// Report whether the database has been adopted, without changing it.
    #[arg(long)]
    status: bool,

    /// Print the report as JSON.
    #[arg(long, value_name = "FORMAT")]
    output: Option<String>,
}

impl Command {
    pub(crate) fn run(self, db: Option<String>) -> anyhow::Result<()> {
        let name = data::resolve_db(db.as_deref())?;
        let json = matches!(self.output.as_deref(), Some("json"));

        if self.status {
            let cfg = WalletConfig::read(&name)?;
            let unmoved = data::engine_dir(&name)? == data::db_dir(&name)?;
            return report(&name, cfg.ufvk.is_some(), false, unmoved, json);
        }

        let mut cfg = WalletConfig::read(&name)?;
        // Whether the wallet files are still at the database root, checked
        // *before* adopting (adoption itself moves nothing, but reading it
        // first keeps the two questions independent). zkv answers this by
        // looking, through `data`, which already resolves the layout for every
        // read path: `zecd::migrate::awaits_migration` is the upstream
        // equivalent, but naming it here would breach the engine seam. The two
        // are pinned against each other by a test inside the seam
        // (`engine::migrate::tests::the_downgradable_check_agrees_with_zecds_own`),
        // which is what keeps zkv from reporting a database as downgradable
        // after a future zecd had moved the files somewhere else.
        let unmoved = data::engine_dir(&name)? == data::db_dir(&name)?;
        let outcome = migrate::ensure_adopted(&name, &mut cfg)?;
        report(
            &name,
            true,
            matches!(outcome, Adoption::Pinned),
            unmoved,
            json,
        )
    }
}

/// Print the adoption state of a database.
///
/// `changed` distinguishes "was already adopted" from "adopted just now", so
/// a repeat run reads as the no-op it is.
fn report(
    name: &str,
    adopted: bool,
    changed: bool,
    unmoved: bool,
    json: bool,
) -> anyhow::Result<()> {
    if json {
        // Stdout is reserved for machine-readable output.
        println!(
            "{}",
            serde_json::json!({
                "database": name,
                "adopted": adopted,
                "changed": changed,
                // False once a node has run and relocated the wallet files,
                // which is the point of no return for a downgrade.
                "downgradable": unmoved,
            })
        );
        return Ok(());
    }

    match (adopted, changed) {
        (true, true) => {
            ui::success(format!("Adopted {name:?} into the wallet engine"));
            // Only while the files are still where an older build would look
            // for them. Once they have moved the warning is advice about a
            // door that has already closed, which is just noise.
            if unmoved {
                ui::warn(
                    "The next sync moves this database's wallet files into a \
                     subdirectory. That step is one-way: an older zkv build \
                     will not find them afterwards. Copy the database \
                     directory first if you may want to go back.",
                );
            }
        }
        (true, false) => ui::success(format!("{name:?} is already adopted")),
        (false, _) => eprintln!(
            "{name:?} has not been adopted yet; run `zkv migrate` (or any command that syncs)"
        ),
    }
    Ok(())
}
