//! Wipe-and-rebootstrap recovery for an unrecoverable reorg.
//!
//! Deletes the on-disk wallet sidecars (data.sqlite, blockmeta.sqlite,
//! blocks/, zkv_state.sqlite) and re-creates the wallet account from
//! `keys.toml`. The chain is the source of truth; every byte we delete
//! is recoverable by re-scanning from the birthday height.

use std::fs;

use anyhow::{anyhow, Context};
use tonic::transport::Channel;

use crate::{
    config::{Role, WalletConfig},
    data::{db_dir, get_db_paths, init_dbs, zkv_state_path},
    protocol::parse_zkv_addr,
};
use zcash_client_backend::{
    data_api::{AccountPurpose, WalletWrite},
    proto::service::compact_tx_streamer_client::CompactTxStreamerClient,
};

/// Delete the wallet sidecars (data.sqlite, blockmeta.sqlite, blocks/,
/// zkv_state.sqlite) under the named database directory. Leaves
/// `keys.toml` and the `security-theater-key` age identity (legacy name
/// `.id`) intact so admin databases can re-bootstrap.
pub fn wipe_sidecars(db_name: &str) -> anyhow::Result<()> {
    let dir = db_dir(db_name)?;
    let (_root, db_data_path) = get_db_paths(db_name)?;
    let _ = fs::remove_file(&db_data_path);
    let _ = fs::remove_file(dir.join("blockmeta.sqlite"));
    let _ = fs::remove_file(zkv_state_path(db_name)?);
    let blocks_dir = dir.join("blocks");
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
pub async fn rebootstrap(
    db_name: &str,
    client: &mut CompactTxStreamerClient<Channel>,
) -> anyhow::Result<()> {
    let cfg = WalletConfig::read(db_name)?;
    let params = cfg.network;

    // Rebuild against the birthday already pinned in keys.toml (verbatim, no
    // buffer). No fresh-tip bail here: this runs inside an in-progress sync
    // recovery on an already-connected client, so a momentarily stale tip must
    // not abort the rebuild (the tip is only the from_treestate anchor).
    let birthday =
        crate::internal::sync::pinned_birthday_unchecked(client, u32::from(cfg.birthday)).await?;

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
