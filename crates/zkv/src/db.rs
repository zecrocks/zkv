//! High-level `Database` handle: the recommended entry point for Rust
//! consumers of the `zkv` library.
//!
//! `zkv` is a thin Redis-style KV layered over Zcash shielded memos: reads
//! come from scanning the chain with a UFVK, writes are signed memos
//! broadcast as zero-value self-sends. The CLI and the `zkv-faucet` binary
//! both wire that together by composing pieces from [`crate::config`],
//! [`crate::data`], [`crate::internal`], and [`crate::remote`]. That
//! composition is the same on every consumer, so this module exposes it
//! as a single [`Database`] struct.
//!
//! # Example: read an oracle value
//!
//! ```no_run
//! use zkv::{db::Database, remote::ConnectionArgs};
//!
//! # async fn run() -> Result<(), zkv::db::ZkvError> {
//! let db = Database::open("zec-usd-oracle", ConnectionArgs::default())?;
//! db.sync().await?;
//! if let Some(price) = db.get("zec_usd", zkv::db::Confirmations::default())? {
//!     println!("ZEC/USD = {price}");
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Example: publish an oracle value
//!
//! ```no_run
//! use zkv::{db::Database, remote::ConnectionArgs};
//!
//! # async fn run() -> Result<(), zkv::db::ZkvError> {
//! let db = Database::open("zec-usd-oracle", ConnectionArgs::default())?;
//! let txid = db.set("zec_usd", "553.88").await?;
//! eprintln!("broadcast {txid}");
//! # Ok(())
//! # }
//! ```
//!
//! # Example: publish several values in one transaction
//!
//! ```no_run
//! use zkv::{db::Database, remote::ConnectionArgs};
//!
//! # async fn run() -> Result<(), zkv::db::ZkvError> {
//! let db = Database::open("price-oracle", ConnectionArgs::default())?;
//! // One "sendmany" tx: one ZIP-317 fee, one txid for both keys.
//! let txid = db.set_many(&[("zec_usd", "553.88"), ("btc_usd", "67250.00")]).await?;
//! eprintln!("broadcast {txid}");
//! # Ok(())
//! # }
//! ```
//!
//! # Example: create an admin database from scratch
//!
//! ```no_run
//! use zkv::{db::Database, data::Network, remote::ConnectionArgs};
//!
//! # async fn run() -> Result<(), zkv::db::ZkvError> {
//! let (db, phrase) = Database::init_admin(
//!     "my-oracle",
//!     Network::Test,
//!     ConnectionArgs::default(),
//! ).await?;
//! eprintln!("BACK UP THIS RECOVERY PHRASE: {phrase}");
//! eprintln!("Fund this address: {}", db.zkv_address()?);
//! // Then `db.init().await?` once the wallet has the ZIP-317 fee.
//! # Ok(())
//! # }
//! ```
//!
//! # Logging
//!
//! `zkv` emits sync / broadcast progress via [`tracing`]. If you don't
//! install a subscriber, that output is silently dropped, which makes
//! sync look like it's hanging. Enable the `default-subscriber` feature
//! (on by default) and call [`install_default_subscriber`] once at
//! startup, or wire up your own `tracing-subscriber`.
//!
//! # What's covered, what isn't
//!
//! `Database` wraps the chain-scan + state-replay + signed-broadcast
//! pipeline. It does **not** auto-shield transparent funds or expose
//! mempool-fetching directly; see [`Database::sync_with_mempool`]. It also
//! never syncs implicitly: see the "Syncing model" note on [`Database`].

use std::collections::HashSet;

pub use crate::internal::funding::{FundingDirection, FundingResult, FundingTx};
pub use crate::protocol::{
    AuditResult, AuditRow, AuthRegistry, Authority, BlockCap, BlockSet, DropReason, HistoryEntry,
    HistoryResult, HistoryStatus, InitState, MemoFormat, ReplayResult, RevokedRole, RowOutcome,
    Scope, VersionState, MAX_DB_VERSION,
};

use zcash_keys::keys::UnifiedFullViewingKey;
use zcash_protocol::ShieldedPool;

use crate::{
    config::{Role, WalletConfig, WalletEngine},
    data::{db_dir, init_dbs, Network},
    internal::{
        account::account_keys,
        funding::load_funding,
        pending,
        protocol::PendingOp,
        state::{
            cached_version, load_audit, load_history_page, load_state_with_height, wallet_tip,
            HistoryOrder,
        },
        sync::{read_sync, CancelFlag, NEAR_TIP_TOLERANCE},
        write::{
            broadcast_init, manage_and_broadcast, prepare, prepare_init, prepare_management,
            write_and_broadcast, write_many_and_broadcast, BatchItem, PreparedWrite, WriteError,
        },
    },
    protocol::{network_from_type, parse_zkv_addr, receiver_domain, Op},
    remote::ConnectionArgs,
};

/// One operation in a batch ("sendmany") write; see [`Database::write_many`].
///
/// A batch is broadcast as a single transaction (one ZIP-317 fee, one txid)
/// carrying one zero-value memo output per op. Ops are applied in order; the
/// read path already handles such multi-output transactions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteOp {
    /// Set `key` to `value`. The wire form is picked automatically by
    /// [`Op::set_for_value`] (`SET`, or `SETL` for empty/newline values).
    Set { key: String, value: String },
    /// Delete `key`.
    Del { key: String },
}

impl WriteOp {
    /// Convenience constructor for a SET op.
    pub fn set(key: impl Into<String>, value: impl Into<String>) -> WriteOp {
        WriteOp::Set {
            key: key.into(),
            value: value.into(),
        }
    }

    /// Convenience constructor for a DEL op.
    pub fn del(key: impl Into<String>) -> WriteOp {
        WriteOp::Del { key: key.into() }
    }
}

/// Confirmation depth for reads.
///
/// `zkv` distinguishes self-sent writes (always visible at any depth
/// because the wallet tracks its own broadcasts) from externally-received
/// writes (only visible at the requested depth). For an oracle reader
/// that wants to ignore unconfirmed external writes, use
/// [`Confirmations::Default`] or higher.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Confirmations {
    /// Include mempool entries (`0` confirmations). The reader also
    /// auto-pulls the live lightwalletd mempool on next sync.
    Mempool,
    /// One confirmation (what the CLI uses for self-sent INIT detection).
    OneBlock,
    /// Default for general reads (3 confirmations); the CLI's `zkv get`
    /// default.
    #[default]
    Default,
    /// Custom depth.
    Custom(u32),
}

impl Confirmations {
    pub fn as_u32(self) -> u32 {
        match self {
            Self::Mempool => 0,
            Self::OneBlock => 1,
            Self::Default => 3,
            Self::Custom(n) => n,
        }
    }
}

impl From<u32> for Confirmations {
    fn from(n: u32) -> Self {
        Self::Custom(n)
    }
}

/// A point-in-time read of a database: the replayed state plus the chain
/// height that state reflects.
///
/// Returned by [`Database::read_at`]. `zkv` reads are pure-local: they
/// replay whatever the local wallet has already scanned and never touch the
/// network (see the "Syncing model" note on [`Database`]). `as_of_height`
/// is the height the wallet had scanned to when this read ran, so a consumer
/// driving its own [`Database::sync`] cadence can decide whether its state
/// is stale enough to warrant another sync: compare it against
/// [`Database::chain_tip`] (the live network tip) to learn how many blocks
/// behind the read is.
#[derive(Clone, Debug)]
pub struct ReadResult {
    /// The replayed per-key state, INIT status, and authorization registry,
    /// with any in-flight `pending.toml` ops merged in (exactly what
    /// [`Database::read`] returns).
    pub replay: ReplayResult,
    /// The chain height the local wallet had scanned when this state was
    /// read. `None` only before the wallet's first successful sync.
    pub as_of_height: Option<u32>,
}

/// Errors from the [`Database`] facade.
///
/// The most common cases are carved out as structured variants so
/// callers can pattern-match (`InsufficientFunds` for balance UX,
/// `NotInitialized` for the "fund-and-init" prompt, …). Anything not
/// yet structured falls through as [`ZkvError::Other`]; that variant
/// will shrink over time as the API matures.
#[derive(Debug, thiserror::Error)]
pub enum ZkvError {
    /// No database with that name under the active data dir.
    #[error("no database named {0:?} (no keys.toml found)")]
    UnknownDatabase(String),

    /// The database hasn't broadcast an INIT memo at the requested
    /// confirmation depth yet.
    #[error(
        "database is not initialized at the requested confirmation depth, \
         broadcast INIT via Database::init or fund the wallet and run the CLI's \
         `zkv init`"
    )]
    NotInitialized,

    /// INIT has been seen but hasn't reached the write threshold.
    #[error("database is initializing ({done}/{required} confirmations)")]
    Initializing { done: u32, required: u32 },

    /// The local wallet hasn't scanned up to within
    /// [`NEAR_TIP_TOLERANCE`] of the
    /// current chain tip, so the "uninitialized" verdict isn't yet
    /// authoritative: a valid INIT could still be sitting in not-yet-scanned
    /// blocks. Re-broadcasting INIT is refused until a sync confirms the
    /// database really is empty.
    #[error(
        "database must be fully synced to the chain tip before broadcasting INIT, run a sync first"
    )]
    NotSynced,

    /// Couldn't confirm a current chain tip when creating a database: the
    /// lightwalletd tip is older than the freshness window (chain stalled,
    /// or the server is stale/unreachable). Refused so the wallet birthday
    /// isn't pinned to a stale tip.
    #[error("can't confirm a current chain tip, the lightwalletd tip is stale or unreachable; check your connection and retry")]
    StaleChainTip,

    /// Operation requires an admin database (one with a spending key).
    #[error("operation requires an admin database; this is watch-only")]
    WatchOnly,

    /// The database is enrolled in the shared scan but the wallet engine has
    /// not imported its account into a shard yet, so there is nothing local to
    /// read. An ordinary transient state on a newly-watched database (the
    /// engine onboards one member per sync pass), and deliberately distinct
    /// from "this database has no wallet key imported", which describes a
    /// database whose local data was lost and which needs repairing.
    #[error(
        "this database is joining the shared scan; its first sync pass has not imported it yet"
    )]
    Importing,

    /// The shared scan refused this database's viewing key outright: a birthday
    /// no tree state can serve, or a key already present in the shard. Unlike
    /// [`ZkvError::Importing`] this never resolves on its own, which is why it
    /// is a separate variant rather than a longer wait.
    #[error("the shared scan cannot import this database: {0}")]
    ImportFailed(String),

    /// This database's signing key lacks the on-chain authority for the
    /// requested operation: it isn't an owner (for a management op), or it
    /// isn't an owner/in-scope writer (for a data op). Distinct from
    /// [`ZkvError::WatchOnly`], which is about *holding a key at all*.
    #[error("not authorized: {0}")]
    Unauthorized(String),

    /// Not enough spendable funds to cover the requested transaction.
    /// `pending` is the in-flight balance (mempool / immature change /
    /// unshielded transparent): funds that aren't spendable right now
    /// but may be soon.
    #[error(
        "insufficient balance: {available} zats spendable, {required} zats needed \
         (pending: {pending})"
    )]
    InsufficientFunds {
        available: u64,
        required: u64,
        pending: u64,
    },

    /// The database has been upgraded to a protocol epoch newer than this build
    /// supports, and the controlling `VERSION` memo blocks `operation` for
    /// out-of-date clients. `required` is the database's announced version;
    /// `supported` is this build's [`MAX_DB_VERSION`]. A warn-only mismatch (no
    /// block flag for this operation) does *not* produce this error; it surfaces
    /// as a stderr/log warning while the operation proceeds.
    #[error(
        "this database requires zkv client version {required} (this build supports up to \
         {supported}); {operation} is blocked, update zkv to the latest version"
    )]
    ClientUpgradeRequired {
        required: u32,
        supported: u32,
        operation: GatedOp,
    },

    /// Catch-all for errors not yet captured by a structured variant.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<crate::internal::sync::TipError> for ZkvError {
    fn from(e: crate::internal::sync::TipError) -> Self {
        match e {
            crate::internal::sync::TipError::StaleTip => ZkvError::StaleChainTip,
            crate::internal::sync::TipError::Other(err) => ZkvError::Other(err),
        }
    }
}

/// An operation that a `VERSION` memo's block flags may forbid for an
/// under-versioned client. See [`ZkvError::ClientUpgradeRequired`] and
/// [`require_supported`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatedOp {
    /// Scanning the chain for new blocks (`blocksync`).
    Sync,
    /// Interpreting/displaying state (`blockread`).
    Read,
    /// Broadcasting writes (`blockwrite`).
    Write,
}

impl std::fmt::Display for GatedOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            GatedOp::Sync => "syncing",
            GatedOp::Read => "reading",
            GatedOp::Write => "writing",
        })
    }
}

/// Check a freshly-read [`VersionState`] against this build for `operation`:
/// `Err(ZkvError::ClientUpgradeRequired)` if the database blocks that operation
/// for this (out-of-date) client, else `Ok(())`. Pure: pass a
/// [`ReplayResult::version`] you already hold (e.g. from [`Database::read`]) so
/// CLI/GUI callers can gate without re-reading. A warn-only mismatch returns
/// `Ok(())`; surface [`VersionState::upgrade_warning`] separately for that.
pub fn require_supported(version: &VersionState, operation: GatedOp) -> Result<()> {
    let blocked = match operation {
        GatedOp::Sync => version.blocks_sync(),
        GatedOp::Read => version.blocks_read(),
        GatedOp::Write => version.blocks_write(),
    };
    if blocked {
        Err(ZkvError::ClientUpgradeRequired {
            required: version.version,
            supported: MAX_DB_VERSION,
            operation,
        })
    } else {
        Ok(())
    }
}

/// Convenience alias for `Result<T, ZkvError>`.
pub type Result<T> = std::result::Result<T, ZkvError>;

/// A handle to one zkv database (admin, which can sign and broadcast, or
/// watch, which is read-only).
///
/// Construct with [`Database::open`] (existing database),
/// [`Database::init_admin`] / [`Database::restore_admin`] (new admin
/// database), or [`Database::init_watch`] (new watch-only database).
/// Database names follow `[A-Za-z0-9_-]{1,64}` (see
/// [`crate::data::validate_db_name`]) and are resolved against the
/// active data directory (`--data-dir`, `$ZKV_DATA`, then the per-OS
/// default: `$HOME/.zkv` on Linux, `$HOME/Library/Application Support/zkv`
/// on macOS, `%APPDATA%\zkv` on Windows).
///
/// # Syncing model
///
/// A `Database` never talks to the network on its own; no implicit
/// background sync, no persistent connection. The only methods that touch
/// lightwalletd are the explicit [`sync`](Database::sync) /
/// [`sync_with_mempool`](Database::sync_with_mempool) calls, the tip probes
/// [`chain_tip`](Database::chain_tip) /
/// [`synced_to_tip`](Database::synced_to_tip), and the *online* write
/// methods ([`set`](Database::set), [`del`](Database::del),
/// [`init`](Database::init), and the `grant_*`/`revoke_*` role ops), which
/// sync once before broadcasting.
///
/// Every **read** ([`read`](Database::read) / [`read_at`](Database::read_at),
/// [`get`](Database::get), [`init_state`](Database::init_state),
/// [`roles`](Database::roles), [`history`](Database::history),
/// [`audit`](Database::audit), [`funding`](Database::funding),
/// [`balance`](Database::balance), [`synced_height`](Database::synced_height))
/// is pure-local: it replays whatever the wallet has already scanned and
/// performs no network I/O.
///
/// So the facade is **"sync once, then schedule it yourself"**: call
/// [`sync`](Database::sync) on whatever cadence you choose (a timer, an
/// external trigger, or just before a batch of reads) and read freely in
/// between. To judge staleness, compare
/// [`read_at`](Database::read_at)'s `as_of_height` against
/// [`chain_tip`](Database::chain_tip). For writes under your own cadence,
/// the `*_no_sync` variants ([`set_no_sync`](Database::set_no_sync),
/// [`del_no_sync`](Database::del_no_sync),
/// [`grant_owner_no_sync`](Database::grant_owner_no_sync), …) skip the
/// forced pre-broadcast sync (they still broadcast immediately). The
/// continuous auto-sync you may have seen is
/// not in the facade; it lives in layers above it: the `zkv get` CLI
/// command (auto-syncs unless `--offline`) and the GUI server's pausable
/// background refresh loop.
pub struct Database {
    name: String,
    cfg: WalletConfig,
    conn: ConnectionArgs,
    /// How long to wait for a database another process holds. `None` skips
    /// immediately, which is what a background loop wants.
    busy_wait: Option<std::time::Duration>,
    /// The wallet engine, built on first use rather than at `open`.
    ///
    /// Not at `open` for two reasons: opening a database is offline and
    /// side-effect-free, while building an engine adopts it (which writes
    /// `keys.toml`); and a handle that performs several writes should share
    /// one node rather than paying a startup each time.
    engine: tokio::sync::OnceCell<crate::engine::EngineRef>,
}

/// How long a one-shot command contributes to a converting database's shared
/// scan before getting on with what it was asked to do. See
/// [`Database::drive_pending_fleet`].
const FLEET_NUDGE: std::time::Duration = std::time::Duration::from_secs(30);

impl Database {
    /// Open an existing zkv database by name.
    ///
    /// Returns [`ZkvError::UnknownDatabase`] if no `keys.toml` exists.
    pub fn open(name: &str, conn: ConnectionArgs) -> Result<Self> {
        let mut cfg = WalletConfig::read(name).map_err(|e| classify_open_error(e, name))?;
        // A watch-only database nobody has chosen for joins the shared scan
        // here. Offline, idempotent, and it does not change what this handle
        // reads: the database keeps its own files until its shard has caught
        // up with them. See `crate::fleet`.
        crate::fleet::enrol_if_unchosen(&mut cfg, name);
        Ok(Self {
            name: name.to_owned(),
            cfg,
            conn,
            busy_wait: Some(crate::engine::DEFAULT_BUSY_WAIT),
            engine: tokio::sync::OnceCell::new(),
        })
    }

    /// Generate a fresh 24-word BIP-39 recovery phrase without creating any
    /// database (no disk writes, no network). Pair with
    /// [`Database::restore_admin_with_pool`] to persist it only after the user
    /// has confirmed they wrote it down, so an abandoned create flow leaves
    /// nothing on disk.
    pub fn generate_phrase() -> String {
        use bip0039::{Count, English, Mnemonic};
        Mnemonic::<English>::generate(Count::Words24)
            .phrase()
            .to_owned()
    }

    /// Create a brand-new admin database with a fresh 24-word mnemonic.
    ///
    /// Connects to lightwalletd to fetch the current chain tip (used as
    /// the wallet birthday, rounded down by 10 blocks for safety).
    /// Returns the opened [`Database`] paired with the recovery phrase.
    /// **Back this up before anything else**; without it the database
    /// is unrecoverable if the local data directory is lost.
    ///
    /// Errors if a database with that name already exists.
    pub async fn init_admin(
        name: &str,
        network: Network,
        conn: ConnectionArgs,
    ) -> Result<(Self, String)> {
        let pool = crate::config::default_pool_for_network(network);
        Self::init_admin_with_pool(name, network, pool, conn).await
    }

    /// Like [`Database::init_admin`], but lets the caller pick the shielded
    /// pool (Ironwood or Sapling) the database lives in. Every memo is read
    /// from and written to this pool; it is fixed at creation. `init_admin`
    /// defaults to Ironwood on every network. The legacy Orchard label is
    /// rejected here ([`ZkvError::Other`]): it shares the Ironwood receiver,
    /// so new databases use Ironwood, while [`Database::restore_admin_with_pool`]
    /// still accepts Orchard for wallets originally created with it.
    pub async fn init_admin_with_pool(
        name: &str,
        network: Network,
        pool: ShieldedPool,
        conn: ConnectionArgs,
    ) -> Result<(Self, String)> {
        use bip0039::{Count, Mnemonic};

        ensure_pool_available(pool, network)?;
        ensure_pool_creatable(pool)?;
        let mnemonic = Mnemonic::generate(Count::Words24);
        let phrase = mnemonic.phrase().to_owned();
        let db = create_admin(name, network, &mnemonic, None, pool, conn).await?;
        Ok((db, phrase))
    }

    /// Restore an admin database from an existing 24-word mnemonic.
    ///
    /// `birthday` lets the caller specify the wallet birthday height.
    /// `None` defaults to (chain tip − 10), which means historical memos
    /// won't be visible.
    pub async fn restore_admin(
        name: &str,
        network: Network,
        recovery_phrase: &str,
        birthday: Option<u32>,
        conn: ConnectionArgs,
    ) -> Result<Self> {
        let pool = crate::config::default_pool_for_network(network);
        Self::restore_admin_with_pool(name, network, recovery_phrase, birthday, pool, conn).await
    }

    /// Like [`Database::restore_admin`], but lets the caller pick the shielded
    /// pool. The pool must match the one chosen at the original
    /// [`Database::init_admin_with_pool`], or the reconstructed zkv address
    /// won't match and the database will look empty.
    pub async fn restore_admin_with_pool(
        name: &str,
        network: Network,
        recovery_phrase: &str,
        birthday: Option<u32>,
        pool: ShieldedPool,
        conn: ConnectionArgs,
    ) -> Result<Self> {
        use bip0039::{English, Mnemonic};
        use secrecy::Zeroize as _;

        ensure_pool_available(pool, network)?;
        let mut normalized = recovery_phrase
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let mnemonic: Mnemonic<English> = Mnemonic::from_phrase(&normalized)
            .map_err(|e| ZkvError::Other(anyhow::anyhow!("invalid recovery phrase: {e}")))?;
        // The normalized phrase is a seed equivalent; wipe the owned copy.
        normalized.zeroize();
        create_admin(name, network, &mnemonic, birthday, pool, conn).await
    }

    /// Create a new watch-only database for an existing `zkv1…` address. The
    /// network and birthday are both read from the address (its HRP and its
    /// metadata item, respectively).
    ///
    /// The database joins the data directory's **shared scan** unless that has
    /// been turned off (see [`crate::fleet`] and `zkv fleet`): many watch-only
    /// databases then share one node, one upstream connection and one pass over
    /// the blocks, instead of running a node each. Use
    /// [`Database::init_watch_standalone`] for a node of its own.
    pub async fn init_watch(name: &str, zkv_address: &str, conn: ConnectionArgs) -> Result<Self> {
        let engine = if crate::fleet::fleet_is_default() {
            WalletEngine::Fleet
        } else {
            WalletEngine::Own
        };
        Self::init_watch_with(name, zkv_address, conn, engine).await
    }

    /// [`Database::init_watch`] with a node of this database's own, whatever
    /// the data directory's default is. This is `zkv watch --standalone`, and
    /// the shape every watch database had before the shared scan existed.
    pub async fn init_watch_standalone(
        name: &str,
        zkv_address: &str,
        conn: ConnectionArgs,
    ) -> Result<Self> {
        Self::init_watch_with(name, zkv_address, conn, WalletEngine::Own).await
    }

    async fn init_watch_with(
        name: &str,
        zkv_address: &str,
        conn: ConnectionArgs,
        engine: WalletEngine,
    ) -> Result<Self> {
        use zcash_client_backend::data_api::{AccountPurpose, WalletWrite};

        let parsed = parse_zkv_addr(zkv_address).map_err(ZkvError::Other)?;
        let network = network_from_type(parsed.network).map_err(ZkvError::Other)?;

        let dir = db_dir(name).map_err(ZkvError::Other)?;
        if dir.join("keys.toml").exists() {
            return Err(ZkvError::Other(anyhow::anyhow!(
                "database {name:?} already exists at {}",
                dir.display()
            )));
        }

        // Refuse to import the same database twice under a different name.
        if let Some(existing) = find_duplicate_by_ufvk(&parsed.ufvk, parsed.pool, network)? {
            return Err(ZkvError::Other(anyhow::anyhow!(
                "this database is already imported as {existing:?}; \
                 switch to it instead of importing it again"
            )));
        }

        // Same fresh-tip guard as the admin-create path: don't build a watch db
        // against a stale/unreachable server's view of the chain. The birthday
        // is carried by the address, so it is pinned verbatim (no buffer).
        let birthday =
            crate::internal::sync::pinned_birthday(&conn, network, parsed.birthday).await?;

        WalletConfig::init_watch(
            name,
            birthday.height(),
            network,
            zkv_address,
            parsed.pool,
            engine,
        )
        .map_err(ZkvError::Other)?;

        if engine.is_fleet_member() {
            // A member has no wallet files of its own: the shard holds its
            // account, and the fleet node imports it from the manifest on a
            // later sync pass. Writing the manifest is therefore the whole of
            // creation, and it is offline.
            let cfg = WalletConfig::read(name).map_err(ZkvError::Other)?;
            let manifest = crate::fleet::manifest_for(&cfg).map_err(ZkvError::Other)?;
            crate::fleet::write_manifest(network, name, &manifest).map_err(ZkvError::Other)?;
        } else {
            let mut db_data = init_dbs(network, name).map_err(ZkvError::Other)?;
            db_data
                .import_account_ufvk(
                    name,
                    &parsed.ufvk,
                    &birthday,
                    AccountPurpose::ViewOnly,
                    None,
                )
                .map_err(|e| ZkvError::Other(anyhow::anyhow!("{e:?}")))?;
            drop(db_data);
        }

        crate::demo::promote_current(name).map_err(ZkvError::Other)?;
        Self::open(name, conn)
    }

    /// Database name as passed to [`Database::open`].
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Admin (can sign writes) or Watch (read-only).
    pub fn role(&self) -> Role {
        self.cfg.role
    }

    /// Mainnet or testnet.
    pub fn network(&self) -> Network {
        self.cfg.network
    }

    /// The canonical `zkv1…` address for this database (the viewing key under a
    /// `zkv` HRP, birthday carried in its metadata item).
    pub fn zkv_address(&self) -> Result<String> {
        // For watch-only databases the address is stored in keys.toml;
        // avoid the wallet-DB round trip in that case.
        if let Some(addr) = &self.cfg.zkv_address {
            return Ok(addr.clone());
        }
        let keys = account_keys(&self.cfg, &self.name).map_err(ZkvError::Other)?;
        Ok(keys.zkv_addr)
    }

    /// The single-pool unified address (this database's pool) that receives
    /// funding for this database (where you send ZEC so the wallet can pay
    /// write fees). Admin-only; watch databases hold no spending key.
    pub fn funding_address(&self) -> Result<String> {
        self.require_admin()?;
        let keys = account_keys(&self.cfg, &self.name).map_err(ZkvError::Other)?;
        Ok(keys.recipient_ua)
    }

    /// The canonical `zkvid1…` public key this database is known by: the
    /// UFVK-derived root signer that becomes owner #1 once INIT confirms
    /// (see [`Database::roles`]). Derived from the UFVK's transparent
    /// component, so it's available for watch-only databases too (no
    /// spending key required); it just identifies the root key in the
    /// authorization registry.
    pub fn signer(&self) -> Result<String> {
        let keys = account_keys(&self.cfg, &self.name).map_err(ZkvError::Other)?;
        Ok(crate::protocol::pubkey_bech32(&keys.verifying_pubkey))
    }

    /// The database's **receiver domain**: the network-tagged hex of its pool
    /// receiver bytes (see [`receiver_domain`]). This, not the `zkv1…` address
    /// string, is what every `ZKV0` signature binds to, so it's what a verifier
    /// reconstructs a signed payload over. Derived from the UFVK, so it's
    /// available for watch-only databases. Used by `zkv verify`.
    ///
    /// [`receiver_domain`]: crate::protocol::receiver_domain
    pub fn receiver(&self) -> Result<String> {
        let keys = account_keys(&self.cfg, &self.name).map_err(ZkvError::Other)?;
        Ok(keys.receiver_hex)
    }

    /// The wallet's total balance in zatoshi.
    ///
    /// Returns [`ZkvError::WatchOnly`] for watch-only databases (they
    /// hold no spending key and therefore no balance). Does not sync
    /// first; call [`Database::sync`] beforehand for a fresh figure.
    pub fn balance(&self) -> Result<u64> {
        use zcash_client_backend::data_api::{wallet::ConfirmationsPolicy, WalletRead};

        self.require_admin()?;
        let (_, db_data_path) = crate::data::get_db_paths(&self.name).map_err(ZkvError::Other)?;
        let db_data = crate::data::open_wallet_db(db_data_path, self.cfg.network)
            .map_err(|e| ZkvError::Other(anyhow::anyhow!("open wallet db: {e}")))?;
        let total = db_data
            .get_wallet_summary(ConfirmationsPolicy::default())
            .map_err(|e| ZkvError::Other(anyhow::anyhow!("wallet summary: {e}")))?
            .map(|s| {
                s.account_balances()
                    .values()
                    .map(|b| u64::from(b.total()))
                    .sum()
            })
            .unwrap_or(0);
        Ok(total)
    }

    /// Funds still confirming, in zatoshi: incoming notes pending
    /// spendability, change pending confirmation, and unshielded
    /// transparent balance. These are already counted in
    /// [`Database::balance`] (which returns `total()`) but are not yet
    /// spendable. Returns [`ZkvError::WatchOnly`] for watch-only databases.
    /// Does not sync first.
    pub fn balance_confirming(&self) -> Result<u64> {
        use zcash_client_backend::data_api::{wallet::ConfirmationsPolicy, WalletRead};

        self.require_admin()?;
        let (_, db_data_path) = crate::data::get_db_paths(&self.name).map_err(ZkvError::Other)?;
        let db_data = crate::data::open_wallet_db(db_data_path, self.cfg.network)
            .map_err(|e| ZkvError::Other(anyhow::anyhow!("open wallet db: {e}")))?;
        let confirming = db_data
            .get_wallet_summary(ConfirmationsPolicy::default())
            .map_err(|e| ZkvError::Other(anyhow::anyhow!("wallet summary: {e}")))?
            .map(|s| {
                s.account_balances()
                    .values()
                    .map(|b| {
                        u64::from(b.value_pending_spendability())
                            + u64::from(b.change_pending_confirmation())
                            + u64::from(b.unshielded_balance().total())
                    })
                    .sum()
            })
            .unwrap_or(0);
        Ok(confirming)
    }

    /// The height the local wallet has scanned to, or `None` if it has
    /// not synced yet. Read straight from the wallet DB; no network.
    ///
    /// This is the wallet's **fully-scanned height** (the height below which
    /// every block has been scanned), which advances as each scan batch
    /// commits during a sync, so a status poll running alongside a catch-up
    /// sync sees it climb. It deliberately does *not* use `chain_height()`,
    /// the wallet's *known chain tip*: that is set once by `update_chain_tip`
    /// at the start of every sync and then sits frozen until the scan
    /// finishes, so it can't track scan progress. Falls back to
    /// `chain_height()` before the first wallet summary exists (a brand-new
    /// wallet with nothing scanned yet).
    pub fn synced_height(&self) -> Result<Option<u32>> {
        use zcash_client_backend::data_api::{wallet::ConfirmationsPolicy, WalletRead};

        let (_, db_data_path) = crate::data::get_db_paths(&self.name).map_err(ZkvError::Other)?;
        let db_data = crate::data::open_wallet_db(db_data_path, self.cfg.network)
            .map_err(|e| ZkvError::Other(anyhow::anyhow!("open wallet db: {e}")))?;
        if let Some(summary) = db_data
            .get_wallet_summary(ConfirmationsPolicy::default())
            .map_err(|e| ZkvError::Other(anyhow::anyhow!("wallet summary: {e}")))?
        {
            return Ok(Some(u32::from(summary.fully_scanned_height())));
        }
        let h = db_data
            .chain_height()
            .map_err(|e| ZkvError::Other(anyhow::anyhow!("chain height: {e}")))?
            .map(u32::from);
        Ok(h)
    }

    /// Ask lightwalletd for the current chain tip height. One network
    /// round-trip; does not touch the local wallet.
    pub async fn chain_tip(&self) -> Result<u32> {
        use zcash_client_backend::proto::service;

        let mut client = self
            .conn
            .connect(self.cfg.network)
            .await
            .map_err(ZkvError::Other)?;
        let tip = client
            .get_latest_block(service::ChainSpec::default())
            .await
            .map_err(|e| ZkvError::Other(anyhow::anyhow!("get_latest_block: {e}")))?
            .into_inner()
            .height;
        u32::try_from(tip).map_err(|_| {
            ZkvError::Other(anyhow::anyhow!("chain tip height {tip} out of u32 range"))
        })
    }

    /// Whether the local wallet has scanned up to the current chain tip
    /// (wallet height is within [`NEAR_TIP_TOLERANCE`]
    /// of the lightwalletd tip and there are no outstanding scan ranges). One
    /// network round-trip for the tip. The tolerance matches the read sync and
    /// the GUI's "synced" indicator, so a database that reads as synced is also
    /// eligible to (re)broadcast INIT.
    ///
    /// Gate for re-broadcasting INIT on an existing database: until the wallet
    /// is caught up, an "uninitialized" verdict isn't authoritative: a valid
    /// INIT could still be sitting in not-yet-scanned blocks.
    pub async fn synced_to_tip(&self) -> Result<bool> {
        crate::internal::sync::wallet_synced_to_tip(&self.name, &self.conn, self.cfg.network)
            .await
            .map_err(ZkvError::Other)
    }

    /// Sync the local wallet against lightwalletd up to chain tip.
    ///
    /// Returns the synced chain height. Status detail is emitted via
    /// `tracing::info!`.
    ///
    /// This is the read facade's sync; it never precedes a spend (the write
    /// methods run their own strict pre-broadcast sync internally), so it uses
    /// the read tolerance: when the wallet is already within
    /// [`sync::NEAR_TIP_TOLERANCE`](crate::internal::sync::NEAR_TIP_TOLERANCE)
    /// blocks of the live tip and has no pending scan, the whole
    /// download/scan/enhance pipeline is skipped (the newest block or two never
    /// changes a confirmed read). A tight read/poll loop therefore does one
    /// cheap `GetLatestBlock` per call instead of a full pass.
    ///
    /// Honors a `blocksync` directive recorded in the snapshot cache: if the
    /// database has moved to an epoch newer than this build supports and blocks
    /// syncing, the network scan is skipped (the current cached tip is returned)
    /// rather than performed. This reflects only already-promoted `VERSION`
    /// memos; a recent one in the live tail is seen on the next read.
    pub async fn sync(&self) -> Result<u32> {
        self.sync_through_engine(Some(NEAR_TIP_TOLERANCE), None)
            .await
    }

    /// Like [`Database::sync`], but cooperatively cancellable: when `cancel`
    /// flips to `true` the in-flight scan stops at the next block-batch boundary
    /// and returns the height reached so far (a partial sync is safe and resumes
    /// on the next call). The GUI's background auto-sync loop uses this so
    /// pausing halts scanning promptly instead of only at the next cycle;
    /// passing `None` is identical to [`Database::sync`].
    pub async fn sync_cancellable(&self, cancel: Option<CancelFlag>) -> Result<u32> {
        self.sync_through_engine(Some(NEAR_TIP_TOLERANCE), cancel)
            .await
    }

    /// Sync, never skipping, so a [`Confirmations::Mempool`] read is as fresh
    /// as this handle can make it.
    ///
    /// **Known gap.** This no longer *pulls* the mempool. Before the wallet
    /// engine, the read drained `GetMempoolStream` and decrypted every
    /// transaction before returning; the node ingests the mempool through a
    /// subscription its actor services only after catch-up, and the
    /// `waitforsync` barrier underneath covers the scan and the enhancement
    /// backlog but not that ingestion. There is no upstream signal to wait on,
    /// so this passes `None` tolerance (never skip, keep the node up and
    /// working) and returns.
    ///
    /// This database's *own* unconfirmed writes are unaffected: they come from
    /// `pending.toml`, which [`Database::read`] merges regardless. What can be
    /// missed is another writer's unconfirmed write, on a short-lived process
    /// that exits before the node's subscription has ingested it. A long-lived
    /// handle keeps a node alive and does see them.
    pub async fn sync_with_mempool(&self) -> Result<u32> {
        if let Some(tip) = self.skip_sync_if_blocked()? {
            return Ok(tip);
        }
        // `None` tolerance: never skip. The node keeps a live mempool
        // subscription while it is caught up, so bringing it up is what makes
        // unconfirmed writes visible at all, and the scan itself costs nothing
        // when the wallet is already at the tip.
        self.sync_through_engine(None, None).await
    }

    /// If the snapshot cache records a `blocksync` directive this build can't
    /// satisfy, return `Some(current_tip)` (skip the scan); otherwise `None`
    /// (caller should sync normally).
    /// Bring the wallet up to date through the engine, unless it is already
    /// close enough that this read cannot change.
    ///
    /// The skip decision is [`read_sync_skippable`]'s, not this method's: it
    /// costs one local height read and one `GetLatestBlock` over zkv's own
    /// transport, and needs no node. That ordering is the point. Most reads on
    /// a warm wallet skip, and starting a node to discover that would cost far
    /// more than the answer.
    async fn sync_through_engine(
        &self,
        tolerance: Option<u32>,
        cancel: Option<CancelFlag>,
    ) -> Result<u32> {
        if let Some(tip) = self.skip_sync_if_blocked()? {
            return Ok(tip);
        }
        let engine = self.engine().await?;
        let out = read_sync(
            engine,
            &self.name,
            &self.conn,
            self.cfg.network,
            tolerance,
            cancel,
        )
        .await;
        // A database converting to the shared scan syncs twice: its own files,
        // which are what this read will use, and then the shard, which is what
        // it will use once the shard catches up. See `drive_pending_fleet`.
        if let Ok(height) = &out {
            self.drive_pending_fleet(*height).await;
        }
        match out {
            Ok(height) => Ok(height),
            // A cancellation is the caller getting what it asked for, not a
            // failure, and the documented contract is the height reached. The
            // engine keeps returning `Cancelled` because *it* cannot tell a
            // stop from a success; the distinction is only meaningful here,
            // where the caller owns the flag it flipped.
            //
            // Without this the GUI counted a deliberate pause as a sync-error
            // strike and could show a failure banner for a button the user
            // pressed. A partial sync is safe and resumes next call, so the
            // wallet's own scanned height is the honest answer.
            Err(e) if is_cancelled(&e) => Ok(self.synced_height()?.unwrap_or(0)),
            Err(e) => Err(classify_sync_error(e)),
        }
    }

    fn skip_sync_if_blocked(&self) -> Result<Option<u32>> {
        let cached = cached_version(&self.name).map_err(ZkvError::Other)?;
        if cached.blocks_sync() {
            tracing::warn!(
                "database disabled syncing for clients older than version {}; skipping scan",
                cached.version
            );
            return Ok(Some(wallet_tip(&self.name).map_err(ZkvError::Other)?));
        }
        Ok(None)
    }

    /// Replay the wallet's stored memos into per-key state at the
    /// requested confirmation depth, then merge in any `pending.toml`
    /// entries the wallet DB hasn't surfaced yet (so a write followed
    /// by an immediate read sees its own pending op).
    ///
    /// Does not sync first; call [`Database::sync`] beforehand for
    /// fresh state. To also learn the height the returned state reflects
    /// (a freshness signal for scheduled readers), use
    /// [`Database::read_at`].
    pub fn read(&self, min_confs: impl Into<Confirmations>) -> Result<ReplayResult> {
        Ok(self.read_at(min_confs)?.replay)
    }

    /// Like [`Database::read`], but also reports the chain height the
    /// returned state reflects (see [`ReadResult`]).
    ///
    /// This is the freshness-aware read for consumers that drive their own
    /// sync cadence: it bundles the replayed state and its "as of" height
    /// into a single wallet-DB pass, so the height can't drift from the
    /// state between two separate calls. Compare `as_of_height` against
    /// [`Database::chain_tip`] to learn how far behind the live chain the
    /// read is.
    ///
    /// Does not sync first.
    pub fn read_at(&self, min_confs: impl Into<Confirmations>) -> Result<ReadResult> {
        let min_confs = min_confs.into().as_u32();
        let (mut replay, tip) =
            load_state_with_height(&self.name, min_confs, /* strict = */ false)
                .map_err(ZkvError::Other)?;
        merge_pending(&self.name, &mut replay, min_confs);
        Ok(ReadResult {
            replay,
            // `load_state_with_height` reports tip 0 for a never-synced
            // wallet; surface that as "no height yet" rather than block 0.
            as_of_height: (tip != 0).then_some(tip),
        })
    }

    /// Convenience: return the confirmed value for `key`, or `None` if
    /// the key is unset or only has pending writes.
    pub fn get(&self, key: &str, min_confs: impl Into<Confirmations>) -> Result<Option<String>> {
        let result = self.read(min_confs)?;
        Ok(result.state.get(key).and_then(|ks| ks.confirmed.clone()))
    }

    /// Read the current INIT state at the configured depth.
    pub fn init_state(&self, min_confs: impl Into<Confirmations>) -> Result<InitState> {
        let result = self.read(min_confs)?;
        Ok(result.init)
    }

    /// Load one page of the append-only write history (SET/DEL + the genesis
    /// INIT) for this database, newest-first with in-flight writes pinned on
    /// top, optionally filtered to keys containing `filter` (case-insensitive
    /// substring).
    ///
    /// The bulk is paginated + filtered by SQLite over the snapshot's
    /// `kv_history`; the small live set (recent tail + `pending.toml`) is
    /// pinned above. `limit = None` returns every matching row (CLI /
    /// programmatic callers); `offset` skips the newest N. Each
    /// [`HistoryEntry`] carries the raw signed memo and a `verified` flag;
    /// [`HistoryResult::total`] is the full match count for pagination.
    ///
    /// `min_confs` controls visibility exactly as [`Database::read`] does.
    /// Does not sync first; call [`Database::sync`] beforehand for fresh
    /// state.
    pub fn history(
        &self,
        filter: Option<&str>,
        min_confs: impl Into<Confirmations>,
        limit: Option<u32>,
        offset: u32,
    ) -> Result<HistoryResult> {
        let min_confs = min_confs.into().as_u32();
        load_history_page(
            &self.name,
            min_confs,
            filter,
            None,
            HistoryOrder::Desc,
            limit,
            offset,
            None,
        )
        .map_err(ZkvError::Other)
    }

    /// Full classification audit of every memo addressed to this database,
    /// each tagged `Applied` / `Pending` / `Dropped(reason)`. Unlike
    /// [`Database::history`] (which pages the snapshot's *applied* writes),
    /// this re-derives from the whole memo set and surfaces the rows replay
    /// rejected (malformed, bad signature, unauthorized, wrong-network/foreign
    /// INIT, unsupported version, etc.) with a standardized [`DropReason`].
    ///
    /// `O(total writes)`; meant for an explicit audit / "rejections" view, not
    /// the hot read path. Does not sync first.
    pub fn audit(&self, min_confs: impl Into<Confirmations>) -> Result<AuditResult> {
        let min_confs = min_confs.into().as_u32();
        load_audit(&self.name, min_confs).map_err(ZkvError::Other)
    }

    /// Like [`Database::history`], but instead of an explicit `offset`, return
    /// the page of size `page_size` that **contains `txid`** in the newest-first
    /// ordering, with [`HistoryResult::offset`] set to that page's start. Lets a
    /// UI jump straight to a specific write while still showing it in full
    /// surrounding context. Falls back to the newest page when `txid` isn't
    /// found (e.g. a purely-pending write not yet in the audit log).
    pub fn history_at_txid(
        &self,
        filter: Option<&str>,
        min_confs: impl Into<Confirmations>,
        page_size: u32,
        txid: &str,
    ) -> Result<HistoryResult> {
        let min_confs = min_confs.into().as_u32();
        load_history_page(
            &self.name,
            min_confs,
            filter,
            None,
            HistoryOrder::Desc,
            Some(page_size),
            0,
            Some(txid),
        )
        .map_err(ZkvError::Other)
    }

    /// Load one page of the database's *funding* ledger: every non-zkv ZEC
    /// transfer in or out of the wallet (deposits and withdrawals), newest
    /// first with mempool transactions pinned on top. Each entry carries the
    /// signed value transferred (fee excluded), the memo, and (for sends)
    /// the external recipient address(es). The database's own ZKV0 memo writes
    /// (valid or not) are excluded, as are transactions that net to a bare fee.
    ///
    /// `limit = None` returns every matching transaction; `offset` skips the
    /// newest N. Admin-only ([`ZkvError::WatchOnly`] otherwise); the funding
    /// ledger is a property of the funded wallet. Does not sync first; call
    /// [`Database::sync`] beforehand for fresh state.
    pub fn funding(&self, limit: Option<u32>, offset: u32) -> Result<FundingResult> {
        self.require_admin()?;
        load_funding(&self.name, limit, offset).map_err(ZkvError::Other)
    }

    /// Broadcast the INIT memo for this admin database. Required once
    /// before any SET/DEL writes are accepted by readers.
    ///
    /// Wallet must be funded with at least the ZIP-317 fee floor
    /// (~5,000 zatoshi). Errors with [`ZkvError::WatchOnly`] on
    /// watch-only databases.
    pub async fn init(&self) -> Result<String> {
        self.require_admin()?;
        let engine = self.engine().await?;
        let out = broadcast_init(&self.name, engine)
            .await
            .map_err(map_write_error);
        out
    }

    /// Sync, sign, build, and broadcast a SET. Returns the broadcast
    /// txid. The wire form is picked automatically by
    /// [`Op::set_for_value`]: `SET` for ordinary values, `SETL` when
    /// the value is empty or contains a newline.
    pub async fn set(&self, key: &str, value: &str) -> Result<String> {
        self.do_write(false, Op::set_for_value(value), key, Some(value))
            .await
    }

    /// Like [`Database::set`] but skips the pre-broadcast sync; it still
    /// signs and broadcasts immediately, it just doesn't refresh the wallet
    /// first. For consumers that drive their own [`Database::sync`] cadence.
    pub async fn set_no_sync(&self, key: &str, value: &str) -> Result<String> {
        self.do_write(true, Op::set_for_value(value), key, Some(value))
            .await
    }

    /// Deprecated alias for [`Database::set_no_sync`]. "Offline" misleads:
    /// the write still broadcasts over the network; it only skips the
    /// pre-broadcast sync.
    #[deprecated(
        note = "renamed to `set_no_sync` (the write still broadcasts; it only skips the pre-broadcast sync)"
    )]
    pub async fn set_offline(&self, key: &str, value: &str) -> Result<String> {
        self.set_no_sync(key, value).await
    }

    /// Same as [`Database::set`] but for DEL.
    pub async fn del(&self, key: &str) -> Result<String> {
        self.do_write(false, Op::Del, key, None).await
    }

    /// Like [`Database::del`] but skips the pre-broadcast sync (it still
    /// broadcasts immediately; see [`Database::set_no_sync`]).
    pub async fn del_no_sync(&self, key: &str) -> Result<String> {
        self.do_write(true, Op::Del, key, None).await
    }

    /// Deprecated alias for [`Database::del_no_sync`]. "Offline" misleads:
    /// the write still broadcasts; it only skips the pre-broadcast sync.
    #[deprecated(
        note = "renamed to `del_no_sync` (the write still broadcasts; it only skips the pre-broadcast sync)"
    )]
    pub async fn del_offline(&self, key: &str) -> Result<String> {
        self.del_no_sync(key).await
    }

    /// Sync, then sign and broadcast many writes as ONE transaction (a
    /// "sendmany"): one ZIP-317 fee and one txid for the whole batch, instead
    /// of one tx per write. Returns the single broadcast txid.
    ///
    /// Ops are applied in order. Multiple ops on the same key in one batch get
    /// consecutive replay versions (the highest-versioned write wins, as usual).
    /// The fee scales with the number of outputs, so the wallet needs a note
    /// covering it; keep batches to a sane size (a few dozen outputs).
    ///
    /// ```no_run
    /// use zkv::{db::{Database, WriteOp}, remote::ConnectionArgs};
    /// # async fn run() -> Result<(), zkv::db::ZkvError> {
    /// let db = Database::open("zec-usd-oracle", ConnectionArgs::default())?;
    /// let txid = db.write_many(&[
    ///     WriteOp::set("zec_usd", "553.88"),
    ///     WriteOp::set("btc_usd", "67250.00"),
    /// ]).await?;
    /// eprintln!("published in one tx: {txid}");
    /// # Ok(())
    /// # }
    /// ```
    pub async fn write_many(&self, ops: &[WriteOp]) -> Result<String> {
        self.do_write_many(false, ops).await
    }

    /// Like [`Database::write_many`] but skips the pre-broadcast sync (it still
    /// broadcasts immediately; see [`Database::set_no_sync`]).
    pub async fn write_many_no_sync(&self, ops: &[WriteOp]) -> Result<String> {
        self.do_write_many(true, ops).await
    }

    /// Convenience over [`Database::write_many`]: SET several key/value pairs in
    /// one transaction. Returns the single broadcast txid.
    ///
    /// ```no_run
    /// use zkv::{db::Database, remote::ConnectionArgs};
    /// # async fn run() -> Result<(), zkv::db::ZkvError> {
    /// let db = Database::open("zec-usd-oracle", ConnectionArgs::default())?;
    /// let txid = db.set_many(&[("zec_usd", "553.88"), ("btc_usd", "67250.00")]).await?;
    /// eprintln!("published in one tx: {txid}");
    /// # Ok(())
    /// # }
    /// ```
    pub async fn set_many(&self, pairs: &[(&str, &str)]) -> Result<String> {
        let ops: Vec<WriteOp> = pairs.iter().map(|(k, v)| WriteOp::set(*k, *v)).collect();
        self.write_many(&ops).await
    }

    /// Like [`Database::set_many`] but skips the pre-broadcast sync.
    pub async fn set_many_no_sync(&self, pairs: &[(&str, &str)]) -> Result<String> {
        let ops: Vec<WriteOp> = pairs.iter().map(|(k, v)| WriteOp::set(*k, *v)).collect();
        self.write_many_no_sync(&ops).await
    }

    /// Send a plain ZEC value transfer to any Zcash address librustzcash
    /// supports (transparent, Sapling, unified, or TEX), distinct from a zkv
    /// write, which is a zero-value memo to this database's own address.
    /// `amount` is a decimal ZEC string (e.g. `"0.01"`). Validates the
    /// recipient against this database's network, syncs, then signs and
    /// broadcasts. Returns the txid.
    ///
    /// Returns [`ZkvError::WatchOnly`] for watch-only databases and
    /// [`ZkvError::InsufficientFunds`] when the wallet can't cover the amount
    /// plus fee; a bad address or amount surfaces as [`ZkvError::Other`].
    pub async fn send(&self, recipient: &str, amount: &str, memo: Option<&str>) -> Result<String> {
        self.send_inner(recipient, amount, memo, false).await
    }

    /// Like [`Database::send`] but skips the pre-broadcast sync. The transfer
    /// is still signed and broadcast immediately; only the wallet refresh is
    /// skipped, mirroring [`Database::set_no_sync`]. Use when you control sync
    /// timing.
    pub async fn send_no_sync(
        &self,
        recipient: &str,
        amount: &str,
        memo: Option<&str>,
    ) -> Result<String> {
        self.send_inner(recipient, amount, memo, true).await
    }

    async fn send_inner(
        &self,
        recipient: &str,
        amount: &str,
        memo: Option<&str>,
        no_sync: bool,
    ) -> Result<String> {
        self.require_admin()?;
        let amount = crate::internal::send::parse_zec(amount)
            .map_err(|m| ZkvError::Other(anyhow::anyhow!(m)))?;
        crate::internal::send::send_funds(
            &self.name,
            self.engine().await?,
            recipient,
            amount,
            memo,
            no_sync,
        )
        .await
        .map_err(map_write_error)
    }

    /// Validate a recipient address for this database's network without
    /// sending, returning a short label for the address kind on success
    /// (e.g. `"unified"`) or a friendly reason on failure. Backs the GUI Send
    /// flow's live "is this a valid address" check. Pure: no network, no
    /// spending key required (so it works on watch-only databases too).
    pub fn validate_recipient(&self, recipient: &str) -> std::result::Result<String, String> {
        crate::internal::send::validate_recipient(recipient, self.cfg.network)
    }

    /// Like [`Database::validate_recipient`] but returns the address's network
    /// and shielded pool alongside its kind, for a richer Send-flow hint.
    pub fn describe_recipient(
        &self,
        recipient: &str,
    ) -> std::result::Result<crate::internal::send::RecipientInfo, String> {
        crate::internal::send::describe_recipient(recipient, self.cfg.network)
    }

    /// Sign + build a SET memo into a [`PreparedWrite`] without
    /// broadcasting. Useful for cold-wallet flows where signing and
    /// broadcasting happen on separate machines. The wire form is
    /// picked via [`Op::set_for_value`].
    pub fn prepare_set(&self, key: &str, value: &str) -> Result<PreparedWrite> {
        self.require_admin()?;
        prepare(&self.name, Op::set_for_value(value), key, Some(value)).map_err(map_write_error)
    }

    /// Same as [`Database::prepare_set`] but always uses the length-framed
    /// `SETL` wire form (rather than letting [`Op::set_for_value`] pick). The
    /// GUI Reference builder uses this for the dedicated SETL opcode page.
    pub fn prepare_setl(&self, key: &str, value: &str) -> Result<PreparedWrite> {
        self.require_admin()?;
        prepare(&self.name, Op::SetL, key, Some(value)).map_err(map_write_error)
    }

    /// Same as [`Database::prepare_set`] but for DEL.
    pub fn prepare_del(&self, key: &str) -> Result<PreparedWrite> {
        self.require_admin()?;
        prepare(&self.name, Op::Del, key, None).map_err(map_write_error)
    }

    /// Sign + build the INIT memo into a [`PreparedWrite`] without
    /// broadcasting.
    pub fn prepare_init(&self) -> Result<PreparedWrite> {
        self.require_admin()?;
        prepare_init(&self.name).map_err(ZkvError::Other)
    }

    /// Read the owner/writer authorization registry at the requested depth.
    ///
    /// The registry is the confirmed projection of every `OWNER*`/`WRITER*`
    /// management memo, seeded with the root (UFVK-derived) signer as owner #1
    /// when INIT confirmed. Use it to answer "who can write to this database,
    /// and with what scope? See [`AuthRegistry`], [`Authority`], [`Scope`].
    pub fn roles(&self, min_confs: impl Into<Confirmations>) -> Result<AuthRegistry> {
        Ok(self.read(min_confs)?.auth)
    }

    /// The database's required protocol version and the capabilities an
    /// under-versioned client must give up, projected from `VERSION` memos at
    /// the requested depth. Pair with [`require_supported`] /
    /// [`VersionState::upgrade_warning`] to gate or warn. Does not sync first.
    pub fn version(&self, min_confs: impl Into<Confirmations>) -> Result<VersionState> {
        Ok(self.read(min_confs)?.version)
    }

    /// The tombstones of the authorization registry: pubkeys that once held
    /// owner/writer authority and were since revoked (and not re-granted),
    /// each with its revocation provenance (when, and the owner who signed
    /// the revoking `OWNERDEL`/`WRITERDEL`).
    ///
    /// Complements [`Database::roles`] (the *current* registry): a pubkey is
    /// either current there or revoked here. Derived from a full audit re-scan
    /// (`O(total writes)`), so it shares the audit's cost; not suitable for
    /// the hot read path. Does not sync first.
    pub fn revoked_roles(&self, min_confs: impl Into<Confirmations>) -> Result<Vec<RevokedRole>> {
        Ok(crate::protocol::revoked_roles(&self.audit(min_confs)?))
    }

    /// Grant (or re-affirm) owner authority to `pubkey` (a zkvid1… key or hex).
    /// Owner-only; broadcast by this database's signing key. Returns the txid.
    pub async fn grant_owner(&self, pubkey: &str) -> Result<String> {
        self.do_manage(false, Op::OwnerAdd, pubkey, None).await
    }

    /// Revoke owner authority from `pubkey`. Owner-only. The last remaining
    /// owner cannot be removed (readers enforce this during replay).
    pub async fn revoke_owner(&self, pubkey: &str) -> Result<String> {
        self.do_manage(false, Op::OwnerDel, pubkey, None).await
    }

    /// Grant (or overwrite) a scoped writer. `scope` is the capability set;
    /// a later call replaces it wholesale. Owner-only. Returns the txid.
    pub async fn grant_writer(&self, pubkey: &str, scope: &Scope) -> Result<String> {
        self.do_manage(false, Op::WriterAdd, pubkey, Some(scope.to_wire()))
            .await
    }

    /// Revoke a writer entirely. Owner-only. Returns the txid.
    pub async fn revoke_writer(&self, pubkey: &str) -> Result<String> {
        self.do_manage(false, Op::WriterDel, pubkey, None).await
    }

    /// Like [`Database::grant_owner`] but skips the pre-broadcast sync, for
    /// consumers that drive their own [`Database::sync`] cadence. It still
    /// broadcasts immediately; the wallet must already hold spendable funds
    /// for the fee.
    pub async fn grant_owner_no_sync(&self, pubkey: &str) -> Result<String> {
        self.do_manage(true, Op::OwnerAdd, pubkey, None).await
    }

    /// Like [`Database::revoke_owner`] but skips the pre-broadcast sync.
    pub async fn revoke_owner_no_sync(&self, pubkey: &str) -> Result<String> {
        self.do_manage(true, Op::OwnerDel, pubkey, None).await
    }

    /// Like [`Database::grant_writer`] but skips the pre-broadcast sync.
    pub async fn grant_writer_no_sync(&self, pubkey: &str, scope: &Scope) -> Result<String> {
        self.do_manage(true, Op::WriterAdd, pubkey, Some(scope.to_wire()))
            .await
    }

    /// Like [`Database::revoke_writer`] but skips the pre-broadcast sync.
    pub async fn revoke_writer_no_sync(&self, pubkey: &str) -> Result<String> {
        self.do_manage(true, Op::WriterDel, pubkey, None).await
    }

    /// Permanently seal the database: once this FINALIZE confirms, no further
    /// writes of any kind are possible (reads still work). Owner-only; broadcast
    /// by this database's signing key. One-way and irreversible. Returns the
    /// txid.
    pub async fn finalize(&self) -> Result<String> {
        self.do_manage(false, Op::Finalize, "", None).await
    }

    /// Like [`Database::finalize`] but skips the pre-broadcast sync, for
    /// consumers driving their own [`Database::sync`] cadence. Still broadcasts
    /// immediately; the wallet must already hold spendable funds for the fee.
    pub async fn finalize_no_sync(&self) -> Result<String> {
        self.do_manage(true, Op::Finalize, "", None).await
    }

    /// Whether a `FINALIZE` has been confirmed at `min_confs`, sealing the
    /// database against all further writes. Pure-local like the other reads;
    /// reflects the last [`Database::sync`].
    pub fn is_finalized(&self, min_confs: impl Into<Confirmations>) -> Result<bool> {
        Ok(self.read(min_confs)?.finalized)
    }

    /// Sign + build a management memo into a [`PreparedWrite`] without
    /// broadcasting (cold-wallet / relay flows).
    pub fn prepare_management(
        &self,
        op: Op,
        target: &str,
        scope: Option<&Scope>,
    ) -> Result<PreparedWrite> {
        self.require_admin()?;
        let scope_str = scope.map(Scope::to_wire);
        prepare_management(&self.name, op, target, scope_str.as_deref()).map_err(map_write_error)
    }

    async fn do_manage(
        &self,
        no_sync: bool,
        op: Op,
        target: &str,
        scope: Option<String>,
    ) -> Result<String> {
        self.require_admin()?;
        let engine = self.engine().await?;
        let out = manage_and_broadcast(&self.name, engine, no_sync, op, target, scope.as_deref())
            .await
            .map_err(map_write_error);
        out
    }

    async fn do_write(
        &self,
        no_sync: bool,
        op: Op,
        key: &str,
        value: Option<&str>,
    ) -> Result<String> {
        self.require_admin()?;
        let engine = self.engine().await?;
        let out = write_and_broadcast(&self.name, engine, no_sync, op, key, value)
            .await
            .map_err(map_write_error);
        out
    }

    async fn do_write_many(&self, no_sync: bool, ops: &[WriteOp]) -> Result<String> {
        self.require_admin()?;
        if ops.is_empty() {
            return Err(ZkvError::Other(anyhow::anyhow!(
                "write_many: empty batch (nothing to write)"
            )));
        }
        // Resolve each public WriteOp to an internal BatchItem, picking the SET
        // wire form per value (matching Database::set).
        let items: Vec<BatchItem> = ops
            .iter()
            .map(|op| match op {
                WriteOp::Set { key, value } => BatchItem {
                    op: Op::set_for_value(value),
                    key,
                    value: Some(value),
                },
                WriteOp::Del { key } => BatchItem {
                    op: Op::Del,
                    key,
                    value: None,
                },
            })
            .collect();
        let engine = self.engine().await?;
        let out = write_many_and_broadcast(&self.name, engine, no_sync, &items)
            .await
            .map_err(map_write_error);
        out
    }

    /// Sync, then broadcast a transaction request prepared and signed
    /// elsewhere, returning the txid.
    ///
    /// For a relay rather than a writer: the faucet broadcasts INIT memos its
    /// users signed, paying the Zcash fee on their behalf, so the signature
    /// and the funds come from different wallets. Everything above this is the
    /// caller's; this handle only supplies the money and the transport.
    ///
    /// Syncs first for the same reason every write does. The node holds its
    /// datadir lock across both, so the spend is built against the tree the
    /// sync just produced and no other scan or spend can interleave. Skipping
    /// that is what produced consensus rejections when a spend re-selected a
    /// note the chain already considered spent.
    ///
    /// Nothing is recorded in `pending.toml`: the memo belongs to the
    /// requester's database, not this one.
    pub async fn broadcast_request(&self, request: zip321::TransactionRequest) -> Result<String> {
        self.require_admin()?;
        let engine = self.engine().await?;
        engine.sync_to_tip(None).await.map_err(map_engine_error)?;
        engine.ship(request).await.map_err(|e| {
            map_write_error(crate::internal::write::insufficient_from_engine(
                e, &self.name,
            ))
        })
    }

    /// Drive the shared scan for a database that is converting to it, and flip
    /// over when it has caught up.
    ///
    /// While converting, a database reads from its own files, so nothing here
    /// affects the sync that just happened. What it does is make the shard
    /// progress even for a user who only ever runs one-shot CLI commands: the
    /// GUI keeps a fleet node running continuously, but `zkv get` would
    /// otherwise never start one, and the conversion would never finish.
    ///
    /// Bounded, because this is somebody's `zkv get`. A shard rescanning from a
    /// year-old birthday takes hours; waiting for it here would turn every read
    /// into that. So it contributes a slice and returns, and the next command
    /// contributes another.
    ///
    /// Best effort throughout: a fleet node that will not start, or a shard
    /// that is behind, leaves the database exactly where it was, still reading
    /// correctly from its own files.
    async fn drive_pending_fleet(&self, own_scanned: u32) {
        use crate::config::WalletEngine;

        if self.cfg.engine != WalletEngine::FleetPending {
            return;
        }
        let Ok(fleet) = crate::engine::shared(self.cfg.network, &self.conn) else {
            return;
        };
        // The member may not be served yet: zkv wrote the manifest, and a node
        // that was already running has not re-read the directory. Asking it to
        // load one it already has is harmless.
        let _ = fleet.load_member(&self.name).await;

        let shard =
            match tokio::time::timeout(FLEET_NUDGE, fleet.sync_wallet(&self.name, None)).await {
                Ok(Ok(out)) => out.scanned_height,
                // Either the slice expired or the shard is not ready. Both mean
                // "not this time", and the next command tries again.
                _ => return,
            };

        let mut cfg = match WalletConfig::read(&self.name) {
            Ok(c) => c,
            Err(_) => return,
        };
        match crate::fleet::flip_if_caught_up(&mut cfg, &self.name, own_scanned, shard) {
            // The flip rewrites `keys.toml`; this handle keeps reading from the
            // old files, which are gone, only if it is reused. It is not: the
            // conversion completes for the *next* `Database::open`, which is
            // the boundary where the engine is chosen.
            Ok(true) => {}
            Ok(false) => {}
            Err(e) => tracing::warn!("completing the shared-scan conversion: {e:#}"),
        }
    }

    /// Skip a database another process is using instead of waiting for it.
    /// Skip a database another process is using instead of waiting for it.
    ///
    /// For a background refresh loop: stalling a worker thread on a database
    /// some other zkv process holds is worse than coming back next cycle, and
    /// the caller reports it as in-use either way.
    pub fn skip_if_busy(mut self) -> Self {
        self.busy_wait = None;
        self
    }

    /// This database's wallet engine, started on first use.
    ///
    /// The engine itself is inert until something needs the chain, so this is
    /// cheap; the node behind it starts on the first operation that syncs or
    /// sends, and lives until the handle is dropped. That is what lets a
    /// caller doing several writes pay one node startup rather than one each.
    async fn engine(&self) -> Result<&crate::engine::EngineRef> {
        self.engine
            .get_or_try_init(|| async {
                // Re-read rather than reuse `self.cfg`: enrolling in the shared
                // scan rewrites `keys.toml`, and this is the first thing that
                // runs afterwards.
                let cfg = WalletConfig::read(&self.name).map_err(ZkvError::Other)?;
                crate::engine::engine_for(&self.name, cfg, &self.conn, self.busy_wait)
                    .map_err(map_engine_error)
            })
            .await
    }

    fn require_admin(&self) -> Result<()> {
        if self.cfg.role != Role::Admin {
            return Err(ZkvError::WatchOnly);
        }
        Ok(())
    }
}

/// Install a sensible default `tracing` subscriber that writes to
/// stderr at the level configured by `RUST_LOG` (default `zkv=info`).
///
/// No-ops if a global subscriber is already installed. Call once at
/// program startup if your application doesn't otherwise wire up
/// `tracing-subscriber`; without a subscriber, `zkv`'s sync progress
/// is silently dropped and a fresh wallet sync looks like a hang.
#[cfg(feature = "default-subscriber")]
pub fn install_default_subscriber() {
    use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Layer};

    let filter = std::env::var("RUST_LOG")
        .ok()
        .and_then(|s| EnvFilter::try_new(s).ok())
        .unwrap_or_else(|| EnvFilter::new("zkv=info"));
    // Colour the logs to match the stderr terminal/ANSI decision the status
    // lines use; on Windows this also enables Virtual Terminal processing so
    // the console renders escape codes instead of printing them literally.
    let layer = tracing_subscriber::fmt::layer()
        .with_ansi(crate::ui::color_enabled())
        .with_writer(std::io::stderr)
        .with_filter(filter);
    let subscriber = tracing_subscriber::registry().with(layer);
    let _ = tracing::subscriber::set_global_default(subscriber);
}

fn classify_open_error(e: anyhow::Error, db_name: &str) -> ZkvError {
    if crate::data::is_import_pending(&e) {
        return ZkvError::Importing;
    }
    let msg = format!("{e:#}");
    if msg.contains("no database named") || msg.contains("no keys.toml") {
        ZkvError::UnknownDatabase(db_name.to_owned())
    } else {
        ZkvError::Other(e)
    }
}

/// Map a sync failure onto the facade's vocabulary.
///
/// Only one case needs naming: a member of the shared scan whose account is not
/// imported yet is [`ZkvError::Importing`], a state that resolves itself, not
/// the anonymous `Other` a caller can only report as a failure. Everything else
/// stays whole, since the engine's own messages name the database and the
/// remedy.
fn classify_sync_error(e: anyhow::Error) -> ZkvError {
    if crate::data::is_import_pending(&e) {
        return ZkvError::Importing;
    }
    ZkvError::Other(e)
}

/// Map a write-path failure to a structured [`ZkvError`].
///
/// The [`internal::write`](crate::internal) layer attaches a typed
/// [`WriteError`] as the source of its `anyhow::Error`, so the facade recovers
/// the structured form by downcasting rather than parsing message text (any
/// other error falls through to [`ZkvError::Other`]). The `ClientUpgradeRequired`
/// operation is always [`GatedOp::Write`]; the write path is the only place
/// that blocks on the version gate.
/// Map an engine failure onto the facade's vocabulary.
///
/// Only the cases the facade already has words for are translated; everything
/// else stays whole as `Other`, since the engine's messages already name the
/// database and the remedy.
/// Whether a sync error is the cooperative stop the caller asked for.
///
/// `read_sync` hands back `anyhow::Error`, so the typed variant has to be
/// recovered by downcast rather than matched.
fn is_cancelled(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<crate::engine::EngineError>(),
        Some(crate::engine::EngineError::Cancelled)
    )
}

pub(crate) fn map_engine_error(e: crate::engine::EngineError) -> ZkvError {
    use crate::engine::EngineError;
    match e {
        // Another process holds the database. That is the same condition the
        // legacy path surfaced by blocking on `DbLock`, so it keeps the same
        // shape rather than becoming a new variant.
        EngineError::Busy(db) => ZkvError::Other(anyhow::anyhow!(
            "database {db:?} is in use by another process"
        )),
        // The shared scan's two import states. Both used to be inferred from
        // the absence of an account, which could not tell them apart; the node
        // publishes them now.
        EngineError::NotImported(_) => ZkvError::Importing,
        EngineError::ImportFailed { reason, .. } => ZkvError::ImportFailed(reason),
        other => ZkvError::Other(other.into()),
    }
}

fn map_write_error(e: anyhow::Error) -> ZkvError {
    let Some(we) = e.downcast_ref::<WriteError>() else {
        return ZkvError::Other(e);
    };
    match we {
        WriteError::WatchOnly { .. } => ZkvError::WatchOnly,
        WriteError::UnauthorizedData { .. } | WriteError::OwnerOnly { .. } => {
            ZkvError::Unauthorized(we.to_string())
        }
        WriteError::NotInitialized { .. } => ZkvError::NotInitialized,
        WriteError::Initializing { done, required, .. } => ZkvError::Initializing {
            done: *done,
            required: *required,
        },
        WriteError::ClientUpgradeRequired {
            required,
            supported,
        } => ZkvError::ClientUpgradeRequired {
            required: *required,
            supported: *supported,
            operation: GatedOp::Write,
        },
        WriteError::InsufficientFunds {
            available,
            required,
            pending,
            ..
        } => ZkvError::InsufficientFunds {
            available: *available,
            required: *required,
            pending: *pending,
        },
    }
}

/// A database's canonical on-chain identity: its [`receiver_domain`]
/// (`"<network>:<receiver_hex>"`), derived from the UFVK, pool, and network.
/// Two databases sharing this identity are the *same* database (same seed,
/// same pool, same network), even if imported under different local names or
/// with different birthdays (the birthday and UFVK string-encoding are not
/// part of the identity).
fn database_identity(
    ufvk: &UnifiedFullViewingKey,
    pool: ShieldedPool,
    network: Network,
) -> Result<String> {
    use zcash_protocol::consensus::Parameters as _;
    receiver_domain(ufvk, pool, network.network_type()).map_err(ZkvError::Other)
}

/// Scan every existing local database and return the name of any whose
/// canonical identity matches the given UFVK + pool + network: the same
/// database already imported under a (possibly different) name. Returns
/// `None` if there is no collision.
fn find_duplicate_by_ufvk(
    ufvk: &UnifiedFullViewingKey,
    pool: ShieldedPool,
    network: Network,
) -> Result<Option<String>> {
    let identity = database_identity(ufvk, pool, network)?;
    for name in crate::data::list_dbs().map_err(ZkvError::Other)? {
        // A database whose config or wallet can't be read can't be the
        // collision we're guarding against; skip it rather than failing the
        // whole import.
        let Ok(cfg) = WalletConfig::read(&name) else {
            continue;
        };
        let Ok(keys) = account_keys(&cfg, &name) else {
            continue;
        };
        if keys.receiver_hex == identity {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// Look for an already-imported local database that shares the identity of the
/// admin database described by `recovery_phrase` (same seed, pool, and
/// network). Returns the colliding database's name, or `None`.
///
/// Used by the seed-import paths (CLI `zkv restore`, [`Database::restore_admin`]
/// and the GUI's create/restore) to refuse importing the same database twice
/// under a different name.
pub fn find_duplicate_database(
    recovery_phrase: &str,
    pool: ShieldedPool,
    network: Network,
) -> Result<Option<String>> {
    use bip0039::{English, Mnemonic};
    use secrecy::Zeroize as _;
    use zcash_keys::keys::UnifiedSpendingKey;
    use zip32::AccountId;

    let mut normalized = recovery_phrase
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mnemonic: Mnemonic<English> = Mnemonic::from_phrase(&normalized)
        .map_err(|e| ZkvError::Other(anyhow::anyhow!("invalid recovery phrase: {e}")))?;
    // The normalized phrase is a seed equivalent; wipe the owned copy.
    normalized.zeroize();
    let mut seed = mnemonic.to_seed("");
    let usk = UnifiedSpendingKey::from_seed(&network, &seed, AccountId::ZERO);
    seed.zeroize();
    let ufvk = usk
        .map_err(|e| ZkvError::Other(anyhow::anyhow!("derive key from phrase: {e}")))?
        .to_unified_full_viewing_key();
    find_duplicate_by_ufvk(&ufvk, pool, network)
}

/// Look for an already-imported local database that shares the identity of the
/// watch-only database described by `zkv_address` (same UFVK, pool, and
/// network). Returns the colliding database's name, or `None`.
///
/// Used by the address-import paths (CLI `zkv watch`, [`Database::init_watch`])
/// to refuse importing the same database twice under a different name.
pub fn find_duplicate_watch_database(zkv_address: &str) -> Result<Option<String>> {
    let parsed = parse_zkv_addr(zkv_address).map_err(ZkvError::Other)?;
    let network = network_from_type(parsed.network).map_err(ZkvError::Other)?;
    find_duplicate_by_ufvk(&parsed.ufvk, parsed.pool, network)
}

/// Reject a pool that isn't available on the target network. Ironwood (NU6.3)
/// is live on every network since mainnet activation (height 3_428_143,
/// 2026-07-28), so today this never fires; the guard stays so the CLI, GUI,
/// and library keep enforcing the policy from one place.
fn ensure_pool_available(pool: ShieldedPool, network: Network) -> Result<()> {
    if pool == ShieldedPool::Ironwood && !crate::config::ironwood_available(network) {
        return Err(ZkvError::Other(anyhow::anyhow!(
            "the Ironwood pool is not available on this network; create the database \
             with the Orchard pool instead"
        )));
    }
    Ok(())
}

/// Reject pools a brand-new database can't be created with: the legacy
/// Orchard label is import-only (it shares the Ironwood receiver, so new
/// databases use Ironwood; restore/watch paths still accept Orchard for
/// wallets originally created with it). Guards the creation facade so the
/// CLI, GUI, and library all enforce the same policy.
fn ensure_pool_creatable(pool: ShieldedPool) -> Result<()> {
    if pool == ShieldedPool::Orchard {
        return Err(ZkvError::Other(anyhow::anyhow!(
            "new databases cannot use the legacy Orchard pool; use Ironwood (the same \
             pool under NU6.3, identical receiver). Orchard remains available when \
             restoring an existing wallet"
        )));
    }
    Ok(())
}

async fn create_admin(
    name: &str,
    network: Network,
    mnemonic: &bip0039::Mnemonic,
    birthday: Option<u32>,
    pool: ShieldedPool,
    conn: ConnectionArgs,
) -> Result<Database> {
    use secrecy::{SecretVec, Zeroize};
    use zcash_client_backend::data_api::WalletWrite;

    let params = network;
    let dir = db_dir(name).map_err(ZkvError::Other)?;
    if dir.join("keys.toml").exists() {
        return Err(ZkvError::Other(anyhow::anyhow!(
            "database {name:?} already exists at {}",
            dir.display()
        )));
    }

    // Refuse to import the same database (same seed, pool, network) twice under
    // a different name. Checked before any network round-trip so it fails fast.
    let ufvk = {
        use secrecy::Zeroize as _;
        use zcash_keys::keys::UnifiedSpendingKey;
        use zip32::AccountId;
        let mut seed = mnemonic.to_seed("");
        let usk = UnifiedSpendingKey::from_seed(&params, &seed, AccountId::ZERO);
        seed.zeroize();
        usk.map_err(|e| ZkvError::Other(anyhow::anyhow!("derive key from phrase: {e}")))?
            .to_unified_full_viewing_key()
    };
    if let Some(existing) = find_duplicate_by_ufvk(&ufvk, pool, params)? {
        return Err(ZkvError::Other(anyhow::anyhow!(
            "this database is already imported as {existing:?}; \
             switch to it instead of importing it again"
        )));
    }

    // Refuse to pin a birthday against a stale/unreachable tip. Honor an
    // explicit `birthday` verbatim; otherwise default to tip − safety buffer.
    let birthday_acct = match birthday {
        Some(height) => crate::internal::sync::pinned_birthday(&conn, params, height).await?,
        None => crate::internal::sync::near_tip_birthday(&conn, params).await?,
    };

    WalletConfig::init_admin(name, mnemonic, birthday_acct.height(), params, pool)
        .map_err(ZkvError::Other)?;

    let seed = {
        let mut s = mnemonic.to_seed("");
        let secret = s.to_vec();
        s.zeroize();
        SecretVec::new(secret)
    };
    let mut db_data = init_dbs(params, name).map_err(ZkvError::Other)?;
    db_data
        .create_account(name, &seed, &birthday_acct, None)
        .map_err(|e| ZkvError::Other(anyhow::anyhow!("{e:?}")))?;
    drop(db_data);

    crate::demo::promote_current(name).map_err(ZkvError::Other)?;

    // Sanity-check the verifying pubkey can be derived. Surfaces
    // misconfiguration as a structured error before the first sync.
    let cfg = WalletConfig::read(name).map_err(ZkvError::Other)?;
    let _ = account_keys(&cfg, name).map_err(ZkvError::Other)?;

    Database::open(name, conn)
}

/// Merge `pending.toml` entries that the wallet DB hasn't surfaced yet,
/// so a `set()` followed by an immediate `read()` shows the in-flight op.
/// Mirrors the logic in `crates/zkv/src/commands/get.rs`.
fn merge_pending(db_name: &str, result: &mut ReplayResult, min_confs: u32) {
    let local_pending = match pending::load(db_name) {
        Ok(p) => p,
        Err(_) => return,
    };
    let local_txids: HashSet<String> = local_pending.iter().map(|e| e.txid.clone()).collect();

    // A locally-broadcast INIT the wallet DB hasn't indexed yet still means
    // the database is initializing. Surface that so callers (and the GUI)
    // treat it as in-flight and don't offer a second INIT broadcast.
    if matches!(result.init, InitState::Uninitialized)
        && local_pending.iter().any(|e| e.op == "INIT")
    {
        result.init = InitState::Initializing {
            done: 0,
            required: min_confs.max(1),
        };
    }

    if min_confs >= 1 {
        for ks in result.state.values_mut() {
            ks.pending
                .retain(|op| op.done() >= 1 || local_txids.contains(op.txid()));
        }
        result.state.retain(|_, ks| {
            ks.confirmed.is_some()
                || ks
                    .pending
                    .iter()
                    .any(|op| matches!(op, PendingOp::Set { .. }))
        });
    }

    let seen_txids: HashSet<String> = result
        .state
        .values()
        .flat_map(|ks| ks.pending.iter().map(|op| op.txid().to_owned()))
        .collect();
    for entry in &local_pending {
        if entry.op == "INIT" || seen_txids.contains(&entry.txid) {
            continue;
        }
        // Compute the synthesized op *before* touching `state`, so a
        // non-data op (INIT handled above, or an OWNER*/WRITER* management
        // memo whose "key" is a pubkey) never inserts a phantom key.
        let op = match entry.op.as_str() {
            // "SET" and "SETL" are the two wire encodings of the same
            // semantic op; pending state doesn't care which was used.
            "SET" | "SETL" => PendingOp::Set {
                value: entry.value.clone().unwrap_or_default(),
                done: 0,
                required: min_confs.max(1),
                txid: entry.txid.clone(),
            },
            "DEL" => PendingOp::Del {
                done: 0,
                required: min_confs.max(1),
                txid: entry.txid.clone(),
            },
            // Management ops (OWNERADD/OWNERDEL/WRITERADD/WRITERDEL) confer
            // no per-key pending state; their effect only shows once
            // confirmed, via the registry.
            _ => continue,
        };
        result
            .state
            .entry(entry.key.clone())
            .or_default()
            .pending
            .push(op);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(n: u32, flags: &str) -> VersionState {
        VersionState {
            version: n,
            blocks: BlockSet::parse(flags).unwrap(),
        }
    }

    #[test]
    fn require_supported_allows_when_at_or_below_max() {
        // Even with every block flag set, a version this build supports passes.
        let v = version(MAX_DB_VERSION, "blockall");
        for op in [GatedOp::Sync, GatedOp::Read, GatedOp::Write] {
            assert!(require_supported(&v, op).is_ok());
        }
    }

    #[test]
    fn require_supported_gates_only_flagged_ops_when_outdated() {
        // Newer database that blocks only writes: reads/syncs still allowed.
        let v = version(MAX_DB_VERSION + 1, "blockwrite");
        assert!(require_supported(&v, GatedOp::Read).is_ok());
        assert!(require_supported(&v, GatedOp::Sync).is_ok());
        match require_supported(&v, GatedOp::Write) {
            Err(ZkvError::ClientUpgradeRequired {
                required,
                supported,
                operation,
            }) => {
                assert_eq!(required, MAX_DB_VERSION + 1);
                assert_eq!(supported, MAX_DB_VERSION);
                assert_eq!(operation, GatedOp::Write);
            }
            other => panic!("expected ClientUpgradeRequired, got {other:?}"),
        }
    }

    #[test]
    fn require_supported_blockall_gates_everything_when_outdated() {
        let v = version(MAX_DB_VERSION + 1, "blockall");
        for op in [GatedOp::Sync, GatedOp::Read, GatedOp::Write] {
            assert!(matches!(
                require_supported(&v, op),
                Err(ZkvError::ClientUpgradeRequired { .. })
            ));
        }
    }

    #[test]
    fn require_supported_warn_only_never_gates() {
        // Outdated but warn-only (empty block set): nothing is gated.
        let v = version(MAX_DB_VERSION + 1, "warn");
        assert!(v.upgrade_warning().is_some());
        for op in [GatedOp::Sync, GatedOp::Read, GatedOp::Write] {
            assert!(require_supported(&v, op).is_ok());
        }
    }

    fn ufvk_for(seed: &[u8], network: Network) -> UnifiedFullViewingKey {
        use zcash_keys::keys::UnifiedSpendingKey;
        UnifiedSpendingKey::from_seed(&network, seed, zip32::AccountId::ZERO)
            .unwrap()
            .to_unified_full_viewing_key()
    }

    #[test]
    fn database_identity_distinguishes_seed_pool_and_network() {
        // The identity is what the duplicate-import guard compares on. It must
        // be stable for the same (seed, pool, network) and differ when any of
        // them changes, so two genuinely-different databases never collide and
        // a re-import of the same one always does.
        let main = Network::Main;
        let test = Network::Test;
        let seed_a = [0x11u8; 32];
        let seed_b = [0x22u8; 32];

        let ufvk_a = ufvk_for(&seed_a, main);
        let id = |u: &UnifiedFullViewingKey, p, n| database_identity(u, p, n).unwrap();

        // Same seed, pool, network: identical identity (the re-import case).
        assert_eq!(
            id(&ufvk_a, ShieldedPool::Orchard, main),
            id(&ufvk_for(&seed_a, main), ShieldedPool::Orchard, main),
        );
        // Different seed: different identity.
        assert_ne!(
            id(&ufvk_a, ShieldedPool::Orchard, main),
            id(&ufvk_for(&seed_b, main), ShieldedPool::Orchard, main),
        );
        // Same seed, different pool: different identity.
        assert_ne!(
            id(&ufvk_a, ShieldedPool::Orchard, main),
            id(&ufvk_a, ShieldedPool::Sapling, main),
        );
        // Same seed, different network: different identity.
        assert_ne!(
            id(&ufvk_for(&seed_a, main), ShieldedPool::Orchard, main),
            id(&ufvk_for(&seed_a, test), ShieldedPool::Orchard, test),
        );
    }

    // The facade recovers structured errors by downcasting the typed
    // `WriteError` the `internal::write` / funding paths attach; no message
    // parsing. The matching producer-side tests (that those paths actually emit
    // these variants) live in `internal::write`.

    #[test]
    fn map_client_upgrade_to_typed_error() {
        let e = anyhow::Error::new(WriteError::ClientUpgradeRequired {
            required: MAX_DB_VERSION + 1,
            supported: MAX_DB_VERSION,
        });
        match map_write_error(e) {
            ZkvError::ClientUpgradeRequired {
                required,
                supported,
                operation,
            } => {
                assert_eq!(required, MAX_DB_VERSION + 1);
                assert_eq!(supported, MAX_DB_VERSION);
                assert_eq!(operation, GatedOp::Write);
            }
            other => panic!("expected ClientUpgradeRequired, got {other:?}"),
        }
    }

    /// A cancellation must be recognisable through the `anyhow` wrapper, so
    /// the facade can honour its documented Ok-on-cancel contract.
    ///
    /// The predicate is what keeps a deliberate GUI pause from counting as a
    /// sync-error strike, and it has to survive the `anyhow` boxing that
    /// `read_sync` does. A plain error, and any other engine error, must still
    /// read as a real failure: swallowing those would hide a broken sync
    /// behind a "you cancelled" that nobody asked for.
    #[test]
    fn only_a_cancellation_reads_as_cancelled() {
        let cancelled: anyhow::Error = crate::engine::EngineError::Cancelled.into();
        assert!(is_cancelled(&cancelled));

        // Wrapped in more context, as a caller adding its own would leave it.
        let with_context = cancelled.context("while syncing");
        assert!(
            is_cancelled(&with_context),
            "a cancellation must stay recognisable under added context",
        );

        for other in [
            anyhow::Error::from(crate::engine::EngineError::Busy("db".to_owned())),
            anyhow::Error::from(crate::engine::EngineError::Unsupported("socks".to_owned())),
            anyhow::anyhow!("transport error"),
        ] {
            assert!(
                !is_cancelled(&other),
                "a real failure must not be mistaken for a cancellation: {other:#}",
            );
        }
    }

    #[test]
    fn map_insufficient_funds_preserves_amounts() {
        let e = anyhow::Error::new(WriteError::InsufficientFunds {
            available: 100,
            required: 15000,
            pending: 2000,
            network: Network::Test,
        });
        match map_write_error(e) {
            ZkvError::InsufficientFunds {
                available,
                required,
                pending,
            } => {
                assert_eq!(available, 100);
                assert_eq!(required, 15000);
                assert_eq!(pending, 2000);
            }
            other => panic!("expected InsufficientFunds, got {other:?}"),
        }
    }

    #[test]
    fn map_initializing_and_not_initialized() {
        let initializing = anyhow::Error::new(WriteError::Initializing {
            db: "demo".to_owned(),
            done: 1,
            required: 3,
        });
        match map_write_error(initializing) {
            ZkvError::Initializing { done, required } => {
                assert_eq!(done, 1);
                assert_eq!(required, 3);
            }
            other => panic!("expected Initializing, got {other:?}"),
        }
        let uninit = anyhow::Error::new(WriteError::NotInitialized {
            db: "demo".to_owned(),
        });
        assert!(matches!(map_write_error(uninit), ZkvError::NotInitialized));
    }

    #[test]
    fn map_watch_only_and_unauthorized() {
        let watch = anyhow::Error::new(WriteError::WatchOnly {
            db: "demo".to_owned(),
        });
        assert!(matches!(map_write_error(watch), ZkvError::WatchOnly));

        let unauth = anyhow::Error::new(WriteError::UnauthorizedData {
            op: "SET".to_owned(),
            key: "k".to_owned(),
        });
        assert!(matches!(map_write_error(unauth), ZkvError::Unauthorized(_)));

        let owner_only = anyhow::Error::new(WriteError::OwnerOnly {
            op: "OWNERADD".to_owned(),
        });
        assert!(matches!(
            map_write_error(owner_only),
            ZkvError::Unauthorized(_)
        ));
    }

    #[test]
    fn map_unrecognized_error_falls_through_to_other() {
        let e = anyhow::anyhow!("disk on fire");
        assert!(matches!(map_write_error(e), ZkvError::Other(_)));
    }

    #[test]
    fn a_pending_import_is_importing_not_a_failure() {
        use anyhow::Context as _;

        let pending = || anyhow::Error::new(crate::data::ImportPending("reader".to_owned()));
        assert!(matches!(
            classify_sync_error(pending()),
            ZkvError::Importing
        ));
        assert!(matches!(
            classify_open_error(pending(), "reader"),
            ZkvError::Importing
        ));
        // It reaches the facade through `?` and `.context(..)`, so recognising
        // it cannot depend on its being the outermost error. This is the case
        // that made `zkv watch` fail its own first sync.
        let wrapped = Err::<(), _>(pending())
            .context("syncing \"reader\"")
            .context("read_sync")
            .unwrap_err();
        assert!(matches!(classify_sync_error(wrapped), ZkvError::Importing));
        assert!(matches!(
            classify_sync_error(anyhow::anyhow!("disk on fire")),
            ZkvError::Other(_)
        ));
    }

    #[test]
    fn classify_open_error_unknown_vs_other() {
        assert!(matches!(
            classify_open_error(anyhow::anyhow!("no database named \"ghost\""), "ghost"),
            ZkvError::UnknownDatabase(name) if name == "ghost"
        ));
        assert!(matches!(
            classify_open_error(anyhow::anyhow!("no keys.toml in /tmp/ghost"), "ghost"),
            ZkvError::UnknownDatabase(_)
        ));
        assert!(matches!(
            classify_open_error(anyhow::anyhow!("disk on fire"), "ghost"),
            ZkvError::Other(_)
        ));
    }
}
