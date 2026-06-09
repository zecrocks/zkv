//! Wallet sync: scan compact blocks, then enhance (fetch full txs so memos appear).
//!
//! Callable from any command via `run_sync()`. Stripped of TUI/defrag features
//! from the upstream zcash-devtool.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use anyhow::anyhow;
use futures_util::{StreamExt, TryStreamExt};
use orchard::tree::MerkleHashOrchard;
use prost::Message;
use rand::rngs::OsRng;
use tokio::{fs::File, io::AsyncWriteExt, task::JoinHandle};
use tonic::{transport::Channel, Code};
use tracing::{debug, error, info};

use zcash_client_backend::{
    data_api::{
        chain::{
            error::Error as ChainError, scan_cached_blocks, BlockSource, ChainState,
            CommitmentTreeRoot,
        },
        scanning::{ScanPriority, ScanRange},
        wallet::decrypt_and_store_transaction,
        AccountBirthday, TransactionDataRequest, TransactionStatus, WalletCommitmentTrees,
        WalletRead, WalletWrite,
    },
    proto::service::{
        self, compact_tx_streamer_client::CompactTxStreamerClient, BlockId, BlockRange,
        RawTransaction,
    },
};
use zcash_client_sqlite::{
    chain::BlockMeta, error::SqliteClientError, util::SystemClock, FsBlockDb, FsBlockDbError,
    WalletDb,
};
use zcash_keys::encoding::AddressCodec;
use zcash_primitives::{
    merkle_tree::HashSer,
    transaction::{Transaction, TxId},
};
use zcash_protocol::consensus::{BlockHeight, BranchId, Parameters};

#[cfg(feature = "transparent-inputs")]
use {
    ::transparent::{
        address::Script,
        bundle::{OutPoint, TxOut},
    },
    zcash_client_backend::wallet::WalletTransparentOutput,
    zcash_client_sqlite::AccountUuid,
    zcash_protocol::value::Zatoshis,
    zcash_script::script,
};

use crate::{
    config::{Role, WalletConfig},
    data::{get_block_path, get_db_paths, open_wallet_db},
    error,
    internal::pending,
    remote::ConnectionArgs,
};

const BATCH_SIZE: u32 = 10_000;

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

/// Fetch the chain tip height on an existing client. One `GetLatestBlock`, with
/// no freshness check (see [`fresh_chain_tip`] for the guarded variant).
async fn chain_tip_height(client: &mut CompactTxStreamerClient<Channel>) -> Result<u32, TipError> {
    client
        .get_latest_block(service::ChainSpec::default())
        .await
        .map_err(|e| TipError::Other(anyhow!("{e}")))?
        .into_inner()
        .height
        .try_into()
        .map_err(|_| TipError::Other(anyhow!("chain tip height out of range")))
}

/// Fetch the chain tip height and reject it if its block timestamp is older
/// than [`TIP_MAX_AGE`]. Cheap: one `GetLatestBlock` plus one `GetTreeState`
/// (for the timestamp), with no compact-block download or scan. This is the "is
/// the server's view of the chain current?" guard the database-creation/import
/// paths share; it does **not** require the local wallet to have caught up to
/// the tip (that is a full sync, a separate thing).
async fn fresh_chain_tip(client: &mut CompactTxStreamerClient<Channel>) -> Result<u32, TipError> {
    let height = chain_tip_height(client).await?;
    let treestate = client
        .get_tree_state(BlockId {
            height: u64::from(height),
            ..Default::default()
        })
        .await
        .map_err(|e| TipError::Other(anyhow!("{e}")))?
        .into_inner();
    if !tip_time_is_fresh(treestate.time) {
        return Err(TipError::StaleTip);
    }
    Ok(height)
}

/// Build an [`AccountBirthday`] at `birthday_height`, anchored to an
/// already-validated fresh `chain_tip`. Shared tail of [`near_tip_birthday`]
/// and [`pinned_birthday`].
async fn account_birthday_at(
    client: &mut CompactTxStreamerClient<Channel>,
    birthday_height: u32,
    chain_tip: u32,
) -> Result<AccountBirthday, TipError> {
    let treestate = client
        .get_tree_state(BlockId {
            height: u64::from(birthday_height).saturating_sub(1),
            ..Default::default()
        })
        .await
        .map_err(|e| TipError::Other(anyhow!("{e}")))?
        .into_inner();
    AccountBirthday::from_treestate(treestate, Some(chain_tip.into()))
        .map_err(|e| TipError::Other(anyhow::Error::new(error::Error::from(e))))
}

/// Pin the birthday for a wallet whose birthday is **not known**, defaulting to
/// the chain tip minus [`BIRTHDAY_SAFETY_BUFFER`] blocks. Used when *generating*
/// a brand-new zkv address (`zkv init`), and as the fallback for `zkv restore` /
/// the facade admin-create when no birthday is supplied. Requires a fresh tip
/// ([`TipError::StaleTip`] otherwise).
pub async fn near_tip_birthday(
    client: &mut CompactTxStreamerClient<Channel>,
) -> Result<AccountBirthday, TipError> {
    let chain_tip = fresh_chain_tip(client).await?;
    let birthday_height = chain_tip.saturating_sub(BIRTHDAY_SAFETY_BUFFER);
    account_birthday_at(client, birthday_height, chain_tip).await
}

/// Pin a birthday at an **already-known** height: importing or watching a zkv
/// address (birthday carried in the address), or a `zkv restore` with an
/// explicit `--birthday`. The height is honored **verbatim** (no safety
/// buffer). Still requires a fresh tip ([`TipError::StaleTip`] otherwise).
pub async fn pinned_birthday(
    client: &mut CompactTxStreamerClient<Channel>,
    birthday_height: u32,
) -> Result<AccountBirthday, TipError> {
    let chain_tip = fresh_chain_tip(client).await?;
    account_birthday_at(client, birthday_height, chain_tip).await
}

/// Like [`pinned_birthday`] but **without** the fresh-tip guard. For the
/// mid-sync wipe-and-rebootstrap recovery, which runs on an already-connected
/// client and must rebuild the wallet from the fixed `keys.toml` birthday even
/// if the tip momentarily looks stale: a hard bail there would abort an
/// in-progress auto-recovery. The tip height is still fetched, but only as the
/// `from_treestate` anchor, not as a freshness gate.
pub async fn pinned_birthday_unchecked(
    client: &mut CompactTxStreamerClient<Channel>,
    birthday_height: u32,
) -> Result<AccountBirthday, TipError> {
    let chain_tip = chain_tip_height(client).await?;
    account_birthday_at(client, birthday_height, chain_tip).await
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
    network: zcash_protocol::consensus::Network,
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

/// Shareable scan-progress counters fed by the scan loop and read by the
/// status spinner. Cheap to clone (`Arc`-backed) and lock-free.
#[derive(Clone, Default)]
pub struct SyncProgress(Arc<SyncProgressInner>);

#[derive(Default)]
struct SyncProgressInner {
    /// Highest block height scanned so far (0 = unknown / not started).
    scanned: AtomicU32,
    /// Chain tip height we're scanning toward (0 = unknown yet).
    tip: AtomicU32,
}

impl SyncProgress {
    fn set_tip(&self, tip: u32) {
        self.0.tip.store(tip, Ordering::Relaxed);
    }

    /// Advance the scanned watermark, never moving it backward (ranges can be
    /// processed out of order, e.g. a verify range before the main sweep).
    fn observe_scanned(&self, height: u32) {
        self.0.scanned.fetch_max(height, Ordering::Relaxed);
    }

    /// The spinner label: `Syncing… <scanned> / <tip> (<pct>%)` once both are
    /// known, otherwise a bare `Syncing…` while we're still probing the tip.
    ///
    /// The percentage is capped at 99: 100% would only show for the instant
    /// before the spinner is torn down (the sync is done), so it's never worth
    /// rendering; completion is signalled by the spinner disappearing.
    fn label(&self) -> String {
        let tip = self.0.tip.load(Ordering::Relaxed);
        let scanned = self.0.scanned.load(Ordering::Relaxed).min(tip);
        if tip == 0 || scanned == 0 {
            return "Syncing…".to_owned();
        }
        let pct = (scanned as u64 * 100 / tip as u64).min(99) as u32;
        format!("Syncing… {scanned} / {tip} ({pct}%)")
    }
}

/// Delay before the spinner's first frame. Zero: a sync always does at least
/// one lightwalletd round-trip, so showing it immediately means the user sees
/// progress *every* time a sync runs (the line is erased on completion, so even
/// an instant cached sync just blinks rather than lingering).
const SPINNER_GRACE: std::time::Duration = std::time::Duration::ZERO;

/// Back-compat alias for [`run_sync`]. The animated status spinner now lives in
/// `run_sync` itself, so every sync path (the CLI commands, the `db::Database`
/// facade, and the write path's pre-broadcast sync) shows it uniformly.
pub async fn run_sync_with_status(
    db_name: &str,
    conn: &ConnectionArgs,
    fetch_mempool_too: bool,
) -> anyhow::Result<u32> {
    run_sync(db_name, conn, fetch_mempool_too).await
}

/// Sync the named database to chain tip, then fetch full transactions so memos
/// are decrypted. Returns the synced chain height.
///
/// When `fetch_mempool_too` is set, additionally pull all current mempool
/// transactions from lightwalletd and decrypt them against this wallet's
/// keys. Callers gate this on `--confirmations 0` so safety-conscious reads
/// don't surface arbitrary unconfirmed state from the wire.
///
/// Strict tip tolerance (0): the pipeline is skipped only when the wallet is
/// exactly at the live tip. Use this for writes (a spend needs an accurate
/// tree) and for an explicit `zkv sync`. Reads should prefer [`run_sync_read`].
pub async fn run_sync(
    db_name: &str,
    conn: &ConnectionArgs,
    fetch_mempool_too: bool,
) -> anyhow::Result<u32> {
    run_sync_tol(db_name, conn, fetch_mempool_too, 0).await
}

/// Read-oriented sync: identical to [`run_sync`], but tolerates being up to
/// [`NEAR_TIP_TOLERANCE`] blocks behind the live tip before deciding it must
/// re-scan. Lets a tight read loop skip the whole download/scan/enhance
/// pipeline for the newest block or two (which confirmed reads at
/// `--confirmations >= 1` ignore anyway), while a fresh mempool pull (when
/// `fetch_mempool_too`) still happens on the skip path.
pub async fn run_sync_read(
    db_name: &str,
    conn: &ConnectionArgs,
    fetch_mempool_too: bool,
) -> anyhow::Result<u32> {
    run_sync_tol(db_name, conn, fetch_mempool_too, NEAR_TIP_TOLERANCE).await
}

/// Read sync whose fast-path skip tolerance comes from the request's
/// confirmation depth ([`read_tip_tolerance`]) instead of the fixed
/// [`NEAR_TIP_TOLERANCE`]. Used by the confirmation-aware read commands
/// (`zkv get`/`history`): a default `-c 3` read skips a re-scan when up to 2
/// blocks behind the tip (those blocks can't yet hold a 3-confirmation write),
/// while `-c 1`/`-c 0` tighten to an exact-tip skip so a just-confirmed write is
/// never missed.
pub async fn run_sync_read_confs(
    db_name: &str,
    conn: &ConnectionArgs,
    min_confs: u32,
    fetch_mempool_too: bool,
) -> anyhow::Result<u32> {
    run_sync_tol(
        db_name,
        conn,
        fetch_mempool_too,
        read_tip_tolerance(min_confs),
    )
    .await
}

/// Shared sync driver: the per-attempt spinner + reorg/corruption recovery
/// loop, parameterized by how many blocks behind the live tip the fast-path
/// skip will accept (`tip_tolerance`). See [`run_sync`] / [`run_sync_read`].
async fn run_sync_tol(
    db_name: &str,
    conn: &ConnectionArgs,
    fetch_mempool_too: bool,
    tip_tolerance: u32,
) -> anyhow::Result<u32> {
    // Serialize against any other zkv process touching this database: a chain
    // scan mutates the wallet DB and the block cache, and two concurrent scans
    // (or a scan racing a spend) would corrupt them. Held for the whole sync;
    // reentrant with the write path's own lock (see `internal::lock`).
    let _lock = crate::internal::lock::DbLock::acquire(db_name)?;
    let mut tried_recovery = false;
    loop {
        // A fresh spinner per attempt, started around the scan only: it is torn
        // down (line erased) *before* any recovery prompt (which reads stdin
        // and writes stderr), so the two never fight over the terminal. The
        // spinner self-suppresses when stderr isn't a TTY, so the facade / GUI /
        // piped callers stay silent; the `progress` atomics it reads are cheap
        // no-ops in that case.
        let progress = SyncProgress::default();
        let label_src = progress.clone();
        let spinner = crate::ui::Spinner::start_with(move || label_src.label(), SPINNER_GRACE);
        let result = run_sync_inner(
            db_name,
            conn,
            fetch_mempool_too,
            tip_tolerance,
            Some(&progress),
        )
        .await;
        spinner.stop().await;

        match result {
            Ok(h) => return Ok(h),
            Err(e) => {
                if !tried_recovery && needs_recovery(&e) && prompt_for_wipe(db_name, &e)? {
                    let cfg = WalletConfig::read(db_name)?;
                    let mut client = conn.connect(cfg.network).await?;
                    crate::internal::recover::wipe_sidecars(db_name)?;
                    crate::internal::recover::rebootstrap(db_name, &mut client).await?;
                    crate::ui::success(
                        "Wallet rebuilt from keys.toml; resuming sync from birthday.",
                    );
                    tried_recovery = true;
                    continue;
                }
                return Err(e);
            }
        }
    }
}

/// Errors that warrant offering a wipe-and-rebootstrap recovery: an
/// unrecoverable reorg, or an uninitialized/corrupt wallet schema (e.g.
/// `no such table: scan_queue` from a half-deleted data.sqlite).
fn needs_recovery(e: &anyhow::Error) -> bool {
    if e.downcast_ref::<UnrecoverableRewind>().is_some() {
        return true;
    }
    let msg = format!("{e:#}");
    msg.contains("no such table: scan_queue")
}

fn prompt_for_wipe(db_name: &str, err: &anyhow::Error) -> anyhow::Result<bool> {
    use std::io::{stderr, stdin, BufRead, IsTerminal, Write};
    eprintln!();
    if let Some(rewind) = err.downcast_ref::<UnrecoverableRewind>() {
        eprintln!(
            "Chain reorg at height {} cannot be recovered: no valid checkpoint with a scanned \
             block exists at or below the conflict point (requested rewind to {}).",
            rewind.at_height, rewind.requested
        );
    } else {
        eprintln!("Wallet sidecar appears uninitialized or corrupt: {err:#}");
    }
    let cfg = WalletConfig::read(db_name)?;
    if cfg.role == Role::Watch && cfg.zkv_address.is_none() {
        eprintln!(
            "This watch-only database was created before zkv stored its address in keys.toml. \
             Re-run `zkv watch <zkv_addr>` with the original address and birthday to rebuild."
        );
        return Ok(false);
    }
    if !stdin().is_terminal() {
        eprintln!(
            "stdin is not a TTY; refusing to auto-wipe. Re-run interactively to confirm, or \
             manually delete the sidecar files (data.sqlite, blockmeta.sqlite, blocks/, \
             zkv_state.sqlite) under the database directory."
        );
        return Ok(false);
    }
    eprint!("Delete local cache and resync the {db_name:?} database from the blockchain? [y/N] ");
    let _ = stderr().flush();
    let mut line = String::new();
    stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES"))
}

async fn run_sync_inner(
    db_name: &str,
    conn: &ConnectionArgs,
    fetch_mempool_too: bool,
    tip_tolerance: u32,
    progress: Option<&SyncProgress>,
) -> anyhow::Result<u32> {
    let config = WalletConfig::read(db_name)?;
    let params = config.network;

    let (fsblockdb_root, db_data_path) = get_db_paths(db_name)?;
    let fsblockdb_root = fsblockdb_root.as_path();
    let mut db_cache = FsBlockDb::for_path(fsblockdb_root).map_err(error::Error::from)?;
    let mut db_data = open_wallet_db(&db_data_path, params)?;
    let mut client = conn.connect(params).await?;

    // Fast path: if the wallet's last-known chain height matches the live tip
    // and there's no pending scan or enhance work, skip the whole pipeline.
    // One cheap `GetLatestBlock` saves a `GetSubtreeRoots` stream plus the
    // scan-range bookkeeping that runs even when nothing has changed.
    let rpc_tip: u32 = client
        .get_latest_block(service::ChainSpec::default())
        .await?
        .get_ref()
        .height
        .try_into()
        .map_err(|_| error::Error::InvalidAmount)?;
    // Surface the tip to the progress spinner as early as possible, so the
    // first frame can show `scanned / tip` instead of a bare "Syncing…". Seed
    // the scanned watermark from the wallet's already-scanned height (best
    // effort) so the percentage is meaningful immediately rather than starting
    // at 0% until the first batch lands.
    if let Some(p) = progress {
        p.set_tip(rpc_tip);
        if let Ok(Some(summary)) = db_data.get_wallet_summary(
            zcash_client_backend::data_api::wallet::ConfirmationsPolicy::default(),
        ) {
            p.observe_scanned(u32::from(summary.fully_scanned_height()));
        }
    }
    // If the chain has advanced no more than `tip_tolerance` blocks since our
    // last sync and there are no pending scan ranges, skip the whole pipeline.
    // For reads (`tip_tolerance` from the request's confirmation depth, see
    // `read_tip_tolerance`, or `NEAR_TIP_TOLERANCE` when there is none) this lets
    // a tight loop avoid a full download/scan/enhance pass just to pick up the
    // newest block or two, which confirmed reads ignore anyway; writes pass 0 so a spend
    // always builds on the exact tip. Note: we intentionally ignore
    // `transaction_data_requests()` here; `TransactionsInvolvingAddress` for
    // transparent receivers gets re-emitted every sync, but with no new blocks
    // there is by definition no new data for it to find.
    let wallet_tip = db_data.chain_height()?.map(u32::from);
    let pending_scan = !db_data.suggest_scan_ranges()?.is_empty();
    let behind = rpc_tip.saturating_sub(wallet_tip.unwrap_or(0));
    let within_tolerance = wallet_tip.is_some() && behind <= tip_tolerance;
    info!(
        "Tip check: rpc={rpc_tip} wallet={wallet_tip:?} behind={behind} \
         tolerance={tip_tolerance} pending_scan={pending_scan}"
    );
    if within_tolerance && !pending_scan {
        info!("already synced to within {tip_tolerance} of {rpc_tip} (wallet at {wallet_tip:?})");
        if fetch_mempool_too {
            let chain_tip = BlockHeight::from_u32(rpc_tip);
            if let Err(e) = fetch_mempool(&mut client, &params, chain_tip, &mut db_data).await {
                tracing::debug!("mempool fetch failed: {e:#}");
            }
        }
        gc_pending(db_name, &db_data_path);
        // Report the height we actually have scanned, not the live tip; within
        // tolerance these differ by at most `tip_tolerance` blocks.
        return Ok(wallet_tip.unwrap_or(rpc_tip));
    }

    update_subtree_roots(&mut client, &mut db_data).await?;

    loop {
        if !sync_pass(
            &mut client,
            &params,
            fsblockdb_root,
            &mut db_cache,
            &mut db_data,
            &db_data_path,
            progress,
        )
        .await?
        {
            break;
        }
    }

    // Enhance: fetch full transactions so memos are decrypted into the wallet DB.
    info!("fetching full transactions to decrypt memos");
    enhance(&mut client, &params, &mut db_data).await?;

    if fetch_mempool_too {
        let chain_tip = db_data
            .chain_height()?
            .unwrap_or(BlockHeight::from_u32(rpc_tip));
        if let Err(e) = fetch_mempool(&mut client, &params, chain_tip, &mut db_data).await {
            tracing::warn!("mempool fetch failed: {e:#}");
        }
    }

    let tip = db_data.chain_height()?.map(u32::from).unwrap_or(0);
    gc_pending(db_name, &db_data_path);
    Ok(tip)
}

/// One pass: download blocks, scan, repeat as suggested. Returns `true` if a
/// retriggering condition (chain tip moved or reorg) suggests we should loop again.
async fn sync_pass<P: Parameters + Send + 'static>(
    client: &mut CompactTxStreamerClient<Channel>,
    params: &P,
    fsblockdb_root: &Path,
    db_cache: &mut FsBlockDb,
    db_data: &mut WalletDb<rusqlite::Connection, P, SystemClock, OsRng>,
    db_data_path: &Path,
    progress: Option<&SyncProgress>,
) -> anyhow::Result<bool> {
    let chain_tip = update_chain_tip(client, db_data).await?;
    if let Some(p) = progress {
        p.set_tip(u32::from(chain_tip));
    }

    #[cfg(feature = "transparent-inputs")]
    for account_id in db_data.get_account_ids()? {
        refresh_utxos(params, client, db_data, account_id, BlockHeight::from(0)).await?;
    }

    let mut scan_ranges = db_data.suggest_scan_ranges()?;
    info!("fetched {} scan ranges", scan_ranges.len());

    let mut block_deletions = vec![];

    // Verify any pending range first.
    loop {
        match scan_ranges.first() {
            Some(scan_range) if scan_range.priority() == ScanPriority::Verify => {
                let block_meta =
                    download_blocks(client, fsblockdb_root, db_cache, scan_range).await?;
                let chain_state =
                    download_chain_state(client, scan_range.block_range().start - 1).await?;
                let updated = scan_blocks(
                    params,
                    fsblockdb_root,
                    db_cache,
                    db_data,
                    db_data_path,
                    &chain_state,
                    scan_range,
                )?;
                block_deletions.push(delete_cached_blocks(fsblockdb_root, block_meta));
                if let Some(p) = progress {
                    p.observe_scanned(u32::from(scan_range.block_range().end).saturating_sub(1));
                }
                if updated {
                    scan_ranges = db_data.suggest_scan_ranges()?;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }

    let scan_ranges = db_data.suggest_scan_ranges()?;
    debug!("Suggested ranges: {:?}", scan_ranges);

    for scan_range in scan_ranges.into_iter().flat_map(|r| {
        (0..).scan(r, |acc, _| {
            if acc.is_empty() {
                None
            } else if let Some((cur, next)) = acc.split_at(acc.block_range().start + BATCH_SIZE) {
                *acc = next;
                Some(cur)
            } else {
                let cur = acc.clone();
                let end = acc.block_range().end;
                *acc = ScanRange::from_parts(end..end, acc.priority());
                Some(cur)
            }
        })
    }) {
        let block_meta = download_blocks(client, fsblockdb_root, db_cache, &scan_range).await?;
        let chain_state = download_chain_state(client, scan_range.block_range().start - 1).await?;
        let updated = scan_blocks(
            params,
            fsblockdb_root,
            db_cache,
            db_data,
            db_data_path,
            &chain_state,
            &scan_range,
        )?;
        block_deletions.push(delete_cached_blocks(fsblockdb_root, block_meta));
        if let Some(p) = progress {
            p.observe_scanned(u32::from(scan_range.block_range().end).saturating_sub(1));
        }

        if updated {
            for deletion in block_deletions {
                deletion.await?;
            }
            return Ok(true);
        }
    }

    for deletion in block_deletions {
        deletion.await?;
    }
    Ok(false)
}

async fn update_subtree_roots<P: Parameters>(
    client: &mut CompactTxStreamerClient<Channel>,
    db_data: &mut WalletDb<rusqlite::Connection, P, SystemClock, OsRng>,
) -> anyhow::Result<()> {
    let mut request = service::GetSubtreeRootsArg::default();
    request.set_shielded_protocol(service::ShieldedProtocol::Sapling);
    let sapling_roots: Vec<CommitmentTreeRoot<sapling::Node>> = client
        .get_subtree_roots(request)
        .await?
        .into_inner()
        .and_then(|root| async move {
            let root_hash = sapling::Node::read(&root.root_hash[..])?;
            Ok(CommitmentTreeRoot::from_parts(
                BlockHeight::from_u32(root.completing_block_height as u32),
                root_hash,
            ))
        })
        .try_collect()
        .await?;
    info!("Sapling tree: {} subtrees", sapling_roots.len());
    db_data.put_sapling_subtree_roots(0, &sapling_roots)?;

    let mut request = service::GetSubtreeRootsArg::default();
    request.set_shielded_protocol(service::ShieldedProtocol::Orchard);
    let orchard_roots: Vec<CommitmentTreeRoot<MerkleHashOrchard>> = client
        .get_subtree_roots(request)
        .await?
        .into_inner()
        .and_then(|root| async move {
            let root_hash = MerkleHashOrchard::read(&root.root_hash[..])?;
            Ok(CommitmentTreeRoot::from_parts(
                BlockHeight::from_u32(root.completing_block_height as u32),
                root_hash,
            ))
        })
        .try_collect()
        .await?;
    info!("Orchard tree: {} subtrees", orchard_roots.len());
    db_data.put_orchard_subtree_roots(0, &orchard_roots)?;

    Ok(())
}

async fn update_chain_tip<P: Parameters>(
    client: &mut CompactTxStreamerClient<Channel>,
    db_data: &mut WalletDb<rusqlite::Connection, P, SystemClock, OsRng>,
) -> anyhow::Result<BlockHeight> {
    let tip_height: BlockHeight = client
        .get_latest_block(service::ChainSpec::default())
        .await?
        .get_ref()
        .height
        .try_into()
        .map_err(|_| error::Error::InvalidAmount)?;
    info!("Chain tip: {}", tip_height);
    db_data.update_chain_tip(tip_height)?;
    Ok(tip_height)
}

async fn download_blocks(
    client: &mut CompactTxStreamerClient<Channel>,
    fsblockdb_root: &Path,
    db_cache: &FsBlockDb,
    scan_range: &ScanRange,
) -> anyhow::Result<Vec<BlockMeta>> {
    info!("Fetching {}", scan_range);
    let mut start = service::BlockId::default();
    start.height = scan_range.block_range().start.into();
    let mut end = service::BlockId::default();
    end.height = (scan_range.block_range().end - 1).into();
    let range = service::BlockRange {
        start: Some(start),
        end: Some(end),
        pool_types: Default::default(),
    };
    let stream = client
        .get_block_range(range)
        .await?
        .into_inner()
        .and_then(|block| async move {
            let (sapling_outputs_count, orchard_actions_count) = block
                .vtx
                .iter()
                .map(|tx| (tx.outputs.len() as u32, tx.actions.len() as u32))
                .fold((0, 0), |(s, o), (sn, on)| (s + sn, o + on));
            let meta = BlockMeta {
                height: block.height(),
                block_hash: block.hash(),
                block_time: block.time,
                sapling_outputs_count,
                orchard_actions_count,
            };
            let encoded = block.encode_to_vec();
            let mut f = File::create(get_block_path(fsblockdb_root, &meta)).await?;
            f.write_all(&encoded).await?;
            Ok(meta)
        });
    tokio::pin!(stream);

    let mut block_meta = vec![];
    while let Some(block) = stream.try_next().await? {
        block_meta.push(block);
    }
    db_cache
        .write_block_metadata(&block_meta)
        .map_err(error::Error::from)?;
    Ok(block_meta)
}

async fn download_chain_state(
    client: &mut CompactTxStreamerClient<Channel>,
    block_height: BlockHeight,
) -> anyhow::Result<ChainState> {
    let tree_state = client
        .get_tree_state(BlockId {
            height: block_height.into(),
            hash: vec![],
        })
        .await?;
    Ok(tree_state.into_inner().to_chain_state()?)
}

fn delete_cached_blocks(fsblockdb_root: &Path, block_meta: Vec<BlockMeta>) -> JoinHandle<()> {
    let fsblockdb_root = fsblockdb_root.to_owned();
    tokio::spawn(async move {
        for meta in block_meta {
            if let Err(e) = tokio::fs::remove_file(get_block_path(&fsblockdb_root, &meta)).await {
                error!("Failed to remove {:?}: {}", meta, e);
            }
        }
    })
}

/// Reorg recovery exhausted: no valid rewind target exists for the conflict point.
/// `run_sync` downcasts to this to drive the wipe-and-rebootstrap prompt.
#[derive(Debug)]
pub struct UnrecoverableRewind {
    pub at_height: BlockHeight,
    pub requested: BlockHeight,
}

impl std::fmt::Display for UnrecoverableRewind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Chain reorg at height {} could not be recovered (requested rewind to {}); \
             no valid checkpoint with a scanned block exists at or below the conflict.",
            self.at_height, self.requested
        )
    }
}

impl std::error::Error for UnrecoverableRewind {}

/// Find the highest height that is in the `blocks` table AND has shared
/// sapling+orchard checkpoints, bounded by `max_height`. Used as a shallow-rewind
/// fallback when the requested deep rewind has no valid target below it.
fn find_shallow_rewind_target(
    db_data_path: &Path,
    max_height: BlockHeight,
) -> anyhow::Result<Option<BlockHeight>> {
    use rusqlite::OptionalExtension;
    let conn = rusqlite::Connection::open_with_flags(
        db_data_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let h: Option<u32> = conn
        .query_row(
            "SELECT MAX(blocks.height) FROM blocks
             JOIN sapling_tree_checkpoints sc ON sc.checkpoint_id = blocks.height
             JOIN orchard_tree_checkpoints oc ON oc.checkpoint_id = blocks.height
             WHERE blocks.height <= ?1",
            [u32::from(max_height)],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    Ok(h.map(BlockHeight::from))
}

fn perform_rewind<P: Parameters>(
    db_data: &mut WalletDb<rusqlite::Connection, P, SystemClock, OsRng>,
    db_data_path: &Path,
    at_height: BlockHeight,
    requested: BlockHeight,
) -> anyhow::Result<BlockHeight> {
    match db_data.truncate_to_height(requested) {
        Ok(h) => Ok(h),
        Err(SqliteClientError::RequestedRewindInvalid {
            safe_rewind_height, ..
        }) => {
            // First try the safe (deeper) rewind reported by the wallet, if any.
            if let Some(safe) = safe_rewind_height.filter(|&s| s < requested) {
                info!("No checkpoint at {requested}; trying safe rewind to {safe}");
                if let Ok(h) = db_data.truncate_to_height(safe) {
                    return Ok(h);
                }
            }
            // Fall back: find the highest valid (blocks ∩ shared checkpoints) at or below
            // the actual conflict height. This handles young wallets whose lowest
            // shared checkpoint (the birthday anchor) has no `blocks` row.
            if let Some(target) = find_shallow_rewind_target(db_data_path, at_height)? {
                info!(
                    "Shallow rewind to {target} (no valid target at-or-below requested {requested})"
                );
                return db_data
                    .truncate_to_height(target)
                    .map_err(|e| anyhow!("{:?}", e));
            }
            Err(UnrecoverableRewind {
                at_height,
                requested,
            }
            .into())
        }
        Err(e) => Err(anyhow!("{:?}", e)),
    }
}

fn scan_blocks<P: Parameters + Send + 'static>(
    params: &P,
    fsblockdb_root: &Path,
    db_cache: &mut FsBlockDb,
    db_data: &mut WalletDb<rusqlite::Connection, P, SystemClock, OsRng>,
    db_data_path: &Path,
    initial_chain_state: &ChainState,
    scan_range: &ScanRange,
) -> anyhow::Result<bool> {
    info!("Scanning {}", scan_range);
    let scan_result = scan_cached_blocks(
        params,
        db_cache,
        db_data,
        scan_range.block_range().start,
        initial_chain_state,
        scan_range.len(),
    );

    match scan_result {
        Err(ChainError::Scan(err)) if err.is_continuity_error() => {
            let requested = err.at_height().saturating_sub(10);
            info!(
                "Chain reorg detected at {}, rewinding to {}",
                err.at_height(),
                requested
            );
            let rewind_height = perform_rewind(db_data, db_data_path, err.at_height(), requested)?;
            db_cache
                .with_blocks(Some(rewind_height + 1), None, |block| {
                    let meta = BlockMeta {
                        height: block.height(),
                        block_hash: block.hash(),
                        block_time: block.time,
                        sapling_outputs_count: 0,
                        orchard_actions_count: 0,
                    };
                    std::fs::remove_file(get_block_path(fsblockdb_root, &meta))
                        .map_err(|e| ChainError::<(), _>::BlockSource(FsBlockDbError::Fs(e)))
                })
                .map_err(|e| anyhow!("{:?}", e))?;
            db_cache
                .truncate_to_height(rewind_height)
                .map_err(|e| anyhow!("{:?}", e))?;
            Ok(true)
        }
        Ok(_) => {
            let latest_ranges = db_data.suggest_scan_ranges()?;
            Ok(if let Some(range) = latest_ranges.first() {
                range.priority() > scan_range.priority()
            } else {
                false
            })
        }
        Err(e) => Err(anyhow!("{:?}", e)),
    }
}

#[cfg(feature = "transparent-inputs")]
async fn refresh_utxos<P: Parameters>(
    params: &P,
    client: &mut CompactTxStreamerClient<Channel>,
    db_data: &mut WalletDb<rusqlite::Connection, P, SystemClock, OsRng>,
    account_id: AccountUuid,
    start_height: BlockHeight,
) -> anyhow::Result<()> {
    let addresses = db_data
        .get_transparent_receivers(account_id, true, true)?
        .into_keys()
        .map(|addr| addr.encode(params))
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Ok(());
    }
    let request = service::GetAddressUtxosArg {
        addresses,
        start_height: start_height.into(),
        max_entries: 0,
    };
    client
        .get_address_utxos_stream(request)
        .await?
        .into_inner()
        .map_err(anyhow::Error::from)
        .and_then(|reply| async move {
            WalletTransparentOutput::from_parts(
                OutPoint::new(reply.txid[..].try_into()?, reply.index.try_into()?),
                TxOut::new(
                    Zatoshis::from_nonnegative_i64(reply.value_zat)?,
                    Script(script::Code(reply.script)),
                ),
                Some(BlockHeight::from(u32::try_from(reply.height)?)),
            )
            .ok_or(anyhow!("non-standard UTXO"))
        })
        .try_for_each(|output| {
            let res = db_data.put_received_transparent_utxo(&output).map(|_| ());
            async move { res.map_err(anyhow::Error::from) }
        })
        .await?;
    Ok(())
}

// ---- Enhance pass ----

fn parse_raw_transaction<P: Parameters>(
    params: &P,
    chain_tip: BlockHeight,
    tx: RawTransaction,
) -> anyhow::Result<(Transaction, Option<BlockHeight>)> {
    let mined_height = (tx.height > 0 && tx.height <= u64::from(u32::MAX))
        .then(|| BlockHeight::from_u32(u32::try_from(tx.height).unwrap()));
    let tx = Transaction::read(
        &tx.data[..],
        BranchId::for_height(params, mined_height.unwrap_or(chain_tip)),
    )?;
    Ok((tx, mined_height))
}

async fn fetch_transaction<P: Parameters>(
    client: &mut CompactTxStreamerClient<Channel>,
    params: &P,
    chain_tip: BlockHeight,
    txid: TxId,
) -> anyhow::Result<Option<(Transaction, Option<BlockHeight>)>> {
    let request = service::TxFilter {
        hash: txid.as_ref().to_vec(),
        ..Default::default()
    };
    let raw_tx = match client.get_transaction(request).await {
        Ok(response) => Ok(Some(response.into_inner())),
        Err(status) => {
            if status.code() == Code::NotFound {
                Ok(None)
            } else {
                Err(status)
            }
        }
    }?;
    raw_tx
        .map(|raw_tx| parse_raw_transaction(params, chain_tip, raw_tx))
        .transpose()
}

/// Pull every current mempool tx from lightwalletd and decrypt against the
/// wallet's keys. Matching txs end up in `data.sqlite` as unmined rows; the
/// existing read path then surfaces them with `mined_height IS NULL`.
///
/// Best-effort: any per-tx parse/decrypt failure is logged and skipped, and
/// stream-level errors are surfaced to the caller (which logs + ignores).
async fn fetch_mempool<P: Parameters>(
    client: &mut CompactTxStreamerClient<Channel>,
    params: &P,
    chain_tip: BlockHeight,
    db_data: &mut WalletDb<rusqlite::Connection, P, SystemClock, OsRng>,
) -> anyhow::Result<()> {
    use zcash_client_backend::proto::service::Empty;

    let mut stream = client.get_mempool_stream(Empty {}).await?.into_inner();
    let mut scanned = 0usize;
    while let Some(raw) = stream.message().await? {
        scanned += 1;
        let (tx, mined_height) = match parse_raw_transaction(params, chain_tip, raw) {
            Ok(parsed) => parsed,
            Err(e) => {
                tracing::debug!("skipping unparseable mempool tx: {e:#}");
                continue;
            }
        };
        // decrypt_and_store_transaction silently no-ops on txs whose outputs
        // don't decrypt to any of our viewing keys, so we don't need to
        // pre-filter.
        if let Err(e) = decrypt_and_store_transaction(params, db_data, &tx, mined_height) {
            tracing::debug!("skipping mempool tx: {e:#}");
        }
    }
    if scanned > 0 {
        info!("mempool: scanned {scanned} tx(s)");
    }
    Ok(())
}

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

/// Txids the wallet has indexed **with a decrypted memo**: mined and present in
/// `v_tx_outputs` with a non-NULL memo. This is exactly the set the read path
/// can see (it sources memos from `v_tx_outputs WHERE memo IS NOT NULL`), so a
/// pending entry whose txid is in this set can be dropped without the state
/// flapping. A tx that is merely mined (compact-scanned) but not yet enhanced
/// has a NULL memo and is deliberately excluded.
fn mined_with_memo_txids(
    conn: &rusqlite::Connection,
) -> rusqlite::Result<std::collections::HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT v.txid FROM v_tx_outputs v \
         JOIN v_transactions t ON t.txid = v.txid \
         WHERE t.mined_height IS NOT NULL AND v.memo IS NOT NULL",
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

async fn enhance<P: Parameters + Send + 'static>(
    client: &mut CompactTxStreamerClient<Channel>,
    params: &P,
    db_data: &mut WalletDb<rusqlite::Connection, P, SystemClock, OsRng>,
) -> anyhow::Result<()> {
    let chain_tip = match db_data.chain_height()? {
        Some(h) => h,
        None => return Ok(()),
    };

    let mut satisfied = BTreeSet::new();
    loop {
        let mut any_new = false;
        for req in db_data.transaction_data_requests()? {
            if satisfied.contains(&req) {
                continue;
            }
            any_new = true;
            match &req {
                TransactionDataRequest::GetStatus(txid) => {
                    let status = fetch_transaction(client, params, chain_tip, *txid)
                        .await?
                        .map_or(TransactionStatus::TxidNotRecognized, |(_, mined)| {
                            mined
                                .map_or(TransactionStatus::NotInMainChain, TransactionStatus::Mined)
                        });
                    db_data.set_transaction_status(*txid, status)?;
                }
                TransactionDataRequest::Enhancement(txid) => {
                    match fetch_transaction(client, params, chain_tip, *txid).await? {
                        None => db_data
                            .set_transaction_status(*txid, TransactionStatus::TxidNotRecognized)?,
                        Some((tx, mined)) => {
                            decrypt_and_store_transaction(params, db_data, &tx, mined)?
                        }
                    }
                }
                TransactionDataRequest::TransactionsInvolvingAddress(tia) => {
                    let address = tia.address().encode(params);
                    let request = service::TransparentAddressBlockFilter {
                        address: address.clone(),
                        range: Some(BlockRange {
                            start: Some(service::BlockId {
                                height: u64::from(tia.block_range_start()),
                                ..Default::default()
                            }),
                            end: tia.block_range_end().map(|h| service::BlockId {
                                height: u64::from(h - 1),
                                ..Default::default()
                            }),
                            pool_types: Default::default(),
                        }),
                    };
                    let mut stream = client.get_taddress_txids(request).await?.into_inner();
                    while let Some(raw_tx) = stream.next().await {
                        let (tx, mined) = parse_raw_transaction(params, chain_tip, raw_tx?)?;
                        decrypt_and_store_transaction(params, db_data, &tx, mined)?;
                    }
                }
            }
            satisfied.insert(req);
        }
        if !any_new {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        mined_with_memo_txids, read_tip_tolerance, tip_time_is_fresh, SyncProgress, TIP_MAX_AGE,
    };
    use zcash_primitives::transaction::TxId;

    // One stub row: (txid, mined_height, decrypted memo bytes).
    type StubRow<'a> = (TxId, Option<i64>, Option<&'a [u8]>);

    // Minimal stand-ins for the wallet's `v_transactions` / `v_tx_outputs`
    // views: only the columns `mined_with_memo_txids` reads. Lets us exercise
    // the pending-GC selection (mined AND memo decrypted) without the full
    // zcash_client_sqlite schema.
    fn stub_wallet_db(rows: &[StubRow]) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE v_transactions (txid BLOB, mined_height INTEGER);
             CREATE TABLE v_tx_outputs (txid BLOB, memo BLOB);",
        )
        .unwrap();
        for (txid, mined, memo) in rows {
            let bytes = txid.as_ref().to_vec();
            conn.execute(
                "INSERT INTO v_transactions (txid, mined_height) VALUES (?1, ?2)",
                rusqlite::params![bytes, mined],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO v_tx_outputs (txid, memo) VALUES (?1, ?2)",
                rusqlite::params![bytes, memo],
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

    #[test]
    fn progress_label_is_bare_until_tip_and_scan_known() {
        let p = SyncProgress::default();
        assert_eq!(p.label(), "Syncing…");
        // Tip known but nothing scanned yet → still bare (avoids "x / tip (0%)").
        p.set_tip(100);
        assert_eq!(p.label(), "Syncing…");
    }

    #[test]
    fn progress_label_shows_scanned_tip_and_percent() {
        let p = SyncProgress::default();
        p.set_tip(3_366_250);
        p.observe_scanned(3_366_176);
        assert_eq!(p.label(), "Syncing… 3366176 / 3366250 (99%)");
    }

    #[test]
    fn progress_scanned_watermark_never_regresses() {
        let p = SyncProgress::default();
        p.set_tip(100);
        p.observe_scanned(50);
        p.observe_scanned(20); // out-of-order range must not move it backward
        assert_eq!(p.label(), "Syncing… 50 / 100 (50%)");
    }

    #[test]
    fn progress_scanned_is_clamped_to_tip() {
        let p = SyncProgress::default();
        p.set_tip(100);
        p.observe_scanned(150); // a range end past the tip shouldn't show >100%
                                // Capped at 99%; 100% is never rendered (the spinner just disappears).
        assert_eq!(p.label(), "Syncing… 100 / 100 (99%)");
    }

    #[test]
    fn progress_percent_never_reaches_100() {
        let p = SyncProgress::default();
        p.set_tip(3_366_250);
        p.observe_scanned(3_366_250); // fully caught up
        assert_eq!(p.label(), "Syncing… 3366250 / 3366250 (99%)");
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
