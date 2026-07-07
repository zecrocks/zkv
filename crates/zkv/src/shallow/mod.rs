//! Shallow sync: chain-window reads without a local wallet.
//!
//! **Experimental.** Public, but not yet covered by semver.
//!
//! Built for price-oracle consumers: instead of scanning the chain from the
//! database birthday through the full wallet stack, a [`ShallowClient`] looks
//! only at a recent block window, trial-decrypting compact blocks with the
//! address's viewing key and validating each memo's signature statelessly. It
//! shows only keys updated within the window, by design.
//!
//! ```no_run
//! use zkv::{remote::ConnectionArgs, shallow::{ShallowClient, ShallowOptions}};
//!
//! # async fn run() -> Result<(), zkv::shallow::ShallowError> {
//! let mut client = ShallowClient::from_address("zkv1...", &ConnectionArgs::default()).await?;
//! let opts = ShallowOptions::default();
//! let state = client.find(&["prices/zec_usd".into()], &opts).await?;
//! if let Some(update) = state.latest.get("prices/zec_usd") {
//!     println!("zec_usd = {:?} (height {})", update.value, update.height);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Trust model (read this)
//!
//! Shallow reads are **weaker than a full sync** in specific, surfaced ways:
//!
//! - **Values cannot be forged.** Every memo's recoverable signature is
//!   verified against the database's receiver domain; an entry is `verified`
//!   only when the recovered signer is the address-derived root key (or an
//!   explicitly supplied extra signer). What's lost is *context*, not
//!   signature integrity.
//! - **No replay high-water.** A verified old memo re-broadcast inside the
//!   window can masquerade as fresh. Mitigations: chain-order last-write-wins,
//!   the `seq`/`signer` fields on every [`ShallowUpdate`] (enforce monotonic
//!   sequence yourself if you need strictness), and the
//!   [`ShallowWarning::SeqOrderMismatch`] rebroadcast tell.
//! - **Delegated writers show `verified: false`** (the on-chain owner/writer
//!   registry needs a full replay); their writes never win a key.
//! - **Management ops are not applied.** OWNER*/WRITER*/FINALIZE/VERSION memos
//!   in the window surface as [`ShallowWarning::ManagementSeen`]; authority
//!   changes are invisible until a full sync.
//! - **lightwalletd is trusted for completeness.** It cannot forge values
//!   (signatures) but it can *omit* blocks or transactions.
//! - **The INIT anchor is what pins database identity** (a never-initialized
//!   database's writes are all dropped by a full replay, so shallow must not
//!   present them as live state). It is verified by default; skipping it
//!   ([`ShallowOptions::verify_init`]) trades that away.
//!
//! Shallow never writes to the trusted local state (`data.sqlite`,
//! `zkv_state.sqlite`, `pending.toml`) and takes no database lock: it is a
//! pure read that can run concurrently with a full sync. The only file it
//! ever touches is the optional INIT-verification cache
//! (`shallow_init.toml`) inside an existing database directory, which is safe
//! to delete.

mod decrypt;
mod source;
#[cfg(test)]
pub(crate) mod testutil;
mod validate;

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use zcash_primitives::transaction::TxId;
use zcash_protocol::ShieldedPool;

use crate::{
    config::WalletConfig,
    data,
    internal::account,
    protocol::{
        network_from_type, parse_zkv_addr, pubkey_bech32, receiver_domain, zkv_verifying_pubkey,
    },
    remote::ConnectionArgs,
};

pub use source::{ChainSource, GrpcSource};
use validate::{validate, InitObservation, RawHit, Validated};
pub use validate::{ShallowCursor, ShallowUpdate, ShallowWarning};

/// Default look-back window, ~1 hour at 75-second blocks: the `scan` window
/// and the `find` search bound alike. Shallow stays shallow by default; a
/// caller that wants a deeper search raises [`ShallowOptions::max_depth`]
/// (CLI `--max-depth`) explicitly.
pub const DEFAULT_SCAN_DEPTH: u32 = 48;

/// How many already-processed blocks a poll re-fetches behind the cursor, so a
/// shallow reorg can't hide an update. Matches the wallet stack's maximum
/// self-rewind. (A poll widens this to `min_confirmations` when that is
/// larger, so an update first seen below the confirmation threshold is still
/// re-examined once it is deep enough.)
pub const REORG_MARGIN: u32 = 10;

/// Compact blocks per `GetBlockRange` request.
const CHUNK: u32 = 1_000;

/// The INIT-verification cache file inside a database directory. A cache of a
/// chain fact; safe to delete (it is re-verified on the next shallow read).
const INIT_CACHE_FILE: &str = "shallow_init.toml";

/// Options for the shallow read drivers. `Default` gives the documented
/// defaults; construct with struct-update syntax for overrides.
#[derive(Clone, Debug)]
pub struct ShallowOptions {
    /// Minimum confirmations for an update to be considered (default 3,
    /// matching `zkv get`). Shallow has no mempool path; 0 behaves as 1.
    pub min_confirmations: u32,
    /// [`ShallowClient::scan`] window size in blocks.
    pub depth: u32,
    /// [`ShallowClient::find`] backward-search bound in blocks (clamped at the
    /// database birthday). Defaults to [`DEFAULT_SCAN_DEPTH`] (~1 hour):
    /// shallow stays shallow unless the caller explicitly asks for a deeper
    /// search.
    pub max_depth: u32,
    /// Verify the root-signed INIT anchor before trusting window data
    /// (default true; see the module docs).
    pub verify_init: bool,
    /// How far past the birthday the INIT walk searches before giving up.
    pub init_scan_limit: u32,
    /// Additional accepted signers (canonical `zkvid1…`), e.g. delegated
    /// writers known out-of-band or seeded from a full snapshot's registry.
    pub extra_signers: BTreeSet<String>,
}

impl Default for ShallowOptions {
    fn default() -> Self {
        Self {
            min_confirmations: 3,
            depth: DEFAULT_SCAN_DEPTH,
            max_depth: DEFAULT_SCAN_DEPTH,
            verify_init: true,
            init_scan_limit: 10_000,
            extra_signers: BTreeSet::new(),
        }
    }
}

/// The verified INIT anchor: where the database's root-signed INIT memo was
/// observed on chain.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InitAnchor {
    pub height: u32,
    /// Display-order transaction id hex.
    pub txid: String,
    /// The raw on-chain INIT memo text, verbatim (the `ZKV0 INIT …` header
    /// plus its signature line). Chain-derivable, kept so the anchor can be
    /// re-verified or displayed offline. `None` on older cache files that
    /// predate this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memo: Option<String>,
}

/// The inclusive block range a shallow read examined.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ScannedRange {
    pub from: u32,
    pub to: u32,
}

/// The result of one shallow read: everything observed in the window, the
/// per-key winners, the trust-model warnings, and a cursor to poll forward
/// from.
#[derive(Clone, Debug, Serialize)]
pub struct ShallowState {
    /// The chain tip the read was anchored to.
    pub tip: u32,
    pub scanned: ScannedRange,
    /// Every classified data op in chain order (verified or not).
    pub updates: Vec<ShallowUpdate>,
    /// Winner per key, last-write-wins among **verified** entries. A `DEL`
    /// winner has `value: None` ("confirmed deleted in the window"); a key
    /// absent here was simply not updated (verifiably) in the window.
    pub latest: std::collections::BTreeMap<String, ShallowUpdate>,
    pub warnings: Vec<ShallowWarning>,
    /// The verified INIT anchor, when verification ran (or was cached).
    pub init: Option<InitAnchor>,
    /// Resume point for [`ShallowClient::poll`].
    pub cursor: ShallowCursor,
}

/// Errors from the shallow read path. Deliberately not [`crate::db::ZkvError`]:
/// the facade's variants describe local-wallet lifecycle states that don't
/// exist here.
#[derive(Debug, thiserror::Error)]
pub enum ShallowError {
    /// The supplied string is not a valid zkv address.
    #[error("invalid zkv address: {0}")]
    Address(String),
    /// Could not reach (or refused) the lightwalletd server.
    #[error("lightwalletd connection failed: {0}")]
    Connect(#[source] anyhow::Error),
    /// No root-signed INIT was found in the search range. The database may
    /// never have been initialized, the address may be wrong, or the INIT may
    /// sit past the scan cap.
    #[error(
        "no root-signed INIT found between blocks {from} and {to}; this database may never \
         have been initialized, the address may be wrong, or its INIT sits past the scan \
         cap. Run a full `zkv sync` to be sure, or skip the anchor check (--no-verify-init)"
    )]
    InitNotFound { from: u32, to: u32 },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// A read-only chain-window client for one database identity.
///
/// Constructed either from a bare `zkv1…` address ([`ShallowClient::from_address`],
/// fully stateless, nothing on disk) or from a local database
/// ([`ShallowClient::from_db`], read-only, which additionally enables the
/// INIT-verification cache in the database directory).
///
/// Generic over its [`ChainSource`] (defaulting to the lightwalletd
/// [`GrpcSource`]) so the drivers are testable against an in-memory chain;
/// the public constructors always build the gRPC form.
pub struct ShallowClient<C: ChainSource = GrpcSource> {
    network: crate::network::Network,
    pool: ShieldedPool,
    birthday: u32,
    /// The signing-domain receiver every `ZKV0` signature binds to.
    receiver: String,
    /// The address-derived root signer, canonical `zkvid1…`.
    root_hex: String,
    zkv_addr: String,
    source: C,
    /// `Some(<data-dir>/<db>)` when constructed from a local database:
    /// enables the INIT cache. Never created; only an existing dir is used.
    cache_dir: Option<PathBuf>,
    /// Memoized verified INIT for this client's lifetime.
    init_anchor: Option<InitAnchor>,
}

impl ShallowClient {
    /// Build a client from a bare zkv address: the database identity (UFVK,
    /// birthday, network, pool, root key) is derived entirely from the
    /// address. No local database directory is read or written.
    ///
    /// The connection verifies the server's reported chain against the
    /// address's network, so an address-only read can't silently scan the
    /// wrong chain.
    pub async fn from_address(addr: &str, conn: &ConnectionArgs) -> Result<Self, ShallowError> {
        let parsed = parse_zkv_addr(addr).map_err(|e| ShallowError::Address(e.to_string()))?;
        let network =
            network_from_type(parsed.network).map_err(|e| ShallowError::Address(e.to_string()))?;
        let receiver = receiver_domain(&parsed.ufvk, parsed.pool, parsed.network)?;
        let root_hex = pubkey_bech32(&zkv_verifying_pubkey(&parsed.ufvk)?);
        let ivk = decrypt::prepare_ivk(&parsed.ufvk, parsed.pool)?;
        let client = conn.connect(network).await.map_err(ShallowError::Connect)?;
        let source = GrpcSource {
            client,
            network,
            pool: parsed.pool,
            ufvk: parsed.ufvk,
            ivk,
        };
        Ok(Self {
            network,
            pool: parsed.pool,
            birthday: parsed.birthday,
            receiver,
            root_hex,
            zkv_addr: addr.to_owned(),
            source,
            cache_dir: None,
            init_anchor: None,
        })
    }

    /// Build a client from a local database (watch or admin), read-only.
    /// Watch databases carry their address in `keys.toml`; admin databases
    /// derive it from the wallet account (the same derivation `zkv address`
    /// performs). Enables the INIT cache in the database directory.
    pub async fn from_db(db_name: &str, conn: &ConnectionArgs) -> Result<Self, ShallowError> {
        let cfg = WalletConfig::read(db_name)?;
        let addr = match &cfg.zkv_address {
            Some(a) => a.clone(),
            None => account::account_keys(&cfg, db_name)?.zkv_addr,
        };
        let mut me = Self::from_address(&addr, conn).await?;
        me.cache_dir = Some(data::db_dir(db_name)?);
        Ok(me)
    }
}

impl<C: ChainSource> ShallowClient<C> {
    /// Build a client over an arbitrary [`ChainSource`] with an explicit
    /// identity. The test seam (the public constructors derive everything
    /// from a real address and connect over gRPC).
    #[cfg(test)]
    pub(crate) fn with_source(
        source: C,
        receiver: String,
        root_hex: String,
        birthday: u32,
        cache_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            network: crate::network::Network::Test,
            pool: ShieldedPool::Orchard,
            birthday,
            receiver,
            root_hex,
            zkv_addr: String::new(),
            source,
            cache_dir,
            init_anchor: None,
        }
    }

    /// The canonical `zkv1…` address this client reads.
    pub fn address(&self) -> &str {
        &self.zkv_addr
    }

    pub fn network(&self) -> crate::network::Network {
        self.network
    }

    pub fn pool(&self) -> ShieldedPool {
        self.pool
    }

    pub fn birthday(&self) -> u32 {
        self.birthday
    }

    /// All validated updates within the last [`ShallowOptions::depth`] blocks.
    pub async fn scan(&mut self, opts: &ShallowOptions) -> Result<ShallowState, ShallowError> {
        let tip = self.tip().await?;
        let init = self.ensure_init(opts, tip).await?;
        let lo = self
            .birthday
            .max(tip.saturating_sub(opts.depth.saturating_sub(1)));
        let raw = self.fetch_and_enhance(lo, tip, tip).await?;
        let v = self.validated(raw, tip, opts);
        Ok(self.state(tip, lo, tip, v, init, opts))
    }

    /// Walk backward from the tip (newest first) until every requested key
    /// has a **verified** winner, bounded by [`ShallowOptions::max_depth`]
    /// and the birthday. Keys that never resolved are simply absent from
    /// [`ShallowState::latest`].
    pub async fn find(
        &mut self,
        keys: &[String],
        opts: &ShallowOptions,
    ) -> Result<ShallowState, ShallowError> {
        let targets: Vec<String> = keys.to_vec();
        self.find_where(
            opts,
            move |latest| targets.iter().all(|k| latest.contains_key(k)),
            0,
        )
        .await
    }

    /// Generic backward search: walk newest-first, enhancing one block's hits
    /// at a time, until `satisfied` reports the per-key winners so far are
    /// enough, then keep going `grace` more blocks below the first satisfying
    /// height before stopping (so near-simultaneous sibling updates, e.g. a
    /// glob's other keys written a few blocks earlier, are still caught).
    ///
    /// Per-height granularity is what keeps this cheap: each candidate
    /// transaction costs a `GetTransaction` round trip, so a search that
    /// resolves in the newest few blocks performs a handful of fetches rather
    /// than enhancing a whole chunk (let alone the whole `max_depth` window).
    /// [`ShallowState::scanned`] reports the range actually covered.
    pub async fn find_where<F>(
        &mut self,
        opts: &ShallowOptions,
        satisfied: F,
        grace: u32,
    ) -> Result<ShallowState, ShallowError>
    where
        F: Fn(&std::collections::BTreeMap<String, ShallowUpdate>) -> bool,
    {
        let tip = self.tip().await?;
        let init = self.ensure_init(opts, tip).await?;
        let floor = self
            .birthday
            .max(tip.saturating_sub(opts.max_depth.saturating_sub(1)));
        let mut raw: Vec<RawHit> = Vec::new();
        let mut v = Validated::default();
        let mut stepper = validate::FindStepper::new(grace);
        // Walk newest-first in progressively larger chunks: a search that
        // resolves near the tip streams a few dozen compact blocks, not a
        // full fixed-size chunk.
        'walk: for (lo, hi) in
            validate::chunks_desc_progressive(floor, tip, DEFAULT_SCAN_DEPTH, CHUNK)
        {
            if stepper.skip_chunk(hi) {
                break;
            }
            tracing::info!("shallow find: scanning blocks {lo}..={hi}");
            // Compact-scan the chunk (cheap, streamed), then enhance hits one
            // block at a time, newest first, so the early stop saves the
            // expensive per-transaction fetches.
            let hits = self.fetch_window(lo, hi).await?;
            let mut by_height: std::collections::BTreeMap<u32, Vec<TxId>> = Default::default();
            for (height, txid) in hits {
                by_height.entry(height).or_default().push(txid);
            }
            for (&height, txids) in by_height.iter().rev() {
                if !stepper.wants(height) {
                    break 'walk;
                }
                let batch: Vec<(u32, TxId)> = txids.iter().map(|t| (height, *t)).collect();
                raw.extend(self.enhance(tip, batch).await?);
                v = self.validated(raw.clone(), tip, opts);
                stepper.observe(height, satisfied(&v.latest));
            }
        }
        let from = stepper.covered_from(floor);
        Ok(self.state(tip, from, tip, v, init, opts))
    }

    /// Poll forward from a previous read's cursor: fetch the new blocks (plus
    /// a reorg re-check margin behind the cursor), and report updates not yet
    /// seen. [`ShallowState::updates`] carries only the *new* observations;
    /// [`ShallowState::latest`] reflects the whole re-fetched window. Returns
    /// an empty state with the same cursor when the tip hasn't advanced.
    pub async fn poll(
        &mut self,
        cursor: &ShallowCursor,
        opts: &ShallowOptions,
    ) -> Result<ShallowState, ShallowError> {
        let tip = self.tip().await?;
        let margin = REORG_MARGIN.max(opts.min_confirmations);
        let Some((lo, hi)) = validate::poll_range(cursor.height, tip, margin) else {
            return Ok(ShallowState {
                tip,
                scanned: ScannedRange {
                    from: cursor.height,
                    to: cursor.height,
                },
                updates: Vec::new(),
                latest: Default::default(),
                warnings: Vec::new(),
                init: self.init_anchor.clone(),
                cursor: cursor.clone(),
            });
        };
        let lo = lo.max(self.birthday);
        let raw = self.fetch_and_enhance(lo, hi, tip).await?;
        let v = self.validated(raw, tip, opts);
        let next_cursor = ShallowCursor::at_tip(tip, &v.updates, margin);
        let fresh = validate::dedup_updates(v.updates, &cursor.recent);
        Ok(ShallowState {
            tip,
            scanned: ScannedRange { from: lo, to: hi },
            updates: fresh,
            latest: v.latest,
            warnings: v.warnings,
            init: self.init_anchor.clone(),
            cursor: next_cursor,
        })
    }

    /// Verify the database's INIT anchor: walk forward from the embedded
    /// birthday (in chunks) until a confirmed, root-signed INIT memo is found
    /// (first-valid-wins, mirroring full replay; the wire address echo is
    /// advisory and ignored). Memoized for the client's lifetime and cached in
    /// the database directory when one is available.
    pub async fn verify_init(&mut self, opts: &ShallowOptions) -> Result<InitAnchor, ShallowError> {
        if let Some(anchor) = &self.init_anchor {
            return Ok(anchor.clone());
        }
        if let Some(anchor) = self.read_init_cache() {
            self.init_anchor = Some(anchor.clone());
            return Ok(anchor);
        }
        let tip = self.tip().await?;
        self.verify_init_at(opts, tip).await
    }

    async fn verify_init_at(
        &mut self,
        opts: &ShallowOptions,
        tip: u32,
    ) -> Result<InitAnchor, ShallowError> {
        let cap = tip.min(self.birthday.saturating_add(opts.init_scan_limit));
        let min_confs = opts.min_confirmations.max(1);
        // Walk forward from the birthday in small, growing chunks (the INIT is
        // the genesis write, so it sits at or just past the birthday: a few
        // dozen blocks usually find it without streaming a full 1000-block
        // window). Within each chunk, enhance hits *oldest-first, one block at
        // a time*, and stop at the first root-signed INIT, so we don't pay a
        // GetTransaction for every later oracle write before reaching it.
        for (lo, hi) in
            validate::chunks_asc_progressive(self.birthday, cap, DEFAULT_SCAN_DEPTH, CHUNK)
        {
            tracing::info!("shallow init: scanning blocks {lo}..={hi}");
            let hits = self.fetch_window(lo, hi).await?;
            let mut by_height: std::collections::BTreeMap<u32, Vec<TxId>> = Default::default();
            for (height, txid) in hits {
                by_height.entry(height).or_default().push(txid);
            }
            for (height, txids) in &by_height {
                let batch: Vec<(u32, TxId)> = txids.iter().map(|t| (*height, *t)).collect();
                let raw = self.enhance(tip, batch).await?;
                let v = validate(
                    raw,
                    &self.receiver,
                    &self.root_hex,
                    &opts.extra_signers,
                    tip,
                    min_confs,
                );
                if let Some(found) = v.inits.iter().find(|i| i.root_signed) {
                    let anchor = anchor_of(found);
                    self.init_anchor = Some(anchor.clone());
                    self.write_init_cache(&anchor);
                    return Ok(anchor);
                }
            }
        }
        Err(ShallowError::InitNotFound {
            from: self.birthday,
            to: cap,
        })
    }

    /// Run the INIT check when the options ask for it (memoized/cached), and
    /// hand back what `ShallowState.init` should carry.
    async fn ensure_init(
        &mut self,
        opts: &ShallowOptions,
        tip: u32,
    ) -> Result<Option<InitAnchor>, ShallowError> {
        if !opts.verify_init {
            return Ok(self.init_anchor.clone());
        }
        if self.init_anchor.is_none() {
            if let Some(anchor) = self.read_init_cache() {
                self.init_anchor = Some(anchor);
            }
        }
        if self.init_anchor.is_none() {
            self.verify_init_at(opts, tip).await?;
        }
        Ok(self.init_anchor.clone())
    }

    /// One `GetLatestBlock` round trip.
    pub async fn chain_tip(&mut self) -> Result<u32, ShallowError> {
        self.tip().await
    }

    async fn tip(&mut self) -> Result<u32, ShallowError> {
        self.source.tip().await
    }

    /// Compact-scan blocks `[lo, hi]`, then enhance every hit. The bulk path
    /// (`scan`/`poll`, whose whole window is wanted); `find_where` interleaves
    /// the two steps itself for its early stop.
    async fn fetch_and_enhance(
        &mut self,
        lo: u32,
        hi: u32,
        tip: u32,
    ) -> Result<Vec<RawHit>, ShallowError> {
        let hits = self.fetch_window(lo, hi).await?;
        self.enhance(tip, hits).await
    }

    /// Candidate `(height, txid)` hits for `[lo, hi]` (the source's compact
    /// trial-decryption pass). No per-transaction fetches; callers decide
    /// which hits are worth enhancing.
    async fn fetch_window(&mut self, lo: u32, hi: u32) -> Result<Vec<(u32, TxId)>, ShallowError> {
        self.source.candidates(lo, hi).await
    }

    /// One transaction fetch per hit (deduped by txid), memos decrypted into
    /// [`RawHit`]s. A hit gone between the compact scan and the fetch
    /// (reorged away) is skipped.
    async fn enhance(
        &mut self,
        tip: u32,
        hits: Vec<(u32, TxId)>,
    ) -> Result<Vec<RawHit>, ShallowError> {
        let mut by_txid: std::collections::BTreeMap<[u8; 32], u32> = Default::default();
        for (height, txid) in hits {
            by_txid.entry(*txid.as_ref()).or_insert(height);
        }
        let mut out = Vec::new();
        for (txid_bytes, seen_height) in by_txid {
            let txid = TxId::from_bytes(txid_bytes);
            let Some((height, memos)) = self
                .source
                .transaction_memos(txid, seen_height, tip)
                .await?
            else {
                continue;
            };
            for (output_index, text) in memos {
                out.push(RawHit {
                    height,
                    txid: txid_bytes,
                    output_index,
                    text,
                });
            }
        }
        Ok(out)
    }

    fn validated(&self, raw: Vec<RawHit>, tip: u32, opts: &ShallowOptions) -> Validated {
        validate(
            raw,
            &self.receiver,
            &self.root_hex,
            &opts.extra_signers,
            tip,
            // Shallow has no mempool path, so 0 confirmations means 1.
            opts.min_confirmations.max(1),
        )
    }

    fn state(
        &self,
        tip: u32,
        from: u32,
        to: u32,
        v: Validated,
        init: Option<InitAnchor>,
        opts: &ShallowOptions,
    ) -> ShallowState {
        let margin = REORG_MARGIN.max(opts.min_confirmations);
        let cursor = ShallowCursor::at_tip(tip, &v.updates, margin);
        ShallowState {
            tip,
            scanned: ScannedRange { from, to },
            updates: v.updates,
            latest: v.latest,
            warnings: v.warnings,
            init,
            cursor,
        }
    }

    // ---- INIT cache (the one optional on-disk artifact) ----

    fn read_init_cache(&self) -> Option<InitAnchor> {
        read_cache_file(self.cache_dir.as_deref()?, &self.receiver)
    }

    fn write_init_cache(&self, anchor: &InitAnchor) {
        if let Some(dir) = &self.cache_dir {
            write_cache_file(dir, &self.receiver, anchor);
        }
    }
}

/// Read the INIT cache from a database directory. Honored only when the
/// recorded receiver matches the caller's, so a copied/renamed database dir
/// can't poison the check.
fn read_cache_file(dir: &std::path::Path, receiver: &str) -> Option<InitAnchor> {
    let text = std::fs::read_to_string(dir.join(INIT_CACHE_FILE)).ok()?;
    let cached: InitCacheFile = toml::from_str(&text).ok()?;
    (cached.receiver == receiver).then_some(InitAnchor {
        height: cached.height,
        txid: cached.txid,
        memo: cached.memo,
    })
}

/// Best-effort cache write (atomic tmp + rename). Failure only costs a
/// re-verification next time. Never creates the directory: shallow must not
/// materialize database dirs.
fn write_cache_file(dir: &std::path::Path, receiver: &str, anchor: &InitAnchor) {
    if !dir.is_dir() {
        return;
    }
    let encoded = match toml::to_string(&InitCacheFile {
        receiver: receiver.to_owned(),
        height: anchor.height,
        txid: anchor.txid.clone(),
        memo: anchor.memo.clone(),
    }) {
        Ok(t) => t,
        Err(_) => return,
    };
    let body = format!(
        "# Cache of the shallow-sync INIT anchor. Safe to delete; it will be re-verified.\n{encoded}"
    );
    let tmp = dir.join(format!("{INIT_CACHE_FILE}.tmp"));
    let dst = dir.join(INIT_CACHE_FILE);
    if std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, &dst).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn anchor_of(obs: &InitObservation) -> InitAnchor {
    InitAnchor {
        height: obs.height,
        txid: TxId::from_bytes(obs.txid).to_string(),
        memo: Some(obs.memo.clone()),
    }
}

#[derive(Serialize, Deserialize)]
struct InitCacheFile {
    receiver: String,
    height: u32,
    txid: String,
    #[serde(default)]
    memo: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::testutil::{fixture, signed_memo, MockSource};
    use super::*;
    use crate::protocol::Op;

    fn scratch_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("zkv-shallow-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    // ---- Driver tests: the scan/find/poll/verify_init orchestration over a
    // scripted in-memory chain (no network). The pure validator and the
    // stop/chunk math have their own tests in `validate`; these cover the
    // wiring: early stop (counted in transaction fetches), the glob grace
    // window, INIT-anchor short-circuits, birthday clamps, and poll dedup.

    /// A client over a mock chain. `verify_init` is off by default so each
    /// test exercises one driver; the INIT tests turn it back on.
    fn client(
        source: MockSource,
        receiver: &str,
        root: &str,
        birthday: u32,
    ) -> ShallowClient<MockSource> {
        ShallowClient::with_source(source, receiver.to_owned(), root.to_owned(), birthday, None)
    }

    fn opts() -> ShallowOptions {
        ShallowOptions {
            min_confirmations: 1,
            verify_init: false,
            ..ShallowOptions::default()
        }
    }

    #[tokio::test]
    async fn find_stops_after_first_satisfying_block() {
        let (receiver, root, sk) = fixture();
        let (mut source, stats) = MockSource::new(1_000);
        // An every-block oracle: without the early stop, find would fetch
        // every one of these transactions.
        for height in 960..=997 {
            source.push(
                height,
                height as u8,
                vec![signed_memo(
                    &receiver,
                    &sk,
                    Op::Set,
                    "k",
                    Some(&height.to_string()),
                    u64::from(height),
                )],
            );
        }
        let mut c = client(source, &receiver, &root, 900);
        let state = c.find(&["k".into()], &opts()).await.expect("find");

        // Newest write wins...
        assert_eq!(
            state.latest.get("k").and_then(|u| u.value.as_deref()),
            Some("997")
        );
        // ...and only that one transaction was enhanced.
        assert_eq!(stats.borrow().tx_fetches.len(), 1);
        // Exact-key search (grace 0) covered down to the satisfying height.
        assert_eq!(state.scanned.from, 997);
    }

    #[tokio::test]
    async fn find_grace_window_catches_siblings_but_not_stale_keys() {
        let (receiver, root, sk) = fixture();
        let (mut source, stats) = MockSource::new(1_000);
        let m = |key: &str| signed_memo(&receiver, &sk, Op::Set, key, Some("v"), 0);
        source.push(990, 1, vec![m("rates/a")]);
        source.push(985, 2, vec![m("rates/b")]);
        // Outside the grace window below the first match (990 - 48 = 942):
        // must not even be fetched.
        source.push(900, 3, vec![m("rates/old")]);

        let mut c = client(source, &receiver, &root, 800);
        let o = ShallowOptions {
            max_depth: 300,
            ..opts()
        };
        let state = c
            .find_where(
                &o,
                |latest| latest.keys().any(|k| k.starts_with("rates/")),
                DEFAULT_SCAN_DEPTH,
            )
            .await
            .expect("find_where");

        assert!(state.latest.contains_key("rates/a"));
        assert!(
            state.latest.contains_key("rates/b"),
            "sibling within the grace window must be caught"
        );
        assert!(
            !state.latest.contains_key("rates/old"),
            "below the grace floor: out of scope"
        );
        assert_eq!(stats.borrow().tx_fetches.len(), 2, "stale key not fetched");
        assert_eq!(state.scanned.from, 990 - DEFAULT_SCAN_DEPTH);
    }

    #[tokio::test]
    async fn find_not_found_walks_to_the_floor_and_reports_it() {
        let (receiver, root, _) = fixture();
        let (source, stats) = MockSource::new(1_000);
        let mut c = client(source, &receiver, &root, 100);
        let state = c.find(&["missing".into()], &opts()).await.expect("find");

        assert!(state.latest.is_empty());
        // Default max_depth (48): floor = 1000 - 47.
        assert_eq!(state.scanned.from, 953);
        // The whole window was compact-scanned, nothing was enhanced.
        assert_eq!(stats.borrow().candidate_ranges, vec![(953, 1_000)]);
        assert!(stats.borrow().tx_fetches.is_empty());
    }

    #[tokio::test]
    async fn verify_init_stops_at_the_genesis_init() {
        let (receiver, root, sk) = fixture();
        let (mut source, stats) = MockSource::new(1_000);
        let init_memo = signed_memo(&receiver, &sk, Op::Init, "zkvtest1echo", None, 0);
        source.push(102, 1, vec![init_memo.clone()]);
        // Later oracle writes that must NOT be fetched by the INIT walk.
        for height in 103..160 {
            source.push(
                height,
                height as u8,
                vec![signed_memo(&receiver, &sk, Op::Set, "k", Some("v"), 0)],
            );
        }
        let mut c = client(source, &receiver, &root, 100);
        let anchor = c.verify_init(&opts()).await.expect("verify_init");

        assert_eq!(anchor.height, 102);
        assert_eq!(anchor.memo.as_deref(), Some(init_memo.as_str()));
        assert_eq!(
            stats.borrow().tx_fetches.len(),
            1,
            "the genesis INIT costs one fetch; later writes are never enhanced"
        );
    }

    #[tokio::test]
    async fn verify_init_skips_a_forged_init() {
        let (receiver, root, root_sk) = fixture();
        let other_sk = secp256k1::SecretKey::from_slice(&[7u8; 32]).expect("sk");
        let (mut source, _) = MockSource::new(1_000);
        source.push(
            101,
            1,
            vec![signed_memo(&receiver, &other_sk, Op::Init, "echo", None, 0)],
        );
        source.push(
            105,
            2,
            vec![signed_memo(&receiver, &root_sk, Op::Init, "echo", None, 0)],
        );
        let mut c = client(source, &receiver, &root, 100);
        let anchor = c.verify_init(&opts()).await.expect("verify_init");
        assert_eq!(anchor.height, 105, "first ROOT-signed INIT wins");
    }

    #[tokio::test]
    async fn verify_init_not_found_is_a_structured_error() {
        let (receiver, root, sk) = fixture();
        let (mut source, _) = MockSource::new(1_000);
        // Data writes but no INIT anywhere in the capped range.
        source.push(
            120,
            1,
            vec![signed_memo(&receiver, &sk, Op::Set, "k", Some("v"), 0)],
        );
        let mut c = client(source, &receiver, &root, 100);
        let o = ShallowOptions {
            init_scan_limit: 50,
            ..opts()
        };
        match c.verify_init(&o).await {
            Err(ShallowError::InitNotFound { from: 100, to: 150 }) => {}
            other => panic!("expected InitNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_init_short_circuits_on_the_cache() {
        let (receiver, root, _) = fixture();
        let dir = scratch_dir("init-cache-hit");
        write_cache_file(
            &dir,
            &receiver,
            &InitAnchor {
                height: 123,
                txid: "beef".into(),
                memo: None,
            },
        );
        let (source, stats) = MockSource::new(1_000);
        let mut c = ShallowClient::with_source(
            source,
            receiver.clone(),
            root.clone(),
            100,
            Some(dir.clone()),
        );
        let anchor = c.verify_init(&opts()).await.expect("verify_init");
        assert_eq!(anchor.height, 123);
        let s = stats.borrow();
        assert!(
            s.candidate_ranges.is_empty() && s.tx_fetches.is_empty(),
            "a cache hit must not touch the chain"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn scan_clamps_the_window_to_the_birthday() {
        let (receiver, root, sk) = fixture();
        let (mut source, _) = MockSource::new(1_000);
        source.push(
            995,
            1,
            vec![signed_memo(&receiver, &sk, Op::Set, "k", Some("v"), 0)],
        );
        // Birthday above (tip - depth): the window must not reach below it.
        let mut c = client(source, &receiver, &root, 990);
        let state = c.scan(&opts()).await.expect("scan");
        assert_eq!(state.scanned.from, 990);
        assert_eq!(
            state.latest.get("k").and_then(|u| u.value.as_deref()),
            Some("v")
        );
    }

    #[tokio::test]
    async fn poll_returns_only_new_updates_and_advances_the_cursor() {
        let (receiver, root, sk) = fixture();
        let (mut source, stats) = MockSource::new(1_000);
        source.push(
            998,
            1,
            vec![signed_memo(&receiver, &sk, Op::Set, "a", Some("1"), 0)],
        );
        let mut c = client(source, &receiver, &root, 900);
        let first = c.scan(&opts()).await.expect("scan");
        assert_eq!(first.updates.len(), 1);
        let cursor = first.cursor.clone();
        assert_eq!(cursor.height, 1_000);

        // Tip unchanged: an empty poll that touches nothing.
        let ranges_before = stats.borrow().candidate_ranges.len();
        let idle = c.poll(&cursor, &opts()).await.expect("poll");
        assert!(idle.updates.is_empty());
        assert_eq!(idle.cursor.height, 1_000);
        assert_eq!(
            stats.borrow().candidate_ranges.len(),
            ranges_before,
            "no new blocks: no fetches"
        );

        // Tip advances with a new write; the margin re-fetch re-sees the old
        // update at 998, which must be deduped out of `updates`.
        c.source.tip = 1_005;
        c.source.push(
            1_003,
            2,
            vec![signed_memo(&receiver, &sk, Op::Set, "b", Some("2"), 0)],
        );
        let next = c.poll(&cursor, &opts()).await.expect("poll");
        assert_eq!(
            next.updates
                .iter()
                .map(|u| u.key.as_str())
                .collect::<Vec<_>>(),
            vec!["b"],
            "only the new update is reported"
        );
        assert!(
            next.latest.contains_key("a"),
            "latest reflects the whole re-fetched window"
        );
        assert_eq!(next.cursor.height, 1_005);
    }

    #[tokio::test]
    async fn scan_runs_init_check_when_enabled() {
        let (receiver, root, sk) = fixture();
        let (mut source, _) = MockSource::new(1_000);
        source.push(
            901,
            1,
            vec![signed_memo(&receiver, &sk, Op::Init, "echo", None, 0)],
        );
        source.push(
            998,
            2,
            vec![signed_memo(&receiver, &sk, Op::Set, "k", Some("v"), 0)],
        );
        let mut c = client(source, &receiver, &root, 900);
        let o = ShallowOptions {
            verify_init: true,
            ..opts()
        };
        let state = c.scan(&o).await.expect("scan");
        assert_eq!(state.init.as_ref().map(|a| a.height), Some(901));
        assert!(state.latest.contains_key("k"));

        // Never-initialized database: the same scan refuses by default.
        let (source2, _) = MockSource::new(1_000);
        let mut c2 = client(source2, &receiver, &root, 900);
        assert!(matches!(
            c2.scan(&o).await,
            Err(ShallowError::InitNotFound { .. })
        ));
    }

    #[test]
    fn init_cache_round_trip_and_receiver_mismatch() {
        let dir = scratch_dir("init-cache");
        // A realistic raw memo: a header line, a newline, and the signature
        // line. The newline must survive the TOML round trip.
        let memo = "ZKV0 INIT zkvtest1echo\n".to_owned() + &"ab".repeat(65);
        let anchor = InitAnchor {
            height: 42,
            txid: "deadbeef".into(),
            memo: Some(memo.clone()),
        };
        write_cache_file(&dir, "test:abcd", &anchor);

        let cached = read_cache_file(&dir, "test:abcd").expect("cache hit");
        assert_eq!(cached.height, 42);
        assert_eq!(cached.txid, "deadbeef");
        assert_eq!(cached.memo.as_deref(), Some(memo.as_str()));

        // A different receiver (a copied/renamed db dir) must be rejected.
        assert!(read_cache_file(&dir, "test:other").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_cache_reads_old_file_without_memo() {
        // A cache file written before the `memo` field existed must still load
        // (the field is optional / defaulted).
        let dir = scratch_dir("init-cache-legacy");
        std::fs::write(
            dir.join(INIT_CACHE_FILE),
            "receiver = \"test:abcd\"\nheight = 7\ntxid = \"beef\"\n",
        )
        .expect("write");
        let cached = read_cache_file(&dir, "test:abcd").expect("cache hit");
        assert_eq!(cached.height, 7);
        assert_eq!(cached.memo, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_cache_write_never_creates_the_dir() {
        let dir = scratch_dir("init-cache-missing").join("does-not-exist");
        let anchor = InitAnchor {
            height: 1,
            txid: "00".into(),
            memo: None,
        };
        write_cache_file(&dir, "test:abcd", &anchor);
        assert!(!dir.exists(), "shallow must not materialize database dirs");
    }

    #[test]
    fn garbage_cache_is_ignored() {
        let dir = scratch_dir("init-cache-garbage");
        std::fs::write(dir.join(INIT_CACHE_FILE), "not toml [[[").expect("write");
        assert!(read_cache_file(&dir, "test:abcd").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
