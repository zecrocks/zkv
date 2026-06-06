//! The bundled "demo-oracles" watch-only database.
//!
//! A fresh zkv install ships with a single read-only demo database so a new
//! user has something to look at before creating their own. It is a testnet,
//! watch-only database pointed at a public oracle feed (it holds no spending
//! key, so it can never write or spend).
//!
//! It is auto-provisioned **exactly once**: the first run that reaches this
//! code creates it, then drops a marker file in the data directory so later
//! runs never re-add it. That makes deleting the demo durable, a user who
//! removes it does not get it silently recreated. The GUI Settings screen
//! offers a manual "Re-import Oracle Demo" button instead (see
//! `should_offer_reimport` and `gui::Engine::reimport_demo`).
//!
//! Provisioning is **two-phase** so the database appears in the list without
//! waiting on the network:
//! * phase 1 (`write_config_and_init`): write `keys.toml` and create the
//!   (empty) wallet DBs. Pure local work (the birthday comes from the
//!   address), so it is instant and the database shows up in [`data::list_dbs`]
//!   right away. It also claims the `current` marker if none is set, so the
//!   demo is the default selection until the user makes their own database.
//! * phase 2 (`import_account`): fetch the birthday treestate from the chain
//!   and import the viewing account, so the database can sync. Idempotent, so
//!   an interrupted first attempt is finished on the next run.
//!
//! Both the CLI (`main`) and the GUI (the `Engine` auto-sync loop) call
//! `ensure` on startup, so the demo appears across both front ends.

use std::path::PathBuf;

use zcash_client_backend::data_api::{AccountPurpose, WalletRead, WalletWrite};
use zcash_protocol::consensus::BlockHeight;

use crate::config::WalletConfig;
use crate::data;
use crate::internal::protocol::{network_from_type, parse_zkv_addr};
use crate::remote::ConnectionArgs;

/// Local name of the bundled demo database. Within
/// [`data::validate_db_name`]'s rules (ASCII, <= 24 chars).
pub const DEMO_DB_NAME: &str = "demo-oracles";

/// The testnet zkv address the demo database watches (network + birthday are
/// carried inside the address itself).
pub const DEMO_ZKV_ADDRESS: &str = "zkvtest1p3ertmjzsdccjjstmkud0cvx5mnm9kgl557qnjnewfh5pc5nxzmpu9s3pq724yz2dzddp5gq8q73vjm3ruluway08e379zsevzghg3cuny3pnm0sakml78sfn6v2c9t0qwu8c40mrpcjllntt84p2zdv4c23a6ufumh775f7l84zkeyf46jq3hq45kadlt7hjatqv0jymj6thkrxytd82j622w6kgscn6uknhvphxgu75mmj2z50q4qkmn85gcrmanjzp3zwtnyjml3ywc594srjzm0sqwcrx42cqly5tp7szyrxpptzja8luvj0u8t8lztawenek3v4p5lyhhp2j3eekj0sg6zkgrgf2plj8cw";

/// Marker file (at the data-dir root) recording that the one-time
/// auto-provision of the demo database has already run. Its presence means
/// "do not auto-provision again", so deleting the demo database is durable.
/// The leading dot keeps it out of [`data::list_dbs`] (which skips dotfiles
/// and non-directories).
const PROVISIONED_MARKER: &str = ".demo-oracles-provisioned";

/// Path to the one-time-provision marker file.
fn marker_path() -> anyhow::Result<PathBuf> {
    Ok(data::zkv_data()?.join(PROVISIONED_MARKER))
}

/// Whether the one-time auto-provision has already run (the marker exists).
pub fn was_provisioned() -> bool {
    marker_path().map(|p| p.exists()).unwrap_or(false)
}

/// Whether the demo database currently exists on disk.
pub fn exists() -> bool {
    data::list_dbs()
        .map(|names| names.iter().any(|n| n == DEMO_DB_NAME))
        .unwrap_or(false)
}

/// Whether the on-disk database named `demo-oracles` is actually *our* demo (a
/// watch-only database pointed at [`DEMO_ZKV_ADDRESS`]), as opposed to a
/// user's own database that happens to share the name. We never modify or
/// delete a database that isn't ours.
fn is_our_demo() -> bool {
    WalletConfig::read(DEMO_DB_NAME)
        .ok()
        .map(|c| c.zkv_address.as_deref() == Some(DEMO_ZKV_ADDRESS))
        .unwrap_or(false)
}

/// Record that the demo has been provisioned, so it is never auto-added again.
pub fn mark_provisioned() -> anyhow::Result<()> {
    std::fs::write(marker_path()?, b"")?;
    Ok(())
}

/// Whether the GUI should offer the manual "Re-import Oracle Demo" button:
/// the demo was provisioned at some point (so this user had it) but is gone
/// now (they deleted it).
pub fn should_offer_reimport() -> bool {
    was_provisioned() && !exists()
}

/// Set `name` as the current database, but only if it should win the slot:
/// when there is no current database, or when the current one is the bundled
/// demo (which only holds the slot provisionally, until the user has a
/// database of their own). An existing *real* selection is left untouched.
/// Used by the `init` / `restore` / watch-import paths so a user's first real
/// database supersedes the demo as `current`.
pub fn promote_current(name: &str) -> anyhow::Result<()> {
    match data::current_db()? {
        None => data::set_current_db(name),
        Some(cur) if cur == DEMO_DB_NAME => data::set_current_db(name),
        Some(_) => Ok(()),
    }
}

/// Phase 1: write the demo's `keys.toml` and create its (empty) wallet DBs.
/// Pure local work, so it is instant and the database is listable immediately.
/// Does not import the viewing account (that needs the chain; see
/// `import_account`). Claims the `current` marker if none is set so the demo
/// is the default selection out of the box.
fn write_config_and_init() -> anyhow::Result<()> {
    let parsed = parse_zkv_addr(DEMO_ZKV_ADDRESS)?;
    let network = network_from_type(parsed.network)?;
    WalletConfig::init_watch(
        DEMO_DB_NAME,
        BlockHeight::from(parsed.birthday),
        network,
        DEMO_ZKV_ADDRESS,
        parsed.pool,
    )?;
    // Create the (empty) wallet DBs so `Database::open` succeeds during the
    // gap before the account import lands (the detail view reads gracefully as
    // an empty, not-yet-synced database rather than erroring).
    let _ = data::init_dbs(network, DEMO_DB_NAME)?;
    data::set_current_db_if_unset(DEMO_DB_NAME)?;
    Ok(())
}

/// Phase 2: fetch the birthday treestate from the chain and import the viewing
/// account so the demo can sync. Idempotent: a no-op if the account is already
/// imported, so an interrupted first attempt is safely finished on a later run.
async fn import_account(conn: &ConnectionArgs) -> anyhow::Result<()> {
    let parsed = parse_zkv_addr(DEMO_ZKV_ADDRESS)?;
    let network = network_from_type(parsed.network)?;
    let (_, data_path) = data::get_db_paths(DEMO_DB_NAME)?;

    // Already imported? Then there is nothing to do.
    if let Ok(db) = data::open_wallet_db(&data_path, network) {
        if !db.get_account_ids()?.is_empty() {
            return Ok(());
        }
    }

    // Birthday is carried by the demo address, so pin it verbatim (no buffer).
    // Refuses a stale/unreachable tip before importing the account.
    let mut client = conn.connect(network).await?;
    let birthday = crate::internal::sync::pinned_birthday(&mut client, parsed.birthday).await?;

    let mut db = data::open_wallet_db(&data_path, network)?;
    db.import_account_ufvk(
        DEMO_DB_NAME,
        &parsed.ufvk,
        &birthday,
        AccountPurpose::ViewOnly,
        None,
    )
    .map_err(|e| anyhow::anyhow!("import demo account: {e:?}"))?;
    Ok(())
}

/// One-time auto-provision of the demo database, called on startup by both the
/// CLI and the GUI. Best-effort and idempotent:
///
/// * If the marker is already set, do nothing (returns `Ok(false)`): the
///   steady-state cheap path on every later run, and what makes a user's
///   deletion stick.
/// * If a database of this name already exists but isn't ours (the user named
///   their own database `demo-oracles`), just set the marker and leave it
///   alone. If it *is* our demo from an interrupted earlier attempt, finish
///   the account import.
/// * Otherwise run phase 1 (instant, local) then phase 2 (network). The marker
///   is written **only** after the account import succeeds, so a failed
///   attempt (e.g. offline) is retried on the next run.
///
/// Returns `Ok(true)` when it newly created the demo database this call.
pub async fn ensure(conn: ConnectionArgs) -> anyhow::Result<bool> {
    if was_provisioned() {
        return Ok(false);
    }
    if exists() {
        if is_our_demo() {
            import_account(&conn).await?;
        }
        mark_provisioned()?;
        return Ok(false);
    }
    // Phase 1 makes it listable + current immediately; phase 2 imports the
    // viewing account so it can sync.
    write_config_and_init()?;
    import_account(&conn).await?;
    mark_provisioned()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_name_is_valid() {
        data::validate_db_name(DEMO_DB_NAME).expect("demo db name must be valid");
    }

    #[test]
    fn demo_address_parses_as_testnet() {
        use zcash_protocol::consensus::Network;

        let parsed = parse_zkv_addr(DEMO_ZKV_ADDRESS).expect("demo address must parse");
        let network =
            network_from_type(parsed.network).expect("demo address network must be known");
        assert!(
            matches!(network, Network::TestNetwork),
            "demo address should be testnet"
        );
    }
}
