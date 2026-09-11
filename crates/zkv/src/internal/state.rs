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
/// Orchard and Ironwood are *distinct value pools* that share the Orchard
/// receiver, so one UFVK decrypts both and a single Orchard/Ironwood
/// database's memos can land under *either* code: `3` for V5 Orchard outputs
/// and `4` for V6 Ironwood ones (a post-NU6.3 database's own writes are built
/// as V6, since an Orchard wallet auto-upgrades on its first send). Read paths
/// must match both, or V6 memos (including this build's own writes on an
/// Ironwood chain) are invisible. A Sapling database stays code 2 only.
fn pool_output_codes(pool: ShieldedPool) -> &'static [i64] {
    match pool {
        ShieldedPool::Sapling => &[2],
        ShieldedPool::Orchard | ShieldedPool::Ironwood => &[3, 4],
    }
}

/// The base note table and index column behind each pool code.
///
/// These are `zcash_client_sqlite`'s own tables, which is the whole reason
/// [`received_outputs_sql`] carries the warning it does.
fn pool_table(code: i64) -> (&'static str, &'static str) {
    match code {
        2 => ("sapling_received_notes", "output_index"),
        3 => ("orchard_received_notes", "action_index"),
        4 => ("ironwood_received_notes", "action_index"),
        other => unreachable!("pool code {other} is not one zkv reads"),
    }
}

/// The spend-record table for a pool code. Note the singular: the table is
/// named for one note's spend, not for the notes table it references
/// (`orchard_received_note_spends`, beside `orchard_received_notes`), which is
/// close enough to derive wrongly and have SQLite report it only at run time.
fn pool_spends_table(code: i64) -> &'static str {
    match code {
        2 => "sapling_received_note_spends",
        3 => "orchard_received_note_spends",
        4 => "ironwood_received_note_spends",
        other => unreachable!("pool code {other} is not one zkv reads"),
    }
}

/// `v_received_outputs`, restricted to a database's own pools and written out
/// against the base tables, as the body of a subquery.
///
/// Columns: `transaction_id`, `output_index`, `account_id`, `value`, `memo`,
/// `pool`.
///
/// **Why not query the view.** `v_received_outputs` and the `v_tx_outputs` /
/// `v_transactions` views built on it are aggregates over every note and every
/// spend in the wallet, and SQLite pushes no `WHERE` term through them:
/// upstream measured this on SQLite 3.50 against the real schema, with the
/// predicate on `txid` *and* on the grouping column, and both plan as a full
/// scan of all four note tables plus `sent_notes`. So a query that wants one
/// account's memos past a watermark paid a whole-history aggregation, which is
/// exactly the `O(total writes)` cost the snapshot exists to remove: a
/// long-lived oracle paid it on every read, and the history page paid it once
/// per row. zecd hit the same wall from the other side and rewrote its
/// per-transaction reads the same way (upstream #249: `gettransaction` went
/// from 4.6 s to an index seek on a 130k-transaction wallet).
///
/// Restricting each arm at the leaves instead turns every access into an index
/// seek, and dropping the pools this database does not read means the other
/// note tables are never opened at all.
///
/// **This mirrors `zcash_client_sqlite`'s own `v_received_outputs` term for
/// term, so if a librustzcash bump changes that view this must change with
/// it.** `the_base_table_scan_matches_the_view_it_replaces` is what catches
/// that: it populates a real wallet database and requires the two to return
/// identical rows.
fn received_outputs_sql(pool: ShieldedPool) -> String {
    pool_output_codes(pool)
        .iter()
        .map(|&code| {
            let (table, index) = pool_table(code);
            format!(
                "SELECT transaction_id, {index} AS output_index, account_id, value, memo, \
                 {code} AS pool FROM {table}"
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ")
}

/// The spend-record tables for a database's pools, as the body of a subquery
/// with one column: `transaction_id`.
///
/// A row means "a note this wallet received was spent by that transaction",
/// which is how [`fill_tx_fee_and_output`] decides whether a transaction's fee
/// is the wallet's own to report.
///
/// Not account-scoped, matching the `v_transactions` lookup it replaces: a zkv
/// database's wallet file holds exactly one account. A shard file holds several,
/// so a fleet member has to add the predicate here (the note tables carry
/// `account_id`; the spend tables reach it through their note).
fn spent_outputs_sql(pool: ShieldedPool) -> String {
    pool_output_codes(pool)
        .iter()
        .map(|&code| format!("SELECT transaction_id FROM {}", pool_spends_table(code)))
        .collect::<Vec<_>>()
        .join(" UNION ALL ")
}

/// The statement [`scan_memos_past_watermark`] runs.
///
/// A function rather than an inline `format!` so the differential test runs
/// this exact text against a populated wallet database, rather than a
/// transcription of it that could drift.
fn scan_memos_sql(pool: ShieldedPool) -> String {
    format!(
        "SELECT COALESCE(MAX(n.memo, sn.memo), n.memo, sn.memo) AS memo,
                t.mined_height AS mined_height,
                b.time AS block_time,
                fa.uuid AS from_account_uuid,
                t.txid AS txid,
                n.output_index AS output_index
         FROM ({received}) n
         JOIN accounts acct ON acct.id = n.account_id
         JOIN transactions t ON t.id_tx = n.transaction_id
         LEFT JOIN blocks b ON b.height = t.mined_height
         LEFT JOIN sent_notes sn
                ON sn.transaction_id = n.transaction_id
               AND sn.output_pool = n.pool
               AND sn.output_index = n.output_index
         LEFT JOIN accounts fa ON fa.id = sn.from_account_id
         WHERE acct.uuid = :account_uuid
           AND (n.memo IS NOT NULL OR fa.uuid = :account_uuid)
           AND (t.mined_height IS NOT NULL
                OR t.expiry_height IS NULL
                OR t.expiry_height = 0
                OR t.expiry_height >= :tip)
           AND (t.mined_height IS NULL
                OR t.mined_height > :wm_height
                OR (t.mined_height = :wm_height
                    AND (t.txid > :wm_txid
                         OR (t.txid = :wm_txid
                             AND n.output_index > :wm_output_index))))
         ORDER BY t.mined_height ASC NULLS LAST, t.txid ASC, n.output_index ASC",
        received = received_outputs_sql(pool),
    )
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
    // The received note carries its own memo directly. For the wallet's own
    // Ironwood self-sends, librustzcash's `backfill_self_send_memos` pass (run
    // post-scan in `put_blocks`) fills the received-note memo from the stored raw
    // transaction, so no `sent_notes` fallback is needed. (An earlier build read
    // the memo via a `COALESCE` scalar subquery against `sent_notes` because the
    // pool-4 received note was scanned with a NULL memo; that workaround is gone
    // now that the received note is populated at its own pool code.)
    // Written against the base tables rather than `v_tx_outputs` joined to
    // `v_transactions`; see [`received_outputs_sql`] for why, and for what has
    // to move if librustzcash changes those views.
    //
    // Three details carry over from the view rather than being simplifications
    // of it. The memo is `MAX(memo)` over the received row and the `sent_notes`
    // row for the same output, which for two values is "the larger of the two,
    // ignoring NULL" (SQLite's scalar `MAX` is NULL if either argument is, so
    // the `COALESCE` supplies the aggregate's NULL-skipping). `from_account_uuid`
    // comes from that same `sent_notes` row, and is what makes an output the
    // wallet created recognisable as a self-send. And the join to
    // `v_transactions` was never a filter here: it emits a row for every
    // (account, transaction) the account received in, which is every row this
    // query can select, so reading `transactions` and `blocks` directly drops
    // nothing.
    let mut stmt = conn.prepare(&scan_memos_sql(pool))?;

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

/// The lowest height at which this account holds an output in its own pool
/// whose transaction has not been fetched in full yet, so its memos are still
/// unknown. `None` when every such transaction has been enhanced.
///
/// Scanning a compact block records the transaction through librustzcash's
/// `put_tx_meta`, which leaves `transactions.raw` NULL; only the enhancement
/// pass (`put_tx_data`, reached via `decrypt_and_store_transaction`) writes
/// `raw`. Compact blocks carry no memos, so `raw IS NULL` is exactly "this
/// transaction's memos are not known yet" -- and a not-yet-known memo reads as
/// NULL, which [`scan_memos_past_watermark`] cannot distinguish from a genuine
/// no-memo output. Such a row must therefore stay *above* the watermark until
/// its memo lands; see [`promote_cutoff`].
///
/// Note this reads the base `transactions` table, which is where `raw` lives:
/// `v_transactions` does not expose it. The outputs come from the base note
/// tables too, for the reason [`received_outputs_sql`] gives.
fn unenhanced_floor(
    conn: &Connection,
    account_uuid_bytes: &[u8],
    pool: ShieldedPool,
) -> anyhow::Result<Option<u32>> {
    let floor: Option<i64> = conn.query_row(
        &format!(
            "SELECT MIN(t.mined_height)
             FROM ({received}) n
             JOIN accounts acct ON acct.id = n.account_id
             JOIN transactions t ON t.id_tx = n.transaction_id
             WHERE acct.uuid = :account_uuid
               AND t.mined_height IS NOT NULL
               AND t.raw IS NULL",
            received = received_outputs_sql(pool),
        ),
        named_params! { ":account_uuid": account_uuid_bytes },
        |row| row.get(0),
    )?;
    Ok(floor
        .filter(|h| *h > 0)
        .map(|h| u32::try_from(h).unwrap_or(u32::MAX)))
}

/// The highest block height a row may be at and still be safe to promote into
/// the snapshot (advancing the watermark to it). A row qualifies only when it
/// is all of:
///
/// * at least [`SAFE_DEPTH`] blocks below the chain `tip` (reorg safety),
/// * at or below the wallet's `fully_scanned` frontier, so every block up to
///   the new watermark has already been scanned and no earlier memo (e.g. the
///   genesis INIT) can be backfilled below the watermark afterward, and
/// * below the lowest un-enhanced row ([`unenhanced_floor`]), so the watermark
///   never passes a transaction whose memos have not been fetched yet.
///
/// The third clamp matters because scanning and enhancement are separate
/// passes: a block can be fully scanned while the memos in it are still
/// unknown (they arrive only with the full transaction). Promoting past such a
/// row would drop it below the watermark, and the memo it later gains would
/// never be read: silent data loss, and for a genesis INIT specifically, a
/// database stuck "uninitialized". A wallet engine that enhances lazily in the
/// background (rather than in the same pass as the scan) widens that window
/// from rare to routine.
///
/// Holding the cutoff back only costs replay work: an un-promoted row stays in
/// the live tail and is replayed on each read, so a transaction that never
/// enhances slows reads down but never corrupts them.
///
/// Pure so the boundary is unit-testable without a wallet DB.
fn promote_cutoff(tip: u32, fully_scanned: u32, unenhanced_from: Option<u32>) -> u32 {
    let scanned_safe = tip.saturating_sub(SAFE_DEPTH).min(fully_scanned);
    match unenhanced_from {
        Some(h) => scanned_safe.min(h.saturating_sub(1)),
        None => scanned_safe,
    }
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
    //
    // Being scanned is not enough on its own, though: compact blocks carry no
    // memos, so a scanned row's memo arrives only when the full transaction is
    // fetched. The cutoff is therefore also held below the lowest row still
    // waiting for that fetch.
    let unenhanced_from = unenhanced_floor(&conn, account_uuid_bytes.as_slice(), cfg.pool)?;
    let promote_cutoff = promote_cutoff(tip, fully_scanned, unenhanced_from);

    // A row waiting on its full transaction is routine while a sync is in
    // flight, and self-clears when the enhancement pass reaches it. One that
    // stays pending far behind the frontier is not: enhancement is stuck, and
    // the snapshot has stopped promoting, so every read replays a tail that
    // only grows. Reads stay correct either way, so this is a diagnostic, not
    // an error.
    if let Some(h) = unenhanced_from {
        let scanned_safe = tip.saturating_sub(SAFE_DEPTH).min(fully_scanned);
        if h.saturating_sub(1) < scanned_safe {
            let held_back = scanned_safe - h.saturating_sub(1);
            if held_back > SAFE_DEPTH {
                tracing::warn!(
                    unenhanced_height = h,
                    held_back_blocks = held_back,
                    "a transaction at height {h} has still not been fetched in full, so its \
                     memos are unknown and the snapshot cannot promote past it; reads stay \
                     correct but replay a growing tail. Re-running sync usually clears it",
                );
            } else {
                tracing::debug!(
                    unenhanced_height = h,
                    held_back_blocks = held_back,
                    "holding the promote cutoff below a transaction awaiting enhancement",
                );
            }
        }
    }
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
/// - **Fee** comes from `transactions.fee`, but only when this wallet *built*
///   the tx, which is to say only when it spent one of its own notes in it. A
///   received tx may report a fee that was paid by the *sender*, not us;
///   showing it as our fee is the "output amount shows up as the fee" bug, so
///   a received write leaves `fee` unset.
///
///   The old form asked `v_transactions.account_balance_delta < 0` for this,
///   which is the same question one step removed: the delta is negative exactly
///   when the account spent more than it received, and a wallet that spends in
///   its own transaction always pays the fee out of that difference. Asking
///   whether it spent is both the intent the doc comment already stated and a
///   query that can use an index, where summing the delta means aggregating
///   every note in the wallet (see [`received_outputs_sql`]). The two answers
///   differ only if a transaction both spends the wallet's notes and pays it
///   more than the fee from outside, which zkv never builds and which would
///   still be a transaction whose fee is ours.
/// - **Output value** is the zatoshi `value` of this write's own shielded
///   output, matched on txid + output index + pool. A plain zkv write is a
///   zero-value output and stays `None`; a nonzero value means the write also
///   moved ZEC (a tip/deposit broadcast with the memo).
///
/// Entries with no resolvable txid (pending-from-`pending.toml`) are skipped.
fn fill_tx_fee_and_output(
    conn: &Connection,
    entries: &mut [HistoryEntry],
    pool: ShieldedPool,
) -> anyhow::Result<()> {
    let mut tx_stmt = conn.prepare(&format!(
        "SELECT t.fee AS fee,
                EXISTS (SELECT 1 FROM ({spends}) s WHERE s.transaction_id = t.id_tx) AS spent
         FROM transactions t
         WHERE t.txid = ?1",
        spends = spent_outputs_sql(pool),
    ))?;
    let mut out_stmt = conn.prepare(&format!(
        "SELECT n.value
         FROM ({received}) n
         JOIN transactions t ON t.id_tx = n.transaction_id
         WHERE t.txid = ?1 AND n.output_index = ?2",
        received = received_outputs_sql(pool),
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
                        let fee: Option<i64> = row.get(0)?;
                        let spent: bool = row.get(1)?;
                        Ok((fee, spent))
                    })
                    .optional()?
                    .and_then(|(fee, spent)| {
                        // We spent one of our own notes in it, so we built it,
                        // so the fee is ours to show.
                        fee.filter(|f| spent && *f >= 0).map(|f| f as u64)
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
        // tip and every transaction has been enhanced, so the only binding
        // constraint is the SAFE_DEPTH reorg margin.
        let tip = 4_000_000;
        assert_eq!(
            promote_cutoff(tip, tip, None),
            tip - SAFE_DEPTH,
            "a caught-up wallet promotes everything older than SAFE_DEPTH",
        );
        assert_eq!(promote_cutoff(tip, tip + 50, None), tip - SAFE_DEPTH);
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
        assert_eq!(promote_cutoff(tip, fully_scanned, None), fully_scanned);
        assert!(promote_cutoff(tip, fully_scanned, None) < tip - SAFE_DEPTH);
    }

    #[test]
    fn promote_cutoff_is_zero_before_any_scan() {
        // Brand-new wallet: nothing scanned, so nothing is promotable yet (the
        // whole history stays in the live tail and is replayed in memory).
        assert_eq!(promote_cutoff(4_000_000, 0, None), 0);
        assert_eq!(promote_cutoff(0, 0, None), 0);
    }

    #[test]
    fn promote_cutoff_stays_below_the_lowest_unenhanced_row() {
        // A block can be scanned while the memos in it are still unknown:
        // compact blocks carry no memos, so a row's memo arrives only when the
        // full transaction is fetched. Promoting past such a row would push it
        // below the watermark, and the memo it later gains would never be read.
        let tip = 4_000_000;
        let unenhanced = 3_500_000;
        assert_eq!(
            promote_cutoff(tip, tip, Some(unenhanced)),
            unenhanced - 1,
            "the cutoff stops one block short of the un-enhanced row",
        );
        // The clamp only ever holds the cutoff back, never pushes it forward:
        // an un-enhanced row above the scanned/reorg-safe frontier is already
        // in the live tail and changes nothing.
        assert_eq!(
            promote_cutoff(tip, tip, Some(tip - 5)),
            tip - SAFE_DEPTH,
            "an un-enhanced row inside the reorg margin is not the binding constraint",
        );
    }

    /// One fixture row: `(txid, mined_height, enhanced, to_account, pool)`.
    /// `enhanced` stands in for a non-NULL `transactions.raw`, and a `None`
    /// height for a transaction still in the mempool.
    type FixtureRow<'a> = (&'a [u8], Option<i64>, bool, &'a [u8], i64);

    /// A stand-in for the two `data.sqlite` relations [`unenhanced_floor`]
    /// reads, shaped like librustzcash's: the `transactions` base table (for
    /// `raw`, which `v_transactions` does not expose) and the `v_tx_outputs`
    /// view, here a plain table since only its columns matter to a SELECT.
    /// Stand-ins for the base tables [`unenhanced_floor`] reads: only the
    /// columns it touches. That the query agrees with the librustzcash view it
    /// replaced is covered by
    /// [`super::differential::the_unenhanced_floor_matches_the_view_it_replaces`],
    /// which runs against a populated real wallet database; this fixture is for
    /// enumerating the selection rules cheaply.
    fn wallet_db_fixture(rows: &[FixtureRow<'_>]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE accounts (id INTEGER PRIMARY KEY, uuid BLOB NOT NULL);
             CREATE TABLE transactions (
                 id_tx INTEGER PRIMARY KEY, txid BLOB NOT NULL, mined_height INTEGER, raw BLOB
             );
             CREATE TABLE sapling_received_notes (
                 transaction_id INTEGER, output_index INTEGER, account_id INTEGER,
                 value INTEGER, memo BLOB
             );
             CREATE TABLE orchard_received_notes (
                 transaction_id INTEGER, action_index INTEGER, account_id INTEGER,
                 value INTEGER, memo BLOB
             );
             CREATE TABLE ironwood_received_notes (
                 transaction_id INTEGER, action_index INTEGER, account_id INTEGER,
                 value INTEGER, memo BLOB
             );",
        )
        .unwrap();
        let mut accounts: Vec<&[u8]> = Vec::new();
        for (i, (txid, mined_height, enhanced, to_account, pool)) in rows.iter().enumerate() {
            let id_tx = i as i64 + 1;
            if !accounts.contains(to_account) {
                accounts.push(to_account);
                conn.execute(
                    "INSERT INTO accounts (id, uuid) VALUES (?1, ?2)",
                    rusqlite::params![accounts.len() as i64, to_account],
                )
                .unwrap();
            }
            let account_id = accounts.iter().position(|a| a == to_account).unwrap() as i64 + 1;
            conn.execute(
                "INSERT INTO transactions (id_tx, txid, mined_height, raw) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id_tx, txid, mined_height, enhanced.then(|| vec![0u8; 4])],
            )
            .unwrap();
            let (table, index_col) = pool_table(*pool);
            conn.execute(
                &format!(
                    "INSERT INTO {table} (transaction_id, {index_col}, account_id, value, memo)
                     VALUES (?1, 0, ?2, 0, NULL)"
                ),
                rusqlite::params![id_tx, account_id],
            )
            .unwrap();
        }
        conn
    }

    const US: &[u8] = b"our-account-uuid";
    const THEM: &[u8] = b"other-account-id";

    #[test]
    fn unenhanced_floor_is_none_when_everything_is_enhanced() {
        let conn = wallet_db_fixture(&[
            (b"tx1", Some(100), true, US, 3),
            (b"tx2", Some(200), true, US, 4),
        ]);
        assert_eq!(
            unenhanced_floor(&conn, US, ShieldedPool::Orchard).unwrap(),
            None,
            "nothing is waiting on a full-transaction fetch",
        );
    }

    #[test]
    fn unenhanced_floor_finds_the_lowest_pending_row() {
        let conn = wallet_db_fixture(&[
            (b"tx1", Some(100), true, US, 3),
            (b"tx2", Some(300), false, US, 3),
            (b"tx3", Some(250), false, US, 4), // lower, and an Ironwood output
            (b"tx4", Some(400), false, US, 3),
        ]);
        assert_eq!(
            unenhanced_floor(&conn, US, ShieldedPool::Orchard).unwrap(),
            Some(250),
            "an Orchard database counts both pool codes 3 and 4",
        );
    }

    #[test]
    fn unenhanced_floor_ignores_other_accounts_pools_and_unmined_rows() {
        let conn = wallet_db_fixture(&[
            (b"tx1", Some(10), false, THEM, 3), // another account
            (b"tx2", Some(20), false, US, 2),   // Sapling, not our pool
            (b"tx3", None, false, US, 3),       // still in the mempool
            (b"tx4", Some(900), false, US, 3),  // the only row that counts
        ]);
        assert_eq!(
            unenhanced_floor(&conn, US, ShieldedPool::Orchard).unwrap(),
            Some(900),
        );
        // ... and a Sapling database sees the mirror image.
        assert_eq!(
            unenhanced_floor(&conn, US, ShieldedPool::Sapling).unwrap(),
            Some(20),
        );
    }

    #[test]
    fn unenhanced_floor_on_an_empty_wallet_is_none() {
        // MIN() over no rows is SQL NULL, not an error or a zero.
        let conn = wallet_db_fixture(&[]);
        assert_eq!(
            unenhanced_floor(&conn, US, ShieldedPool::Orchard).unwrap(),
            None,
        );
    }

    #[test]
    fn promote_cutoff_with_unenhanced_genesis_promotes_nothing() {
        // The pathological case this clamp exists for: the very first block
        // holding one of our outputs is scanned but not yet enhanced. Nothing
        // may promote until it is, so the genesis INIT cannot be buried.
        assert_eq!(promote_cutoff(4_000_000, 4_000_000, Some(1)), 0);
        // Saturating: a row at height 0 is not a real mined height, but the
        // arithmetic must not underflow regardless.
        assert_eq!(promote_cutoff(4_000_000, 4_000_000, Some(0)), 0);
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

/// The base-table rewrites, checked against the librustzcash views they
/// replace on a real wallet database.
///
/// zkv's read path used to select from `v_tx_outputs` joined to
/// `v_transactions`, and those views aggregate every note in the wallet, so
/// every read paid a whole-history aggregation (see [`received_outputs_sql`]).
/// The replacements are written against the base tables and must answer
/// *identically*, so each test here runs both and requires the same rows. A
/// librustzcash bump that changes either view fails the build rather than
/// quietly changing what zkv reads.
///
/// The fixture is a real `data.sqlite` created through
/// [`crate::data::open_wallet_db`] (so the schema and the views are the ones
/// shipping), populated by direct inserts rather than by scanning a chain.
#[cfg(test)]
mod differential {
    use super::*;
    use rusqlite::params;

    const US: [u8; 16] = [1u8; 16];
    const THEM: [u8; 16] = [2u8; 16];

    /// The query zkv ran before the rewrite, kept here as the reference the new
    /// one is required to agree with.
    fn legacy_scan_sql(pool: ShieldedPool) -> String {
        let pools = pool_output_codes(pool)
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "SELECT v.memo AS memo,
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
             ORDER BY t.mined_height ASC NULLS LAST, v.txid ASC, v.output_index ASC"
        )
    }

    type ScanRow = (
        Option<Vec<u8>>,
        Option<i64>,
        Option<i64>,
        Option<Vec<u8>>,
        Vec<u8>,
        i64,
    );

    fn run_scan(conn: &Connection, sql: &str, wm: &snapshot::Watermark) -> Vec<ScanRow> {
        let mut stmt = conn.prepare(sql).unwrap();
        stmt.query_map(
            named_params! {
                ":account_uuid": &US[..],
                ":tip": 400,
                ":wm_height": wm.height,
                ":wm_txid": &wm.txid,
                ":wm_output_index": wm.output_index,
            },
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }

    fn txid(n: u8) -> Vec<u8> {
        vec![n; 32]
    }

    /// A populated wallet database covering every shape the read path
    /// distinguishes: a self-send (the wallet created the output, so it has a
    /// `sent_notes` row and a spend), a plain receive, an unenhanced row, an
    /// unmined row, another account's note, another pool's note, a memo-less
    /// output, and a self-send whose received memo has not been backfilled.
    fn fixture() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.sqlite");
        drop(crate::data::open_wallet_db(&path, crate::network::Network::Test).unwrap());
        let conn = Connection::open(&path).unwrap();

        for (id, uuid) in [(1i64, US), (2, THEM)] {
            conn.execute(
                "INSERT INTO accounts (id, uuid, account_kind, uivk, birthday_height)
                 VALUES (?1, ?2, 1, ?3, 1)",
                params![id, &uuid[..], format!("uivk{id}")],
            )
            .unwrap();
        }
        for (height, time) in [(100i64, 1_000i64), (200, 2_000)] {
            conn.execute(
                "INSERT INTO blocks (height, hash, time, sapling_tree) VALUES (?1, ?2, ?3, ?4)",
                params![height, vec![height as u8; 32], time, Vec::<u8>::new()],
            )
            .unwrap();
        }

        // (id_tx, txid byte, mined_height, enhanced, expiry, fee)
        type TxRow = (i64, u8, Option<i64>, bool, Option<i64>, Option<i64>);
        let txs: &[TxRow] = &[
            (1, 0xA1, Some(100), true, Some(150), Some(1_000)),
            (2, 0xB2, Some(200), true, Some(250), Some(2_000)),
            (3, 0xC3, Some(200), false, Some(250), None),
            (4, 0xD4, None, true, Some(500), Some(4_000)),
            (5, 0xE5, Some(100), true, Some(150), None),
            (6, 0xF6, Some(100), true, Some(150), None),
            (7, 0x17, Some(100), true, Some(150), None),
            (8, 0x28, Some(100), true, Some(150), Some(8_000)),
            (9, 0x39, Some(100), true, Some(150), None),
        ];
        for (id, b, mined, enhanced, expiry, fee) in txs {
            conn.execute(
                "INSERT INTO transactions
                     (id_tx, txid, block, mined_height, expiry_height, raw, fee, min_observed_height)
                 VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6, 1)",
                params![
                    id,
                    txid(*b),
                    mined,
                    expiry,
                    enhanced.then(|| vec![0u8; 4]),
                    fee
                ],
            )
            .unwrap();
        }

        // (table, transaction_id, index, account_id, memo)
        let notes: &[(&str, i64, i64, i64, Option<&str>)] = &[
            // A self-send: the wallet created this output (sent_notes below).
            ("orchard", 1, 0, 1, Some("one")),
            // A plain receive, in the Ironwood pool.
            ("ironwood", 2, 1, 1, Some("two")),
            // Mined but not yet enhanced: memo unknown.
            ("orchard", 3, 0, 1, None),
            // Unmined, still within its expiry.
            ("orchard", 4, 0, 1, Some("four")),
            // Another account's note.
            ("orchard", 5, 0, 2, Some("them")),
            // Another pool: invisible to an Ironwood database.
            ("sapling", 6, 0, 1, Some("sapling")),
            // No memo and not a self-send: excluded by the read path.
            ("orchard", 7, 0, 1, None),
            // A self-send whose received memo was not backfilled, so the memo
            // has to come off the sent_notes row.
            ("orchard", 8, 0, 1, None),
            // The note tx1 spends, so the wallet "spent in" tx1.
            ("orchard", 9, 0, 1, Some("prior")),
        ];
        for (i, (pool, tx, idx, acct, memo)) in notes.iter().enumerate() {
            let id = i as i64 + 1;
            let (table, index_col, extra_cols, extra_vals) = match *pool {
                "sapling" => ("sapling_received_notes", "output_index", ", rcm", ", X''"),
                "orchard" => (
                    "orchard_received_notes",
                    "action_index",
                    ", rho, rseed",
                    ", X'', X''",
                ),
                _ => (
                    "ironwood_received_notes",
                    "action_index",
                    ", rho, rseed, note_version",
                    ", X'', X'', 3",
                ),
            };
            conn.execute(
                &format!(
                    "INSERT INTO {table}
                         (id, transaction_id, {index_col}, account_id, diversifier, value,
                          is_change, memo{extra_cols})
                     VALUES (?1, ?2, ?3, ?4, X'', ?5, 0, ?6{extra_vals})"
                ),
                params![id, tx, idx, acct, 100_000 + id, memo.map(|m| m.as_bytes())],
            )
            .unwrap();
        }

        // The wallet created tx1's and tx8's outputs, and spent a note in tx1.
        // The third row is an output the wallet *sent* to somebody else in tx1,
        // with a memo and no received note of its own: visible to the view,
        // invisible to the read path.
        conn.execute(
            "INSERT INTO sent_notes
                 (id, transaction_id, output_pool, output_index, from_account_id, value, memo)
             VALUES (1, 1, 3, 0, 1, 100001, ?1),
                    (2, 8, 3, 0, 1, 100008, ?2),
                    (3, 1, 3, 7, 1, 500000, ?3)",
            params![&b"one"[..], &b"eight"[..], &b"outgoing"[..]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO orchard_received_note_spends (orchard_received_note_id, transaction_id)
             VALUES (9, 1)",
            [],
        )
        .unwrap();
        (dir, conn)
    }

    /// A history entry carrying only what [`fill_tx_fee_and_output`] reads.
    /// `HistoryEntry` has no `Default`, and spelling out every field at three
    /// call sites would bury the two that matter.
    fn entry_for(blob: &[u8], output_index: u32) -> HistoryEntry {
        HistoryEntry {
            op: crate::protocol::Op::Set,
            key: String::new(),
            value: None,
            height: None,
            timestamp: None,
            txid: hex_txid(blob),
            output_index,
            signature: None,
            seq: None,
            signer: None,
            verified: None,
            status: crate::protocol::HistoryStatus::Confirmed { confirmations: 1 },
            memo: None,
            fee: None,
            output_value: None,
        }
    }

    #[test]
    fn the_base_table_scan_matches_the_view_it_replaces() {
        let (_dir, conn) = fixture();
        for pool in [ShieldedPool::Ironwood, ShieldedPool::Sapling] {
            for wm in [
                snapshot::Watermark::default(),
                snapshot::Watermark {
                    height: 100,
                    txid: txid(0xA1),
                    output_index: 0,
                },
                snapshot::Watermark {
                    height: 200,
                    txid: txid(0xFF),
                    output_index: 0,
                },
            ] {
                let new = run_scan(&conn, &scan_memos_sql(pool), &wm);
                let old = run_scan(&conn, &legacy_scan_sql(pool), &wm);
                assert_eq!(
                    new, old,
                    "pool {pool:?}, watermark height {}: the base-table scan must return \
                     exactly what the view returned",
                    wm.height,
                );
            }
        }
    }

    /// The rows the reference query is actually asked to produce, so a fixture
    /// that accidentally selects nothing cannot make the comparison vacuous.
    #[test]
    fn the_scan_sees_the_writes_the_fixture_planted() {
        let (_dir, conn) = fixture();
        let rows = run_scan(
            &conn,
            &scan_memos_sql(ShieldedPool::Ironwood),
            &snapshot::Watermark::default(),
        );
        let memos: Vec<Option<String>> = rows
            .iter()
            .map(|r| {
                r.0.as_ref()
                    .map(|m| String::from_utf8_lossy(m).into_owned())
            })
            .collect();
        assert_eq!(
            memos,
            vec![
                // Chain order is (height, txid, output index), and these three
                // share a height, so they come back in txid order: 0x28, 0x39,
                // 0xA1. tx8's received memo is NULL and the sent_notes row
                // supplies it.
                Some("eight".into()),
                Some("prior".into()),
                Some("one".into()),
                Some("two".into()),
                Some("four".into()),
            ],
            "mined rows in chain order, then the unmined one",
        );
        // The self-sends are attributed to this account; the plain receive is not.
        assert_eq!(rows[0].3.as_deref(), Some(&US[..]));
        assert_eq!(rows[2].3.as_deref(), Some(&US[..]));
        assert_eq!(rows[3].3, None);
        // Another account's note, another pool's note, the unenhanced row and
        // the memo-less receive are all absent.
        assert!(!memos.contains(&Some("them".into())));
        assert!(!memos.contains(&Some("sapling".into())));
    }

    #[test]
    fn the_unenhanced_floor_matches_the_view_it_replaces() {
        let (_dir, conn) = fixture();
        for pool in [ShieldedPool::Ironwood, ShieldedPool::Sapling] {
            let pools = pool_output_codes(pool)
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let old: Option<i64> = conn
                .query_row(
                    &format!(
                        "SELECT MIN(tx.mined_height)
                         FROM v_tx_outputs v
                         JOIN transactions tx ON tx.txid = v.txid
                         WHERE v.to_account_uuid = :account_uuid
                           AND v.output_pool IN ({pools})
                           AND tx.mined_height IS NOT NULL
                           AND tx.raw IS NULL",
                    ),
                    named_params! { ":account_uuid": &US[..] },
                    |r| r.get(0),
                )
                .unwrap();
            let new = unenhanced_floor(&conn, &US, pool).unwrap();
            assert_eq!(
                new,
                old.filter(|h| *h > 0).map(|h| h as u32),
                "pool {pool:?}",
            );
        }
        // And it is the height the fixture planted, not a vacuous None.
        assert_eq!(
            unenhanced_floor(&conn, &US, ShieldedPool::Ironwood).unwrap(),
            Some(200),
        );
    }

    #[test]
    fn the_fee_and_output_lookups_match_the_views_they_replace() {
        let (_dir, conn) = fixture();
        let pool = ShieldedPool::Ironwood;
        let pools = "3, 4";
        for (tx_byte, output_index) in [(0xA1u8, 0i64), (0xB2, 1), (0x28, 0)] {
            let blob = txid(tx_byte);

            let old_fee: Option<u64> = conn
                .query_row(
                    "SELECT account_balance_delta, fee_paid FROM v_transactions WHERE txid = ?1",
                    [&blob],
                    |row| {
                        let delta: Option<i64> = row.get(0)?;
                        let fee: Option<i64> = row.get(1)?;
                        Ok((delta, fee))
                    },
                )
                .optional()
                .unwrap()
                .and_then(|(delta, fee)| {
                    let outgoing = delta.unwrap_or(0) < 0;
                    fee.filter(|f| outgoing && *f >= 0).map(|f| f as u64)
                });

            let mut entries = vec![entry_for(&blob, output_index as u32)];
            fill_tx_fee_and_output(&conn, &mut entries, pool).unwrap();
            assert_eq!(
                entries[0].fee, old_fee,
                "tx {tx_byte:#x}: the fee must match what the view reported",
            );

            let old_value: Option<i64> = conn
                .query_row(
                    &format!(
                        "SELECT value FROM v_tx_outputs
                         WHERE txid = ?1 AND output_index = ?2 AND output_pool IN ({pools})"
                    ),
                    params![&blob, output_index],
                    |r| r.get(0),
                )
                .optional()
                .unwrap();
            assert_eq!(
                entries[0].output_value,
                old_value.filter(|v| *v > 0).map(|v| v as u64),
                "tx {tx_byte:#x}: the output value must match what the view reported",
            );
        }

        // Not vacuous: tx1 is a self-send, so its fee is the wallet's own and
        // is reported; tx2 is a plain receive, so its fee is the sender's.
        let mut entries = vec![entry_for(&txid(0xA1), 0), entry_for(&txid(0xB2), 1)];
        fill_tx_fee_and_output(&conn, &mut entries, pool).unwrap();
        assert_eq!(entries[0].fee, Some(1_000));
        assert_eq!(entries[1].fee, None);
    }

    /// The pending-GC set must be exactly the mined transactions whose memo the
    /// read path can see, which is what lets a pending entry be dropped without
    /// the state flapping.
    ///
    /// The reference is the old view query plus one predicate,
    /// `to_account_uuid IS NOT NULL`, which keeps the rows the wallet
    /// *received*. Without it the view also matches an output the wallet sent
    /// to a third party, which has no received note and which the read path
    /// therefore cannot see; the fixture plants one (tx10) so this is exercised
    /// rather than assumed. Dropping a pending entry on the strength of such a
    /// row would drop it while the write was still invisible.
    #[test]
    fn the_pending_gc_set_is_what_the_read_path_can_see() {
        let (_dir, conn) = fixture();
        let mut old: Vec<String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT DISTINCT v.txid FROM v_tx_outputs v
                     JOIN v_transactions t ON t.txid = v.txid
                     WHERE t.mined_height IS NOT NULL
                       AND v.memo IS NOT NULL
                       AND v.to_account_uuid IS NOT NULL",
                )
                .unwrap();
            let rows = stmt
                .query_map([], |r| r.get::<_, Vec<u8>>(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .filter_map(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
                .map(|a| zcash_primitives::transaction::TxId::from_bytes(a).to_string())
                .collect::<Vec<_>>();
            rows
        };
        let mut new: Vec<String> = crate::internal::sync::mined_with_memo_txids(&conn)
            .unwrap()
            .into_iter()
            .collect();
        old.sort();
        new.sort();
        assert_eq!(new, old);
        assert!(!new.is_empty(), "the fixture must plant memos to compare");
    }

    /// `HistoryEntry`'s txid field is the display (reversed) hex, which is what
    /// `fill_tx_fee_and_output` converts back to a blob.
    fn hex_txid(blob: &[u8]) -> String {
        let arr = <[u8; 32]>::try_from(blob).unwrap();
        zcash_primitives::transaction::TxId::from_bytes(arr).to_string()
    }
}
