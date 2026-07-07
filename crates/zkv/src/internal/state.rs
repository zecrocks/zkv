//! Load memos from a wallet database and replay them into per-key state +
//! init status. Shared by `zkv get`, the write path, and the init poll loop.
//!
//! The read path is `snapshot → tail`: a sidecar `zkv_state.sqlite` holds
//! the materialized projection for memos buried at least
//! [`crate::internal::snapshot::SAFE_DEPTH`] blocks deep, and the wallet DB
//! is queried only for rows past the snapshot's watermark. Those rows are
//! partitioned into a *promotable* batch (mined deep enough to extend the
//! snapshot) and a *live tail* (recent confirmed + mempool). The promotable
//! batch is written into the snapshot in one transaction; the live tail
//! flows through [`replay_with_seed`] on top of the snapshot's seed.

use std::collections::{HashMap, HashSet};

use rusqlite::{named_params, Connection, OptionalExtension};
use zcash_client_backend::data_api::WalletRead;
use zcash_primitives::transaction::TxId;
use zcash_protocol::memo::{Memo, MemoBytes};
use zcash_protocol::ShieldedPool;

use crate::{
    config::WalletConfig,
    data::{get_db_paths, open_wallet_db, zkv_state_path},
    internal::{
        account::account_keys,
        pending,
        protocol::{
            history_entry_folding, history_entry_from_memo, parse_text_memo,
            render_memo_with_comment, replay_audit, replay_with_seed, AuditEntry, AuditResult,
            HistoryEntry, HistoryResult, HistoryStatus, InitState, Op, ReplayResult, VersionState,
            WriteStatus,
        },
        snapshot::{self, PromoteRow, SAFE_DEPTH},
    },
};

/// Decoded memo row from `data.sqlite`. `mined_height` / `block_time` are
/// the raw signed values the wallet stores (NULL or non-positive height
/// both mean "mempool"; `block_time` is NULL for unmined txs). Status
/// classification happens in the caller.
struct DecodedRow {
    text: String,
    mined_height: Option<i64>,
    block_time: Option<i64>,
    from_uuid: Option<Vec<u8>>,
    txid: Vec<u8>,
    output_index: u32,
}

/// The `v_tx_outputs.output_pool` codes for a database's shielded pool,
/// matching `zcash_client_sqlite`'s `pool_code` (Sapling = 2, Orchard = 3,
/// Ironwood = 4; transparent is 0 and carries no memo).
///
/// The Orchard *value pool* spans two codes: `3` for V5 Orchard outputs and
/// `4` for V6 Ironwood outputs. Ironwood shares the Orchard receiver and value
/// pool, and a post-NU6.3 database's own writes are built as V6 (an Orchard
/// wallet auto-upgrades on its first send), so a single Orchard/Ironwood
/// database's memos can land under *either* code. Read paths must match both,
/// or V6 memos (including this build's own writes on an Ironwood chain) are
/// invisible. A Sapling database stays code 2 only.
fn pool_output_codes(pool: ShieldedPool) -> &'static [i64] {
    match pool {
        ShieldedPool::Sapling => &[2],
        ShieldedPool::Orchard | ShieldedPool::Ironwood => &[3, 4],
    }
}

/// Render [`pool_output_codes`] as a SQL `IN`-list body (e.g. `"3, 4"`) for
/// inlining into a query. The values are trusted integer constants, never user
/// input, so string interpolation is safe here.
fn pool_in_list(pool: ShieldedPool) -> String {
    pool_output_codes(pool)
        .iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Query `data.sqlite` for this database's-pool text memos addressed to this
/// account that are strictly past the snapshot `watermark` (plus every unmined
/// row), already decoded to text and in chain order
/// `(mined_height ASC NULLS LAST, txid ASC, output_index ASC)`.
///
/// Shared by [`load_state`] (which partitions the result into a promotable
/// batch + live tail) and [`load_history`] (which classifies every row into
/// a [`HistoryEntry`]). The expiry-height filter hides mempool entries whose
/// tx lightwalletd has already evicted but the wallet still holds.
fn scan_memos_past_watermark(
    conn: &Connection,
    account_uuid_bytes: &[u8],
    tip: u32,
    watermark: &snapshot::Watermark,
    pool: ShieldedPool,
) -> anyhow::Result<Vec<DecodedRow>> {
    // Ironwood self-send memo recovery: the Orchard->Ironwood auto-upgrade
    // records the wallet's own memo output in `sent_notes` under the Orchard
    // receiver's pool code (3) while the on-chain note is scanned as Ironwood
    // (pool 4). `v_tx_outputs` joins received notes to `sent_notes` on matching
    // pool code, so the pool-4 received note is left with a NULL memo even
    // though the memo sits in the pool-3 `sent_notes` row. Recover it
    // via a scalar subquery matching `sent_notes` on `(txid, output_index)`
    // *ignoring* the pool code: codes 3 and 4 alias the same Orchard value pool
    // and receiver, so the same output position identifies the same memo. Only
    // the wallet's own sends (`from_account_uuid`) can be NULL-and-recoverable,
    // so the extra `OR from_account` term keeps foreign traffic untouched.
    let mut stmt = conn.prepare(&format!(
        "SELECT COALESCE(v.memo, (
                    SELECT sn.memo FROM sent_notes sn
                    JOIN transactions stx ON stx.id_tx = sn.transaction_id
                    WHERE stx.txid = v.txid
                      AND sn.output_index = v.output_index
                      AND sn.output_pool IN ({pools})
                      AND sn.memo IS NOT NULL
                    LIMIT 1
                )) AS memo,
                t.mined_height, t.block_time, v.from_account_uuid, v.txid, v.output_index
         FROM v_tx_outputs v
         JOIN v_transactions t ON t.txid = v.txid AND t.account_uuid = v.to_account_uuid
         WHERE v.to_account_uuid = :account_uuid
           AND v.output_pool IN ({pools})
           AND (v.memo IS NOT NULL OR v.from_account_uuid = :account_uuid)
           AND (t.mined_height IS NOT NULL
                OR t.expiry_height IS NULL
                OR t.expiry_height = 0
                OR t.expiry_height >= :tip)
           AND (t.mined_height IS NULL
                OR t.mined_height > :wm_height
                OR (t.mined_height = :wm_height
                    AND (v.txid > :wm_txid
                         OR (v.txid = :wm_txid
                             AND v.output_index > :wm_output_index))))
         ORDER BY t.mined_height ASC NULLS LAST, v.txid ASC, v.output_index ASC",
        pools = pool_in_list(pool),
    ))?;

    let decoded: Vec<DecodedRow> = stmt
        .query_and_then(
            named_params! {
                ":account_uuid": account_uuid_bytes,
                ":tip": tip,
                ":wm_height": watermark.height,
                ":wm_txid": &watermark.txid,
                ":wm_output_index": watermark.output_index,
            },
            |row| -> anyhow::Result<Option<DecodedRow>> {
                let bytes: Option<Vec<u8>> = row.get("memo")?;
                let mined_height: Option<i64> = row.get("mined_height")?;
                let block_time: Option<i64> = row.get("block_time")?;
                let txid_bytes: Option<Vec<u8>> = row.get("txid")?;
                let from_uuid: Option<Vec<u8>> = row.get("from_account_uuid")?;
                let output_index: u32 = row.get("output_index")?;

                let Some(memo_bytes) = bytes else {
                    return Ok(None);
                };
                let Ok(mb) = MemoBytes::from_bytes(&memo_bytes) else {
                    return Ok(None);
                };
                let Ok(memo) = Memo::try_from(mb) else {
                    return Ok(None);
                };
                let Memo::Text(t) = memo else { return Ok(None) };
                Ok(Some(DecodedRow {
                    text: t.to_string(),
                    mined_height,
                    block_time,
                    from_uuid,
                    txid: txid_bytes.unwrap_or_default(),
                    output_index,
                }))
            },
        )?
        .filter_map(|r| r.transpose())
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(decoded)
}

/// Convert a raw `txid` storage blob (little-endian) into conventional
/// display-order hex, matching `pay()`'s return format. Empty/short blobs
/// yield an empty string.
pub(crate) fn txid_hex(txid_bytes: &[u8]) -> String {
    <[u8; 32]>::try_from(txid_bytes)
        .ok()
        .map(|arr| TxId::from_bytes(arr).to_string())
        .unwrap_or_default()
}

/// Inverse of [`txid_hex`]: turn a display txid (big-endian hex, as the API
/// hands out) back into the little-endian 32-byte storage blob used in
/// `kv_history`. `None` unless it decodes to exactly 32 bytes. `TxId`'s
/// `Display` reverses the stored bytes, so we reverse on the way back.
fn txid_blob_from_hex(hex_str: &str) -> Option<Vec<u8>> {
    let mut bytes = hex::decode(hex_str).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    bytes.reverse();
    Some(bytes)
}

/// Default confirmation depth used for self-sent INIT detection in the
/// init-flow poll loop (success after 1 confirmation, per the plan). Read
/// commands keep their own `--confirmations` (default 3).
pub const INIT_CONFIRMATIONS: u32 = 1;

/// Effective confirmation threshold for one decoded memo row.
///
/// INIT is the database's genesis claim: signature-gated (a non-root signer is
/// `ForgedInit`) and once-only, so it is treated as confirmed as soon as it is
/// [`INIT_CONFIRMATIONS`] deep, independent of the read's stricter data-write
/// depth. This matters for an INIT a faucet broadcast on our behalf: that memo
/// is *externally received* (the faucet, not us, created the tx), so under the
/// default 3-confirmation read it would be dropped while 1-2 blocks deep, and
/// once the local pending record is pruned the database would flap back to
/// "uninitialized" for a block or two before settling. Confirming INIT at its
/// own depth keeps a freshly mined INIT visible the whole way through. Capped at
/// `min_confs` so an explicit shallower read (e.g. mempool, `-c 0`) still wins.
/// All other ops use `min_confs` unchanged.
fn effective_min_confs(is_init: bool, min_confs: u32) -> u32 {
    if is_init {
        INIT_CONFIRMATIONS.min(min_confs)
    } else {
        min_confs
    }
}

/// The database's required [`VersionState`] from the snapshot cache alone, without
/// sync or full replay. See [`snapshot::cached_version`]. Lets command/facade
/// code honor a `blocksync` flag *before* hitting the network; reflects only
/// memos already promoted into the snapshot (recent ones in the live tail are
/// only visible after a full [`load_state`]).
pub fn cached_version(db_name: &str) -> anyhow::Result<VersionState> {
    snapshot::cached_version(&zkv_state_path(db_name)?)
}

/// The wallet's current scanned chain height, read cheaply without a sync.
/// Used to return an honest "tip" when a `blocksync` directive makes the facade
/// skip the network scan.
pub fn wallet_tip(db_name: &str) -> anyhow::Result<u32> {
    let cfg = WalletConfig::read(db_name)?;
    let (_, db_data_path) = get_db_paths(db_name)?;
    let db_data = open_wallet_db(&db_data_path, cfg.network)?;
    Ok(db_data.chain_height()?.map(u32::from).unwrap_or(0))
}

/// Read the wallet's known chain tip and its **fully-scanned** frontier in one
/// pass: `(tip, fully_scanned)`.
///
/// `tip` is `chain_height()` (set by `update_chain_tip` at the start of a sync,
/// then frozen until the scan finishes). `fully_scanned` is the height below
/// which every block from the wallet birthday has been scanned with no gaps
/// (`WalletSummary::fully_scanned_height`); it climbs as each scan batch
/// commits. The two differ during a catch-up / backfill sync, which is exactly
/// when the snapshot watermark must not be allowed to outrun the scan (see the
/// `promote_cutoff` clamp in [`load_state_with_height`]). `fully_scanned`
/// defaults to 0 before the first wallet summary exists (nothing scanned yet).
fn tip_and_fully_scanned(
    db_data_path: &std::path::Path,
    network: crate::network::Network,
) -> anyhow::Result<(u32, u32)> {
    use zcash_client_backend::data_api::wallet::ConfirmationsPolicy;
    let db_data = open_wallet_db(db_data_path, network)?;
    let tip = db_data.chain_height()?.map(u32::from).unwrap_or(0);
    let fully_scanned = db_data
        .get_wallet_summary(ConfirmationsPolicy::default())?
        .map(|s| u32::from(s.fully_scanned_height()))
        .unwrap_or(0);
    Ok((tip, fully_scanned))
}

/// The highest block height a row may be at and still be safe to promote into
/// the snapshot (advancing the watermark to it). A row qualifies only when it
/// is both:
///
/// * at least [`SAFE_DEPTH`] blocks below the chain `tip` (reorg safety), and
/// * at or below the wallet's `fully_scanned` frontier, so every block up to
///   the new watermark has already been scanned and no earlier memo (e.g. the
///   genesis INIT) can be backfilled below the watermark afterward.
///
/// Pure so the boundary is unit-testable without a wallet DB.
fn promote_cutoff(tip: u32, fully_scanned: u32) -> u32 {
    tip.saturating_sub(SAFE_DEPTH).min(fully_scanned)
}

/// Load all of this database's-pool text memos addressed to its account and
/// replay them at the caller's confirmation threshold.
///
/// `min_confs` is the threshold for treating any memo (including INIT) as
/// confirmed; self-sent memos below this threshold still surface as
/// `Confirming` / `Initializing`.
pub fn load_state(db_name: &str, min_confs: u32, strict: bool) -> anyhow::Result<ReplayResult> {
    Ok(load_state_with_height(db_name, min_confs, strict)?.0)
}

/// Like [`load_state`], but also returns the chain height the local wallet
/// had scanned when the state was read (the state's "as of" height; `0`
/// means the wallet has never synced). The facade
/// ([`crate::db::Database::read_at`]) uses this to bundle a freshness signal
/// into a read in one wallet-DB pass, so the height can't drift from the
/// state between two separate queries.
pub fn load_state_with_height(
    db_name: &str,
    min_confs: u32,
    strict: bool,
) -> anyhow::Result<(ReplayResult, u32)> {
    let cfg = WalletConfig::read(db_name)?;
    let keys = account_keys(&cfg, db_name)?;

    // Re-open just to read the chain tip and the fully-scanned frontier. The
    // wallet-summary query is cheap and lives behind the same connection as the
    // memo SELECT below; we intentionally don't borrow the WalletDb across that
    // boundary.
    let (_, db_data_path) = get_db_paths(db_name)?;
    let (tip, fully_scanned) = tip_and_fully_scanned(&db_data_path, cfg.network)?;

    let account_uuid_bytes = keys.account_uuid_bytes;
    let receiver_hex = keys.receiver_hex;
    let pk = keys.verifying_pubkey;

    // Open the snapshot first so the watermark filters the SQL query.
    // If the wallet has rewound past our watermark (catastrophic, far
    // beyond Zcash's typical reorg depth), wipe and rebuild from scratch.
    let mut snap = snapshot::open(&zkv_state_path(db_name)?)?;
    let watermark = snapshot::read_watermark(&snap)?;
    if tip != 0 && tip < watermark.height {
        tracing::warn!(
            "wallet tip {tip} is behind snapshot watermark height {}, wiping snapshot",
            watermark.height,
        );
        snapshot::wipe(&mut snap)?;
    }

    // Self-heal a snapshot corrupted by an older build (before the genesis-INIT
    // guard landed): a watermark that advanced while the database is still
    // uninitialized means the genesis INIT got buried below it, so every read
    // sees an empty auth registry and drops every write as unauthorized. The
    // promote gate now makes that state unreachable, so any snapshot exhibiting
    // it is legacy-corrupt: wipe it and let the scan below rebuild from the
    // (re-scannable) wallet data, which now picks up the INIT.
    let watermark = snapshot::read_watermark(&snap)?;
    if watermark.height > 0 && matches!(snapshot::read_init_state(&snap)?, InitState::Uninitialized)
    {
        tracing::warn!(
            watermark_height = watermark.height,
            "snapshot watermark advanced while uninitialized (buried genesis INIT); \
             wiping and rebuilding"
        );
        snapshot::wipe(&mut snap)?;
    }
    let watermark = snapshot::read_watermark(&snap)?;

    // Watermark filter: include unmined rows (`mined_height IS NULL`) plus
    // any row strictly past the lexicographic `(height, txid, output_index)`
    // watermark. With an empty watermark (fresh snapshot, watermark.height = 0,
    // empty txid blob), every confirmed row qualifies; `txid > X''` holds for
    // all non-empty BLOBs.
    let conn = Connection::open(&db_data_path)?;
    crate::data::configure_sqlite(&conn)?;
    let decoded = scan_memos_past_watermark(
        &conn,
        account_uuid_bytes.as_slice(),
        tip,
        &watermark,
        cfg.pool,
    )?;

    // Partition into promotable vs live tail. Promotable rows are mined
    // strictly past the current watermark and at least SAFE_DEPTH blocks
    // deep; everything else (recent confirmed, mempool) stays in the
    // tail and is replayed in memory each read.
    //
    // The cutoff is additionally clamped to the wallet's *fully-scanned*
    // height. The wallet does not scan monotonically from the birthday: it
    // scans the chain tip first and backfills older ranges later, so a block
    // below the chain tip may still be unscanned. Promoting (and dropping) a
    // row at height H advances the watermark to H, after which any row at a
    // height <= H is excluded from future scans (it falls below the
    // watermark). If a not-yet-backfilled block below H (e.g. the genesis
    // INIT near the birthday) is scanned *after* that, it would be silently
    // skipped forever, leaving the database stuck "uninitialized" with every
    // write dropped as NotInitialized. Clamping to `fully_scanned` guarantees
    // every block at or below the watermark has already been scanned, so no
    // earlier memo can appear after the fact.
    let promote_cutoff = promote_cutoff(tip, fully_scanned);
    let mut promotable: Vec<PromoteRow> = Vec::new();
    let mut tail: Vec<(String, WriteStatus, String, Option<u32>)> = Vec::new();
    for row in decoded {
        let DecodedRow {
            text,
            mined_height,
            block_time,
            from_uuid,
            txid: txid_bytes,
            output_index,
        } = row;
        let is_mempool = mined_height.is_none_or(|h| h <= 0);
        let mined_u32 = mined_height
            .filter(|h| *h > 0)
            .map(|h| u32::try_from(h).unwrap_or(u32::MAX));
        let block_time_u32 = block_time
            .filter(|t| *t > 0)
            .map(|t| u32::try_from(t).unwrap_or(u32::MAX));

        if let Some(h) = mined_u32 {
            if h <= promote_cutoff {
                promotable.push(PromoteRow {
                    mined_height: h,
                    txid: txid_bytes.clone(),
                    output_index,
                    block_time: block_time_u32,
                    memo_text: text,
                });
                continue;
            }
        }

        // Tail row. Classify status the same way the original load_state did.
        let confs: u32 = if is_mempool {
            0
        } else {
            tip.saturating_sub(mined_u32.unwrap_or(0)).saturating_add(1)
        };
        let is_self_sent = from_uuid
            .as_deref()
            .map(|b| b == account_uuid_bytes.as_slice())
            .unwrap_or(false);
        // INIT confirms at its own (shallower) depth so a faucet-broadcast
        // (externally-received) INIT isn't dropped below the data-write
        // threshold; see `effective_min_confs`.
        let is_init = parse_text_memo(&text).is_some_and(|c| c.op == Op::Init);
        let eff_confs = effective_min_confs(is_init, min_confs);
        let status = if is_mempool {
            WriteStatus::Confirming {
                done: 0,
                required: eff_confs,
            }
        } else if confs >= eff_confs {
            WriteStatus::Confirmed
        } else if is_self_sent {
            WriteStatus::Confirming {
                done: confs,
                required: eff_confs,
            }
        } else {
            // Externally-received memo below the caller's confirmation
            // threshold: drop, matching pre-snapshot behavior.
            continue;
        };
        // Match `pay()`'s return format (conventional display order, not
        // the storage byte order in the BLOB).
        tail.push((text, status, txid_hex(&txid_bytes), block_time_u32));
    }

    // Apply promotable batch to the snapshot in one transaction, then
    // re-load the seed for the in-memory pass.
    if !promotable.is_empty() {
        snapshot::promote(&mut snap, &promotable, &receiver_hex, &pk)?;
    }
    let seed = snapshot::load_seed(&snap)?;
    drop(snap);

    let replay = replay_with_seed(tail, Some(seed), &receiver_hex, &pk, strict)?;
    Ok((replay, tip))
}

/// In-flight-first rank for ordering: pending (0) above confirming (1)
/// above confirmed (2).
fn inflight_rank(s: &HistoryStatus) -> u8 {
    match s {
        HistoryStatus::Pending => 0,
        HistoryStatus::Confirming { .. } => 1,
        HistoryStatus::Confirmed { .. } => 2,
    }
}

/// Case-insensitive substring match of a key against an optional filter
/// (mirrors the snapshot's `LIKE '%filter%'`).
fn key_matches(key: &str, filter: Option<&str>) -> bool {
    match filter {
        Some(f) if !f.is_empty() => key.to_lowercase().contains(&f.to_lowercase()),
        _ => true,
    }
}

/// Load one page of a database's write history (SET/DEL + the genesis INIT),
/// newest-first with in-flight writes pinned on top.
///
/// The bulk lives in the snapshot's `kv_history` (paginated + key-filtered by
/// SQLite, with each row's `block_time` cached (no per-row block lookup); the
/// small **live** set (recent tail past the watermark + `pending.toml`) is
/// computed in memory and pinned above the confirmed page. The result is the
/// virtual newest-first list `live ++ deep` sliced by `[offset, offset+limit)`;
/// `limit = None` returns everything (CLI / programmatic callers). `total`
/// counts all matches for pagination. Read-only apart from the catastrophic
/// tip-below-watermark wipe.
/// Sort direction for [`load_history_page`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistoryOrder {
    /// Newest write first (the default; in-flight writes pinned on top).
    #[default]
    Desc,
    /// Oldest write first (genesis INIT leads).
    Asc,
}

// The paging knobs (filter / ops / order / limit / offset / locate) are all
// independent and orthogonal; bundling them into a struct would just move the
// noise to every call site without adding clarity.
#[allow(clippy::too_many_arguments)]
pub fn load_history_page(
    db_name: &str,
    min_confs: u32,
    filter: Option<&str>,
    ops: Option<&[String]>,
    order: HistoryOrder,
    limit: Option<u32>,
    offset: u32,
    locate: Option<&str>,
) -> anyhow::Result<HistoryResult> {
    let cfg = WalletConfig::read(db_name)?;
    let keys = account_keys(&cfg, db_name)?;

    let (_, db_data_path) = get_db_paths(db_name)?;
    let tip: u32 = {
        let db_data = open_wallet_db(&db_data_path, cfg.network)?;
        db_data.chain_height()?.map(u32::from).unwrap_or(0)
    };

    let account_uuid_bytes = keys.account_uuid_bytes;
    let receiver_hex = keys.receiver_hex;
    // The database's own address, derived from the viewing key this reader
    // already holds. Used below to display the genesis (INIT) row's address
    // instead of the memo's *echoed* address, which is unsigned and therefore
    // not authenticated (a relayer could have rewritten it without breaking the
    // receiver-bound INIT signature). The reader trusts its own derivation, not
    // the wire echo.
    let zkv_addr = keys.zkv_addr;
    let pk = keys.verifying_pubkey;
    // Canonical `zkvid1…` form, matching the registry and recovered-signer
    // strings (`pubkey_bech32`); this feeds `HistoryResult.signer` for display.
    let signer = crate::internal::protocol::pubkey_bech32(&pk);

    let mut snap = snapshot::open(&zkv_state_path(db_name)?)?;
    let watermark = snapshot::read_watermark(&snap)?;
    if tip != 0 && tip < watermark.height {
        tracing::warn!(
            "wallet tip {tip} is behind snapshot watermark height {}, wiping snapshot",
            watermark.height,
        );
        snapshot::wipe(&mut snap)?;
    }
    let watermark = snapshot::read_watermark(&snap)?;

    // Seed the authorization registry / init flag / confirmed key-state from
    // the snapshot, then fold the live tail on top in chain order (below) so
    // each tail write is attributed to its real signer and `verified` reflects
    // authorization in a multi-signer database, exactly mirroring the read
    // path's `replay_with_seed`. `decoded` is ordered oldest-first, so the fold
    // runs in chain order before the newest-first display sort.
    let mut seed = snapshot::load_seed(&snap)?;

    // ---- LIVE: tail past the watermark + pending.toml (small) ----
    let conn = Connection::open(&db_data_path)?;
    crate::data::configure_sqlite(&conn)?;
    let decoded = scan_memos_past_watermark(
        &conn,
        account_uuid_bytes.as_slice(),
        tip,
        &watermark,
        cfg.pool,
    )?;
    let mut live: Vec<HistoryEntry> = Vec::new();
    for row in decoded {
        let DecodedRow {
            text,
            mined_height,
            block_time,
            from_uuid,
            txid: txid_bytes,
            output_index,
        } = row;
        let is_mempool = mined_height.is_none_or(|h| h <= 0);
        let mined_u32 = mined_height
            .filter(|h| *h > 0)
            .map(|h| u32::try_from(h).unwrap_or(u32::MAX));
        let timestamp = block_time
            .filter(|t| *t > 0)
            .map(|t| u32::try_from(t).unwrap_or(u32::MAX));
        let confs: u32 = if is_mempool {
            0
        } else {
            tip.saturating_sub(mined_u32.unwrap_or(0)).saturating_add(1)
        };
        let is_self_sent = from_uuid
            .as_deref()
            .map(|b| b == account_uuid_bytes.as_slice())
            .unwrap_or(false);
        // INIT confirms at its own (shallower) depth so a faucet-broadcast
        // (externally-received) INIT isn't dropped below the data-write
        // threshold; see `effective_min_confs`.
        let is_init = parse_text_memo(&text).is_some_and(|c| c.op == Op::Init);
        let eff_confs = effective_min_confs(is_init, min_confs);
        let status = if is_mempool {
            HistoryStatus::Pending
        } else if confs >= eff_confs {
            HistoryStatus::Confirmed {
                confirmations: confs,
            }
        } else if is_self_sent {
            HistoryStatus::Confirming {
                done: confs,
                required: eff_confs,
            }
        } else {
            continue; // externally-received below threshold: drop
        };
        // Map the display status to the replay `WriteStatus` for the fold: a
        // mempool/pending write is "confirming" with 0 confirmations, so a
        // pending management op confers no registry change yet (matching
        // `replay_with_seed`).
        let write_status = match &status {
            HistoryStatus::Confirmed { .. } => WriteStatus::Confirmed,
            HistoryStatus::Confirming { done, required } => WriteStatus::Confirming {
                done: *done,
                required: *required,
            },
            HistoryStatus::Pending => WriteStatus::Confirming {
                done: 0,
                required: min_confs,
            },
        };
        if let Some(entry) = history_entry_folding(
            &receiver_hex,
            &signer,
            &text,
            mined_u32,
            timestamp,
            txid_hex(&txid_bytes),
            output_index,
            status,
            &write_status,
            &mut seed.init,
            &mut seed.auth,
            &mut seed.finalized,
            &mut seed.state,
            &mut seed.kv_versions,
            &mut seed.target_versions,
        ) {
            if key_matches(&entry.key, filter) {
                live.push(entry);
            }
        }
    }

    // Merge pending.toml exactly like the read path: under min_confs >= 1 drop
    // wire-only mempool entries (keep our own, matched by txid), then add
    // locally-broadcast txs the wallet hasn't surfaced yet.
    let local_pending = pending::load(db_name).unwrap_or_default();
    let local_txids: HashSet<String> = local_pending.iter().map(|e| e.txid.clone()).collect();
    if min_confs >= 1 {
        live.retain(|e| {
            !matches!(e.status, HistoryStatus::Pending) || local_txids.contains(&e.txid)
        });
    }
    let seen_txids: HashSet<String> = live.iter().map(|e| e.txid.clone()).collect();
    for entry in &local_pending {
        if seen_txids.contains(&entry.txid) || !key_matches(&entry.key, filter) {
            continue;
        }
        // INIT is the genesis entry; SET/DEL are data writes. (Management ops
        // OWNER*/WRITER* are recorded in pending.toml too but don't belong in
        // the key/value write log, so they fall through the `_ => continue`.)
        let (op, value) = match entry.op.as_str() {
            "SET" => (Op::Set, Some(entry.value.clone().unwrap_or_default())),
            "DEL" => (Op::Del, None),
            "INIT" => (Op::Init, None),
            _ => continue,
        };
        // Prefer the exact signed memo we stored at broadcast; re-parse it so
        // the entry carries the real signature + verified flag, exactly like a
        // wallet-indexed tail entry. Fall back to a memo-less entry for older
        // pending.toml rows written before the `memo` field existed.
        let parsed = entry.memo.as_deref().and_then(|text| {
            history_entry_from_memo(
                &receiver_hex,
                &pk,
                text,
                None,
                None,
                entry.txid.clone(),
                0,
                HistoryStatus::Pending,
            )
        });
        live.push(parsed.unwrap_or_else(|| HistoryEntry {
            op,
            key: entry.key.clone(),
            value,
            height: None,
            timestamp: None,
            txid: entry.txid.clone(),
            output_index: 0,
            signature: None,
            seq: None,
            signer: None,
            verified: None,
            status: HistoryStatus::Pending,
            memo: entry.memo.clone(),
            fee: None,
            output_value: None,
        }));
    }

    // Newest-first, in-flight pinned on top. `live` is all newer than the
    // deep (kv_history) rows, which `history_page` already returns DESC.
    live.sort_by(|a, b| {
        inflight_rank(&a.status)
            .cmp(&inflight_rank(&b.status))
            .then(
                b.height
                    .unwrap_or(u32::MAX)
                    .cmp(&a.height.unwrap_or(u32::MAX)),
            )
            .then(b.output_index.cmp(&a.output_index))
    });

    // Display-only op filter on the live tail. The auth fold above already saw
    // every op, so verification / authorization are unaffected; this only
    // restricts what's shown (and counted for pagination).
    if let Some(ops) = ops.filter(|o| !o.is_empty()) {
        live.retain(|e| ops.iter().any(|o| o == e.op.as_str()));
    }

    // If asked to jump to a specific write (txid) in full context, override
    // `offset` with the page that contains it: find its rank in the live tail
    // first (small), else in the deep snapshot, then snap to a page boundary.
    // Falls back to the requested offset when the txid can't be located. Only
    // meaningful for the default newest-first paging.
    let offset = match (order, locate, limit) {
        (HistoryOrder::Desc, Some(hex), Some(lim)) if lim > 0 => {
            let rank = if let Some(i) = live.iter().position(|e| e.txid == hex) {
                Some(i as u64)
            } else if let Some(blob) = txid_blob_from_hex(hex) {
                snapshot::history_locate(&snap, filter, &blob)?.map(|deep| live.len() as u64 + deep)
            } else {
                None
            };
            match rank {
                Some(r) => ((r / lim as u64) * lim as u64) as u32,
                None => offset,
            }
        }
        _ => offset,
    };

    // ---- DEEP: page of confirmed history from the snapshot ----
    let deep_count = snapshot::history_count(&snap, filter, ops)?;
    let total = live.len() as u64 + deep_count;
    let live_len = live.len();
    let off = offset as usize;

    // Map a deep `kv_history` row to a display entry. Deep rows are
    // authorized-by-construction: `promote` ran `decide` (and recovered +
    // stored this signer) before inserting, so `verified = true` here also
    // means "was authorized."
    let row_to_entry = |row: snapshot::HistRow| -> Option<HistoryEntry> {
        let op = match row.op.as_str() {
            "SET" => Op::Set,
            "SETL" => Op::SetL,
            "DEL" => Op::Del,
            "INIT" => Op::Init,
            _ => return None,
        };
        let confirmations = tip.saturating_sub(row.mined_height).saturating_add(1);
        let memo = render_memo_with_comment(
            op,
            &row.key,
            row.value.as_deref(),
            row.seq,
            &row.signature,
            row.comment.as_deref(),
        );
        Some(HistoryEntry {
            op,
            key: row.key,
            value: row.value,
            height: Some(row.mined_height),
            timestamp: row.block_time,
            txid: txid_hex(&row.txid),
            output_index: row.output_index,
            signature: Some(row.signature),
            seq: Some(row.seq),
            signer: Some(row.signer),
            verified: Some(true),
            status: HistoryStatus::Confirmed { confirmations },
            memo: Some(memo),
            fee: None,
            output_value: None,
        })
    };

    // Assemble the page. The virtual list is `live ++ deep` newest-first; for
    // ascending it reverses to `deep ++ live` oldest-first. Either way the deep
    // portion is paged in SQL (it can be large) and the small live tail in
    // memory, so `limit` is honoured exactly in both directions.
    let mut entries: Vec<HistoryEntry> = Vec::new();
    match order {
        HistoryOrder::Desc => {
            let (live_slice, deep_take, deep_off): (Vec<HistoryEntry>, Option<u32>, u32) =
                match limit {
                    None => (
                        live.into_iter().skip(off).collect(),
                        None,
                        off.saturating_sub(live_len) as u32,
                    ),
                    Some(lim) => {
                        let lim = lim as usize;
                        let slice: Vec<HistoryEntry> =
                            live.into_iter().skip(off).take(lim).collect();
                        let remaining = lim - slice.len();
                        (
                            slice,
                            Some(remaining as u32),
                            off.saturating_sub(live_len) as u32,
                        )
                    }
                };
            entries.extend(live_slice);
            for row in snapshot::history_page(&snap, filter, ops, false, deep_take, deep_off)? {
                if let Some(e) = row_to_entry(row) {
                    entries.push(e);
                }
            }
        }
        HistoryOrder::Asc => {
            let deep_count = deep_count as usize;
            let deep_off = off.min(deep_count) as u32;
            let deep_avail = deep_count.saturating_sub(off);
            let (deep_take, live_off, live_take): (Option<u32>, usize, Option<usize>) = match limit
            {
                None => (None, off.saturating_sub(deep_count), None),
                Some(lim) => {
                    let lim = lim as usize;
                    let deep_take = lim.min(deep_avail);
                    (
                        Some(deep_take as u32),
                        off.saturating_sub(deep_count),
                        Some(lim - deep_take),
                    )
                }
            };
            for row in snapshot::history_page(&snap, filter, ops, true, deep_take, deep_off)? {
                if let Some(e) = row_to_entry(row) {
                    entries.push(e);
                }
            }
            // `live` is newest-first; reverse for oldest-first, then page it.
            let mut live_asc = live;
            live_asc.reverse();
            let tail = live_asc.into_iter().skip(live_off);
            match live_take {
                Some(t) => entries.extend(tail.take(t)),
                None => entries.extend(tail),
            }
        }
    }

    // Fill the true per-transaction fee and this write's own output value for
    // the page from the wallet's own records. Best effort: a query failure just
    // leaves these unset rather than failing the whole history load.
    let _ = fill_tx_fee_and_output(&conn, &mut entries, cfg.pool);
    for e in &mut entries {
        // Show the genesis row's address as the reader's own derived address,
        // not the unsigned wire echo (F5): the echo is advisory and could have
        // been altered by a relayer, so it must not be presented as the
        // authenticated database identity.
        if e.op == Op::Init {
            e.key = zkv_addr.clone();
        }
    }

    Ok(HistoryResult {
        signer,
        entries,
        total,
        offset,
        limit,
    })
}

/// Full classification audit of the entire memo stream, with a standardized
/// drop reason for every memo that did not take effect.
///
/// Unlike [`load_history_page`] (which pages the snapshot's `kv_history` of
/// *applied* writes), this re-derives from **all** memos in the wallet DB via
/// the shared [`replay_audit`] classifier, so it surfaces the rows that replay
/// *dropped*: malformed, bad-signature, unauthorized, wrong-network/foreign
/// INIT, unsupported-version, etc. `O(total writes)`; meant for an explicit
/// audit (`zkv history --include-invalid`), not the hot read path. Does not
/// touch the snapshot and does not merge `pending.toml`.
pub fn load_audit(db_name: &str, min_confs: u32) -> anyhow::Result<AuditResult> {
    let cfg = WalletConfig::read(db_name)?;
    let keys = account_keys(&cfg, db_name)?;

    let (_, db_data_path) = get_db_paths(db_name)?;
    let tip: u32 = {
        let db_data = open_wallet_db(&db_data_path, cfg.network)?;
        db_data.chain_height()?.map(u32::from).unwrap_or(0)
    };

    let account_uuid_bytes = keys.account_uuid_bytes;
    let receiver_hex = keys.receiver_hex;
    let pk = keys.verifying_pubkey;

    // Full scan: an empty (default) watermark means "every row".
    let conn = Connection::open(&db_data_path)?;
    crate::data::configure_sqlite(&conn)?;
    let decoded = scan_memos_past_watermark(
        &conn,
        account_uuid_bytes.as_slice(),
        tip,
        &snapshot::Watermark::default(),
        cfg.pool,
    )?;

    let mut entries: Vec<AuditEntry> = Vec::new();
    for row in decoded {
        let is_mempool = row.mined_height.is_none_or(|h| h <= 0);
        let mined_u32 = row
            .mined_height
            .filter(|h| *h > 0)
            .map(|h| u32::try_from(h).unwrap_or(u32::MAX));
        let timestamp = row
            .block_time
            .filter(|t| *t > 0)
            .map(|t| u32::try_from(t).unwrap_or(u32::MAX));
        let confs: u32 = if is_mempool {
            0
        } else {
            tip.saturating_sub(mined_u32.unwrap_or(0)).saturating_add(1)
        };
        // For the audit we never drop a row pre-classification (the point is to
        // show everything); a mined-but-shallow memo is just Confirming. Drop
        // *reasons* still come from the shared classifier regardless of status.
        let status = if !is_mempool && confs >= min_confs {
            WriteStatus::Confirmed
        } else {
            WriteStatus::Confirming {
                done: confs,
                required: min_confs,
            }
        };
        entries.push(AuditEntry {
            mined_height: mined_u32,
            timestamp,
            txid: txid_hex(&row.txid),
            text: row.text,
            status,
        });
    }

    Ok(replay_audit(entries, &receiver_hex, &pk))
}

/// Fill each entry's `fee` and `output_value` from the wallet DB.
///
/// - **Fee** comes from `v_transactions.fee_paid`, but only when this wallet
///   *built* the tx (its `account_balance_delta` is negative, i.e. we spent).
///   A received tx may report a `fee_paid` that was paid by the *sender*, not
///   us; showing it as our fee is the "output amount shows up as the fee" bug,
///   so a received write leaves `fee` unset.
/// - **Output value** is the zatoshi `value` of this write's own shielded
///   output (`v_tx_outputs`, matched on txid + output_index + pool). A plain
///   zkv write is a zero-value output and stays `None`; a nonzero value means
///   the write also moved ZEC (a tip/deposit broadcast with the memo).
///
/// Entries with no resolvable txid (pending-from-`pending.toml`) are skipped.
fn fill_tx_fee_and_output(
    conn: &Connection,
    entries: &mut [HistoryEntry],
    pool: ShieldedPool,
) -> anyhow::Result<()> {
    let mut tx_stmt =
        conn.prepare("SELECT account_balance_delta, fee_paid FROM v_transactions WHERE txid = ?1")?;
    let mut out_stmt = conn.prepare(&format!(
        "SELECT value FROM v_tx_outputs
         WHERE txid = ?1 AND output_index = ?2 AND output_pool IN ({pools})",
        pools = pool_in_list(pool),
    ))?;
    // A single tx can carry several writes; cache its fee lookup by txid.
    let mut fee_cache: HashMap<String, Option<u64>> = HashMap::new();
    for e in entries.iter_mut() {
        let Some(blob) = txid_blob_from_hex(&e.txid) else {
            continue;
        };
        let fee = match fee_cache.get(&e.txid) {
            Some(f) => *f,
            None => {
                let f = tx_stmt
                    .query_row([&blob], |row| {
                        let delta: Option<i64> = row.get(0)?;
                        let fee: Option<i64> = row.get(1)?;
                        Ok((delta, fee))
                    })
                    .optional()?
                    .and_then(|(delta, fee)| {
                        // Outgoing (we spent) ⇒ the fee is ours to show.
                        let outgoing = delta.unwrap_or(0) < 0;
                        fee.filter(|f| outgoing && *f >= 0).map(|f| f as u64)
                    });
                fee_cache.insert(e.txid.clone(), f);
                f
            }
        };
        e.fee = fee;

        let value: Option<i64> = out_stmt
            .query_row(rusqlite::params![&blob, e.output_index], |row| row.get(0))
            .optional()?;
        e.output_value = value.filter(|v| *v > 0).map(|v| v as u64);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::snapshot::SAFE_DEPTH;

    #[test]
    fn effective_min_confs_confirms_init_at_its_own_depth() {
        // INIT confirms at INIT_CONFIRMATIONS (1), independent of the read's
        // stricter data-write depth, so a faucet-broadcast (externally-received)
        // INIT isn't dropped while 1-2 blocks deep under a default -c 3 read.
        assert_eq!(effective_min_confs(true, 3), INIT_CONFIRMATIONS);
        assert_eq!(effective_min_confs(true, 1), 1);
        // Capped at the caller's depth: a mempool / -c 0 read still wins.
        assert_eq!(effective_min_confs(true, 0), 0);
        // Non-INIT ops are unchanged.
        assert_eq!(effective_min_confs(false, 3), 3);
        assert_eq!(effective_min_confs(false, 0), 0);
    }

    #[test]
    fn promote_cutoff_uses_reorg_margin_when_fully_synced() {
        // Steady state: the fully-scanned frontier is at (or above) the chain
        // tip, so the only binding constraint is the SAFE_DEPTH reorg margin.
        let tip = 4_000_000;
        assert_eq!(
            promote_cutoff(tip, tip),
            tip - SAFE_DEPTH,
            "a caught-up wallet promotes everything older than SAFE_DEPTH",
        );
        assert_eq!(promote_cutoff(tip, tip + 50), tip - SAFE_DEPTH);
    }

    #[test]
    fn promote_cutoff_clamps_to_fully_scanned_during_backfill() {
        // Mid-backfill: the wallet has set its chain tip (via update_chain_tip)
        // but has only scanned the recent region; older blocks near the
        // birthday (where the INIT lives) are not scanned yet. The cutoff must
        // never exceed the fully-scanned frontier, or the watermark could jump
        // past the still-unscanned INIT block and lose it forever.
        let tip = 4_000_000;
        let fully_scanned = 3_900_000; // far below tip - SAFE_DEPTH
        assert_eq!(promote_cutoff(tip, fully_scanned), fully_scanned);
        assert!(promote_cutoff(tip, fully_scanned) < tip - SAFE_DEPTH);
    }

    #[test]
    fn promote_cutoff_is_zero_before_any_scan() {
        // Brand-new wallet: nothing scanned, so nothing is promotable yet (the
        // whole history stays in the live tail and is replayed in memory).
        assert_eq!(promote_cutoff(4_000_000, 0), 0);
        assert_eq!(promote_cutoff(0, 0), 0);
    }

    #[test]
    fn key_matches_is_case_insensitive_substring() {
        // Mirrors the snapshot's `LIKE '%filter%'`: case-insensitive contains.
        assert!(key_matches("UserName", Some("name")));
        assert!(key_matches("username", Some("NAME")));
        assert!(key_matches("zec_usd", Some("usd")));
        assert!(!key_matches("zec_usd", Some("eur")));
        // No filter (None) or an empty filter matches everything.
        assert!(key_matches("anything", None));
        assert!(key_matches("anything", Some("")));
    }

    #[test]
    fn inflight_rank_pins_in_flight_above_confirmed() {
        // The history view sorts ascending by this rank, so in-flight writes
        // (pending → confirming) pin above confirmed ones.
        assert_eq!(inflight_rank(&HistoryStatus::Pending), 0);
        assert_eq!(
            inflight_rank(&HistoryStatus::Confirming {
                done: 1,
                required: 3
            }),
            1
        );
        assert_eq!(
            inflight_rank(&HistoryStatus::Confirmed { confirmations: 5 }),
            2
        );
        assert!(
            inflight_rank(&HistoryStatus::Pending)
                < inflight_rank(&HistoryStatus::Confirmed { confirmations: 5 })
        );
    }

    #[test]
    fn txid_blob_from_hex_reverses_and_rejects_bad_input() {
        // 32-byte display-order hex decodes to the little-endian wire blob
        // (reversed), matching how the wallet stores txids.
        let display_hex: String = (0u8..32).map(|i| format!("{i:02x}")).collect();
        let blob = txid_blob_from_hex(&display_hex).expect("32-byte hex parses");
        assert_eq!(blob.len(), 32);
        // Reversed: the first display byte (0x00) becomes the last blob byte.
        assert_eq!(blob[0], 0x1f);
        assert_eq!(blob[31], 0x00);
        // Wrong length and non-hex are rejected.
        assert!(txid_blob_from_hex("dead").is_none());
        assert!(txid_blob_from_hex(&"zz".repeat(32)).is_none());
    }
}
