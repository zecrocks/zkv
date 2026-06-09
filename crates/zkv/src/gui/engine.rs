//! Transport-agnostic GUI engine: the database operations behind every
//! GUI action, with no HTTP/IPC coupling.
//!
//! Both transports call into the same [`Engine`]:
//! - `zkv gui-browser`: the axum server in [`super`] wraps each method in
//!   a thin JSON handler (see `handle_*`).
//! - `zkv gui` (desktop): the binary's Tauri command layer wraps each
//!   method in a `#[tauri::command]`.
//!
//! # Threading model
//!
//! The facade's read/balance calls are synchronous (they block on
//! rusqlite); its write/sync/init calls are `async` but hold a
//! `rusqlite::Connection` across await points, so their futures are not
//! `Send`. Engine methods run as `Send` tasks (axum handlers / Tauri
//! commands), so the non-`Send` work is funnelled through `run_blocking`,
//! which hops to a `spawn_blocking` thread and drives the non-`Send` future
//! with a runtime [`Handle`]. A per-database lock registry (`Engine::db_lock`)
//! serializes operations on the *same* database while letting *different*
//! databases run in parallel. This is what lets [`Engine::run_auto_sync`]
//! sync several databases concurrently.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::runtime::Handle;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;

use zcash_protocol::consensus;
use zcash_protocol::ShieldedProtocol;

use crate::{
    config::{Role, WalletConfig},
    data::{self, Network},
    db::{Confirmations, Database, FundingDirection, FundingResult, ZkvError},
    internal::pending,
    protocol::{
        AuditResult, AuthRegistry, Authority, GrantedRole, HistoryResult, HistoryStatus, InitState,
        KeyState, Op, PendingOp, RevokedRole, RowOutcome, Scope,
    },
    remote::{ConnectionArgs, Servers},
};

/// Default number of concurrent background sync workers.
pub(crate) const DEFAULT_SYNC_WORKERS: usize = 5;
/// Upper bound accepted by the settings endpoint.
pub(crate) const MAX_SYNC_WORKERS: usize = 16;
/// Delay between background sync cycles.
const AUTO_SYNC_INTERVAL: Duration = Duration::from_secs(20);
/// Default history page size when the client doesn't specify one.
const DEFAULT_HISTORY_PAGE: u32 = 100;

// ===================================================================
// Response DTOs (serialized identically by axum Json and Tauri invoke)
// ===================================================================

#[derive(Serialize)]
pub struct StatusResp {
    pub version: String,
    /// Short git SHA this binary was built from (`git rev-parse --short HEAD`,
    /// with a `-dirty` suffix for an unclean tree, `unknown` outside a
    /// checkout). The status-bar version chip toggles to this on click.
    pub git_sha: String,
    /// Host OS this binary is running on: `"macos"`, `"linux"`, `"windows"`,
    /// etc. (`std::env::consts::OS`). Shown after the version in Settings.
    pub platform: String,
    pub server: String,
    pub current: Option<String>,
    pub databases: usize,
    pub network: Option<String>,
    pub chain_tip: Option<u32>,
    pub synced: Option<u32>,
    pub latency_ms: Option<u64>,
    /// Background concurrent-sync worker count (configurable in Settings).
    pub sync_workers: usize,
    /// Whether the global "pause all syncing" toggle is on.
    pub paused_all: bool,
    /// Whether the Settings screen should offer the "Re-import Oracle Demo"
    /// button: the bundled demo database was provisioned at some point but the
    /// user has since deleted it. See [`crate::demo::should_offer_reimport`].
    pub demo_reimport_available: bool,
    /// Whether the first-run onboarding has been completed or dismissed for
    /// this data dir (`data::was_onboarded`). The boot path shows the welcome
    /// overlay only when this is false and there is no real database.
    pub onboarded: bool,
    /// Whether this build is past its expiry ([`crate::freshness`]). Drives the
    /// out-of-date banner above the navbar.
    pub build_out_of_date: bool,
}

/// One network's lightwalletd server, as probed by [`Engine::servers`].
#[derive(Serialize)]
pub struct ServerRow {
    /// `host:port` we would dial for this network.
    pub server: String,
    /// `true` if the `GetLightdInfo` probe succeeded.
    pub online: bool,
    /// Server tip height (best chain), when online.
    pub block_height: Option<u64>,
    /// Backend node + version (`"zcashd 6.20.0"` / `"Zebra 2.1.0"`), when
    /// online and parseable.
    pub backend: Option<String>,
}

/// Mainnet + testnet server probes for the Settings screen.
#[derive(Serialize)]
pub struct ServersResp {
    pub mainnet: ServerRow,
    pub testnet: ServerRow,
    /// The resolved data directory, display-formatted (`~/.zkv` on Unix, the
    /// full path on Windows). See [`crate::data::data_dir_display`].
    pub data_dir: String,
}

/// The third-party license bundle for the "View Licenses" screen.
#[derive(Serialize)]
pub struct LicensesResp {
    pub text: String,
}

/// Outcome of a native "save to file" dialog (desktop transport only).
/// `saved` is false when the user cancelled the picker; `path` is the
/// chosen location on success (for a confirmation message).
#[derive(Serialize)]
pub struct SaveResp {
    pub saved: bool,
    pub path: Option<String>,
}

#[derive(Serialize)]
pub struct DbSummary {
    pub name: String,
    pub role: String,
    pub network: String,
    /// Shielded pool this database lives in: `"sapling"` or `"orchard"`.
    pub pool: String,
    /// lightwalletd endpoint for this db's network. Resolved locally (no
    /// round-trip), so the status bar can show the clicked db's server the
    /// instant you select it, before its full detail loads, instead of
    /// briefly holding the previous db's server during the switch.
    pub server: String,
    pub birthday: u32,
    pub keys: usize,
    pub unsynced: usize,
    /// Whether per-database auto-sync is paused (drives the sidebar icon).
    /// Reflects only per-db pause, never the global pause-all.
    pub paused: bool,
    /// Block timestamp (unix seconds) of the most recent confirmed write to
    /// any key, for "most recently updated" sidebar ordering. `None` if the
    /// database has no confirmed writes yet.
    pub updated_at: Option<u32>,
    /// Local wallet's scanned height. The status bar reads this the instant a
    /// db is selected (the summary list is already loaded and isn't cleared on
    /// click), so the synced/block readout doesn't flicker to "syncing" during
    /// the ~100ms before the full detail loads. `None` only if the wallet DB
    /// can't be opened.
    pub synced: Option<u32>,
}

#[derive(Serialize)]
pub struct KeyStatus {
    pub kind: String, // confirmed | confirming | pending | deleting
    pub done: u32,
    pub required: u32,
}

#[derive(Serialize)]
pub struct KeyRow {
    pub key: String,
    pub value: Option<String>,
    pub status: KeyStatus,
    pub txid: Option<String>,
    pub deleted: bool,
    pub size: Option<usize>,
    /// Block timestamp (unix seconds) of the latest confirmed write to this
    /// key, for the Browse "Last update" column. `None` if unknown / only
    /// in-flight writes so far.
    pub updated_at: Option<u32>,
}

#[derive(Serialize)]
pub struct DbDetail {
    pub name: String,
    pub role: String,
    pub network: String,
    /// Shielded pool this database lives in: `"sapling"` or `"orchard"`.
    pub pool: String,
    /// lightwalletd endpoint for this db's network (e.g. `zec.rocks:443`).
    /// Resolved locally (no network round-trip), so the status bar can show
    /// the right server the instant you switch databases.
    pub server: String,
    pub birthday: u32,
    pub address: String,
    /// Orchard funding UA (where to send ZEC for write fees). `None` for
    /// watch-only databases.
    pub funding_address: Option<String>,
    /// The database's UFVK-derived root signer (canonical `zkvid1…`). Carried on the
    /// detail so the Browse "Updated by" chip resolves from the same atomic load
    /// as the key rows, rather than a separate roles fetch that could land a beat
    /// out of step and make the detail pane update after the table. `None` only
    /// if it couldn't be derived.
    pub signer: Option<String>,
    pub init: String,
    pub init_done: u32,
    pub init_required: u32,
    pub balance: Option<u64>,
    /// Funds still confirming (subset of `balance`). `None` for watch-only.
    pub confirming: Option<u64>,
    pub synced: Option<u32>,
    /// Authoritative "the wallet has scanned up to the live chain tip" verdict
    /// (`Database::synced_to_tip`: within tip tolerance AND no outstanding scan
    /// ranges). Computed via one lightwalletd round-trip, but **only** when the
    /// read came back `uninitialized` (the sole case the frontend's first-sync
    /// gate consults it); `false` otherwise. The frontend uses this to leave the
    /// "Starting sync…" panel for the "needs initialization" state once it is
    /// certain no INIT exists, even when the separate status-poll `chain_tip` is
    /// momentarily unavailable.
    pub synced_to_tip: bool,
    pub keys: Vec<KeyRow>,
    /// Whether the `/history` endpoint is wired up (it now is). The
    /// frontend keeps the flag so an older backend degrades gracefully.
    pub history_available: bool,
    /// Whether per-database auto-sync is currently paused for this db.
    pub paused: bool,
    /// The database's required protocol epoch, projected from `VERSION` memos
    /// (`GENESIS_DB_VERSION` = 1 until an owner broadcasts one).
    pub db_version: u32,
    /// The maximum protocol epoch this build supports (`MAX_DB_VERSION`). When
    /// `db_version > client_max_version`, this client is out of date.
    pub client_max_version: u32,
    /// The controlling `VERSION` memo's block flags in wire form (`warn` /
    /// `blockwrite` / `blocksync,blockread` / `blockall`).
    pub block_flags: String,
    /// Whether this (out-of-date) client must stop scanning the chain.
    pub blocks_sync: bool,
    /// Whether this (out-of-date) client must refuse to read/display state.
    pub blocks_read: bool,
    /// Whether this (out-of-date) client must refuse to broadcast writes.
    pub blocks_write: bool,
}

#[derive(Serialize)]
pub struct HistoryStatusResp {
    pub kind: String, // confirmed | confirming | pending
    pub done: u32,
    pub required: u32,
    pub confirmations: u32,
}

#[derive(Serialize)]
pub struct HistoryEntryResp {
    pub op: String, // SET | DEL | INIT
    pub key: String,
    pub value: Option<String>,
    pub height: Option<u32>,
    /// Block timestamp (unix seconds), or null if not yet mined.
    pub timestamp: Option<u32>,
    pub txid: String,
    pub output_index: u32,
    pub signature: Option<String>,
    /// The replay-protection sequence this write referenced on the wire (the
    /// `[seq]` prefix on the signature line). `null` only for not-yet-cached
    /// pending entries.
    pub seq: Option<u64>,
    /// Compressed-hex of the signer that authored this write (a delegated
    /// owner/writer in a multi-signer database, which may differ from the
    /// database root). `null` for not-yet-confirmable pending entries.
    pub signer: Option<String>,
    /// `"owner"` / `"writer"` / `null`, resolved from the current registry so
    /// the UI can label the signer and link to Roles. `null` when the signer
    /// holds no current role (e.g. a since-revoked writer) or is unknown.
    pub signer_role: Option<String>,
    pub verified: Option<bool>,
    pub status: HistoryStatusResp,
    pub memo: Option<String>,
    /// Actual fee paid (zatoshi), or `None` when the wallet didn't create the
    /// tx (received-only) or hasn't indexed it yet.
    pub fee: Option<u64>,
    /// Value (zatoshi) carried by this write's own output, when nonzero (a
    /// tip/deposit broadcast with the memo). `None` for a plain zkv write.
    pub output_value: Option<u64>,
}

#[derive(Serialize)]
pub struct HistoryResp {
    /// The database's creator: the pubkey that signed `INIT` (the UFVK-derived
    /// root key). A persistent trait of the database; surfaced even if the
    /// creator's owner authority is later revoked. Per-write attribution is on
    /// each entry's `signer`.
    pub creator: String,
    /// One page, newest-first with in-flight writes pinned on top.
    pub entries: Vec<HistoryEntryResp>,
    /// Total matching rows across all pages (drives the pagination bar).
    pub total: u64,
    pub offset: u32,
    pub limit: Option<u32>,
}

/// One entry in the authorization registry: an owner (full authority) or a
/// scoped writer.
#[derive(Serialize)]
pub struct RoleRow {
    /// `"owner"` or `"writer"`.
    pub role: String,
    /// The signer's canonical `zkvid1…` public key.
    pub pubkey: String,
    /// Writer capability tokens (`CREATE`/`UPDATE`/`DESTROY`, canonical
    /// order). Empty for owners, who can write any key.
    pub capabilities: Vec<String>,
    /// Mined height of the grant that established this role (the `INIT` for the
    /// creator, otherwise the `OWNERSET`/`WRITERSET`), or null if unmined or
    /// the grant couldn't be located.
    pub height: Option<u32>,
    /// Block timestamp (unix seconds) of that grant, or null. The Roles detail
    /// pane shows it as "added `<when>`".
    pub timestamp: Option<u32>,
    /// The owner that signed the grant (the creator self-signs its `INIT`), as a
    /// canonical `zkvid1…`, or null if unknown.
    pub granted_by: Option<String>,
}

/// One revoked entry in the authorization registry: a pubkey that once held
/// owner/writer authority and has since been revoked, with its provenance.
#[derive(Serialize)]
pub struct RevokedRoleRow {
    /// The role held immediately before revocation: `"owner"` or `"writer"`.
    pub role: String,
    /// The revoked signer's compressed-hex public key (66 chars).
    pub pubkey: String,
    /// Writer capability tokens last held (empty for a revoked owner).
    pub capabilities: Vec<String>,
    /// Mined height of the revoking management op, or null if unmined.
    pub height: Option<u32>,
    /// Block timestamp (unix seconds) of the revocation, or null.
    pub timestamp: Option<u32>,
    /// Compressed-hex of the owner that signed the revocation, or null.
    pub revoked_by: Option<String>,
}

#[derive(Serialize)]
pub struct RolesResp {
    /// The database's creator: the UFVK-derived key that signed `INIT`
    /// (canonical `zkvid1…`). Not a role but a persistent trait; surfaced so
    /// the UI can always show it and mark its row, even once its owner
    /// authority is revoked. `null` if it couldn't be derived.
    pub creator: Option<String>,
    /// Owners first, then scoped writers; each already sorted by pubkey.
    pub rows: Vec<RoleRow>,
    /// Revoked owners/writers (tombstones), newest revocation first.
    pub revoked: Vec<RevokedRoleRow>,
}

#[derive(Serialize)]
pub struct RejectionResp {
    /// `SET` / `SETL` / `DEL` / `INIT` / `OWNER*` / `WRITER*`, or null when the
    /// memo failed to parse far enough to identify an opcode.
    pub op: Option<String>,
    pub key: Option<String>,
    pub value: Option<String>,
    pub height: Option<u32>,
    /// Block timestamp (unix seconds) of the mined height, or null for
    /// mempool / unresolved.
    pub timestamp: Option<u32>,
    pub txid: String,
    /// The raw memo text exactly as broadcast on-chain, for inspection.
    pub raw: String,
    /// Human-readable, standardized reason the write was rejected.
    pub reason: String,
    /// Compressed-hex of the recovered signer, present iff the signature was
    /// cryptographically valid (so the rejection is an authorization/lifecycle
    /// decision, not a signature failure). `null` for malformed / bad-signature
    /// memos.
    pub signer: Option<String>,
    /// Whether the signature recovered to a valid signer. `true` ⇒ "Valid
    /// Signature ✓, Authorized ✗"; `false` ⇒ the signature itself is invalid.
    pub signature_valid: bool,
}

#[derive(Serialize)]
pub struct RejectionsResp {
    /// Rejected writes, newest-first.
    pub entries: Vec<RejectionResp>,
    pub total: u64,
}

#[derive(Serialize)]
pub struct FundingTxResp {
    pub txid: String,
    pub height: Option<u32>,
    /// Block timestamp (unix seconds), or null until mined.
    pub timestamp: Option<u32>,
    /// `"received"`, `"sent"`, `"self"` (sent to one of our own addresses), or
    /// `"zkv"` (a bare-fee zkv write).
    pub direction: String,
    /// Absolute value transferred (zatoshi), fee excluded. For `"self"` and
    /// `"zkv"` this is the net effect (the fee), matching librustzcash's
    /// balance delta.
    pub amount: u64,
    /// For `"self"`, the gross value (zatoshi) routed to one of our own
    /// addresses; null otherwise.
    pub self_sent: Option<u64>,
    /// Fee paid (zatoshi), or null for received-only transactions.
    pub fee: Option<u64>,
    pub memo: Option<String>,
    /// External recipient address(es) for sends; empty for receives.
    pub recipients: Vec<String>,
    /// Whether the tx carries a zkv memo, so the detail pane can link to the
    /// write in History. Always true for a `"zkv"` row.
    pub is_zkv: bool,
    pub pending: bool,
    /// On-chain confirmations (`tip − height + 1`); `0` while in the mempool.
    pub confirmations: u32,
    /// Confirmations required before this tx counts as confirmed: the ZIP-315
    /// depth for its direction (10 for an external receive, 3 for our own
    /// send/self-transfer), matching the wallet's spendability policy.
    pub required: u32,
    /// Whether the tx has reached `required` confirmations. Until then it is
    /// still confirming, exactly as the wallet balance reports it.
    pub confirmed: bool,
}

#[derive(Serialize)]
pub struct FundingResp {
    /// One page, newest-first with mempool transactions pinned on top.
    pub entries: Vec<FundingTxResp>,
    /// Total matching transactions across all pages (drives pagination).
    pub total: u64,
    pub offset: u32,
    pub limit: Option<u32>,
}

#[derive(Serialize)]
pub struct QrResp {
    /// A standalone SVG document for the QR code (white quiet zone + black
    /// modules), safe to inline.
    pub svg: String,
}

#[derive(Serialize)]
pub struct CreateResp {
    pub name: String,
    pub address: String,
    /// Recovery phrase, shown once to the local user, like the CLI's
    /// `zkv init` does on the terminal.
    pub phrase: String,
    pub funding_address: String,
}

/// A freshly generated recovery phrase, not yet bound to any database. Returned
/// by [`Engine::generate_phrase`] so the create flow can show the seed for the
/// user to confirm *before* the database directory is persisted.
#[derive(Serialize)]
pub struct PhraseResp {
    pub phrase: String,
}

#[derive(Serialize)]
pub struct SyncResp {
    pub synced: u32,
}

#[derive(Serialize)]
pub struct PauseResp {
    pub paused: bool,
}

#[derive(Serialize)]
pub struct SettingsResp {
    pub sync_workers: usize,
}

#[derive(Serialize)]
pub struct TxResp {
    pub txid: String,
}

/// Result of validating a recipient address for the Send flow. `valid` is the
/// verdict; on success `kind` is a short label for the address type
/// (`"unified"`, `"sapling"`, `"transparent"`, `"TEX"`), on failure `error`
/// is a short, user-facing reason.
#[derive(Serialize)]
pub struct AddrCheckResp {
    pub valid: bool,
    pub kind: Option<String>,
    /// The address's network label (`main`/`test`/`regtest` → `mainnet`/…),
    /// so the UI can show "valid X address (network, pool)". `None` on failure.
    pub network: Option<String>,
    /// The shielded pool the recipient pays into (`orchard`/`sapling`), or
    /// `transparent` for a transparent / TEX recipient. `None` on failure.
    pub pool: Option<String>,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct SignPreviewResp {
    /// The exact signed memo text (`ZKV0 …` followed by the signature line),
    /// byte-for-byte what the online write path would put on chain. A power
    /// user can copy this and broadcast it from any wallet themselves.
    pub memo: String,
    /// The single-pool recipient UA the memo must be sent to (this database's
    /// own address): the other half a hand-broadcaster needs.
    pub recipient: String,
}

/// What a `zkv1…` address reveals without adding it: the network and pool it
/// commits to, plus the birthday encoded in its zkv-meta item. Used by the
/// restore flow to show a "mainnet · orchard" badge and to resolve the
/// birthday + network for the actual restore call.
#[derive(Serialize)]
pub struct ZkvAddrInfoResp {
    /// `"mainnet"` / `"testnet"` / `"regtest"`.
    pub network: String,
    /// Shielded pool this database lives in: `"sapling"` or `"orchard"`.
    pub pool: String,
    /// The wallet birthday height encoded inside the address.
    pub birthday: u32,
}

#[derive(Serialize)]
pub struct AddDbResp {
    pub name: String,
    pub role: String,
}

#[derive(Serialize)]
pub struct OkResp {
    pub ok: bool,
}

/// The decrypted recovery phrase for an admin database, returned once to the
/// local user by the Danger Zone "show seed phrase" action. `phrase` is the
/// space-separated 24-word BIP-39 mnemonic, exactly as shown at creation time.
#[derive(Serialize)]
pub struct RevealPhraseResp {
    pub name: String,
    pub phrase: String,
}

/// Outcome of a faucet request (funds or sponsored INIT), proxied through the
/// backend so the call isn't subject to browser CORS and its failures land in
/// the logs (`RUST_LOG`). `outcome` is one of:
/// - `"ok"`: the faucet accepted the request (HTTP 2xx).
/// - `"outdated"`: the response mentions "update", or the faucet is
///   unreachable (down / no longer running). The GUI shows "Your app is
///   outdated".
/// - `"error"`: the faucet was reachable but returned a non-2xx status. The
///   GUI shows "Try again later".
#[derive(Serialize)]
pub struct FaucetResp {
    pub outcome: String,
    /// The broadcast txid, when the faucet returned one (the sponsored-INIT
    /// path). `None` for the fund-only call or when the faucet's 2xx response
    /// carried no txid. Lets the GUI's INIT receipt show the txid even though
    /// the faucet, not the local wallet, created the transaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub txid: Option<String>,
}

/// A signed-but-unbroadcast memo, produced by the Reference view's per-opcode
/// builder (the GUI equivalent of `zkv sign`). `unsigned` is the `ZKV0 …` body
/// shown before signing; `signed` is the full memo text (body + signature
/// line) to copy and relay through a funded wallet.
#[derive(Serialize)]
pub struct SignMemoResp {
    pub unsigned: String,
    pub signed: String,
    pub recipient_ua: String,
    pub zkv_addr: String,
}

// ===================================================================
// Engine
// ===================================================================

/// Transport-agnostic GUI state. Cheap to clone (always behind an `Arc`).
pub struct Engine {
    pub(crate) conn: ConnectionArgs,
    /// Base lightwalletd label (mainnet); [`Engine::status`] recomputes it
    /// per the active database's network.
    server_label: String,
    /// Per-database serialization registry. Operations on the same db take
    /// the same lock (rusqlite isn't friendly to concurrent writers, and
    /// reads promote the snapshot sidecar); different dbs run in parallel.
    locks: std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Databases whose continuous auto-sync is paused (per-db). Held only
    /// for brief, non-await locking, so a std Mutex is correct.
    paused: std::sync::Mutex<HashSet<String>>,
    /// Max number of databases the background loop syncs concurrently.
    sync_workers: AtomicUsize,
    /// Global pause: when set, the background loop halts entirely.
    paused_all: AtomicBool,
    /// Cached build-freshness verdict. The mainnet height probe runs once (on
    /// the first [`Engine::status`] call) and the answer is reused for the
    /// process lifetime: we don't care about the long-running edge case where a
    /// session crosses the cutoff mid-run.
    out_of_date: tokio::sync::OnceCell<bool>,
}

impl Engine {
    /// Build a fresh engine for the given lightwalletd connection.
    pub fn new(conn: ConnectionArgs) -> Arc<Self> {
        // Every GUI transport funnels through here. The GUI drives sync from a
        // background loop while it may share the launching terminal, so mute
        // the CLI's interactive chrome (the "Syncing…" spinner + status lines):
        // that console should carry only `tracing` log events. See
        // `ui::set_quiet`.
        crate::ui::set_quiet();
        let server_label = server_endpoint(&conn, consensus::Network::MainNetwork);
        Arc::new(Engine {
            conn,
            server_label,
            locks: std::sync::Mutex::new(HashMap::new()),
            paused: std::sync::Mutex::new(HashSet::new()),
            sync_workers: AtomicUsize::new(DEFAULT_SYNC_WORKERS),
            paused_all: AtomicBool::new(false),
            out_of_date: tokio::sync::OnceCell::new(),
        })
    }

    /// Return the per-database lock for `name`, creating it on first use.
    /// Callers `await` the returned `Mutex` to serialize same-db work.
    fn db_lock(&self, name: &str) -> Arc<Mutex<()>> {
        let mut m = self.locks.lock().unwrap();
        m.entry(name.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Ambient status: version, active db, network, chain tip, sync state.
    pub async fn status(&self) -> Result<StatusResp, ZkvError> {
        let names = data::list_dbs().map_err(ZkvError::Other)?;
        let current = data::current_db().map_err(ZkvError::Other)?;

        let mut network = None;
        let mut chain_tip = None;
        let mut synced = None;
        let mut latency_ms = None;
        let mut server = self.server_label.clone();
        let mut is_mainnet = false;

        if let Some(name) = &current {
            let conn = self.conn.clone();
            let n = name.clone();
            let db = run_blocking(move |_| Database::open(&n, conn)).await;
            if let Ok(db) = db {
                let net = db.network();
                is_mainnet = matches!(net.into(), consensus::Network::MainNetwork);
                network = Some(net.name().to_owned());
                server = server_endpoint(&self.conn, net.into());
                synced = db.synced_height().ok().flatten();
                let started = std::time::Instant::now();
                if let Ok(tip) = db.chain_tip().await {
                    chain_tip = Some(tip);
                    latency_ms = Some(started.elapsed().as_millis() as u64);
                }
            }
        }

        // Build-freshness verdict, computed once and cached. The clock check is
        // free; the clock-tamper-resistant height check needs a mainnet tip, so
        // reuse the current-db tip when it is already mainnet, else probe
        // mainnet directly (a failed probe leaves the clock check standing
        // alone). Done lazily on first call so launch isn't blocked on the net.
        let out_of_date = *self
            .out_of_date
            .get_or_init(|| async {
                let mainnet_tip = if is_mainnet {
                    chain_tip
                } else {
                    self.conn
                        .server_info(consensus::Network::MainNetwork)
                        .await
                        .ok()
                        .map(|i| i.block_height as u32)
                };
                crate::freshness::build_out_of_date(mainnet_tip)
            })
            .await;

        Ok(StatusResp {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            git_sha: env!("ZKV_GIT_SHA").to_owned(),
            platform: std::env::consts::OS.to_owned(),
            server,
            current,
            databases: names.len(),
            network,
            chain_tip,
            synced,
            latency_ms,
            sync_workers: self.sync_workers.load(Ordering::Relaxed),
            paused_all: self.paused_all.load(Ordering::Relaxed),
            demo_reimport_available: crate::demo::should_offer_reimport(),
            onboarded: data::was_onboarded(),
            build_out_of_date: out_of_date,
        })
    }

    /// Record that the first-run onboarding has been completed or dismissed, so
    /// the welcome overlay is not shown again for this data dir. Persisted in
    /// `.zkv` (not the browser), so it is per-install, not per-origin.
    pub fn mark_onboarded(&self) -> Result<(), ZkvError> {
        data::mark_onboarded().map_err(ZkvError::Other)
    }

    /// Probe the configured lightwalletd for both networks (mainnet + testnet)
    /// and report each server's endpoint, tip height, and backend node. Drives
    /// the two server rows on the Settings screen. Each probe is independent:
    /// one network being unreachable (or the operator not serving it) only
    /// marks that row offline. The two probes run concurrently.
    pub async fn servers(&self) -> ServersResp {
        let probe = |network: consensus::Network| {
            let conn = self.conn.clone();
            async move {
                let server = server_endpoint(&conn, network);
                match conn.server_info(network).await {
                    Ok(info) => ServerRow {
                        server: info.endpoint,
                        online: true,
                        block_height: Some(info.block_height),
                        backend: Some(info.backend),
                    },
                    Err(_) => ServerRow {
                        server,
                        online: false,
                        block_height: None,
                        backend: None,
                    },
                }
            }
        };
        let (mainnet, testnet) = tokio::join!(
            probe(consensus::Network::MainNetwork),
            probe(consensus::Network::TestNetwork),
        );
        // Display-formatted (e.g. `~/.zkv` on Unix, the full path on Windows);
        // best-effort, blank if it can't be resolved.
        let data_dir = data::data_dir_display().unwrap_or_default();
        ServersResp {
            mainnet,
            testnet,
            data_dir,
        }
    }

    /// The third-party license bundle (generated at build time, embedded
    /// gzip-compressed, inflated lazily). Served the same way on both
    /// transports so the desktop view doesn't depend on Tauri's `frontendDist`
    /// carrying a generated file.
    pub fn licenses(&self) -> LicensesResp {
        LicensesResp {
            text: super::assets::licenses_text().to_owned(),
        }
    }

    /// One summary row per local database (sidebar list).
    pub async fn list_databases(&self) -> Result<Vec<DbSummary>, ZkvError> {
        let conn = self.conn.clone();
        // Snapshot the per-db paused set (drives the sidebar icon; never the
        // global pause-all). Cloned out so the closure stays `Send + 'static`.
        let paused = self.paused.lock().unwrap().clone();
        run_blocking(move |_| {
            let names = data::list_dbs().map_err(ZkvError::Other)?;
            let mut out = Vec::with_capacity(names.len());
            for name in names {
                let cfg = match WalletConfig::read(&name) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                // Local-only state read for the sidebar counts + scanned
                // height. No network. The synced height lets the status bar
                // reflect the clicked db immediately, before its detail loads.
                let (keys, unsynced, updated_at, synced) = match Database::open(&name, conn.clone())
                {
                    Ok(db) => {
                        let synced = db.synced_height().ok().flatten();
                        match db.read(Confirmations::Default) {
                            Ok(result) => {
                                let (k, u) = count_keys(&result.state);
                                let updated =
                                    result.state.values().filter_map(|ks| ks.updated_at).max();
                                (k, u, updated, synced)
                            }
                            Err(_) => (0, 0, None, synced),
                        }
                    }
                    Err(_) => (0, 0, None, None),
                };
                let is_paused = paused.contains(&name);
                out.push(DbSummary {
                    name,
                    role: role_str(cfg.role).to_owned(),
                    network: Network::from(cfg.network).name().to_owned(),
                    pool: crate::config::pool_label(cfg.pool).to_owned(),
                    server: server_endpoint(&conn, Network::from(cfg.network).into()),
                    birthday: u32::from(cfg.birthday),
                    keys,
                    unsynced,
                    paused: is_paused,
                    updated_at,
                    synced,
                });
            }
            Ok::<_, ZkvError>(out)
        })
        .await
    }

    /// Full detail for one database: keys, balance, funding UA, init state.
    pub async fn detail(&self, name: String) -> Result<DbDetail, ZkvError> {
        let conn = self.conn.clone();
        let paused = self.paused.lock().unwrap().contains(&name);
        run_blocking(move |h| {
            let cfg = WalletConfig::read(&name).map_err(|e| classify_unknown(e, &name))?;
            let db = Database::open(&name, conn.clone())?;
            let server = server_endpoint(&conn, db.network().into());
            let result = db.read(Confirmations::Default)?;
            let (init, init_done, init_required) = init_parts(&result.init);
            // Only the uninitialized verdict is provisional (an INIT could still
            // be sitting in not-yet-scanned blocks), so that is the only case
            // worth a tip probe. For initialized/initializing the frontend's
            // gate already treats the verdict as final, so skip the round-trip.
            let synced_to_tip = if matches!(result.init, InitState::Uninitialized) {
                h.block_on(db.synced_to_tip()).unwrap_or(false)
            } else {
                false
            };
            let balance = match db.balance() {
                Ok(b) => Some(b),
                Err(ZkvError::WatchOnly) => None,
                Err(e) => return Err(e),
            };
            let confirming = match db.balance_confirming() {
                Ok(c) => Some(c),
                Err(ZkvError::WatchOnly) => None,
                Err(e) => return Err(e),
            };
            // Admin-only: the (single-pool) UA to fund for write fees (None
            // for watch-only). Surfaced so the UI can show a "deposit" QR.
            let funding_address = match db.funding_address() {
                Ok(a) => Some(a),
                Err(ZkvError::WatchOnly) => None,
                Err(e) => return Err(e),
            };
            Ok::<_, ZkvError>(DbDetail {
                name: db.name().to_owned(),
                role: role_str(cfg.role).to_owned(),
                network: db.network().name().to_owned(),
                pool: crate::config::pool_label(cfg.pool).to_owned(),
                server,
                birthday: u32::from(cfg.birthday),
                address: db.zkv_address()?,
                funding_address,
                signer: db.signer().ok(),
                init,
                init_done,
                init_required,
                balance,
                confirming,
                synced: db.synced_height()?,
                synced_to_tip,
                keys: key_rows(&result.state),
                history_available: true,
                paused,
                db_version: result.version.version,
                client_max_version: crate::protocol::MAX_DB_VERSION,
                block_flags: result.version.blocks.to_wire(),
                blocks_sync: result.version.blocks_sync(),
                blocks_read: result.version.blocks_read(),
                blocks_write: result.version.blocks_write(),
            })
        })
        .await
    }

    /// A page of the append-only signed write history.
    pub async fn history(
        &self,
        name: String,
        filter: Option<String>,
        limit: Option<u32>,
        offset: u32,
        locate: Option<String>,
    ) -> Result<HistoryResp, ZkvError> {
        // Read-only against the snapshot + wallet DB (no promote); no lock.
        let conn = self.conn.clone();
        run_blocking(move |_| {
            let db = Database::open(&name, conn)?;
            let limit = limit.unwrap_or(DEFAULT_HISTORY_PAGE);
            let result = match locate.as_deref() {
                // Jump to the page holding a specific write, in full context.
                Some(txid) => {
                    db.history_at_txid(filter.as_deref(), Confirmations::Default, limit, txid)?
                }
                None => db.history(
                    filter.as_deref(),
                    Confirmations::Default,
                    Some(limit),
                    offset,
                )?,
            };
            // The registry resolves each entry's signer to its current role
            // (owner/writer) for the UI label + Roles link. Best-effort: a
            // failure leaves roles unlabeled rather than sinking the page.
            let auth = db.roles(Confirmations::Default).ok();
            Ok::<_, ZkvError>(history_resp(result, auth.as_ref()))
        })
        .await
    }

    /// Every memo replay *rejected* for this database, newest-first, each with
    /// its standardized reason. Backs the GUI's "Rejections" tab. Read-only
    /// full re-scan (no promote, no lock).
    pub async fn rejections(&self, name: String) -> Result<RejectionsResp, ZkvError> {
        let conn = self.conn.clone();
        run_blocking(move |_| {
            let db = Database::open(&name, conn)?;
            let audit = db.audit(Confirmations::Default)?;
            Ok::<_, ZkvError>(rejections_resp(audit))
        })
        .await
    }

    /// The on-chain authorization registry: owners and scoped writers, plus
    /// this database's own UFVK-derived root signer so the UI can mark its row.
    pub async fn roles(&self, name: String) -> Result<RolesResp, ZkvError> {
        // Read-only against the snapshot + wallet DB (no promote); no lock,
        // matching the other read handlers.
        let conn = self.conn.clone();
        run_blocking(move |_| {
            let db = Database::open(&name, conn)?;
            let auth = db.roles(Confirmations::Default)?;
            // One audit re-scan feeds both the per-role grant provenance (when /
            // by whom each current role was granted) and the revoked tombstones,
            // both surfaced alongside the current registry.
            let audit = db.audit(Confirmations::Default)?;
            let granted = crate::protocol::granted_roles(&audit);
            let revoked = crate::protocol::revoked_roles(&audit);
            // The creator (INIT signer = UFVK-derived root) is a persistent
            // trait; best-effort so a derivation failure doesn't sink the list.
            let creator = db.signer().ok();
            Ok::<_, ZkvError>(roles_resp(&auth, granted, revoked, creator))
        })
        .await
    }

    /// A page of the database's funding ledger (non-zkv ZEC transfers in/out).
    pub async fn funding(
        &self,
        name: String,
        limit: Option<u32>,
        offset: u32,
    ) -> Result<FundingResp, ZkvError> {
        // Read-only against the wallet DB (no promote); no lock.
        let conn = self.conn.clone();
        run_blocking(move |_| {
            let db = Database::open(&name, conn)?;
            let limit = limit.unwrap_or(DEFAULT_HISTORY_PAGE);
            let result = db.funding(Some(limit), offset)?;
            Ok::<_, ZkvError>(funding_resp(result))
        })
        .await
    }

    /// Sync one database to the chain tip (optionally including the mempool).
    pub async fn sync(&self, name: String, mempool: bool) -> Result<SyncResp, ZkvError> {
        let lock = self.db_lock(&name);
        let _guard = lock.lock().await;
        let conn = self.conn.clone();
        let synced = run_blocking(move |h| {
            let db = Database::open(&name, conn)?;
            if mempool {
                h.block_on(db.sync_with_mempool())
            } else {
                h.block_on(db.sync())
            }
        })
        .await?;
        Ok(SyncResp { synced })
    }

    /// Broadcast the database's INIT transaction. When `require_sync` is set
    /// (re-broadcast on an existing db), first require a full sync to tip so
    /// we don't double-INIT a db whose valid INIT is still unscanned.
    pub async fn init(&self, name: String, require_sync: bool) -> Result<TxResp, ZkvError> {
        let lock = self.db_lock(&name);
        let _guard = lock.lock().await;
        let conn = self.conn.clone();
        let txid = run_blocking(move |h| {
            let db = Database::open(&name, conn)?;
            if require_sync && !h.block_on(db.synced_to_tip())? {
                return Err(ZkvError::NotSynced);
            }
            h.block_on(db.init())
        })
        .await?;
        Ok(TxResp { txid })
    }

    /// Ask the hosted faucet to fund this database's address. The POST runs
    /// here in the backend (not the browser), so it isn't subject to CORS and
    /// its outcome is logged. Returns the faucet `outcome` (see [`FaucetResp`]).
    /// Never a hard error for transport failures: those are folded into the
    /// `outcome`.
    pub async fn faucet_funds(&self, name: String) -> Result<FaucetResp, ZkvError> {
        let conn = self.conn.clone();
        let address = run_blocking(move |_| {
            let db = Database::open(&name, conn)?;
            db.zkv_address()
        })
        .await?;
        let outcome = match faucet_call(
            "/faucet",
            &serde_json::json!({ "address": address }),
            "funds",
        )
        .await
        {
            FaucetCall::Ok(_) => "ok",
            FaucetCall::Outdated => "outdated",
            FaucetCall::Error => "error",
        };
        Ok(FaucetResp {
            outcome: outcome.to_owned(),
            txid: None,
        })
    }

    /// Ask the hosted faucet to broadcast this database's INIT memo (it pays the
    /// fee), so an unfunded database can initialize. The INIT memo is signed
    /// locally, then POSTed from the backend. On success we record a pending
    /// INIT (keyed by the zkv address, mirroring `write::broadcast_init`) using
    /// the txid the faucet returns, so the database flips to "initializing"
    /// immediately instead of looking "uninitialized" until our own wallet
    /// scans the faucet's tx. Returns the faucet `outcome`.
    pub async fn faucet_init(&self, name: String) -> Result<FaucetResp, ZkvError> {
        let conn = self.conn.clone();
        let nm = name.clone();
        let (memo, zkv_addr) = run_blocking(move |_| {
            let db = Database::open(&nm, conn)?;
            let p = db.prepare_init()?;
            Ok::<_, ZkvError>((p.memo_text, p.zkv_addr))
        })
        .await?;
        let mut init_txid: Option<String> = None;
        let outcome = match faucet_call("/init", &serde_json::json!({ "memo": memo }), "init").await
        {
            FaucetCall::Ok(body) => {
                match faucet_txid(&body) {
                    Some(txid) => {
                        let entry = pending::PendingEntry {
                            txid: txid.clone(),
                            op: "INIT".to_owned(),
                            key: zkv_addr,
                            value: None,
                            memo: Some(memo),
                            broadcast_at_unix: pending::now_unix(),
                        };
                        if let Err(e) = pending::append(&name, entry) {
                            tracing::warn!(target: "zkv::gui::faucet", "recording pending faucet INIT: {e:#}");
                        }
                        init_txid = Some(txid);
                    }
                    None => {
                        tracing::warn!(target: "zkv::gui::faucet", body = %body, "faucet init: 2xx but no txid in response")
                    }
                }
                "ok"
            }
            FaucetCall::Outdated => "outdated",
            FaucetCall::Error => "error",
        };
        Ok(FaucetResp {
            outcome: outcome.to_owned(),
            txid: init_txid,
        })
    }

    /// Set a key to a value (online broadcast, or offline-signed memo).
    pub async fn set_key(
        &self,
        name: String,
        key: String,
        value: String,
        offline: bool,
    ) -> Result<TxResp, ZkvError> {
        let lock = self.db_lock(&name);
        let _guard = lock.lock().await;
        let conn = self.conn.clone();
        let txid = run_blocking(move |h| {
            let db = Database::open(&name, conn)?;
            if offline {
                h.block_on(db.set_no_sync(&key, &value))
            } else {
                h.block_on(db.set(&key, &value))
            }
        })
        .await?;
        Ok(TxResp { txid })
    }

    /// Delete a key.
    pub async fn del_key(&self, name: String, key: String) -> Result<TxResp, ZkvError> {
        let lock = self.db_lock(&name);
        let _guard = lock.lock().await;
        let conn = self.conn.clone();
        let txid = run_blocking(move |h| {
            let db = Database::open(&name, conn)?;
            h.block_on(db.del(&key))
        })
        .await?;
        Ok(TxResp { txid })
    }

    /// Validate a recipient address for the Send flow without sending. Backs
    /// the modal's live "is this a valid Zcash address" check, so it never
    /// errors on a bad address: an invalid address comes back as
    /// `{ valid: false, error }`. Purely local: no sync, no broadcast.
    pub async fn check_address(
        &self,
        name: String,
        address: String,
    ) -> Result<AddrCheckResp, ZkvError> {
        let conn = self.conn.clone();
        run_blocking(move |_| {
            let db = Database::open(&name, conn)?;
            Ok::<_, ZkvError>(match db.describe_recipient(&address) {
                Ok(info) => AddrCheckResp {
                    valid: true,
                    kind: Some(info.kind),
                    network: Some(info.network),
                    pool: info.pool,
                    error: None,
                },
                Err(reason) => AddrCheckResp {
                    valid: false,
                    kind: None,
                    network: None,
                    pool: None,
                    error: Some(reason),
                },
            })
        })
        .await
    }

    /// Send a plain ZEC value transfer to any Zcash address (the GUI Send
    /// button). `amount` is a decimal ZEC string. Syncs, validates the
    /// recipient against the database's network, then signs and broadcasts.
    pub async fn send(
        &self,
        name: String,
        recipient: String,
        amount: String,
        memo: Option<String>,
    ) -> Result<TxResp, ZkvError> {
        let lock = self.db_lock(&name);
        let _guard = lock.lock().await;
        let conn = self.conn.clone();
        let txid = run_blocking(move |h| {
            let db = Database::open(&name, conn)?;
            h.block_on(db.send(&recipient, &amount, memo.as_deref()))
        })
        .await?;
        Ok(TxResp { txid })
    }

    /// Sign a write memo without broadcasting it: the GUI counterpart of
    /// `zkv sign`, backing the Reference view's per-opcode builder. Dispatches
    /// by opcode to the matching `prepare_*` facade method (each enforces
    /// admin-only, initialization, and authorization, surfacing failures as
    /// the usual structured `ZkvError`s), and returns the unsigned `ZKV0 …`
    /// body plus the full signed memo text. Purely local: no sync, no
    /// broadcast. `VERSION` has no broadcast path in this build and is
    /// rejected.
    pub async fn sign_memo(
        &self,
        name: String,
        op: String,
        key: Option<String>,
        value: Option<String>,
        scope: Option<String>,
    ) -> Result<SignMemoResp, ZkvError> {
        let lock = self.db_lock(&name);
        let _guard = lock.lock().await;
        let conn = self.conn.clone();
        run_blocking(move |_h| {
            let db = Database::open(&name, conn)?;
            let bad = |m: String| ZkvError::Other(anyhow::anyhow!(m));
            let need_key = || {
                key.clone()
                    .ok_or_else(|| bad(format!("{op} requires a key")))
            };
            let prepared = match op.to_ascii_uppercase().as_str() {
                "SET" => db.prepare_set(&need_key()?, value.as_deref().unwrap_or(""))?,
                "SETL" => db.prepare_setl(&need_key()?, value.as_deref().unwrap_or(""))?,
                "DEL" => db.prepare_del(&need_key()?)?,
                "INIT" => db.prepare_init()?,
                "OWNERSET" => db.prepare_management(Op::OwnerSet, &need_key()?, None)?,
                "OWNERDEL" => db.prepare_management(Op::OwnerDel, &need_key()?, None)?,
                "WRITERDEL" => db.prepare_management(Op::WriterDel, &need_key()?, None)?,
                // FINALIZE is header-only (no key/target); an owner-only seal.
                "FINALIZE" => db.prepare_management(Op::Finalize, "", None)?,
                "WRITERSET" => {
                    let s = scope
                        .as_deref()
                        .ok_or_else(|| bad("WRITERSET requires a scope".into()))?;
                    let parsed = Scope::parse(s).ok_or_else(|| {
                        bad(format!(
                            "invalid scope {s:?}: expected a comma-separated subset of \
                             CREATE,UPDATE,DESTROY"
                        ))
                    })?;
                    db.prepare_management(Op::WriterSet, &need_key()?, Some(&parsed))?
                }
                other => return Err(bad(format!("opcode {other:?} cannot be signed"))),
            };
            // The signature is always the final line; everything before it is
            // the unsigned body (correct even for multi-line SETL values).
            let unsigned = prepared
                .memo_text
                .rsplit_once('\n')
                .map(|(body, _)| body)
                .unwrap_or(prepared.memo_text.as_str())
                .to_owned();
            Ok::<_, ZkvError>(SignMemoResp {
                unsigned,
                signed: prepared.memo_text,
                recipient_ua: prepared.recipient_ua,
                zkv_addr: prepared.zkv_addr,
            })
        })
        .await
    }

    /// Sign a write memo **without broadcasting**, returning the exact memo
    /// text and its recipient UA. Backs the write modal's live "signed memo"
    /// preview: as the user types, the GUI debounces a call here so the block
    /// can show the *real* signature (and a power user can copy the verbatim
    /// memo to broadcast it themselves) instead of a placeholder. The ECDSA
    /// signing runs on the blocking pool like every other facade call, so it
    /// never stalls the UI. Local-only: no chain I/O, no `pending.toml`
    /// record, no broadcast. Safe to call repeatedly while typing.
    /// `op` is `set`/`setl`/`del`/`init` (`set`/`setl` both auto-pick the wire
    /// form from the value, matching the broadcast path). No db lock: like the
    /// read handlers, it only loads local state.
    pub async fn sign_preview(
        &self,
        name: String,
        op: String,
        key: String,
        value: Option<String>,
    ) -> Result<SignPreviewResp, ZkvError> {
        let conn = self.conn.clone();
        run_blocking(move |_| {
            let db = Database::open(&name, conn)?;
            let prepared = match op.as_str() {
                "set" | "setl" => db.prepare_set(&key, value.as_deref().unwrap_or_default())?,
                "del" => db.prepare_del(&key)?,
                "init" => db.prepare_init()?,
                other => {
                    return Err(ZkvError::Other(anyhow::anyhow!(
                        "sign_preview: unknown op {other:?}"
                    )))
                }
            };
            Ok::<_, ZkvError>(SignPreviewResp {
                memo: prepared.memo_text,
                recipient: prepared.recipient_ua,
            })
        })
        .await
    }

    /// Create a brand-new admin database in the given shielded pool,
    /// returning its recovery phrase.
    pub async fn create(
        &self,
        name: String,
        network: Network,
        pool: ShieldedProtocol,
        phrase: Option<String>,
    ) -> Result<CreateResp, ZkvError> {
        let lock = self.db_lock(&name);
        let _guard = lock.lock().await;
        let conn = self.conn.clone();
        run_blocking(move |h| {
            // With a confirmed phrase (the deferred create flow) persist that
            // exact seed; without one, mint a fresh seed in place (CLI parity).
            let (db, phrase) = match phrase {
                Some(p) => (
                    h.block_on(Database::restore_admin_with_pool(
                        &name, network, &p, None, pool, conn,
                    ))?,
                    p,
                ),
                None => h.block_on(Database::init_admin_with_pool(&name, network, pool, conn))?,
            };
            // The one sanctioned exception to "the GUI never writes the CLI
            // `current` marker": the first user-created database claims
            // `current` when it is unset or still the bundled demo
            // (`promote_current`). Later creates and any explicit CLI `use` are
            // left untouched. Best-effort, like the CLI's own init path.
            let _ = crate::demo::promote_current(&name);
            Ok::<_, ZkvError>(CreateResp {
                name: db.name().to_owned(),
                address: db.zkv_address()?,
                phrase,
                funding_address: db.funding_address()?,
            })
        })
        .await
    }

    /// Generate a fresh recovery phrase WITHOUT touching disk. The create flow
    /// shows it for the user to write down, then calls [`Engine::create`] with
    /// the confirmed phrase, so abandoning the flow before confirmation leaves
    /// no database directory and no sidebar entry behind.
    pub fn generate_phrase(&self) -> PhraseResp {
        PhraseResp {
            phrase: Database::generate_phrase(),
        }
    }

    /// Open the data directory in the OS file manager (Finder / Explorer / the
    /// freedesktop default). The gui server always runs on the user's own
    /// machine, so this works for both the desktop and browser transports.
    pub fn open_data_dir(&self) -> Result<OkResp, ZkvError> {
        let dir = data::zkv_data().map_err(ZkvError::Other)?;
        open_in_file_manager(&dir).map_err(ZkvError::Other)?;
        Ok(OkResp { ok: true })
    }

    /// Add a watch-only database from a zkv address.
    pub async fn watch(
        &self,
        zkv_address: String,
        name: Option<String>,
    ) -> Result<AddDbResp, ZkvError> {
        // Resolve the db name up front so we can take its per-db lock.
        let name = name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| derive_watch_name(&zkv_address));
        let lock = self.db_lock(&name);
        let _guard = lock.lock().await;
        let conn = self.conn.clone();
        run_blocking(move |h| {
            let db = h.block_on(Database::init_watch(&name, &zkv_address, conn))?;
            // First user-created database claims the CLI `current` marker if it
            // is unset or still the bundled demo; see `create`. Best-effort.
            let _ = crate::demo::promote_current(&name);
            Ok::<_, ZkvError>(AddDbResp {
                name: db.name().to_owned(),
                role: role_str(db.role()).to_owned(),
            })
        })
        .await
    }

    /// Re-add the bundled "demo-oracles" watch-only database after the user
    /// has deleted it. Drives the Settings "Re-import Oracle Demo" button,
    /// which only appears when [`crate::demo::should_offer_reimport`] is true.
    /// Reuses the normal [`watch`](Self::watch) path (so it takes the per-db
    /// lock and refuses a duplicate) against the bundled demo address.
    pub async fn reimport_demo(&self) -> Result<AddDbResp, ZkvError> {
        let resp = self
            .watch(
                crate::demo::DEMO_ZKV_ADDRESS.to_owned(),
                Some(crate::demo::DEMO_DB_NAME.to_owned()),
            )
            .await?;
        // A manual re-import counts as provisioned, so the one-time
        // auto-provision stays disabled (it already was, but keep it robust).
        let _ = crate::demo::mark_provisioned();
        Ok(resp)
    }

    /// Inspect a `zkv1…` address without adding it: parse it and report the
    /// network, pool, and birthday it commits to. The restore flow calls this
    /// when a valid address is pasted, both to show a network/pool badge and to
    /// resolve the network + birthday for the restore itself (so the user only
    /// has to paste the address and their phrase).
    pub async fn inspect_address(&self, address: String) -> Result<ZkvAddrInfoResp, ZkvError> {
        run_blocking(move |_| {
            let parsed =
                crate::protocol::parse_zkv_addr(address.trim()).map_err(ZkvError::Other)?;
            let network: Network = crate::protocol::network_from_type(parsed.network)
                .map_err(ZkvError::Other)?
                .into();
            Ok::<_, ZkvError>(ZkvAddrInfoResp {
                network: network.name().to_owned(),
                pool: crate::config::pool_label(parsed.pool).to_owned(),
                birthday: parsed.birthday,
            })
        })
        .await
    }

    /// Check whether a 24-word recovery phrase actually controls the database
    /// named by a `zkv1…` address: derive the phrase's account-0 UFVK on the
    /// address's network and compare its receiver identity (under the address's
    /// pool) to the address's own. The restore flow calls this when both an
    /// address and a full phrase are present, so a wrong phrase (or a phrase
    /// pasted against the wrong address) is caught before it spends a sync.
    /// `Ok(true)` means the phrase is this database's admin seed; `Ok(false)` a
    /// valid phrase for a *different* database; an error only if the phrase
    /// isn't a valid BIP-39 mnemonic or the address won't parse.
    pub async fn verify_phrase(&self, phrase: String, address: String) -> Result<bool, ZkvError> {
        run_blocking(move |_| {
            use bip0039::{English, Mnemonic};
            use secrecy::Zeroize as _;
            use zcash_keys::keys::UnifiedSpendingKey;
            use zip32::AccountId;

            let parsed =
                crate::protocol::parse_zkv_addr(address.trim()).map_err(ZkvError::Other)?;
            let params =
                crate::protocol::network_from_type(parsed.network).map_err(ZkvError::Other)?;

            let mut normalized = phrase
                .to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let mnemonic: Mnemonic<English> = Mnemonic::from_phrase(&normalized)
                .map_err(|e| ZkvError::Other(anyhow::anyhow!("invalid recovery phrase: {e}")))?;
            normalized.zeroize();
            let ufvk_seed = {
                let mut seed = mnemonic.to_seed("");
                let usk = UnifiedSpendingKey::from_seed(&params, &seed, AccountId::ZERO);
                seed.zeroize();
                usk.map_err(|e| ZkvError::Other(anyhow::anyhow!("derive key from phrase: {e}")))?
                    .to_unified_full_viewing_key()
            };

            // Same seed ⟹ same single-pool receiver ⟹ same database identity.
            let want = crate::protocol::receiver_domain(&parsed.ufvk, parsed.pool, parsed.network)
                .map_err(ZkvError::Other)?;
            let got = crate::protocol::receiver_domain(&ufvk_seed, parsed.pool, parsed.network)
                .map_err(ZkvError::Other)?;
            Ok::<_, ZkvError>(want == got)
        })
        .await
    }

    /// Restore an admin database from a recovery phrase. `pool` is the shielded
    /// pool the original database lives in (Orchard by default; a pasted zkv
    /// address pins it, so a Sapling database restores correctly). `birthday`
    /// is required: the caller supplies it from the pasted address or an
    /// explicit height. A `None` birthday is rejected rather than guessed, so a
    /// restore never silently starts near the chain tip and misses history.
    pub async fn restore(
        &self,
        name: String,
        phrase: String,
        network: Network,
        pool: ShieldedProtocol,
        birthday: Option<u32>,
    ) -> Result<AddDbResp, ZkvError> {
        // A bare phrase (no zkv address, no height) has no safe starting point:
        // recovering it would mean scanning for the INIT, which is future work.
        let birthday = birthday.ok_or_else(|| {
            ZkvError::Other(anyhow::anyhow!(
                "restoring needs a birthday height or a zkv address"
            ))
        })?;
        let lock = self.db_lock(&name);
        let _guard = lock.lock().await;
        let conn = self.conn.clone();
        run_blocking(move |h| {
            let db = h.block_on(Database::restore_admin_with_pool(
                &name,
                network,
                &phrase,
                Some(birthday),
                pool,
                conn,
            ))?;
            // First user-created database claims the CLI `current` marker if it
            // is unset or still the bundled demo; see `create`. Best-effort.
            let _ = crate::demo::promote_current(&name);
            Ok::<_, ZkvError>(AddDbResp {
                name: db.name().to_owned(),
                role: role_str(db.role()).to_owned(),
            })
        })
        .await
    }

    /// Permanently delete a database's **local** state: the whole
    /// `<data-dir>/<name>/` directory (wallet DB, the snapshot sidecar, the
    /// stored seed, and `keys.toml`). This is a local cache wipe only: the
    /// confirmed writes live on the Zcash chain and stay readable by anyone
    /// holding the database's zkv address. Mirrors the CLI `zkv remove`.
    pub async fn forget(&self, name: String) -> Result<OkResp, ZkvError> {
        // Verify it exists first, so an unknown name surfaces as a clean
        // `UnknownDatabase` rather than a silent success.
        let check = name.clone();
        run_blocking(move |_| {
            WalletConfig::read(&check)
                .map(|_| ())
                .map_err(|e| classify_unknown(e, &check))
        })
        .await?;

        // Hold the per-db lock across the delete so no sync/write (including the
        // background auto-sync, which takes the same lock) is mid-scan on files
        // we're removing.
        let lock = self.db_lock(&name);
        let _guard = lock.lock().await;

        data::erase_wallet_state(&name).await;

        // Clear the "current" marker if it pointed at the forgotten db (there's
        // no "unset" helper; a removed marker reads back as None, like
        // `zkv remove`).
        if data::current_db().ok().flatten().as_deref() == Some(name.as_str()) {
            if let Ok(dir) = data::zkv_data() {
                let _ = std::fs::remove_file(dir.join("current"));
            }
        }

        // Drop the in-memory per-db pause flag; nothing references it now.
        self.paused.lock().unwrap().remove(&name);

        Ok(OkResp { ok: true })
    }

    /// Decrypt and return the admin recovery phrase for `name`, the local
    /// backup of its spending key (the same 24 words shown at creation time).
    /// Drives the Danger Zone "show seed phrase" action. Errors with
    /// [`ZkvError::WatchOnly`] for a watch-only database (no seed on disk) and
    /// [`ZkvError::UnknownDatabase`] for an unknown name.
    pub async fn reveal_phrase(&self, name: String) -> Result<RevealPhraseResp, ZkvError> {
        run_blocking(move |_| {
            let cfg = WalletConfig::read(&name).map_err(|e| classify_unknown(e, &name))?;
            if cfg.role == Role::Watch {
                return Err(ZkvError::WatchOnly);
            }
            let phrase = cfg.decrypt_mnemonic_phrase().map_err(ZkvError::Other)?;
            Ok::<_, ZkvError>(RevealPhraseResp { name, phrase })
        })
        .await
    }

    /// Switch the active ("current") database.
    pub fn set_current(&self, name: String) -> Result<OkResp, ZkvError> {
        data::set_current_db(&name).map_err(ZkvError::Other)?;
        Ok(OkResp { ok: true })
    }

    /// Pause or resume continuous auto-sync for a single database. In-memory
    /// only (resets on restart); no wallet IO.
    pub fn set_pause(&self, name: String, paused: bool) -> PauseResp {
        let mut set = self.paused.lock().unwrap();
        if paused {
            set.insert(name);
        } else {
            set.remove(&name);
        }
        PauseResp { paused }
    }

    /// Toggle the global "pause all syncing" switch. When on,
    /// [`Engine::run_auto_sync`] halts entirely. In-memory only.
    pub fn set_pause_all(&self, paused: bool) -> PauseResp {
        self.paused_all.store(paused, Ordering::Relaxed);
        PauseResp { paused }
    }

    /// Set the number of databases the background loop syncs concurrently.
    /// Clamped to `1..=MAX_SYNC_WORKERS`; takes effect next cycle.
    pub fn set_settings(&self, sync_workers: usize) -> SettingsResp {
        let n = sync_workers.clamp(1, MAX_SYNC_WORKERS);
        self.sync_workers.store(n, Ordering::Relaxed);
        SettingsResp { sync_workers: n }
    }

    /// Render `data` as a scannable QR-code SVG. Pure CPU (no wallet).
    pub fn qr(&self, data: String) -> Result<QrResp, ZkvError> {
        let svg = qr_svg(&data).map_err(ZkvError::Other)?;
        Ok(QrResp { svg })
    }

    /// Detached background task: forever, sync every database that isn't
    /// paused, up to `sync_workers` of them at a time. Per-cycle errors are
    /// logged and never abort the loop. A single panicking sync can't kill
    /// the task either (`run_blocking` turns a `JoinError` into [`ZkvError`],
    /// and a worker that still panics is reported by `JoinSet::join_next` and
    /// ignored).
    pub async fn run_auto_sync(self: Arc<Self>) {
        // First run: provision the bundled "demo-oracles" watch-only database
        // once (best effort). Runs on a blocking thread like every other
        // facade call so the sqlite work doesn't stall an async worker. A
        // failure (e.g. offline) is logged and retried on the next launch.
        {
            let conn = self.conn.clone();
            let res = run_blocking(move |h| {
                h.block_on(crate::demo::ensure(conn))
                    .map_err(ZkvError::Other)
            })
            .await;
            match res {
                Ok(true) => tracing::info!("provisioned the bundled demo database"),
                Ok(false) => {}
                Err(e) => tracing::debug!("demo database not provisioned: {e}"),
            }
        }

        loop {
            // Global pause halts the whole engine until resumed.
            if self.paused_all.load(Ordering::Relaxed) {
                tokio::time::sleep(AUTO_SYNC_INTERVAL).await;
                continue;
            }

            let names = match run_blocking(|_| data::list_dbs().map_err(ZkvError::Other)).await {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!("auto-sync: listing databases failed: {e}");
                    Vec::new()
                }
            };

            // Drop per-db-paused databases up front (lock released immediately).
            let names: Vec<String> = {
                let paused = self.paused.lock().unwrap();
                names.into_iter().filter(|n| !paused.contains(n)).collect()
            };

            let workers = self.sync_workers.load(Ordering::Relaxed).max(1);
            let sem = Arc::new(Semaphore::new(workers));
            let mut set = JoinSet::new();
            for name in names {
                let engine = self.clone();
                let sem = sem.clone();
                set.spawn(async move {
                    // Cap concurrency to `workers`.
                    let _permit = sem.acquire_owned().await.expect("semaphore not closed");
                    // Serialize against same-db work (interactive writes, creates).
                    let lock = engine.db_lock(&name);
                    let _guard = lock.lock().await;
                    let conn = engine.conn.clone();
                    let label = name.clone();
                    if let Err(e) = run_blocking(move |h| {
                        let db = Database::open(&name, conn)?;
                        h.block_on(db.sync())
                    })
                    .await
                    {
                        tracing::warn!("auto-sync {label}: {e}");
                    }
                });
            }
            // Drain this cycle's workers before sleeping. JoinErrors (a worker
            // panicked) are ignored so one bad db can't stop the loop.
            while set.join_next().await.is_some() {}

            tokio::time::sleep(AUTO_SYNC_INTERVAL).await;
        }
    }
}

// ===================================================================
// Helpers
// ===================================================================

/// Base URL of the hosted faucet the "Use our faucet" buttons talk to. All
/// paths live under this root: `/faucet` funds a database's address; `/init`
/// broadcasts a sponsored INIT.
const FAUCET_BASE_URL: &str = "https://zec.rocks/zkv/backend";

/// Classified result of a faucet call. `Ok` carries the (2xx) response body so
/// the caller can pull a `txid` out of it.
enum FaucetCall {
    Ok(String),
    Outdated,
    Error,
}

/// POST a JSON body to the faucet and classify the result. Infallible by
/// design: every transport failure is folded into a variant (and logged) so
/// the GUI's button states map straight from it. Classification:
/// - body mentions "update" (any status): `Outdated` ("Your app is outdated").
/// - the faucet is unreachable (connect/timeout/DNS, i.e. it's no longer
///   running): `Outdated` too, since an old client can't tell a moved/retired
///   endpoint from an upgrade.
/// - reachable but non-2xx: `Error` ("Try again later").
/// - 2xx: `Ok(body)`.
async fn faucet_call(path: &str, body: &serde_json::Value, what: &str) -> FaucetCall {
    let url = format!("{FAUCET_BASE_URL}{path}");
    let result = async {
        let resp = reqwest::Client::new().post(&url).json(body).send().await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        Ok::<_, reqwest::Error>((status, text))
    }
    .await;
    match result {
        Ok((status, text)) => {
            if text.to_ascii_lowercase().contains("update") {
                tracing::warn!(target: "zkv::gui::faucet", %url, %status, body = %text, "faucet {what}: client outdated");
                FaucetCall::Outdated
            } else if !status.is_success() {
                tracing::warn!(target: "zkv::gui::faucet", %url, %status, body = %text, "faucet {what}: HTTP error");
                FaucetCall::Error
            } else {
                tracing::info!(target: "zkv::gui::faucet", %url, %status, body = %text, "faucet {what}: ok");
                FaucetCall::Ok(text)
            }
        }
        // Could not reach the faucet at all (it's down / no longer running):
        // treat it like an outdated client, per product decision.
        Err(e) => {
            tracing::warn!(target: "zkv::gui::faucet", %url, error = %e, "faucet {what}: unreachable");
            FaucetCall::Outdated
        }
    }
}

/// Pull the `txid` field out of a faucet JSON response body, if present.
fn faucet_txid(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("txid").and_then(|t| t.as_str()).map(str::to_owned))
}

/// Run facade work on a blocking thread. The closure receives a runtime
/// [`Handle`] so it can `block_on` the non-`Send` async facade futures.
pub(crate) async fn run_blocking<F, T>(f: F) -> Result<T, ZkvError>
where
    F: FnOnce(Handle) -> Result<T, ZkvError> + Send + 'static,
    T: Send + 'static,
{
    let handle = Handle::current();
    tokio::task::spawn_blocking(move || f(handle))
        .await
        .map_err(|e| ZkvError::Other(anyhow::anyhow!("background task failed: {e}")))?
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::Admin => "admin",
        Role::Watch => "watch",
    }
}

/// Reveal `path` in the OS file manager. Spawns the per-platform opener
/// (`open` / `explorer` / `xdg-open`) and returns once it's launched; the exit
/// status is ignored (Windows `explorer` reports non-zero even on success), so
/// only a failure to spawn the opener at all surfaces as an error.
fn open_in_file_manager(path: &std::path::Path) -> anyhow::Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(path)
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not open {} with {opener}: {e}", path.display()))?;
    Ok(())
}

fn init_parts(init: &InitState) -> (String, u32, u32) {
    match init {
        InitState::Uninitialized => ("uninitialized".to_owned(), 0, 0),
        InitState::Initializing { done, required } => ("initializing".to_owned(), *done, *required),
        InitState::Initialized => ("initialized".to_owned(), 0, 0),
    }
}

/// (keys, unsynced) where `keys` is confirmed-or-incoming keys and
/// `unsynced` is the subset with in-flight (pending) writes.
fn count_keys(state: &BTreeMap<String, KeyState>) -> (usize, usize) {
    let mut keys = 0;
    let mut unsynced = 0;
    for ks in state.values() {
        let has_set = matches!(ks.pending.last(), Some(PendingOp::Set { .. }));
        if ks.confirmed.is_some() || has_set {
            keys += 1;
        }
        if !ks.pending.is_empty() {
            unsynced += 1;
        }
    }
    (keys, unsynced)
}

fn key_rows(state: &BTreeMap<String, KeyState>) -> Vec<KeyRow> {
    let mut rows = Vec::new();
    for (key, ks) in state {
        let (value, status, txid, deleted) = match ks.pending.last() {
            Some(PendingOp::Set {
                value,
                done,
                required,
                txid,
            }) => {
                let kind = if *done == 0 { "pending" } else { "confirming" };
                let shown = ks.confirmed.clone().unwrap_or_else(|| value.clone());
                (
                    Some(shown),
                    KeyStatus {
                        kind: kind.to_owned(),
                        done: *done,
                        required: *required,
                    },
                    Some(txid.clone()),
                    false,
                )
            }
            Some(PendingOp::Del {
                done,
                required,
                txid,
            }) => (
                ks.confirmed.clone(),
                KeyStatus {
                    kind: "deleting".to_owned(),
                    done: *done,
                    required: *required,
                },
                Some(txid.clone()),
                true,
            ),
            None => (
                ks.confirmed.clone(),
                KeyStatus {
                    kind: "confirmed".to_owned(),
                    done: 0,
                    required: 0,
                },
                // The confirmed value's own write txid (for the "view in
                // history" jump), cached on the snapshot row.
                ks.last_txid.clone(),
                false,
            ),
        };

        // Skip keys with nothing to show (e.g. a deleted key that's
        // fully confirmed gone).
        if value.is_none() && !deleted {
            continue;
        }

        let size = value.as_ref().map(|v| v.len());
        rows.push(KeyRow {
            key: key.clone(),
            value,
            status,
            txid,
            deleted,
            size,
            updated_at: ks.updated_at,
        });
    }
    rows
}

/// Map a facade [`HistoryResult`] page into the DTO. The facade already
/// orders newest-first with in-flight writes pinned on top, so this is a
/// straight field map.
fn history_resp(result: HistoryResult, auth: Option<&AuthRegistry>) -> HistoryResp {
    let entries = result
        .entries
        .into_iter()
        .map(|e| {
            // Resolve the per-entry signer to its *current* registry role
            // (owner/writer) for the label + Roles link. This reflects the
            // present registry, not the historical role at write time.
            let signer_role = auth
                .zip(e.signer.as_deref())
                .and_then(|(a, s)| a.authority_of(s))
                .map(|authority| match authority {
                    Authority::Owner => "owner".to_owned(),
                    Authority::Writer(_) => "writer".to_owned(),
                });
            let status = match e.status {
                HistoryStatus::Confirmed { confirmations } => HistoryStatusResp {
                    kind: "confirmed".to_owned(),
                    done: 0,
                    required: 0,
                    confirmations,
                },
                HistoryStatus::Confirming { done, required } => HistoryStatusResp {
                    kind: "confirming".to_owned(),
                    done,
                    required,
                    confirmations: 0,
                },
                HistoryStatus::Pending => HistoryStatusResp {
                    kind: "pending".to_owned(),
                    done: 0,
                    required: 0,
                    confirmations: 0,
                },
            };
            HistoryEntryResp {
                op: e.op.as_str().to_owned(),
                key: e.key,
                value: e.value,
                height: e.height,
                timestamp: e.timestamp,
                txid: e.txid,
                output_index: e.output_index,
                signature: e.signature,
                seq: e.seq,
                signer: e.signer,
                signer_role,
                verified: e.verified,
                status,
                memo: e.memo,
                fee: e.fee,
                output_value: e.output_value,
            }
        })
        .collect();
    HistoryResp {
        creator: result.signer,
        entries,
        total: result.total,
        offset: result.offset,
        limit: result.limit,
    }
}

/// Project an audit into just the rejected rows (newest-first), each with its
/// standardized [`DropReason`] rendered to a string. Applied/pending rows are
/// dropped; this view is only the rejections.
fn rejections_resp(audit: AuditResult) -> RejectionsResp {
    let mut entries: Vec<RejectionResp> = audit
        .rows
        .into_iter()
        .filter_map(|r| match r.outcome {
            RowOutcome::Dropped(reason) => Some(RejectionResp {
                op: r.op.map(|o| o.as_str().to_owned()),
                key: r.key,
                value: r.value,
                height: r.mined_height,
                timestamp: r.timestamp,
                txid: r.txid,
                raw: r.raw,
                reason: reason.to_string(),
                // A recovered signer means the signature is valid, so the
                // rejection is an authorization/lifecycle decision; let the UI
                // split "Valid Signature ✓ / Authorized ✗".
                signer: r.signer,
                signature_valid: !reason.is_signature_failure(),
            }),
            RowOutcome::Applied | RowOutcome::Pending => None,
        })
        .collect();
    // replay_audit yields chain order (oldest-first); the GUI shows newest-first.
    entries.reverse();
    let total = entries.len() as u64;
    RejectionsResp { entries, total }
}

/// Map the authorization registry into the JSON DTO: owners first (full
/// authority, empty capability list), then scoped writers; each group in the
/// registry's canonical `BTree` order. Pubkeys (rows, `creator`, and revoked
/// tombstones) are the canonical `zkvid1…` strings the registry already keys
/// on, so the frontend's `row.pubkey === creator` "this is the creator" match
/// works directly.
fn roles_resp(
    auth: &AuthRegistry,
    granted: Vec<GrantedRole>,
    revoked: Vec<RevokedRole>,
    creator: Option<String>,
) -> RolesResp {
    // The active set stays sourced from the (well-tested) registry projection;
    // `granted` only enriches each row with when/by-whom it was granted. A
    // pubkey absent from `granted` (only possible under audit/read skew) just
    // shows no grant date rather than vanishing from the list.
    let prov: HashMap<&str, &GrantedRole> =
        granted.iter().map(|g| (g.pubkey.as_str(), g)).collect();
    let mut rows = Vec::new();
    for owner in auth.owners() {
        let g = prov.get(owner);
        rows.push(RoleRow {
            role: "owner".to_owned(),
            pubkey: owner.to_owned(),
            capabilities: Vec::new(),
            height: g.and_then(|g| g.height),
            timestamp: g.and_then(|g| g.timestamp),
            granted_by: g.and_then(|g| g.granted_by.clone()),
        });
    }
    for (writer, scope) in auth.writers() {
        let g = prov.get(writer);
        rows.push(RoleRow {
            role: "writer".to_owned(),
            pubkey: writer.to_owned(),
            capabilities: scope
                .capabilities()
                .map(|c| c.as_str().to_owned())
                .collect(),
            height: g.and_then(|g| g.height),
            timestamp: g.and_then(|g| g.timestamp),
            granted_by: g.and_then(|g| g.granted_by.clone()),
        });
    }
    let revoked = revoked
        .into_iter()
        .map(|r| RevokedRoleRow {
            role: if r.was_owner { "owner" } else { "writer" }.to_owned(),
            pubkey: r.pubkey,
            capabilities: r.capabilities,
            height: r.height,
            timestamp: r.timestamp,
            revoked_by: r.revoked_by,
        })
        .collect();
    RolesResp {
        creator,
        rows,
        revoked,
    }
}

/// Map a facade [`FundingResult`] page into the DTO. The facade already orders
/// newest-first with mempool pinned on top, so this is a straight field map.
fn funding_resp(result: FundingResult) -> FundingResp {
    let entries = result
        .entries
        .into_iter()
        .map(|t| FundingTxResp {
            txid: t.txid,
            height: t.height,
            timestamp: t.timestamp,
            direction: match t.direction {
                FundingDirection::Received => "received".to_owned(),
                FundingDirection::Sent => "sent".to_owned(),
                FundingDirection::SelfTransfer => "self".to_owned(),
                FundingDirection::ZkvOperation => "zkv".to_owned(),
            },
            amount: t.amount,
            self_sent: t.self_sent,
            fee: t.fee,
            memo: t.memo,
            recipients: t.recipients,
            is_zkv: t.is_zkv,
            pending: t.pending,
            confirmations: t.confirmations,
            required: t.required,
            confirmed: t.confirmed,
        })
        .collect();
    FundingResp {
        entries,
        total: result.total,
        offset: result.offset,
        limit: result.limit,
    }
}

/// Derive a local nickname for a watched address (`watch-<8 chars>`),
/// matching the CLI's `zkv watch` fallback shape.
fn derive_watch_name(addr: &str) -> String {
    let suffix: String = addr
        .split(':')
        .nth(1)
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .skip(6) // past the "uview1" HRP
        .take(8)
        .collect();
    if suffix.is_empty() {
        "watch".to_owned()
    } else {
        format!("watch-{suffix}")
    }
}

/// Human-facing lightwalletd endpoint, e.g. `zec.rocks:443`. Falls back
/// to the operator name when the operator doesn't serve that network.
fn server_endpoint(conn: &ConnectionArgs, network: consensus::Network) -> String {
    match conn.server.pick(network) {
        Ok(s) => s.to_string(),
        Err(_) => match &conn.server {
            Servers::Hosted(op) => format!("{op:?}").to_lowercase(),
            Servers::Custom(_) => "custom".to_owned(),
        },
    }
}

/// Classify a `WalletConfig::read` failure: a missing db/keys file becomes a
/// structured [`ZkvError::UnknownDatabase`]; anything else is opaque.
pub(crate) fn classify_unknown(e: anyhow::Error, name: &str) -> ZkvError {
    let msg = format!("{e:#}");
    if msg.contains("no database named") || msg.contains("no keys.toml") {
        ZkvError::UnknownDatabase(name.to_owned())
    } else {
        ZkvError::Other(e)
    }
}

/// Encode `data` to a QR matrix and emit a self-contained SVG: a white
/// background (with a 4-module quiet zone) plus one path of black squares
/// for the dark modules.
fn qr_svg(data: &str) -> anyhow::Result<String> {
    use std::fmt::Write as _;

    let code =
        qrcode::QrCode::new(data.as_bytes()).map_err(|e| anyhow::anyhow!("encode QR: {e}"))?;
    let n = code.width();
    let colors = code.to_colors();
    let quiet = 4usize;
    let dim = n + quiet * 2;

    let mut path = String::new();
    for y in 0..n {
        for x in 0..n {
            if matches!(colors[y * n + x], qrcode::Color::Dark) {
                let _ = write!(path, "M{} {}h1v1h-1z", x + quiet, y + quiet);
            }
        }
    }

    Ok(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {dim} {dim}\" \
         shape-rendering=\"crispEdges\" role=\"img\" aria-label=\"QR code\">\
         <rect width=\"{dim}\" height=\"{dim}\" fill=\"#ffffff\"/>\
         <path d=\"{path}\" fill=\"#000000\"/></svg>"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_svg_renders_scannable_svg() {
        let svg = qr_svg("zkv1test1").unwrap();
        assert!(svg.starts_with("<svg"), "must be an svg document");
        assert!(svg.contains("viewBox=\"0 0 "), "scales via viewBox");
        assert!(svg.contains("<rect"), "has a white quiet-zone background");
        assert!(svg.contains("<path d=\"M"), "has dark-module path data");
        assert!(svg.ends_with("</svg>"));
        // Deterministic, and distinct inputs differ.
        assert_eq!(svg, qr_svg("zkv1test1").unwrap());
        assert_ne!(svg, qr_svg("zkv1test2").unwrap());
    }

    #[test]
    fn history_resp_maps_fields_and_paging() {
        use crate::protocol::{HistoryEntry, Op};

        let mk = |key: &str, status: HistoryStatus| HistoryEntry {
            op: Op::Set,
            key: key.to_owned(),
            value: Some("v".to_owned()),
            height: Some(10),
            timestamp: Some(1_700_000_000),
            txid: format!("tx-{key}"),
            output_index: 0,
            signature: Some("ab".to_owned()),
            seq: Some(0),
            signer: Some("cafe".to_owned()),
            verified: Some(true),
            status,
            memo: Some("ZKV0 SET k v\n<sig>".to_owned()),
            fee: Some(10_000),
            output_value: None,
        };
        // The facade already orders newest-first with in-flight pinned;
        // history_resp keeps that order and just maps + passes paging through.
        let result = HistoryResult {
            signer: "deadbeef".to_owned(),
            entries: vec![
                mk("newest", HistoryStatus::Pending),
                mk(
                    "mid",
                    HistoryStatus::Confirming {
                        done: 1,
                        required: 3,
                    },
                ),
                mk("oldest", HistoryStatus::Confirmed { confirmations: 9 }),
            ],
            total: 42,
            offset: 0,
            limit: Some(100),
        };

        let resp = history_resp(result, None);
        assert_eq!(resp.creator, "deadbeef");
        assert_eq!(resp.total, 42);
        assert_eq!(resp.limit, Some(100));
        // Per-entry signer passes through; with no registry there is no role.
        assert_eq!(resp.entries[0].signer.as_deref(), Some("cafe"));
        assert_eq!(resp.entries[0].signer_role, None);
        // Order preserved.
        assert_eq!(resp.entries[0].key, "newest");
        assert_eq!(resp.entries[2].key, "oldest");
        // Status kind + numeric fields map across the three variants.
        assert_eq!(resp.entries[0].status.kind, "pending");
        assert_eq!(resp.entries[1].status.kind, "confirming");
        assert_eq!(resp.entries[1].status.done, 1);
        assert_eq!(resp.entries[1].status.required, 3);
        assert_eq!(resp.entries[2].status.kind, "confirmed");
        assert_eq!(resp.entries[2].status.confirmations, 9);
        assert_eq!(resp.entries[0].op, "SET");
    }

    #[test]
    fn rejections_resp_keeps_only_dropped_newest_first() {
        use crate::protocol::{AuditResult, AuditRow, DropReason, InitState, Op, RowOutcome};
        use std::collections::BTreeMap;

        let row = |h: u32, op: Op, key: &str, outcome: RowOutcome| AuditRow {
            mined_height: Some(h),
            timestamp: None,
            txid: format!("tx{h}"),
            op: Some(op),
            key: Some(key.to_owned()),
            value: Some("v".to_owned()),
            raw: String::new(),
            signer: None,
            outcome,
        };
        // Chain order (oldest-first): a mix of applied / pending / dropped.
        let audit = AuditResult {
            rows: vec![
                row(1, Op::Set, "a", RowOutcome::Applied),
                row(
                    2,
                    Op::Set,
                    "b",
                    RowOutcome::Dropped(DropReason::NoWriteAuthority),
                ),
                row(3, Op::Del, "c", RowOutcome::Pending),
                row(
                    4,
                    Op::OwnerDel,
                    "d",
                    RowOutcome::Dropped(DropReason::LastOwnerProtected),
                ),
            ],
            init: InitState::Initialized,
            state: BTreeMap::new(),
            auth: Default::default(),
            version: Default::default(),
        };

        let resp = rejections_resp(audit);
        // Only the two dropped rows survive, newest-first, with their reasons.
        assert_eq!(resp.total, 2);
        assert_eq!(resp.entries.len(), 2);
        assert_eq!(resp.entries[0].height, Some(4));
        assert_eq!(resp.entries[0].op.as_deref(), Some("OWNERDEL"));
        assert_eq!(resp.entries[0].reason, "attempt to remove the last owner");
        assert_eq!(resp.entries[1].height, Some(2));
        assert_eq!(resp.entries[1].reason, "signer has no write authority");
    }

    #[test]
    fn roles_resp_maps_owners_then_writers_with_caps() {
        use crate::protocol::{pubkey_bech32, pubkey_of, Op};

        // Two real keys so the writer survives `apply_management`'s pubkey
        // canonicalization; the owner just needs to be present. Identities are
        // the canonical `zkvid1…` form.
        let owner_key = pubkey_bech32(&pubkey_of(
            &secp256k1::SecretKey::from_slice(&[3u8; 32]).unwrap(),
        ));
        let writer_key = pubkey_bech32(&pubkey_of(
            &secp256k1::SecretKey::from_slice(&[7u8; 32]).unwrap(),
        ));

        let mut auth = AuthRegistry::default();
        auth.insert_owner(owner_key.clone());
        let _ = auth.apply_management(Op::WriterSet, &writer_key, Some("CREATE,DESTROY"));

        let resp = roles_resp(&auth, Vec::new(), Vec::new(), Some(owner_key.clone()));

        // Creator passes through as the canonical zkvid1…; owners before writers.
        assert_eq!(resp.creator.as_deref(), Some(owner_key.as_str()));
        assert!(owner_key.starts_with("zkvid1"));
        assert_eq!(resp.rows.len(), 2);
        assert_eq!(resp.rows[0].role, "owner");
        assert_eq!(resp.rows[0].pubkey, owner_key);
        assert!(
            resp.rows[0].capabilities.is_empty(),
            "owners carry no scope"
        );
        assert_eq!(resp.rows[1].role, "writer");
        assert_eq!(resp.rows[1].pubkey, writer_key);
        // Capabilities map in canonical order (CREATE < UPDATE < DESTROY).
        assert_eq!(resp.rows[1].capabilities, vec!["CREATE", "DESTROY"]);

        // An empty registry maps to no rows but still carries the signer.
        let empty = roles_resp(&AuthRegistry::default(), Vec::new(), Vec::new(), None);
        assert!(empty.rows.is_empty());
        assert!(empty.creator.is_none());
    }

    #[test]
    fn funding_resp_maps_fields_and_direction() {
        use crate::db::{FundingDirection, FundingResult, FundingTx};

        let result = FundingResult {
            entries: vec![
                FundingTx {
                    txid: "tx-in".to_owned(),
                    height: None,
                    timestamp: None,
                    direction: FundingDirection::Received,
                    amount: 100_000_000,
                    self_sent: None,
                    fee: None,
                    memo: Some("thanks".to_owned()),
                    recipients: vec![],
                    is_zkv: false,
                    pending: true,
                    confirmations: 0,
                    required: 10,
                    confirmed: false,
                },
                FundingTx {
                    txid: "tx-out".to_owned(),
                    height: Some(42),
                    timestamp: Some(1_700_000_000),
                    direction: FundingDirection::Sent,
                    amount: 50_000_000,
                    self_sent: None,
                    fee: Some(10_000),
                    memo: None,
                    recipients: vec!["u1recipient".to_owned()],
                    is_zkv: false,
                    pending: false,
                    confirmations: 5,
                    required: 3,
                    confirmed: true,
                },
                FundingTx {
                    txid: "tx-self".to_owned(),
                    height: Some(43),
                    timestamp: Some(1_700_000_100),
                    direction: FundingDirection::SelfTransfer,
                    amount: 10_000,
                    self_sent: Some(25_000_000),
                    fee: Some(10_000),
                    memo: None,
                    recipients: vec![],
                    is_zkv: false,
                    pending: false,
                    confirmations: 2,
                    required: 3,
                    confirmed: false,
                },
                FundingTx {
                    txid: "tx-zkv".to_owned(),
                    height: Some(44),
                    timestamp: Some(1_700_000_200),
                    direction: FundingDirection::ZkvOperation,
                    amount: 10_000,
                    self_sent: None,
                    fee: Some(10_000),
                    memo: None,
                    recipients: vec![],
                    is_zkv: true,
                    pending: false,
                    confirmations: 4,
                    required: 3,
                    confirmed: true,
                },
            ],
            total: 4,
            offset: 0,
            limit: Some(100),
        };

        let resp = funding_resp(result);
        assert_eq!(resp.total, 4);
        assert_eq!(resp.entries.len(), 4);
        assert_eq!(resp.entries[0].direction, "received");
        assert_eq!(resp.entries[0].amount, 100_000_000);
        assert_eq!(resp.entries[0].fee, None);
        assert!(resp.entries[0].pending);
        assert!(!resp.entries[0].is_zkv);
        assert_eq!(resp.entries[1].direction, "sent");
        assert_eq!(resp.entries[1].fee, Some(10_000));
        assert_eq!(resp.entries[1].recipients, vec!["u1recipient".to_owned()]);
        assert!(!resp.entries[1].pending);
        assert_eq!(resp.entries[2].direction, "self");
        assert_eq!(resp.entries[2].amount, 10_000);
        assert_eq!(resp.entries[2].self_sent, Some(25_000_000));
        assert_eq!(resp.entries[2].fee, Some(10_000));
        // A bare-fee zkv write maps to the "zkv" direction and flags is_zkv so
        // the detail pane can link to the write in History.
        assert_eq!(resp.entries[3].direction, "zkv");
        assert_eq!(resp.entries[3].amount, 10_000);
        assert_eq!(resp.entries[3].fee, Some(10_000));
        assert!(resp.entries[3].is_zkv);
        // Confirmation status rides through unchanged.
        assert_eq!(resp.entries[0].confirmations, 0);
        assert_eq!(resp.entries[0].required, 10);
        assert!(!resp.entries[0].confirmed);
        assert!(resp.entries[1].confirmed);
        assert_eq!(resp.entries[2].required, 3);
        assert!(!resp.entries[2].confirmed);
    }
}
