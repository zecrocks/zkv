//! The wallet engine: the one place zkv talks to [`zecd`].
//!
//! Everything that needs a wallet rather than a memo lives behind this seam:
//! scanning the chain, holding `data.sqlite`, spending, broadcasting. zkv
//! supplies the parts that are about *memos* (the protocol tier, the snapshot
//! sidecar, `pending.toml`, the shallow reader) and lets an embedded zecd node
//! be the wallet.
//!
//! # Why a seam
//!
//! zecd names a supported embedding surface in its crate docs and treats
//! everything else it exposes as internal API with no stability promise across
//! commits, which is a distinction Rust cannot express: zecd's own binary is
//! built from the same crate, so everything that binary needs must be `pub`.
//! Confining every `zecd::` path to this module means an upstream fix, or an
//! upstream break, is a change here and nowhere else. Nothing outside
//! `crate::engine` may name a `zecd::` path.
//!
//! # Shape
//!
//! One embedded node per zkv database, with zecd's datadir set to the database
//! directory. That satisfies zecd's "at most one spending wallet per node"
//! invariant trivially, and it puts zecd's datadir lock on the `.lock` file
//! zkv used to lock itself (`data::LOCK_FILE`), so two zkv processes
//! still exclude each other, now through the node. zkv takes no lock of its
//! own any more; the rule that survives is that nothing may hold a file lock
//! on that path across a call that starts a node, since `flock` is not
//! reentrant across handles.
//!
//! The node starts lazily: `Engine::open` touches no network and starts
//! nothing, so the read paths (which query `data.sqlite` directly) stay exactly
//! as cheap as they were. Only an operation that needs the chain, such as
//! `Engine::sync_to_tip`, brings a node up, and it stays up until
//! `Engine::shutdown`. A one-shot CLI command therefore pays one startup; a
//! long-lived surface (the GUI's auto-sync loop, the faucet) pays one per
//! session rather than one per operation.
//!
//! A multi-thread tokio runtime is required: zecd's scan and proving paths call
//! `block_in_place`, which panics on a current-thread runtime. Every zkv binary
//! already builds one.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use zecd::node::Node;

use crate::config::WalletConfig;
use crate::data::db_dir;
use crate::remote::ConnectionArgs;

pub(crate) mod config;
pub(crate) mod fleet;
pub mod migrate;
pub(crate) mod node;
pub(crate) mod probe;
pub(crate) mod ship;

pub(crate) use config::WALLET;
pub use fleet::{engine_for, is_running, shared, shutdown_all, EngineRef};
pub use migrate::Adoption;
pub use node::SyncOutcome;

/// How long to keep retrying a node start that another process is blocking.
///
/// zecd *refuses* a held datadir lock where zkv's own lock *blocks*, so without
/// a retry a `zkv set` run while the GUI happens to be syncing would fail
/// outright rather than waiting its turn, which is the behaviour every zkv
/// command has today.
pub const DEFAULT_BUSY_WAIT: Duration = Duration::from_secs(60);

/// What went wrong in the engine.
///
/// Deliberately not [`crate::db::ZkvError`]: those variants are the facade's
/// vocabulary, and the mapping between the two belongs at the facade boundary
/// rather than here.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// Another process holds this database's lock and did not release it in
    /// time. Carries the database name.
    #[error("database '{0}' is in use by another process")]
    Busy(String),

    /// The requested configuration cannot be served by the engine, such as a
    /// regtest activation height that disagrees with the one zkv compiles in.
    /// (A SOCKS5 proxy used to land here too, until the node learned to dial
    /// through one; see `config::proxy_token`.)
    #[error("{0}")]
    Unsupported(String),

    /// The caller asked to stop, through the cancel flag.
    #[error("cancelled")]
    Cancelled,

    /// A shared-scan member has been placed but its account is not in a shard
    /// yet, so it has scanned nothing of its own however far its shard has got.
    ///
    /// Transient by construction: the node imports one member per pass, and the
    /// import needs a connected pass because it wants the tree state below the
    /// member's birthday. Distinguished from [`EngineError::ImportFailed`],
    /// which never resolves.
    #[error("'{0}' has joined the shared scan but has not been imported yet")]
    NotImported(String),

    /// A shared-scan member the node's database refused outright: a birthday no
    /// tree state can serve, or a viewing key already present in the shard.
    ///
    /// Permanent until the manifest is corrected, and worth telling the user
    /// about rather than showing as a healthy-looking empty database, which is
    /// what it looked like before the node published the reason.
    #[error("the shared scan cannot import '{wallet}': {reason}")]
    ImportFailed { wallet: String, reason: String },

    /// The node answered with a Bitcoin-Core-dialect error. `code` is the
    /// wire code (-6 insufficient funds, -8 invalid parameter, and so on), so
    /// callers can branch on it without matching message text.
    ///
    /// `funds` carries the amounts behind a `-6`, when the node reported them.
    /// It is in-process detail: the wire error object stays exactly Bitcoin
    /// Core's `code` + `message`, so this is `None` for anything that crossed
    /// a socket, and for every other code.
    #[error("wallet error {code}: {message}")]
    Rpc {
        code: i32,
        message: String,
        funds: Option<InsufficientFunds>,
    },

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<zecd::error::RpcError> for EngineError {
    fn from(e: zecd::error::RpcError) -> Self {
        // `ErrorDetails` is `#[non_exhaustive]`: a future variant is detail
        // zkv has no use for, not a reason to fail.
        let funds = match e.details {
            Some(zecd::error::ErrorDetails::InsufficientFunds(f)) => Some(f),
            _ => None,
        };
        EngineError::Rpc {
            code: e.code,
            message: e.message,
            funds,
        }
    }
}

impl From<zecd::typed::ClientError> for EngineError {
    fn from(e: zecd::typed::ClientError) -> Self {
        match e {
            zecd::typed::ClientError::Rpc(e) => e.into(),
            // A decode failure means zecd's response shape and the typed
            // struct disagree, which is an upstream bug rather than anything
            // the caller did; carry it through as-is so it is reportable.
            other => EngineError::Other(anyhow::Error::new(other)),
        }
    }
}

/// An exclusive hold on a database's datadir lock, for work that must happen
/// with **no node running**.
///
/// Taken through zecd's own `lock_datadir`, so it is the same lock a node
/// takes rather than one only zkv respects. The guard is boxed opaquely
/// because naming its type would mean depending on `fmutex` directly; nothing
/// here ever reads it, only drops it.
///
/// Released on drop. **Not reentrant:** `flock` is per open file description,
/// so a node started while this is held would fail to acquire what its own
/// process already has. Drop it before opening an engine.
pub struct DatadirLock {
    _guard: Box<dyn std::any::Any + Send>,
}

/// Take the datadir lock for `db_dir`, or report the database as in use.
///
/// For the destructive paths that run *outside* a node and would otherwise
/// race one: `zkv sync --rebuild` deletes the very files a running node holds
/// open, and on Unix an unlinked file keeps being written, so a concurrent
/// node would flush a second wallet into the directory after the wipe.
pub fn lock_datadir(db_dir: &std::path::Path, db_name: &str) -> Result<DatadirLock, EngineError> {
    match zecd::lock::lock_datadir(db_dir) {
        Ok(guard) => Ok(DatadirLock {
            _guard: Box::new(guard),
        }),
        Err(e) if zecd::lock::is_datadir_locked(&e) => Err(EngineError::Busy(db_name.to_owned())),
        Err(e) => Err(EngineError::Other(e)),
    }
}

/// The amounts behind an insufficient-funds refusal, as the node reports them.
///
/// Re-exported for the same reason as [`codes`]: a caller outside this module
/// reads the numbers without naming a `zecd::` path.
pub use zecd::error::InsufficientFunds;

/// The Bitcoin-Core-dialect RPC codes zkv branches on, re-exported so callers
/// outside this module can match them without naming a `zecd::` path (and so
/// they stay pinned to upstream's values rather than being copied).
pub mod codes {
    /// The node could not fund the transaction.
    pub const INSUFFICIENT_FUNDS: i32 = zecd::error::codes::RPC_WALLET_INSUFFICIENT_FUNDS;
    /// The node has no wallet under that name.
    pub const UNKNOWN_WALLET: i32 = zecd::error::codes::RPC_WALLET_NOT_FOUND;
    /// The send was refused by policy, such as the privacy ladder.
    pub const POLICY_REFUSED: i32 = zecd::error::codes::RPC_WALLET_ERROR;
}

/// A zkv database's wallet engine.
///
/// Cheap to construct and inert until something needs the chain; see the module
/// docs for the node lifecycle.
pub struct Engine {
    db_name: String,
    db_dir: PathBuf,
    cfg: WalletConfig,
    /// Which shape of node this is; see [`EngineKind`].
    kind: EngineKind,
    /// The zecd configuration for this database, resolved once at open.
    app: zecd::config::AppConfig,
    /// The running node, started on first use. `None` until then, and again
    /// after `Engine::shutdown`.
    node: tokio::sync::Mutex<Option<Arc<Node>>>,
    busy_wait: Option<Duration>,
    /// Live progress per wallet, for a caller rendering it. An own-node engine
    /// only ever has the one entry; a fleet node has one per member it has
    /// synced, which is why this is a map rather than a field.
    progress: std::sync::Mutex<std::collections::HashMap<String, Arc<SyncProgress>>>,
}

/// What a node serves, which decides its datadir and how a caller addresses it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineKind {
    /// One database, whose directory is the node's datadir. Every admin
    /// database, and any watch-only one that opted out of the shared scan.
    Own,
    /// A network's shared scan: one node under `<data-dir>/.fleet/<network>`,
    /// serving every watch-only member of that network from one connection and
    /// one pass over the blocks. Addressed per member by name.
    Fleet { network: crate::network::Network },
}

/// How far along the sync in flight is, published as it runs.
///
/// A sync is one `await` from the caller's point of view, so a caller that
/// wants to render progress cannot poll for it. This is the side channel:
/// [`Engine::sync_to_tip`] stores each observation here, and
/// [`Engine::progress`] hands out the handle to read them from. Both numbers
/// are `0` before the first observation, which reads as "nothing to show yet".
#[derive(Debug, Default)]
pub struct SyncProgress {
    scanned: AtomicU32,
    tip: AtomicU32,
}

impl SyncProgress {
    /// The height scanned so far, and the chain tip it is measured against.
    /// The tip is `None` until the node reports one.
    pub fn read(&self) -> (u32, Option<u32>) {
        let scanned = self.scanned.load(Ordering::Relaxed);
        let tip = match self.tip.load(Ordering::Relaxed) {
            0 => None,
            t => Some(t),
        };
        (scanned, tip)
    }

    pub(crate) fn store(&self, scanned: u32, tip: Option<u32>) {
        self.scanned.store(scanned, Ordering::Relaxed);
        self.tip.store(tip.unwrap_or(0), Ordering::Relaxed);
    }
}

impl Engine {
    /// Prepare the engine for a database that already exists on disk.
    ///
    /// Node-less and offline: this reads `keys.toml`, maps it onto a zecd
    /// configuration, and stops. It does not dial, scan, or create anything.
    pub fn open(db_name: &str, conn: &ConnectionArgs) -> Result<Engine, EngineError> {
        let cfg = WalletConfig::read(db_name).map_err(EngineError::Other)?;
        Engine::from_config(db_name, cfg, conn)
    }

    /// `Engine::open` for a caller that already holds the config, so a
    /// freshly-created database does not re-read the file it just wrote.
    pub fn from_config(
        db_name: &str,
        mut cfg: WalletConfig,
        conn: &ConnectionArgs,
    ) -> Result<Engine, EngineError> {
        let db_dir = db_dir(db_name).map_err(EngineError::Other)?;
        // Every path into the engine comes through here, so this is where a
        // database that predates the engine is adopted. It has to happen
        // before any node starts: see `migrate`.
        migrate::ensure_adopted(db_name, &mut cfg)?;
        let app = config::app_config(&db_dir, &cfg, conn)?;
        Ok(Engine {
            db_name: db_name.to_string(),
            db_dir,
            cfg,
            kind: EngineKind::Own,
            app,
            node: tokio::sync::Mutex::new(None),
            busy_wait: Some(DEFAULT_BUSY_WAIT),
            progress: Default::default(),
        })
    }

    /// How long to wait for another process to release this database before
    /// giving up with [`EngineError::Busy`]. `None` fails immediately, which
    /// is what a background loop wants so it can skip the database and come
    /// back next pass.
    pub fn with_busy_wait(mut self, wait: Option<Duration>) -> Self {
        self.busy_wait = wait;
        self
    }

    /// Live progress of this engine's own sync; see [`SyncProgress`].
    ///
    /// On a fleet node this is the progress of the *default* wallet, which is
    /// nobody's: use [`Engine::progress_for`] with the member's name.
    pub fn progress(&self) -> Arc<SyncProgress> {
        self.progress_for(config::WALLET)
    }

    /// Live progress of one wallet's sync. A fleet node scans several members
    /// and reports each separately, so the caller says which it is rendering.
    pub fn progress_for(&self, wallet: &str) -> Arc<SyncProgress> {
        let mut map = self.progress.lock().expect("progress map poisoned");
        Arc::clone(map.entry(wallet.to_owned()).or_default())
    }

    /// What this node serves; see [`EngineKind`].
    pub fn kind(&self) -> &EngineKind {
        &self.kind
    }

    /// The database this engine belongs to.
    pub fn db_name(&self) -> &str {
        &self.db_name
    }

    /// The database's directory, which is also zecd's datadir and wallet dir.
    pub fn db_dir(&self) -> &std::path::Path {
        &self.db_dir
    }

    /// This database's `keys.toml`.
    pub fn config(&self) -> &WalletConfig {
        &self.cfg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The side channel a caller renders progress from.
    ///
    /// The distinction that matters is "no tip yet" versus a real height: the
    /// node reports `chain_tip: None` while it cannot say, and a caller must
    /// show a bare working line then rather than claim block 0.
    #[test]
    fn progress_reads_back_what_a_sync_stored() {
        let progress = SyncProgress::default();
        assert_eq!(progress.read(), (0, None), "nothing observed yet");

        progress.store(1_234, None);
        assert_eq!(progress.read(), (1_234, None), "scanning, tip unknown");

        progress.store(1_240, Some(2_000));
        assert_eq!(progress.read(), (1_240, Some(2_000)));

        // A later observation replaces the earlier one, including back to
        // "unknown" if the node stops reporting a tip.
        progress.store(1_250, None);
        assert_eq!(progress.read(), (1_250, None));
    }
}
