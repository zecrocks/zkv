//! What is left of zkv's own wallet-sync surface now that the scan itself runs
//! in the wallet engine.
//!
//! The compact-block download, the scan, the reorg rewind and the enhancement
//! pass all live in the node ([`crate::engine`]). What stays here is the part
//! that is either about *memos* rather than blocks, or that has to run when no
//! node exists yet:
//!
//! - [`read_sync`] and friends: the read-side sequence (decide whether a scan
//!   can be skipped, otherwise ask the node to catch up, then prune
//!   `pending.toml`). The pruning is zkv's, not the node's.
//! - Birthday pinning ([`near_tip_birthday`], [`pinned_birthday`]): a wallet's
//!   birthday has to be chosen *before* the wallet it belongs to exists, so
//!   there is no node to ask. These keep zkv's own lightwalletd transport.
//! - [`wallet_synced_to_tip`], the INIT re-broadcast gate.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::anyhow;

use zcash_client_backend::{
    data_api::{AccountBirthday, WalletRead},
    proto::service,
};
use zcash_primitives::transaction::TxId;

use crate::{
    data::{get_db_paths, open_wallet_db},
    internal::pending,
    remote::ConnectionArgs,
};

/// How many blocks behind the live tip a *read* sync may be and still skip the
/// whole download/scan/enhance pipeline. Reads default to `--confirmations 3`,
/// so the newest block or two never affect confirmed state; paying for a full
/// pipeline pass on every read just to pick up a block the reader will ignore
/// is wasted work. Write syncs keep a tolerance of 0 (an accurate tree to build
/// a spend on), so this only relaxes the read path.
///
/// Used now only as the default for read syncs with no explicit confirmation
/// depth (the facade `Database::sync`, `balance`, `show`, `watch`). Reads that
/// carry a `--confirmations` derive their tolerance from it instead, via
/// [`read_tip_tolerance`].
pub const NEAR_TIP_TOLERANCE: u32 = 1;

/// The fast-path skip tolerance for a read at `min_confs` confirmations: how
/// many blocks behind the live tip the wallet may sit and still provably return
/// the same *confirmed* state, so a re-scan would be wasted work.
///
/// A write mined in the not-yet-scanned region `(wallet_tip, rpc_tip]` has at
/// most `behind = rpc_tip - wallet_tip` confirmations (its deepest block,
/// `wallet_tip + 1`, sits `behind` blocks from the tip). So if
/// `behind < min_confs`, none of those unscanned writes can reach the display
/// threshold and the confirmed read is identical whether or not we scan; the
/// exact safe bound is therefore `min_confs - 1`.
///
/// This both relaxes and tightens the old fixed [`NEAR_TIP_TOLERANCE`] of 1
/// depending on the request: at the default `-c 3` it skips up to 2 blocks
/// behind (was 1), while a low `-c 1`/`-c 0` correctly drops to 0 so a
/// freshly-confirmed write is never skipped (the old fixed 1 could hide it). A
/// mempool read (`-c 0`) pulls the mempool separately and wants the freshest
/// tip, so 0 is right there too.
pub fn read_tip_tolerance(min_confs: u32) -> u32 {
    min_confs.saturating_sub(1)
}

/// The tolerance a read at `min_confs` should pass to [`read_sync`].
///
/// `None` at zero confirmations: such a read is asking for unconfirmed state,
/// which only a running node can supply, so it must never skip.
pub fn read_sync_tolerance(min_confs: u32) -> Option<u32> {
    if min_confs == 0 {
        None
    } else {
        Some(read_tip_tolerance(min_confs))
    }
}

/// Maximum age of the chain tip we'll accept before pinning a new wallet
/// birthday (`zkv init`/`restore`) or treating an "uninitialized" verdict as
/// authoritative enough to (re)broadcast INIT. If lightwalletd's tip is older
/// than this, the chain has stalled or the server is stale/unreachable;
/// better to refuse than act on a stale view of the chain.
pub const TIP_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(300);

/// Whether `block_time` (a block's unix timestamp, seconds) is within
/// [`TIP_MAX_AGE`] of the local clock. A future-dated block (clock skew) is
/// treated as fresh.
pub fn tip_time_is_fresh(block_time: u32) -> bool {
    let now = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs(),
        // A clock set before the unix epoch can't tell us whether the tip is
        // fresh; fail closed (the function's own philosophy: "better to refuse
        // than act on a stale view").
        Err(e) => {
            tracing::warn!("system clock is before the unix epoch ({e}); treating tip as stale");
            return false;
        }
    };
    u64::from(block_time).saturating_add(TIP_MAX_AGE.as_secs()) >= now
}

/// Safety margin, in blocks, subtracted from the chain tip when defaulting the
/// birthday of a brand-new wallet (a `zkv init`, or a `zkv restore` that didn't
/// specify `--birthday`). A freshly-reported tip can sit a few blocks ahead of
/// what a from-scratch scan actually reaches; backing the birthday off by this
/// much keeps a new wallet from being pinned just ahead of its own first
/// scannable block. Applied **only** when defaulting near the tip, never to an
/// already-known birthday (an imported address, a stored `keys.toml`, or an
/// explicit `--birthday`), which is always honored verbatim.
pub const BIRTHDAY_SAFETY_BUFFER: u32 = 10;

/// Failure while pinning a wallet birthday against the live chain tip.
///
/// Kept distinct from a plain `anyhow::Error` so the facade can map a stale tip
/// to [`crate::db::ZkvError::StaleChainTip`] while the CLI just renders this
/// type's `Display` (both carry the same message). Named `TipError` rather than
/// `BirthdayError` to avoid colliding with zcash's own `BirthdayError`.
#[derive(Debug)]
pub enum TipError {
    /// The lightwalletd tip is older than [`TIP_MAX_AGE`]: the chain has
    /// stalled, or the server is stale/unreachable. Refused so we never pin a
    /// birthday (or build a wallet) against a stale view of the chain.
    StaleTip,
    /// Any other failure (RPC transport, tree-state parse).
    Other(anyhow::Error),
}

impl std::fmt::Display for TipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TipError::StaleTip => write!(
                f,
                "can't confirm a current chain tip (latest block is over {}s old), \
                 lightwalletd is stale or unreachable; check your connection and retry",
                TIP_MAX_AGE.as_secs(),
            ),
            TipError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for TipError {}

impl From<anyhow::Error> for TipError {
    fn from(e: anyhow::Error) -> Self {
        TipError::Other(e)
    }
}

/// Fetch the chain tip height and reject it if its block timestamp is older
/// than [`TIP_MAX_AGE`]. Cheap: no compact-block download, no scan. This is the
/// "is the server's view of the chain current?" guard the database-creation and
/// import paths share; it does **not** require the local wallet to have caught
/// up to the tip (that is a full sync, a separate thing).
async fn fresh_chain_tip(
    conn: &ConnectionArgs,
    network: crate::network::Network,
) -> Result<u32, TipError> {
    let tip = crate::engine::probe::tip_status(conn, network).await?;
    // Regtest block timestamps are synthetic (zebra's regtest genesis is
    // dated 2011 and `generate`d blocks advance one second per block), so the
    // wall-clock freshness gate can never pass and means nothing there: the
    // harness controls mining, so the tip is current by construction.
    if network == crate::network::Network::Regtest {
        return Ok(tip.height);
    }
    if !tip_time_is_fresh(tip.time) {
        return Err(TipError::StaleTip);
    }
    Ok(tip.height)
}

/// Pin the birthday for a wallet whose birthday is **not known**, defaulting to
/// the chain tip minus [`BIRTHDAY_SAFETY_BUFFER`] blocks. Used when *generating*
/// a brand-new zkv address (`zkv init`), and as the fallback for `zkv restore` /
/// the facade admin-create when no birthday is supplied. Requires a fresh tip
/// ([`TipError::StaleTip`] otherwise; regtest tips are always accepted, see
/// `fresh_chain_tip`).
pub async fn near_tip_birthday(
    conn: &ConnectionArgs,
    network: crate::network::Network,
) -> Result<AccountBirthday, TipError> {
    let chain_tip = fresh_chain_tip(conn, network).await?;
    let birthday_height = chain_tip.saturating_sub(BIRTHDAY_SAFETY_BUFFER);
    account_birthday_at(conn, network, birthday_height, chain_tip).await
}

/// Pin a birthday at an **already-known** height: importing or watching a zkv
/// address (birthday carried in the address), or a `zkv restore` with an
/// explicit `--birthday`. The height is honored **verbatim** (no safety
/// buffer). Still requires a fresh tip ([`TipError::StaleTip`] otherwise;
/// regtest tips are always accepted, see `fresh_chain_tip`).
pub async fn pinned_birthday(
    conn: &ConnectionArgs,
    network: crate::network::Network,
    birthday_height: u32,
) -> Result<AccountBirthday, TipError> {
    let chain_tip = fresh_chain_tip(conn, network).await?;
    account_birthday_at(conn, network, birthday_height, chain_tip).await
}

/// Like [`pinned_birthday`] but **without** the fresh-tip guard. For the
/// wipe-and-rebootstrap recovery, which must rebuild the wallet from the fixed
/// `keys.toml` birthday even if the tip momentarily looks stale: a hard bail
/// there would abort a rebuild the user explicitly asked for. The tip is still
/// fetched, but only as the `recover_until` anchor, not as a freshness gate.
pub async fn pinned_birthday_unchecked(
    conn: &ConnectionArgs,
    network: crate::network::Network,
    birthday_height: u32,
) -> Result<AccountBirthday, TipError> {
    let chain_tip = crate::engine::probe::tip_status(conn, network)
        .await?
        .height;
    account_birthday_at(conn, network, birthday_height, chain_tip).await
}

/// Build an [`AccountBirthday`] at `birthday_height`, anchored to an
/// already-read `chain_tip`. Shared tail of the three pinning entry points.
async fn account_birthday_at(
    conn: &ConnectionArgs,
    network: crate::network::Network,
    birthday_height: u32,
    chain_tip: u32,
) -> Result<AccountBirthday, TipError> {
    crate::engine::probe::account_birthday(conn, network, birthday_height, Some(chain_tip))
        .await
        .map_err(TipError::Other)
}

/// Whether the local wallet has scanned up to the current lightwalletd tip:
/// wallet `chain_height()` is within [`NEAR_TIP_TOLERANCE`] of the rpc tip AND
/// there are no outstanding scan ranges. One connection + `GetLatestBlock`.
/// Used to gate re-broadcasting INIT on an existing database so we don't
/// double-INIT one whose valid INIT is still in not-yet-scanned blocks.
///
/// The tolerance must match the read sync's skip condition in `run_sync_tol`
/// (and the GUI's "synced" indicator): a read sync leaves the wallet up to
/// `NEAR_TIP_TOLERANCE` blocks behind the live tip and never closes that gap,
/// so requiring exact equality here would make INIT refuse indefinitely on a
/// database the rest of the UI already reports as fully synced.
pub async fn wallet_synced_to_tip(
    db_name: &str,
    conn: &ConnectionArgs,
    network: crate::network::Network,
) -> anyhow::Result<bool> {
    let (_, db_data_path) = get_db_paths(db_name)?;
    let db_data = open_wallet_db(&db_data_path, network)?;
    let wallet_tip = db_data.chain_height()?.map(u32::from);
    let pending_scan = !db_data.suggest_scan_ranges()?.is_empty();
    let mut client = conn.connect(network).await?;
    let rpc_tip: u32 = client
        .get_latest_block(service::ChainSpec::default())
        .await?
        .into_inner()
        .height
        .try_into()
        .map_err(|_| anyhow!("chain tip height out of range"))?;
    let behind = rpc_tip.saturating_sub(wallet_tip.unwrap_or(0));
    let within_tolerance = wallet_tip.is_some() && behind <= NEAR_TIP_TOLERANCE;
    Ok(within_tolerance && !pending_scan)
}

/// Whether a read may skip syncing at the given tip `tolerance`, and if so the
/// wallet height it should report.
///
/// `Some(height)` means a scan would be wasted work: the confirmed state a
/// reader sees is provably identical with or without it, by the argument on
/// [`read_tip_tolerance`]. `None` means the wallet has to catch up first.
///
/// This lives here, beside the tolerance it applies, rather than in the caller.
/// The rule is subtle and its failure mode is quiet, a read that silently
/// misses a confirmed write, so there should be exactly one statement of it.
///
/// Cheap by design: one local height read and one `GetLatestBlock`. It
/// deliberately does not need a wallet-engine node, because the common case is
/// that no sync is required and starting a node to discover that would cost
/// far more than the answer is worth.
pub async fn read_sync_skippable(
    db_name: &str,
    conn: &ConnectionArgs,
    network: crate::network::Network,
    tolerance: u32,
) -> anyhow::Result<Option<u32>> {
    // A member the shared scan has not imported yet has no local files to
    // measure, so the honest answer is "nothing to skip on", not a failure. It
    // matters that this is not an error: the caller's next step is the sync
    // that *causes* the import, so erroring here strands a freshly-watched
    // database in the state it is trying to leave. That is exactly what
    // `zkv watch` did on its first sync.
    let (_, db_data_path) = match get_db_paths(db_name) {
        Ok(paths) => paths,
        Err(e) if crate::data::is_import_pending(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    let db_data = open_wallet_db(&db_data_path, network)?;
    let Some(wallet_tip) = db_data.chain_height()?.map(u32::from) else {
        // Never scanned: there is no height to report and nothing to compare.
        return Ok(None);
    };
    // A gap anywhere in the scanned range can hide a confirmed write at any
    // depth, so tolerance says nothing about it.
    if !db_data.suggest_scan_ranges()?.is_empty() {
        return Ok(None);
    }
    drop(db_data);

    let mut client = conn.connect(network).await?;
    let rpc_tip: u32 = client
        .get_latest_block(service::ChainSpec::default())
        .await?
        .into_inner()
        .height
        .try_into()
        .map_err(|_| anyhow!("chain tip height out of range"))?;

    let behind = rpc_tip.saturating_sub(wallet_tip);
    if behind <= tolerance {
        tracing::debug!(
            wallet_tip,
            rpc_tip,
            behind,
            tolerance,
            "wallet is near enough the tip that a read at this depth cannot change",
        );
        return Ok(Some(wallet_tip));
    }
    Ok(None)
}

/// Bring the wallet up to date for a read, through the wallet engine.
///
/// The one place the read-sync sequence lives: decide whether a scan can be
/// skipped ([`read_sync_skippable`]), and only otherwise ask the node to catch
/// up. Both the `db::Database` facade and the one-shot CLI commands call this,
/// so the ordering, and the decision not to start a node when the answer is
/// already known, are stated once.
///
/// Takes the engine rather than building one, because a long-lived caller
/// keeps a node across operations while a CLI command wants a fresh one.
pub async fn read_sync(
    engine: &crate::engine::Engine,
    db_name: &str,
    conn: &ConnectionArgs,
    network: crate::network::Network,
    tolerance: Option<u32>,
    cancel: Option<CancelFlag>,
) -> anyhow::Result<u32> {
    // `None` means never skip. That is what a read wanting unconfirmed writes
    // asks for: the node keeps a live mempool subscription while it is caught
    // up, so bringing it up is what makes the mempool visible at all, and
    // skipping would leave a cold caller with no view of it.
    if let Some(tolerance) = tolerance {
        if let Some(height) = read_sync_skippable(db_name, conn, network, tolerance).await? {
            // Prune here too, not only after a scan. A skip means the wallet is
            // already near the tip, which is exactly when a broadcast written
            // moments ago has confirmed and its `pending.toml` row is ready to
            // drop. A caller whose reads always skip (a warm wallet on a quiet
            // chain) would otherwise never prune at all.
            prune_pending(db_name);
            return Ok(height);
        }
    }
    // The wallet name is the database name on a shared-scan node, which serves
    // many, and zecd's `default` on a node of this database's own.
    let height = engine
        .sync_wallet(engine_wallet_name(engine, db_name), cancel)
        .await?
        .scanned_height;
    prune_pending(db_name);
    Ok(height)
}

/// The zecd wallet name a database is addressed by on a given engine.
pub(crate) fn engine_wallet_name<'a>(engine: &crate::engine::Engine, db_name: &'a str) -> &'a str {
    match engine.kind() {
        crate::engine::EngineKind::Own => crate::engine::WALLET,
        crate::engine::EngineKind::Fleet { .. } => db_name,
    }
}

/// Drop `pending.toml` rows whose transaction has now mined with its memo
/// readable.
///
/// The node knows nothing about that file: it is zkv's own record of what it
/// has broadcast but not yet seen on chain, so the pruning that used to ride
/// along with zkv's scan has to be driven from the paths that replaced it.
/// Left undone the file grows without bound, every read keeps merging writes
/// the chain confirmed long ago, and the write path's version picker keeps
/// counting them as still in flight.
///
/// Best effort by design: an unreadable wallet database is the caller's
/// problem to report, not a reason to fail a sync that otherwise succeeded.
pub(crate) fn prune_pending(db_name: &str) {
    if let Ok((_, db_data_path)) = get_db_paths(db_name) {
        gc_pending(db_name, &db_data_path);
    }
}

/// [`read_sync`] with the transient status line the CLI shows.
///
/// The spinner only paints if the sync outlasts a short grace period, so a read
/// that skips, or one that catches up in a few hundred milliseconds, stays
/// silent. Status goes to stderr, leaving stdout clean for the value.
///
/// The label is recomputed per frame from the engine's live progress, so a long
/// catch-up shows how far along it is rather than only that it is working. It
/// stays a bare "Syncing..." until the node has reported a tip to measure
/// against, which is the first slice and any node too far behind to say.
pub async fn read_sync_with_status(
    engine: &crate::engine::Engine,
    db_name: &str,
    conn: &ConnectionArgs,
    network: crate::network::Network,
    tolerance: Option<u32>,
) -> anyhow::Result<u32> {
    let progress = engine.progress();
    let spinner = crate::ui::Spinner::start_with(
        move || match progress.read() {
            (scanned, Some(tip)) if scanned > 0 && tip > 0 => {
                format!("Syncing... (block {scanned} of {tip})")
            }
            _ => "Syncing...".to_string(),
        },
        SPINNER_GRACE,
    );
    let out = read_sync(engine, db_name, conn, network, tolerance, None).await;
    spinner.stop().await;
    out
}

/// Delay before the spinner's first frame. Zero: a sync always does at least
/// one lightwalletd round-trip, so showing it immediately means the user sees
/// progress *every* time a sync runs (the line is erased on completion, so even
/// an instant cached sync just blinks rather than lingering).
const SPINNER_GRACE: std::time::Duration = std::time::Duration::ZERO;

/// A cooperative cancellation flag for a sync in progress. When it flips to
/// `true`, the scan loop stops at the next block-batch boundary and returns the
/// height reached so far. A partial sync is always safe: scanning is resumable
/// (the wallet DB commits per batch) and the snapshot/tail read model tolerates
/// a wallet that trails the tip. Used by the GUI auto-sync loop so pausing
/// halts in-flight scans promptly instead of only at the next cycle. `None`
/// means never cancel (every CLI/manual sync path passes `None`).
pub type CancelFlag = Arc<AtomicBool>;

/// After a sync pass, prune `pending.toml` entries whose tx the wallet has now
/// indexed **with its memo decrypted**. The local cache bridges the gap between
/// broadcast (we know the memo exists) and the read path seeing it on chain.
///
/// We deliberately require a decrypted memo (`v_tx_outputs.memo IS NOT NULL`),
/// not just a mined tx: the compact-block scan records a tx as mined in
/// `v_transactions` *before* `enhance` downloads the full tx and decrypts its
/// memo into `v_tx_outputs`, and the tolerance-skip sync path runs this GC
/// without scanning at all. Pruning on mined-only would drop the pending entry
/// during that window, while the read path (which sources memos from
/// `v_tx_outputs WHERE memo IS NOT NULL`) still can't see the write, so the
/// state would flap (a confirming INIT briefly reverting to "uninitialized",
/// or a just-set key vanishing) until the next enhance. Gating on the decrypted
/// memo keeps the pending entry until the on-chain row can take over seamlessly.
/// Every pending entry is a zkv write/INIT, which always carries a memo, so this
/// never strands a legitimate entry (the staleness GC in `pending::load` is the
/// backstop for a tx that never lands).
fn gc_pending(db_name: &str, db_data_path: &Path) {
    let conn = match rusqlite::Connection::open(db_data_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("pending GC: open db: {e}");
            return;
        }
    };
    let seen = match mined_with_memo_txids(&conn) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("pending GC: query: {e}");
            return;
        }
    };
    if let Err(e) = pending::prune(db_name, &seen) {
        tracing::warn!("pending GC failed: {e:#}");
    }
}

/// Txids the wallet has indexed **with a decrypted memo**: mined, with a
/// received note carrying a non-NULL memo. This is exactly the set the read
/// path can see (it selects memos the same way), so a pending entry whose txid
/// is in this set can be dropped without the state flapping. A tx that is
/// merely mined (compact-scanned) but not yet enhanced has a NULL memo and is
/// deliberately excluded.
///
/// Reads the base note tables rather than `v_tx_outputs` joined to
/// `v_transactions`, for the reason
/// [`crate::internal::state::received_outputs_sql`] gives: those views are
/// whole-wallet aggregates, and this runs after every sync.
///
/// The memo may sit on the received note or, for an output the wallet itself
/// created, on the matching `sent_notes` row: the view this replaced took the
/// larger of the two and so does the read path, so a self-send whose received
/// memo has not been backfilled still counts as visible. Missing that would
/// keep a confirmed write pending forever.
///
/// Every pool is scanned, not just the database's own, and no account predicate
/// is applied. Both match the query this replaces, and both are harmless here:
/// the set is only ever used to *drop* a pending entry whose txid we broadcast
/// ourselves, so a wider set cannot strand one. A fleet member sharing a shard
/// file with other accounts would want the account predicate.
///
/// One deliberate narrowing against the old query: an output the wallet *sent*
/// to somebody else, with no received note of its own, is not counted. The read
/// path cannot see such an output either (it reads received notes), so counting
/// it could only drop a pending entry the read path was still blind to.
pub(crate) fn mined_with_memo_txids(
    conn: &rusqlite::Connection,
) -> rusqlite::Result<std::collections::HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT t.txid FROM (
             SELECT transaction_id, output_index AS output_index, memo, 2 AS pool
                 FROM sapling_received_notes
             UNION ALL SELECT transaction_id, action_index, memo, 3 FROM orchard_received_notes
             UNION ALL SELECT transaction_id, action_index, memo, 4 FROM ironwood_received_notes
         ) n \
         JOIN transactions t ON t.id_tx = n.transaction_id \
         LEFT JOIN sent_notes sn \
                ON sn.transaction_id = n.transaction_id \
               AND sn.output_pool = n.pool \
               AND sn.output_index = n.output_index \
         WHERE t.mined_height IS NOT NULL \
           AND (n.memo IS NOT NULL OR sn.memo IS NOT NULL)",
    )?;
    let mut seen = std::collections::HashSet::new();
    let rows = stmt.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
    for r in rows {
        if let Some(arr) = r.ok().and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok()) {
            seen.insert(TxId::from_bytes(arr).to_string());
        }
    }
    Ok(seen)
}

#[cfg(test)]
mod tests {
    use super::{mined_with_memo_txids, read_tip_tolerance, tip_time_is_fresh, TIP_MAX_AGE};
    use zcash_primitives::transaction::TxId;

    // One stub row: (txid, mined_height, decrypted memo bytes).
    type StubRow<'a> = (TxId, Option<i64>, Option<&'a [u8]>);

    // Minimal stand-ins for the base tables `mined_with_memo_txids` reads: only
    // the columns it touches. Lets us exercise the pending-GC selection (mined
    // AND memo decrypted) without the full zcash_client_sqlite schema. The
    // query's agreement with the real one is covered by
    // `internal::state::tests::the_base_table_scan_matches_the_view_it_replaces`,
    // which runs against a populated real wallet database.
    //
    // The notes go in the Orchard table; the query unions all three pools and
    // this exercises one of them.
    fn stub_wallet_db(rows: &[StubRow]) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE transactions (id_tx INTEGER PRIMARY KEY, txid BLOB, mined_height INTEGER);
             CREATE TABLE sapling_received_notes (
                 transaction_id INTEGER, output_index INTEGER, memo BLOB
             );
             CREATE TABLE orchard_received_notes (
                 transaction_id INTEGER, action_index INTEGER, memo BLOB
             );
             CREATE TABLE ironwood_received_notes (
                 transaction_id INTEGER, action_index INTEGER, memo BLOB
             );
             CREATE TABLE sent_notes (
                 transaction_id INTEGER, output_pool INTEGER, output_index INTEGER, memo BLOB
             );",
        )
        .unwrap();
        for (i, (txid, mined, memo)) in rows.iter().enumerate() {
            let id_tx = i as i64 + 1;
            let bytes = txid.as_ref().to_vec();
            conn.execute(
                "INSERT INTO transactions (id_tx, txid, mined_height) VALUES (?1, ?2, ?3)",
                rusqlite::params![id_tx, bytes, mined],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO orchard_received_notes (transaction_id, action_index, memo)
                 VALUES (?1, 0, ?2)",
                rusqlite::params![id_tx, memo],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn pending_gc_keeps_mined_tx_until_its_memo_is_decrypted() {
        // The regression: the compact-block scan records a tx as mined before
        // `enhance` decrypts its memo, so pruning on mined-only drops the
        // pending entry while the read path still can't see the write (a
        // confirming INIT briefly reverting to "uninitialized"). Only a mined
        // tx WITH a decrypted memo should be considered "seen".
        let decrypted = TxId::from_bytes([1u8; 32]); // mined + memo  -> seen
        let memo_pending = TxId::from_bytes([2u8; 32]); // mined, no memo -> NOT seen
        let unmined = TxId::from_bytes([3u8; 32]); // memo but unmined -> NOT seen
        let conn = stub_wallet_db(&[
            (decrypted, Some(730), Some(b"ZKV0 INIT ...".as_slice())),
            (memo_pending, Some(730), None),
            (unmined, None, Some(b"ZKV0 SET k v".as_slice())),
        ]);

        let seen = mined_with_memo_txids(&conn).unwrap();
        assert!(seen.contains(&decrypted.to_string()));
        assert!(!seen.contains(&memo_pending.to_string()));
        assert!(!seen.contains(&unmined.to_string()));
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn read_tip_tolerance_is_confs_minus_one() {
        // Default `-c 3` read: the newest 2 blocks can't hold a 3-confirmation
        // write, so a re-scan up to 2 blocks behind is skippable.
        assert_eq!(read_tip_tolerance(3), 2);
        // `-c 1`: a one-confirmation write in the very next block matters, so a
        // skip is only safe at the exact tip.
        assert_eq!(read_tip_tolerance(1), 0);
        // `-c 0` (mempool read): never skip when behind; the mempool is pulled
        // separately and the freshest tip is wanted.
        assert_eq!(read_tip_tolerance(0), 0);
        assert_eq!(read_tip_tolerance(10), 9);
    }

    fn now_secs() -> u32 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32
    }

    #[test]
    fn fresh_tip_is_fresh() {
        // A block mined a few seconds ago is fresh.
        assert!(tip_time_is_fresh(now_secs().saturating_sub(10)));
    }

    #[test]
    fn stale_tip_is_not_fresh() {
        // A block older than the window (plus slack) is stale.
        let stale = now_secs().saturating_sub(TIP_MAX_AGE.as_secs() as u32 + 60);
        assert!(!tip_time_is_fresh(stale));
    }

    #[test]
    fn future_tip_is_treated_as_fresh() {
        // Clock skew: a future-dated block must not be rejected.
        assert!(tip_time_is_fresh(now_secs().saturating_add(120)));
    }
}
