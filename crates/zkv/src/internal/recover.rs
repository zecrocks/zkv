//! Wipe-and-rebootstrap recovery for an unrecoverable reorg.
//!
//! Deletes the on-disk wallet sidecars (data.sqlite, blockmeta.sqlite,
//! blocks/, zkv_state.sqlite) and re-creates the wallet account from
//! `keys.toml`. The chain is the source of truth; every byte we delete
//! is recoverable by re-scanning from the birthday height.

use std::fs;

use anyhow::{anyhow, Context};

use crate::{
    config::{Role, WalletConfig},
    data::init_dbs,
    protocol::parse_zkv_addr,
    remote::ConnectionArgs,
};
use zcash_client_backend::data_api::{AccountPurpose, WalletWrite};

/// Delete a SQLite database file *and* its `-wal`/`-shm` companions.
///
/// Removing only the main file while a stale write-ahead log survives is a
/// footgun: on the next open SQLite recovers the WAL into the freshly
/// re-created database, resurrecting the old contents — including an old
/// on-disk *schema*. That is exactly what broke a rebootstrap across a
/// librustzcash schema change: a leftover `data.sqlite-wal` carrying the
/// previous `addresses` layout got replayed into the new DB, and the upstream
/// migration then failed ("addresses_new has 11 columns but 12 values were
/// supplied"). Best-effort: a missing companion is fine.
fn remove_sqlite_db(path: &std::path::Path) {
    let _ = fs::remove_file(path);
    for suffix in ["-wal", "-shm"] {
        let mut companion = path.as_os_str().to_os_string();
        companion.push(suffix);
        let _ = fs::remove_file(std::path::PathBuf::from(companion));
    }
}

/// Delete the wallet sidecars (data.sqlite, blockmeta.sqlite, blocks/,
/// zkv_state.sqlite - each with its `-wal`/`-shm` companions) under the named
/// database directory. Leaves `keys.toml` and the `security-theater-key` age
/// identity (legacy name `.id`) intact so admin databases can re-bootstrap.
///
/// Must run with **no node holding the database**: these are the files the node
/// keeps open, and the wallet the node checks its viewing-key pin against.
pub fn wipe_sidecars(db_name: &str) -> anyhow::Result<()> {
    wipe_sidecars_in(&crate::data::db_dir(db_name)?)
}

/// [`wipe_sidecars`] against an explicit database directory. Split out so the
/// layout handling is testable without a resolved data dir.
///
/// Note what this leaves behind: the `zec/lrz` directory itself survives, empty.
/// That is deliberate rather than incidental. With no `data.sqlite` in either
/// place, [`crate::data::engine_dir_in`] resolves back to the database root, so
/// the rebuild lays a fresh wallet down at the root and the node's next start
/// migrates it into the nested layout, exactly as it does for a database
/// created by `zkv init`. One code path, not two.
pub(crate) fn wipe_sidecars_in(root: &std::path::Path) -> anyhow::Result<()> {
    // The block cache and wallet DB sit wherever the engine layout puts them,
    // which is not the database root once a node has migrated them. Joining
    // them off the root would delete nothing and report success, leaving the
    // real files behind for the next open to find. `zkv_state.sqlite` is zkv's
    // own and does stay at the root.
    let engine_dir = crate::data::engine_dir_in(root)?;
    remove_sqlite_db(&engine_dir.join(crate::data::DATA_DB));
    remove_sqlite_db(&engine_dir.join("blockmeta.sqlite"));
    remove_sqlite_db(&root.join(crate::data::ZKV_STATE_DB));
    let blocks_dir = engine_dir.join(crate::data::BLOCKS_FOLDER);
    if blocks_dir.exists() {
        fs::remove_dir_all(&blocks_dir)
            .with_context(|| format!("removing {}", blocks_dir.display()))?;
    }
    Ok(())
}

/// Re-create `data.sqlite` and the wallet account from `keys.toml`.
/// Handles both admin (decrypts seed and runs `create_account`) and watch
/// (re-imports the persisted UFVK via `import_account_ufvk`). Watch
/// databases created before the `zkv_address` field was persisted will
/// fail with an instruction to re-run `zkv watch <addr>`.
pub async fn rebootstrap(db_name: &str, conn: &ConnectionArgs) -> anyhow::Result<()> {
    let cfg = WalletConfig::read(db_name)?;
    let params = cfg.network;

    // Rebuild against the birthday already pinned in keys.toml (verbatim, no
    // buffer). No fresh-tip bail here: the user asked for this rebuild
    // explicitly, so a momentarily stale tip must not abort it (the tip is
    // only the recovery-window anchor).
    let birthday =
        crate::internal::sync::pinned_birthday_unchecked(conn, params, u32::from(cfg.birthday))
            .await?;

    let mut db_data = init_dbs(params, db_name)?;
    match cfg.role {
        Role::Admin => {
            let seed = cfg.decrypt_seed()?;
            db_data
                .create_account(db_name, &seed, &birthday, None)
                .map_err(|e| anyhow!("{:?}", e))?;
        }
        Role::Watch => {
            let addr = cfg.zkv_address.as_deref().ok_or_else(|| {
                anyhow!(
                    "this watch database was created before zkv stored its address in keys.toml; \
                     re-run `zkv watch <zkv_addr>` with the original address to rebuild it"
                )
            })?;
            let parsed = parse_zkv_addr(addr)?;
            db_data
                .import_account_ufvk(
                    db_name,
                    &parsed.ufvk,
                    &birthday,
                    AccountPurpose::ViewOnly,
                    None,
                )
                .map_err(|e| anyhow!("{:?}", e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{remove_sqlite_db, wipe_sidecars_in};

    /// A wipe must take the `-wal`/`-shm` companions with the main file; leaving
    /// a stale WAL behind lets SQLite resurrect the old (possibly
    /// incompatible-schema) database on the next open.
    #[test]
    fn remove_sqlite_db_deletes_wal_and_shm_companions() {
        let base = std::env::temp_dir().join(format!(
            "zkv-wipe-{}-{:?}.sqlite",
            std::process::id(),
            std::thread::current().id()
        ));
        let companion = |suffix: &str| {
            let mut p = base.as_os_str().to_os_string();
            p.push(suffix);
            std::path::PathBuf::from(p)
        };
        let wal = companion("-wal");
        let shm = companion("-shm");
        for p in [&base, &wal, &shm] {
            std::fs::write(p, b"x").unwrap();
            assert!(p.exists());
        }
        remove_sqlite_db(&base);
        assert!(!base.exists(), "main db file should be removed");
        assert!(!wal.exists(), "-wal companion should be removed");
        assert!(!shm.exists(), "-shm companion should be removed");
    }

    /// A wipe has to find the wallet where the engine put it. Once a node has
    /// run, `data.sqlite` and the block cache live under `zec/lrz/`, and a wipe
    /// that joined them off the database root would delete nothing, report
    /// success, and leave the next open reading the very files it claimed to
    /// have removed. `zkv_state.sqlite` is zkv's own and does stay at the root.
    #[test]
    fn a_wipe_finds_the_wallet_under_the_nested_engine_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let engine = root.join("zec").join("lrz");
        std::fs::create_dir_all(engine.join("blocks")).unwrap();

        std::fs::write(engine.join("data.sqlite"), b"x").unwrap();
        std::fs::write(engine.join("data.sqlite-wal"), b"x").unwrap();
        std::fs::write(engine.join("blockmeta.sqlite"), b"x").unwrap();
        std::fs::write(engine.join("blocks").join("00001.bin"), b"x").unwrap();
        std::fs::write(root.join("zkv_state.sqlite"), b"x").unwrap();
        // Untouchable: without these the database cannot be rebuilt at all.
        std::fs::write(root.join("keys.toml"), b"x").unwrap();
        std::fs::write(root.join("security-theater-key"), b"x").unwrap();

        wipe_sidecars_in(root).unwrap();

        assert!(!engine.join("data.sqlite").exists());
        assert!(!engine.join("data.sqlite-wal").exists());
        assert!(!engine.join("blockmeta.sqlite").exists());
        assert!(!engine.join("blocks").exists());
        assert!(!root.join("zkv_state.sqlite").exists());
        assert!(root.join("keys.toml").exists(), "keys.toml must survive");
        assert!(
            root.join("security-theater-key").exists(),
            "the age identity must survive, or the seed is unreadable",
        );

        // And with no wallet in either place, the rebuild lands at the root for
        // the node to migrate: the same path a fresh `zkv init` takes.
        assert_eq!(crate::data::engine_dir_in(root).unwrap(), root);
    }

    /// A pre-engine database still has its wallet at the root, and a wipe must
    /// find it there too: adoption is lazy, so this is the state of every
    /// database that has not yet had a node started on it.
    #[test]
    fn a_wipe_finds_the_wallet_under_the_legacy_root_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("blocks")).unwrap();
        std::fs::write(root.join("data.sqlite"), b"x").unwrap();
        std::fs::write(root.join("blockmeta.sqlite"), b"x").unwrap();
        std::fs::write(root.join("blocks").join("00001.bin"), b"x").unwrap();

        wipe_sidecars_in(root).unwrap();

        assert!(!root.join("data.sqlite").exists());
        assert!(!root.join("blockmeta.sqlite").exists());
        assert!(!root.join("blocks").exists());
    }
}
