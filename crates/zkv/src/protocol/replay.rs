use super::*;

/// Result of replaying a memo stream: the database's init status, the
/// per-key state, and the owner/writer authorization registry. Callers must
/// check `init` before treating `state` as authoritative: an `Uninitialized`
/// or `Initializing` database has an empty `state` map, but the meaning is
/// "no committed writes yet," not "the admin has deleted everything."
///
/// `auth` is the confirmed projection of the owner/writer registry: who may
/// write and with what authority. It is empty until INIT is honored, at which
/// point the root (UFVK-derived) signer becomes the first owner.
#[derive(Clone, Debug)]
pub struct ReplayResult {
    pub init: InitState,
    pub state: BTreeMap<String, KeyState>,
    pub auth: AuthRegistry,
    /// Whether a `FINALIZE` has been confirmed. Once `true`, the database is
    /// permanently sealed: every subsequent write is dropped during replay.
    /// Like `auth`, this flips only on a *confirmed* FINALIZE.
    pub finalized: bool,
    /// The database's required protocol epoch and the capabilities an
    /// under-versioned client must give up, projected from `VERSION` memos.
    /// [`VersionState::default`] (= [`GENESIS_DB_VERSION`], no blocks) until an
    /// owner broadcasts a `VERSION` memo.
    pub version: VersionState,
    /// Per-key replay-protection version: the count of honored data writes
    /// (`SET`/`SETL`/`DEL`) to each key so far. Folded into the signing domain
    /// (see [`signing_domain`]). A key keeps its version through a `DEL`
    /// (tombstone), so a replayed original creation cannot recreate it. Only
    /// non-zero entries are tracked; an absent key reads as version 0.
    pub kv_versions: BTreeMap<String, u64>,
    /// Per-target replay-protection version for management ops: the count of
    /// honored `OWNER*`/`WRITER*` ops against each target pubkey. Keyed by the
    /// canonical `zkvid1…` target. Survives revocation (tombstone) so a replayed
    /// `OWNERDEL`/`WRITERDEL` cannot re-fire after a re-grant.
    pub target_versions: BTreeMap<String, u64>,
}

/// Status of a single historical write at query time.
///
/// Mirrors the read path's visibility rules: an externally-received write
/// below the caller's threshold is dropped entirely (never reaches a
/// `HistoryEntry`), a self-sent write below the threshold is `Confirming`,
/// and a mempool / locally-broadcast write is `Pending`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryStatus {
    /// Mined at or beyond the caller's confirmation threshold.
    /// `confirmations` is the current depth (`tip - height + 1`).
    Confirmed { confirmations: u32 },
    /// Mined but below the threshold (self-sent writes only). `done` is the
    /// current depth; `required` is the threshold.
    Confirming { done: u32, required: u32 },
    /// In the mempool / locally-broadcast, not yet mined.
    Pending,
}

/// One entry in a database's append-only write history (SET, DEL, or the
/// database's genesis INIT).
///
/// Deep entries come from the snapshot's `kv_history` (verified once at
/// promote time, so [`HistoryEntry::verified`] is `Some(true)` without a
/// re-check); live-tail entries are produced by [`history_entry_from_memo`],
/// which verifies the signature at query time. The INIT entry's [`key`] is
/// the claimed zkv address, so a per-key filter naturally excludes it.
///
/// [`key`]: HistoryEntry::key
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub op: Op,
    /// For SET/DEL, the key; for INIT, the zkv address being claimed.
    pub key: String,
    /// `Some` for SET, `None` for DEL/INIT.
    pub value: Option<String>,
    /// Mined height, or `None` if still in the mempool / pending.
    pub height: Option<u32>,
    /// Block timestamp (unix seconds) of the mined height, or `None` if
    /// not yet mined / unknown.
    pub timestamp: Option<u32>,
    /// Display-order (big-endian) txid hex. Empty only for entries
    /// synthesized from `pending.toml` before the wallet indexes the tx.
    pub txid: String,
    pub output_index: u32,
    /// 130-char hex of the 65-byte recoverable ECDSA signature. `None` only for
    /// pending-from-`pending.toml` entries (the signature isn't cached
    /// locally; it reappears once the wallet decrypts its own broadcast).
    pub signature: Option<String>,
    /// The replay-protection sequence this write referenced on the wire (the
    /// `[seq]` prefix on the signature line; see [`encode_sig_line`]). `None`
    /// only for pending-from-`pending.toml` entries whose signed memo wasn't
    /// cached locally (older rows predating the `memo` field).
    pub seq: Option<u64>,
    /// Compressed-hex of the signer recovered from this write's signature:
    /// the delegated owner/writer that authored it, which may differ from the
    /// database's root key. `None` for pending-from-`pending.toml` entries not
    /// yet confirmable.
    pub signer: Option<String>,
    /// Signature verification result: `Some(true)` when the signature is
    /// cryptographically valid **and** the recovered signer was authorized
    /// (owner, or writer with adequate scope) for this op at this chain
    /// position; `Some(false)` for an invalid signature or an unauthorized
    /// signer; `None` for not-yet-verifiable pending entries.
    pub verified: Option<bool>,
    pub status: HistoryStatus,
    /// The raw on-chain memo text (reconstructed for deep entries via
    /// [`render_memo_text`]). `None` for pending-from-`pending.toml`.
    pub memo: Option<String>,
    /// The actual fee paid (zatoshi) for the transaction carrying this write,
    /// read from the wallet's own transaction record. `None` when the wallet
    /// didn't create the tx (received-only) or hasn't indexed it yet; it is
    /// filled by the history loader, not by `history_entry_from_memo`.
    pub fee: Option<u64>,
    /// Value (zatoshi) carried by this write's own shielded output. `Some(v)`
    /// with `v > 0` when the write also moved ZEC (a tip/deposit broadcast
    /// alongside the memo, e.g. a faucet INIT); `None` for a plain zero-value
    /// zkv write. Filled by the history loader, like `fee`.
    pub output_value: Option<u64>,
}

/// A page of a database's write history plus its single authorized signer.
///
/// `entries` is one page (newest-first; in-flight pinned above confirmed).
/// `total` is the full match count across all pages (drives pagination UIs);
/// `offset`/`limit` echo the request (`limit = None` means "all rows").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryResult {
    /// Hex of the database's root (UFVK-derived) verifying pubkey (owner #1).
    /// Informational; per-entry attribution is on each
    /// [`HistoryEntry::signer`], which may be a delegated owner/writer.
    pub signer: String,
    /// One page of entries, newest-first; in-flight (pending → confirming)
    /// sorted above confirmed.
    pub entries: Vec<HistoryEntry>,
    /// Total entries matching the filter across all pages.
    pub total: u64,
    pub offset: u32,
    pub limit: Option<u32>,
}

/// Build a [`HistoryEntry`] from a raw live-tail memo, verifying its
/// signature against `pk`.
///
/// Returns `None` only when the memo isn't a zkv command at all (a plain
/// text memo). SET, DEL, and INIT all produce an entry. The caller supplies
/// the already-classified [`HistoryStatus`] (and is responsible for dropping
/// externally-received writes below the threshold before calling, matching
/// the read path).
// One history entry from a memo plus its chain position (height / timestamp /
// txid / output_index) and status: all distinct per-entry inputs, not a
// reusable bundle, so a context struct would only move the noise to the caller.
#[allow(clippy::too_many_arguments)]
pub fn history_entry_from_memo(
    receiver: &str,
    pk: &secp256k1::PublicKey,
    text: &str,
    height: Option<u32>,
    timestamp: Option<u32>,
    txid: String,
    output_index: u32,
    status: HistoryStatus,
) -> Option<HistoryEntry> {
    let cmd = parse_text_memo(text)?;
    // Rebuild the signed payload exactly as the read path does (sequence prefix
    // and any first-line comment included).
    let payload = payload_for(receiver, &cmd);
    let verified = verify_command(pk, &payload, &cmd.sig_hex);
    Some(HistoryEntry {
        op: cmd.op,
        key: cmd.key,
        value: cmd.value,
        height,
        timestamp,
        txid,
        output_index,
        signature: Some(cmd.sig_hex),
        seq: Some(cmd.seq),
        // This helper has no authorization registry, so it cannot attribute
        // the per-entry signer or judge authorization; it is used only for the
        // `pending.toml` fallback where the entry is `Pending` (signer `None`).
        signer: None,
        verified: Some(verified),
        status,
        memo: Some(text.to_owned()),
        output_value: None,
        fee: None,
    })
}

/// Build a [`HistoryEntry`] from a live-tail memo while folding it into an
/// evolving `(init, auth, state)`, so the per-entry signer is attributed and
/// `verified` reflects real authorization in a multi-signer database.
///
/// This shares the classifier (`classify_in_memory`) and the fold
/// (`apply_in_memory`) with [`replay_with_seed`], so history attribution
/// cannot drift from what readers actually apply. The caller must invoke it
/// over the live tail in **chain order** (oldest first), seeding `init`/`auth`/
/// `state` from the snapshot, exactly as replay does; management ops in the
/// tail mutate the registry here even though they emit no history entry.
///
/// Returns `None` for a non-zkv memo, and for management ops (OWNER*/WRITER*),
/// which fold above but are not part of the key/value write log. SET, SETL,
/// DEL, and INIT each produce an entry. `hist_status` is the display status;
/// `write_status` is the same status mapped to [`WriteStatus`] for the fold
/// (a *pending* management op must confer no registry change, matching replay).
// Per-entry inputs (chain position / status) plus the mutable fold state; like
// history_entry_from_memo the position args are one-shot, so bundling them just
// relocates the noise to the call site.
#[allow(clippy::too_many_arguments)]
pub fn history_entry_folding(
    receiver: &str,
    root_hex: &str,
    text: &str,
    height: Option<u32>,
    timestamp: Option<u32>,
    txid: String,
    output_index: u32,
    hist_status: HistoryStatus,
    write_status: &WriteStatus,
    init: &mut InitState,
    auth: &mut AuthRegistry,
    finalized: &mut bool,
    state: &mut BTreeMap<String, KeyState>,
    kv_versions: &mut BTreeMap<String, u64>,
    target_versions: &mut BTreeMap<String, u64>,
) -> Option<HistoryEntry> {
    let c = {
        // Immutable view for classification; its borrows end at the block so the
        // fold below can take the same fields by `&mut`.
        let view = ReplayView {
            receiver,
            root_hex,
            init: &*init,
            auth: &*auth,
            finalized: *finalized,
            state: &*state,
            kv_versions: &*kv_versions,
            target_versions: &*target_versions,
        };
        classify_in_memory(text, write_status, &view)
    }?;
    let signature = c.sig_hex.clone();
    let signer = c.signer_hex.clone();
    // Fold forward so subsequent rows authorize against the evolving registry /
    // key-existence / finalized latch / versions, including management ops and
    // FINALIZE, which never emit an entry. The history view doesn't track the
    // protocol epoch (version gating is a client-capability check, not a
    // write-authorization input, and VERSION ops emit no history entry), so any
    // VERSION op folds into a throwaway state.
    let mut version = VersionState::default();
    let outcome = apply_in_memory(
        &c,
        write_status,
        &txid,
        timestamp,
        root_hex,
        state,
        init,
        auth,
        finalized,
        &mut version,
        kv_versions,
        target_versions,
    );
    let op = c.op?;
    if !matches!(op, Op::Init | Op::Set | Op::SetL | Op::Del) {
        return None; // management op: folded above, not a write-log entry
    }
    // `verified` = signature valid AND signer authorized for this op here. A
    // dropped row (bad signature OR unauthorized signer) is `false`.
    let verified = matches!(outcome, RowOutcome::Applied | RowOutcome::Pending);
    Some(HistoryEntry {
        op,
        key: c.key.unwrap_or_default(),
        value: c.value,
        height,
        timestamp,
        txid,
        output_index,
        signature,
        seq: Some(c.seq),
        signer,
        verified: Some(verified),
        status: hist_status,
        memo: Some(text.to_owned()),
        fee: None,
        output_value: None,
    })
}

/// Convenience: signed payload for an INIT memo. The signing key must be the
/// `zkv_verifying_pubkey`'s secret counterpart (the root key).
///
/// `receiver` is the database's [`receiver_domain`]. INIT binds **only** the
/// receiver (no key, no value, no version), so the embedded address the wire
/// memo carries (`ZKV0 INIT <addr>`) is an unsigned, advisory self-description:
/// re-exporting with a different birthday does not invalidate the INIT, and a
/// confirmed INIT is gated solely on "signed by the root key, first wins".
pub fn signed_init_payload(receiver: &str) -> Vec<u8> {
    signed_payload(receiver, Op::Init, "", None)
}

/// Convenience: build the two-line INIT memo. The embedded `zkv_addr` is an
/// advisory echo only; it is **not** part of the signed payload (see
/// [`signed_init_payload`]). A future version may carry reserved config tokens.
pub fn build_init_memo(zkv_addr: &str, sig: &[u8; SIG_LEN]) -> anyhow::Result<MemoBytes> {
    // INIT is not version-CAS'd, so it carries no sequence prefix (seq = 0).
    build_memo(Op::Init, zkv_addr, None, 0, sig)
}

/// A single not-yet-confirmed op applied on top of the confirmed state.
///
/// `txid` is the hex txid of the memo that produced this op, threaded through
/// from the SQL row. Empty in tests / synthetic inputs. The CLI uses it to
/// decide whether a mempool entry (`done == 0`) was broadcast by this client
/// (matches an entry in `pending.toml`) or arrived off the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingOp {
    Set {
        value: String,
        done: u32,
        required: u32,
        txid: String,
    },
    Del {
        done: u32,
        required: u32,
        txid: String,
    },
}

impl PendingOp {
    pub fn txid(&self) -> &str {
        match self {
            PendingOp::Set { txid, .. } | PendingOp::Del { txid, .. } => txid,
        }
    }

    pub fn done(&self) -> u32 {
        match self {
            PendingOp::Set { done, .. } | PendingOp::Del { done, .. } => *done,
        }
    }
}

/// One row's worth of input to `replay`. Production code threads txids
/// through so the display layer can match against the local pending cache;
/// tests pass `(text, status)` and get an empty txid via the `From` impl.
pub struct ReplayEntry {
    pub text: String,
    pub status: WriteStatus,
    pub txid: String,
    /// Block timestamp (unix seconds), if mined. Threaded so the reducer can
    /// record each key's last-update time. Tests' tuple shims default `None`.
    pub block_time: Option<u32>,
}

impl From<(String, WriteStatus)> for ReplayEntry {
    fn from((text, status): (String, WriteStatus)) -> Self {
        Self {
            text,
            status,
            txid: String::new(),
            block_time: None,
        }
    }
}

impl From<(String, WriteStatus, String)> for ReplayEntry {
    fn from((text, status, txid): (String, WriteStatus, String)) -> Self {
        Self {
            text,
            status,
            txid,
            block_time: None,
        }
    }
}

impl From<(String, WriteStatus, String, Option<u32>)> for ReplayEntry {
    fn from((text, status, txid, block_time): (String, WriteStatus, String, Option<u32>)) -> Self {
        Self {
            text,
            status,
            txid,
            block_time,
        }
    }
}

/// Per-key state after replay: the last confirmed value (if any) plus any
/// pending (not-yet-confirmed) ops queued on top of it. Pending ops are
/// chain-ordered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyState {
    pub confirmed: Option<String>,
    pub pending: Vec<PendingOp>,
    /// Block timestamp (unix seconds) of the latest confirmed write to this
    /// key (the "last update" time). `None` until a confirmed write is seen.
    pub updated_at: Option<u32>,
    /// Display-order txid of that latest confirmed write.
    pub last_txid: Option<String>,
}

/// Replay a sequence of memo entries (in chain order) into per-key state plus
/// init status. Equivalent to [`replay_with_seed`] with `seed = None`.
///
/// Production callers always go through [`replay_with_seed`] so the seed
/// path is exercised end-to-end; this shim exists so the test suite (which
/// rarely cares about seeding) stays terse.
#[cfg(test)]
pub fn replay<I, T>(
    entries: I,
    receiver: &str,
    pk: &secp256k1::PublicKey,
    strict: bool,
) -> anyhow::Result<ReplayResult>
where
    I: IntoIterator<Item = T>,
    T: Into<ReplayEntry>,
{
    replay_with_seed(entries, None, receiver, pk, strict)
}

/// Replay a sequence of memo entries (in chain order) into per-key state,
/// init status, and the owner/writer registry, optionally starting from a
/// previously computed snapshot.
///
/// Each entry is `(memo_text, status)`. The first valid signed INIT memo
/// (signed by the root key `pk` derived from the address's UFVK, with
/// embedded zkv_addr matching this database's address) gates the rest: until
/// INIT is observed at `Confirmed` status, all other memos are dropped as
/// pre-INIT noise. Subsequent INIT memos are ignored; first valid INIT wins.
/// When INIT is honored at `Confirmed`, the root key becomes the first owner.
///
/// # Authorization
///
/// With recoverable signatures, every memo's signer is recovered from its
/// signature (no pubkey on the wire) and checked against the registry:
///
/// - **Owners** (`OWNERSET`/`OWNERDEL`/`WRITERSET`/`WRITERDEL` and any
///   `SET`/`SETL`/`DEL`): an owner may do anything. Only owners may issue
///   management ops; a management op signed by a non-owner is dropped. The
///   last remaining owner cannot be removed.
/// - **Writers** (`SET`/`SETL`/`DEL` within scope): a writer's [`Scope`]
///   gates `CREATE` (set a key with no confirmed value), `UPDATE` (set a key
///   that already has one), and `DESTROY` (`DEL`). Out-of-scope writes are
///   dropped.
/// - Anyone else: dropped.
///
/// Only `Confirmed` management ops mutate the registry; a pending grant does
/// not yet confer authority. Authorization for data ops is always evaluated
/// against the confirmed registry.
///
/// When `seed` is `Some`, replay starts from `seed.state` / `seed.init` /
/// `seed.auth` and folds `entries` on top. The seed is treated as a
/// chain-ordered prefix already replayed by an earlier call. Callers passing
/// a seed must ensure `entries` is the strict tail past the seed's watermark.
/// Pending ops in `seed.state` are dropped before folding; they belong to a
/// previous query and are recomputed from the live tail.
///
/// `entries` must be ordered by mined_height ASC (mempool last), txid ASC,
/// output_index ASC. Malformed memos and unrecoverable signatures are dropped
/// silently unless `strict` is set; authorization failures are always silent
/// drops (they are policy, not wire corruption).
///
/// Pruning: keys with no confirmed value and no pending Set are dropped from
/// the returned map (a pending Del on a nonexistent key is a no-op).
pub fn replay_with_seed<I, T>(
    entries: I,
    seed: Option<ReplayResult>,
    receiver: &str,
    pk: &secp256k1::PublicKey,
    strict: bool,
) -> anyhow::Result<ReplayResult>
where
    I: IntoIterator<Item = T>,
    T: Into<ReplayEntry>,
{
    let (
        mut state,
        mut init,
        mut auth,
        mut finalized,
        mut version,
        mut kv_versions,
        mut target_versions,
    ) = match seed {
        Some(s) => {
            // The persisted snapshot only carries confirmed values. Pending
            // queues from a prior query are stale; the live tail will
            // rebuild them from the current mempool / unconfirmed set. The
            // version maps, by contrast, are confirmed projections and seed
            // the live tail directly (a tombstone version has no `state`
            // entry, so it rides only on `kv_versions`).
            let mut state = s.state;
            for ks in state.values_mut() {
                ks.pending.clear();
            }
            (
                state,
                s.init,
                s.auth,
                s.finalized,
                s.version,
                s.kv_versions,
                s.target_versions,
            )
        }
        None => (
            BTreeMap::new(),
            InitState::Uninitialized,
            AuthRegistry::default(),
            false,
            VersionState::default(),
            BTreeMap::new(),
            BTreeMap::new(),
        ),
    };
    let root_hex = pubkey_bech32(pk);

    for entry in entries {
        let ReplayEntry {
            text,
            status,
            txid,
            block_time,
        } = entry.into();
        let view = ReplayView {
            receiver,
            root_hex: &root_hex,
            init: &init,
            auth: &auth,
            finalized,
            state: &state,
            kv_versions: &kv_versions,
            target_versions: &target_versions,
        };
        let Some(c) = classify_in_memory(&text, &status, &view) else {
            // Not a zkv memo at all. Strict callers still treat this as a hard
            // error (preserving prior behavior); otherwise it's foreign traffic.
            if strict {
                bail!("malformed zkv memo: {text:?}");
            }
            continue;
        };
        // Strict mode bails on wire corruption (malformed / unrecoverable sig)
        // but never on authorization/lifecycle policy; those are silent drops.
        if strict {
            match c.outcome {
                RowOutcome::Dropped(DropReason::MalformedMemo(_)) => {
                    bail!("malformed zkv memo: {text:?}")
                }
                RowOutcome::Dropped(DropReason::UnsupportedVersion { version }) => {
                    bail!("unsupported zkv protocol version {version}: {text:?}")
                }
                RowOutcome::Dropped(DropReason::BadSignature) => {
                    bail!(
                        "invalid zkv signature for key {:?}",
                        c.key.as_deref().unwrap_or("")
                    )
                }
                _ => {}
            }
        }
        let outcome = apply_in_memory(
            &c,
            &status,
            &txid,
            block_time,
            &root_hex,
            &mut state,
            &mut init,
            &mut auth,
            &mut finalized,
            &mut version,
            &mut kv_versions,
            &mut target_versions,
        );
        if let RowOutcome::Dropped(reason) = outcome {
            tracing::debug!(%reason, "zkv replay dropped memo");
        }
    }
    state.retain(|_, ks| {
        ks.confirmed.is_some()
            || ks
                .pending
                .iter()
                .any(|op| matches!(op, PendingOp::Set { .. }))
    });
    Ok(ReplayResult {
        init,
        state,
        auth,
        finalized,
        version,
        kv_versions,
        target_versions,
    })
}

/// What replay did with a single memo row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowOutcome {
    /// Folded into confirmed state, the registry, or the init flag.
    Applied,
    /// Recognized and authorized but below the confirmation threshold; queued
    /// as a pending data op, or a confirming INIT / management op (which
    /// confers no registry change yet).
    Pending,
    /// Not applied. Carries the standardized reason.
    Dropped(DropReason),
}

/// The classification of one memo: its parsed fields (when available) plus the
/// [`RowOutcome`]. Produced by the shared classifier and applied by either the
/// in-memory replay or the history builder. `op`/`key`/`value`/`signer_hex`
/// are `None` when the memo failed to parse (or the signer didn't recover).
#[derive(Clone, Debug)]
pub struct Classified {
    pub op: Option<Op>,
    pub key: Option<String>,
    pub value: Option<String>,
    /// 130-char hex of the recoverable signature, when the memo parsed far
    /// enough to carry one (`None` for malformed / unsupported-version memos).
    pub sig_hex: Option<String>,
    pub signer_hex: Option<String>,
    /// The replay-protection sequence the writer referenced (from the wire), so
    /// the apply step can advance the entity's high-water to `seq + 1` (absorbing
    /// any gap left by an unconfirmed write). 0 for non-versioned / unparsed.
    pub seq: u64,
    pub outcome: RowOutcome,
}

/// An immutable view of a database's confirmed replayed state, passed to the
/// classifier so the in-memory replay (`replay_with_seed` / `replay_audit`) and
/// the `verify_memo` path judge a memo against the same projection. Borrows
/// only, so it is cheap to rebuild per row from the live fold or over a finished
/// [`ReplayResult`].
struct ReplayView<'a> {
    receiver: &'a str,
    root_hex: &'a str,
    init: &'a InitState,
    auth: &'a AuthRegistry,
    finalized: bool,
    state: &'a BTreeMap<String, KeyState>,
    kv_versions: &'a BTreeMap<String, u64>,
    target_versions: &'a BTreeMap<String, u64>,
}

impl<'a> ReplayView<'a> {
    /// A view over a finished [`ReplayResult`] (the `verify_memo` path).
    fn of(receiver: &'a str, root_hex: &'a str, r: &'a ReplayResult) -> Self {
        Self {
            receiver,
            root_hex,
            init: &r.init,
            auth: &r.auth,
            finalized: r.finalized,
            state: &r.state,
            kv_versions: &r.kv_versions,
            target_versions: &r.target_versions,
        }
    }
}

/// The pure authorization + lifecycle decision for one parsed, signer-recovered
/// command. This is the single source of truth shared by the in-memory replay,
/// the snapshot promote path, and the history builder, so the three cannot
/// disagree about what is applied versus dropped.
///
/// `key_exists` is the *confirmed* existence of `key`, supplied by the caller
/// from its own store (in-memory map or a `SELECT 1 FROM kv`). It stays a plain
/// `bool` (not a store handle), so this function is pure and infallible; the
/// snapshot path's lookup is a fallible SQLite query the caller runs and
/// `?`-propagates before calling in.
///
/// Management ops that pass the init + owner gates are reported `Applied`; the
/// *semantic* sub-failures (last-owner protection, writer-targets-owner, bad
/// target/scope) are surfaced later by [`AuthRegistry::apply_management`]'s
/// return value, because they cannot be decided without consulting the registry
/// mutation. The resulting registry state is identical either way.
// A pure, store-agnostic decision over genuinely distinct inputs: the per-row
// op / key / signer / status, and the {root, init, auth, finalized} gate.
// `key_exists` stays a plain bool precisely so the in-memory and SQL callers
// each supply it from their own store; a struct would not remove the per-row
// args. This is the single shared gate, so it deliberately stays a free fn.
#[allow(clippy::too_many_arguments)]
pub fn decide(
    op: Op,
    key: &str,
    signer_hex: &str,
    root_hex: &str,
    status: &WriteStatus,
    init: &InitState,
    auth: &AuthRegistry,
    finalized: bool,
    key_exists: bool,
) -> RowOutcome {
    let applied_or_pending = match status {
        WriteStatus::Confirmed => RowOutcome::Applied,
        WriteStatus::Confirming { .. } => RowOutcome::Pending,
    };
    // A confirmed FINALIZE seals the database: nothing more is ever applied,
    // including a second FINALIZE. This one-way latch is checked before any
    // per-op logic so the rule is uniform across every opcode.
    if finalized {
        return RowOutcome::Dropped(DropReason::Finalized);
    }
    match op {
        Op::Init => {
            // INIT is gated solely on its receiver-bound root signature: the
            // signer (recovered from the signature over this database's
            // receiver) must be the root key, and the first valid INIT wins.
            // The address the wire memo echoes is advisory; it is *not* part
            // of the signed payload (see `signed_init_payload`), so a corrected
            // birthday or a cosmetically-different echo cannot forge or
            // invalidate an INIT. A signature from anyone but root over our
            // receiver is computationally infeasible, so this fully ties the
            // INIT to this database without pinning the birthday/UFVK string.
            if signer_hex != root_hex {
                return RowOutcome::Dropped(DropReason::ForgedInit);
            }
            // First valid INIT wins; any later one is a duplicate.
            if !matches!(init, InitState::Uninitialized) {
                return RowOutcome::Dropped(DropReason::DuplicateInit);
            }
            let _ = key; // the embedded echo is not authorization input
            applied_or_pending
        }
        // Owner-only ops: the registry-management ops plus FINALIZE and VERSION.
        // They share the init + owner gate; their distinct effects (registry
        // mutation, the finalized latch, or the VERSION transition) are applied
        // and may refine to Dropped later.
        Op::OwnerSet
        | Op::OwnerDel
        | Op::WriterSet
        | Op::WriterDel
        | Op::Finalize
        | Op::Version => {
            if !init.is_initialized() {
                return RowOutcome::Dropped(DropReason::NotInitialized);
            }
            if !auth.is_owner(signer_hex) {
                return RowOutcome::Dropped(DropReason::NotOwner);
            }
            applied_or_pending
        }
        Op::Set | Op::SetL | Op::Del => {
            if !init.is_initialized() {
                return RowOutcome::Dropped(DropReason::NotInitialized);
            }
            if let Err(reason) = auth.authorize(signer_hex, op, key_exists) {
                return RowOutcome::Dropped(reason);
            }
            applied_or_pending
        }
    }
}

/// Parse, recover the signer, and [`decide`] for one entry against an in-memory
/// state map. Returns `None` for a non-zkv memo (foreign traffic, not a row).
fn classify_in_memory(text: &str, status: &WriteStatus, view: &ReplayView) -> Option<Classified> {
    let cmd = match parse_text_memo_detailed(text) {
        Ok(cmd) => cmd,
        Err(MemoReject::NotZkv) => return None,
        Err(MemoReject::UnsupportedVersion(version)) => {
            return Some(Classified {
                op: None,
                key: None,
                value: None,
                sig_hex: None,
                signer_hex: None,
                seq: 0,
                outcome: RowOutcome::Dropped(DropReason::UnsupportedVersion { version }),
            })
        }
        Err(MemoReject::Malformed(fmt)) => {
            return Some(Classified {
                op: None,
                key: None,
                value: None,
                sig_hex: None,
                signer_hex: None,
                seq: 0,
                outcome: RowOutcome::Dropped(DropReason::MalformedMemo(fmt)),
            })
        }
    };
    let (signer_hex, outcome) = classify_parsed(&cmd, status, view);
    Some(Classified {
        op: Some(cmd.op),
        key: Some(cmd.key),
        value: cmd.value,
        sig_hex: Some(cmd.sig_hex),
        signer_hex,
        seq: cmd.seq,
        outcome,
    })
}

/// Recover the signer of an already-parsed command and classify it
/// (version-CAS + [`decide`]) against a database's confirmed state. Returns the
/// recovered signer (canonical `zkvid1…`, `None` on a bad signature) and the
/// [`RowOutcome`].
///
/// This is the post-parse core of `classify_in_memory`, factored out so the
/// public [`verify_memo`] (the `zkv verify` command) reaches the same verdict
/// as the audit/history view; there is one copy of the authorization +
/// ordering logic.
fn classify_parsed(
    cmd: &ZkvCommand,
    status: &WriteStatus,
    view: &ReplayView,
) -> (Option<String>, RowOutcome) {
    // Reconstruct the exact payload the writer signed and recover the signer.
    // INIT binds only the receiver (its wire address is an advisory, unsigned
    // echo); every other op binds the receiver plus the replay-protection
    // sequence the writer put on the wire (the `seq` prefix on the signature
    // line); a first-line comment, when present, is folded in. See
    // [`recover_memo_signer`] / [`payload_for`].
    let Some(signer) = recover_memo_signer(cmd, view.receiver) else {
        return (None, RowOutcome::Dropped(DropReason::BadSignature));
    };
    let signer_hex = pubkey_bech32(&signer);
    // version-CAS (bounded-forward): a versioned op's sequence must fall in the
    // entity's accepted window `current ..= current + VERSION_WINDOW`. Below it
    // is a stale replay / lost CAS; far above it is a desync beyond tolerance.
    // Either way drop as `StaleVersion`, but keep the recovered signer for the
    // audit. See [`seq_in_window`] / [`VERSION_WINDOW`].
    if !seq_in_window(
        cmd.op,
        &cmd.key,
        cmd.seq,
        view.kv_versions,
        view.target_versions,
    ) {
        return (
            Some(signer_hex),
            RowOutcome::Dropped(DropReason::StaleVersion),
        );
    }
    let key_exists = view
        .state
        .get(&cmd.key)
        .is_some_and(|ks| ks.confirmed.is_some());
    let outcome = decide(
        cmd.op,
        &cmd.key,
        &signer_hex,
        view.root_hex,
        status,
        view.init,
        view.auth,
        view.finalized,
        key_exists,
    );
    (Some(signer_hex), outcome)
}

/// Reconstruct the receiver-bound payload a parsed command was signed over (via
/// [`payload_for`], so any first-line comment is folded into the domain) and
/// recover the signer's public key (`None` if the signature does not recover).
fn recover_memo_signer(cmd: &ZkvCommand, receiver: &str) -> Option<secp256k1::PublicKey> {
    recover_signer(&payload_for(receiver, cmd), &cmd.sig_hex)
}

/// The result of verifying a single raw zkv memo (the `zkv verify` command).
///
/// Produced by [`verify_signature`] (signature only) and [`verify_memo`]
/// (signature **plus** authorization and ordering against a database's replayed
/// state). The signature check is identical in both; only [`outcome`] (the
/// authorization/ordering verdict) depends on having local state.
///
/// [`outcome`]: MemoVerification::outcome
#[derive(Clone, Debug)]
pub struct MemoVerification {
    /// The opcode the memo carried.
    pub op: Op,
    /// The wire key: the data key for SET/SETL/DEL, the target `zkvid1…` for the
    /// management ops, the advisory address echo for INIT, empty for FINALIZE.
    pub key: String,
    /// The value, for the ops that carry one.
    pub value: Option<String>,
    /// The replay-protection sequence the writer signed over.
    pub seq: u64,
    /// 130-char hex of the 65-byte recoverable signature.
    pub sig_hex: String,
    /// Whether the signature is cryptographically valid over the receiver-bound
    /// payload, i.e. a signer was recovered. This is the "the message
    /// verifies" check, and the only check [`verify_signature`] performs.
    pub signature_valid: bool,
    /// The recovered signer's canonical `zkvid1…` pubkey, or `None` when the
    /// signature did not recover (`signature_valid == false`).
    pub signer: Option<String>,
    /// Whether the recovered signer is the database's root key (the INIT signer
    /// / owner #1). `None` when no signer recovered.
    pub is_root: Option<bool>,
    /// The full authorization + ordering verdict, present only for
    /// [`verify_memo`] (it needs a database's replayed state). `Applied` means
    /// the memo would be honored as a confirmed write; `Dropped(reason)` gives
    /// the reason it would not (unauthorized signer, stale/replayed sequence,
    /// finalized database, …). `None` for signature-only verification, which by
    /// design checks neither authorization nor ordering.
    pub outcome: Option<RowOutcome>,
}

impl MemoVerification {
    fn from_command(
        cmd: ZkvCommand,
        root_hex: &str,
        signer: Option<String>,
        outcome: Option<RowOutcome>,
    ) -> Self {
        let is_root = signer.as_ref().map(|s| s == root_hex);
        Self {
            op: cmd.op,
            key: cmd.key,
            value: cmd.value,
            seq: cmd.seq,
            sig_hex: cmd.sig_hex,
            signature_valid: signer.is_some(),
            signer,
            is_root,
            outcome,
        }
    }
}

/// Verify a raw zkv memo's **signature only**, against a database identified
/// solely by its `receiver` domain ([`receiver_domain`]) and `root_hex` (the
/// canonical `zkvid1…` of its UFVK-derived root key), both derivable from a
/// zkv address with no synced state.
///
/// Recovers the signer from the receiver-bound payload and reports whether the
/// signature is valid and who signed it. It deliberately does **not** check
/// whether that signer is authorized to write, nor whether the sequence is in
/// order (replay protection): those need the database's replayed state. Use
/// [`verify_memo`] for the full verdict.
///
/// `Err` reports a memo that didn't parse far enough to carry a signature
/// (foreign traffic, malformed framing, or a newer protocol version).
pub fn verify_signature(
    text: &str,
    receiver: &str,
    root_hex: &str,
) -> Result<MemoVerification, MemoReject> {
    let cmd = parse_text_memo_detailed(text)?;
    let signer = recover_memo_signer(&cmd, receiver).map(|pk| pubkey_bech32(&pk));
    Ok(MemoVerification::from_command(cmd, root_hex, signer, None))
}

/// Fully verify a raw zkv memo against a database's replayed `state`:
/// signature, authorization (owner / writer scope), **and** ordering / replay
/// protection (the version-CAS window). The memo is judged as if it were a
/// confirmed write at the head of the supplied state.
///
/// `receiver` and `root_hex` identify the database exactly as for
/// [`verify_signature`]; `state` is a replayed snapshot (e.g. from
/// `load_state` / `Database::read`). The verdict is on
/// [`MemoVerification::outcome`].
pub fn verify_memo(
    text: &str,
    receiver: &str,
    root_hex: &str,
    state: &ReplayResult,
) -> Result<MemoVerification, MemoReject> {
    let cmd = parse_text_memo_detailed(text)?;
    let view = ReplayView::of(receiver, root_hex, state);
    let (signer, outcome) = classify_parsed(&cmd, &WriteStatus::Confirmed, &view);
    Ok(MemoVerification::from_command(
        cmd,
        root_hex,
        signer,
        Some(outcome),
    ))
}

/// version-CAS window check: is a versioned op's wire `seq` within the entity's
/// accepted window `current ..= current + VERSION_WINDOW` (with `current` the
/// entity's high-water, 0 if absent)? Non-versioned ops (INIT/VERSION/FINALIZE)
/// are always in window. The single source of truth for replay protection,
/// shared by the in-memory replay ([`classify_parsed`]) and the snapshot promote
/// (`apply_row`) so the two can't drift. See [`VERSION_WINDOW`] / [`bump_hw`].
pub(crate) fn seq_in_window(
    op: Op,
    key: &str,
    seq: u64,
    kv_versions: &BTreeMap<String, u64>,
    target_versions: &BTreeMap<String, u64>,
) -> bool {
    if !op.is_versioned() {
        return true;
    }
    let current = if op.is_data() {
        kv_versions.get(key)
    } else {
        target_versions.get(key)
    }
    .copied()
    .unwrap_or(0);
    seq >= current && seq <= current.saturating_add(VERSION_WINDOW)
}

/// Advance an entity's high-water sequence to at least `seq + 1`, the next
/// expected lower bound for the version-CAS window. Monotonic: it never moves
/// backward, so an out-of-order tail row can't lower the bound. Shared by every
/// honored-write site so the in-memory replay and the snapshot stay in lockstep.
pub(crate) fn bump_hw(map: &mut BTreeMap<String, u64>, key: String, seq: u64) {
    let e = map.entry(key).or_insert(0);
    *e = (*e).max(seq + 1);
}

/// Apply a classified row to in-memory state, returning the *final* outcome.
/// A management op reported `Applied` by [`decide`] may refine to `Dropped`
/// here if the registry mutation was a policy no-op (e.g. last-owner
/// protection; the state is unchanged either way. Shared by
/// [`replay_with_seed`] and [`replay_audit`].
// Folds one row into the seven disjoint mutable projections of a ReplayResult;
// each match arm touches a different subset, so threading them as a single &mut
// struct would force partial-borrow gymnastics without making the apply clearer.
#[allow(clippy::too_many_arguments)]
fn apply_in_memory(
    c: &Classified,
    status: &WriteStatus,
    txid: &str,
    block_time: Option<u32>,
    root_hex: &str,
    state: &mut BTreeMap<String, KeyState>,
    init: &mut InitState,
    auth: &mut AuthRegistry,
    finalized: &mut bool,
    version: &mut VersionState,
    kv_versions: &mut BTreeMap<String, u64>,
    target_versions: &mut BTreeMap<String, u64>,
) -> RowOutcome {
    match c.outcome {
        RowOutcome::Dropped(reason) => RowOutcome::Dropped(reason),
        RowOutcome::Applied => {
            let op = c.op.expect("applied row carries an op");
            match op {
                Op::Init => {
                    *init = InitState::Initialized;
                    auth.insert_owner(root_hex.to_owned());
                    RowOutcome::Applied
                }
                Op::OwnerSet | Op::OwnerDel | Op::WriterSet | Op::WriterDel => {
                    let target = c.key.as_deref().unwrap_or("");
                    let result = auth.apply_management(op, target, c.value.as_deref());
                    // Advance the target's high-water for EVERY owner-authorized,
                    // in-window management op, including one `apply_management`
                    // rejects as a *policy* no-op (e.g. LastOwnerProtected). If
                    // the bump were skipped on those, the unbumped on-chain memo
                    // would stay inside the replay window; and because some policy
                    // no-ops are state-dependent (last-owner protection flips once
                    // a second owner is seated), a replayed seq-0 `OWNERDEL <self>`
                    // could later remove that owner. Bumping unconditionally here
                    // mirrors the `Pending` branch below and the data-op tombstone
                    // rule, so a verbatim re-broadcast is dropped `StaleVersion`.
                    bump_hw(target_versions, target.to_owned(), c.seq);
                    match result {
                        Ok(()) => RowOutcome::Applied,
                        Err(reason) => RowOutcome::Dropped(reason),
                    }
                }
                Op::Version => {
                    match version.apply_version(c.key.as_deref().unwrap_or(""), c.value.as_deref())
                    {
                        Ok(()) => RowOutcome::Applied,
                        Err(reason) => RowOutcome::Dropped(reason),
                    }
                }
                Op::Set | Op::SetL => {
                    let key = c.key.clone().unwrap_or_default();
                    let entry = state.entry(key.clone()).or_default();
                    entry.confirmed = c.value.clone();
                    entry.updated_at = block_time;
                    entry.last_txid = Some(txid.to_owned());
                    bump_hw(kv_versions, key, c.seq);
                    RowOutcome::Applied
                }
                Op::Del => {
                    let key = c.key.clone().unwrap_or_default();
                    let entry = state.entry(key.clone()).or_default();
                    entry.confirmed = None;
                    // The high-water persists past the delete as a tombstone (the
                    // key may be pruned from `state`, but `kv_versions` keeps it),
                    // so a replayed original creation can't recreate the key.
                    bump_hw(kv_versions, key, c.seq);
                    RowOutcome::Applied
                }
                Op::Finalize => {
                    *finalized = true;
                    RowOutcome::Applied
                }
            }
        }
        RowOutcome::Pending => {
            let op = c.op.expect("pending row carries an op");
            if let WriteStatus::Confirming { done, required } = status {
                let (done, required) = (*done, *required);
                match op {
                    Op::Init => *init = InitState::Initializing { done, required },
                    Op::Set | Op::SetL => {
                        let key = c.key.clone().unwrap_or_default();
                        let entry = state.entry(key.clone()).or_default();
                        entry.pending.push(PendingOp::Set {
                            value: c.value.clone().unwrap_or_default(),
                            done,
                            required,
                            txid: txid.to_owned(),
                        });
                        // A pending data op advances the per-key high-water too, so
                        // a writer's own rapid successive same-key writes (each
                        // signed over the next sequence via the write path's
                        // in-flight count) all verify and stay visible in the live
                        // tail. Pending ops are always at the chain tip, after
                        // every confirmed row, so this never disturbs the
                        // snapshot/tail agreement (promote sees confirmed rows only).
                        bump_hw(kv_versions, key, c.seq);
                    }
                    Op::Del => {
                        let key = c.key.clone().unwrap_or_default();
                        let entry = state.entry(key.clone()).or_default();
                        entry.pending.push(PendingOp::Del {
                            done,
                            required,
                            txid: txid.to_owned(),
                        });
                        bump_hw(kv_versions, key, c.seq);
                    }
                    // Confirming owner-only management op: it confers no registry
                    // change until confirmed, but it *does* advance the target's
                    // high-water (symmetrically with pending data ops (F1)), so a
                    // writer's back-to-back management ops to the same target each
                    // sign the next sequence and all verify in the live tail rather
                    // than the second colliding on a stale sequence.
                    Op::OwnerSet | Op::OwnerDel | Op::WriterSet | Op::WriterDel => {
                        bump_hw(target_versions, c.key.clone().unwrap_or_default(), c.seq);
                    }
                    // FINALIZE and VERSION are not version-CAS'd and confer no
                    // registry or finalized-flag change until confirmed; no-ops.
                    Op::Finalize | Op::Version => {}
                }
            }
            RowOutcome::Pending
        }
    }
}

/// One row of input to [`replay_audit`]: a memo plus its chain position and
/// query-time status. Like [`ReplayEntry`] but carries the real `mined_height`
/// so the audit view can show a height column.
pub struct AuditEntry {
    pub mined_height: Option<u32>,
    /// Block timestamp (unix seconds) of the mined height, or `None` for
    /// mempool / unresolved.
    pub timestamp: Option<u32>,
    pub txid: String,
    pub text: String,
    pub status: WriteStatus,
}

/// One classified row in a replay history: what the memo was and what happened
/// to it. `op`/`key`/`value` are `None` when the memo failed to parse.
#[derive(Clone, Debug)]
pub struct AuditRow {
    pub mined_height: Option<u32>,
    /// Block timestamp (unix seconds) of the mined height, or `None` for
    /// mempool / unresolved.
    pub timestamp: Option<u32>,
    pub txid: String,
    pub op: Option<Op>,
    pub key: Option<String>,
    pub value: Option<String>,
    /// The raw memo text exactly as broadcast on-chain: what the writer
    /// signed and what readers parse. Preserved so a rejected write can be
    /// inspected byte-for-byte.
    pub raw: String,
    /// Compressed-hex of the recovered signer, or `None` when the signature
    /// could not be recovered at all (malformed / unsupported-version /
    /// bad-signature memo). `Some` here means the signature is
    /// cryptographically valid; any drop is then an authorization/lifecycle
    /// decision, not a signature failure.
    pub signer: Option<String>,
    pub outcome: RowOutcome,
}

/// Result of [`replay_audit`]: every recognized memo in chain order with its
/// outcome, plus the final replayed state. Unlike [`replay_with_seed`], the
/// state is **not** pruned; an audit keeps keys that were created and later
/// deleted.
#[derive(Clone, Debug)]
pub struct AuditResult {
    pub rows: Vec<AuditRow>,
    pub init: InitState,
    pub state: BTreeMap<String, KeyState>,
    pub auth: AuthRegistry,
    pub version: VersionState,
}

/// Replay a memo stream from scratch (no seed), recording a [`AuditRow`] per
/// recognized memo with the standardized [`DropReason`] for any that did not
/// take effect. Non-zkv memos are filtered out entirely (they aren't rows).
///
/// `entries` must be in chain order (mined_height ASC, mempool last, then txid
/// / output_index ASC), exactly as [`replay_with_seed`] expects.
pub fn replay_audit(
    entries: impl IntoIterator<Item = AuditEntry>,
    receiver: &str,
    pk: &secp256k1::PublicKey,
) -> AuditResult {
    let mut state: BTreeMap<String, KeyState> = BTreeMap::new();
    let mut init = InitState::Uninitialized;
    let mut auth = AuthRegistry::default();
    let mut finalized = false;
    let mut version = VersionState::default();
    let mut kv_versions: BTreeMap<String, u64> = BTreeMap::new();
    let mut target_versions: BTreeMap<String, u64> = BTreeMap::new();
    let root_hex = pubkey_bech32(pk);
    let mut rows = Vec::new();
    for entry in entries {
        let AuditEntry {
            mined_height,
            timestamp,
            txid,
            text,
            status,
        } = entry;
        let view = ReplayView {
            receiver,
            root_hex: &root_hex,
            init: &init,
            auth: &auth,
            finalized,
            state: &state,
            kv_versions: &kv_versions,
            target_versions: &target_versions,
        };
        let Some(c) = classify_in_memory(&text, &status, &view) else {
            continue; // not a zkv memo; not a row
        };
        let signer = c.signer_hex.clone();
        // The drop reason for a defeated replay / lost CAS is decided in
        // `classify_in_memory` (it compares the wire `seq` against the entity's
        // current version and yields `StaleVersion`), so the audit reason is
        // already precise here; no post-hoc signature bookkeeping needed.
        let outcome = apply_in_memory(
            &c,
            &status,
            &txid,
            None,
            &root_hex,
            &mut state,
            &mut init,
            &mut auth,
            &mut finalized,
            &mut version,
            &mut kv_versions,
            &mut target_versions,
        );
        rows.push(AuditRow {
            mined_height,
            timestamp,
            txid,
            op: c.op,
            key: c.key,
            value: c.value,
            raw: text,
            signer,
            outcome,
        });
    }
    AuditResult {
        rows,
        init,
        state,
        auth,
        version,
    }
}

/// A pubkey that once held authority on this database and has since been
/// fully revoked (no longer a current owner or writer and has not been
/// re-granted). Carries the revocation provenance: when it happened and
/// which owner signed the revoking `OWNERDEL`/`WRITERDEL`.
///
/// Current owners/writers live on [`AuditResult::auth`] / [`AuthRegistry`];
/// this is the complement (the tombstones), so a roles view can show
/// "revoked owner" / "revoked writer" rows with the date and revoker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevokedRole {
    /// The revoked signer's compressed-hex public key.
    pub pubkey: String,
    /// The role held immediately before revocation: `true` = owner,
    /// `false` = scoped writer.
    pub was_owner: bool,
    /// Capability tokens the writer last held (`CREATE`/`UPDATE`/`DESTROY`,
    /// canonical order). Empty for a revoked owner.
    pub capabilities: Vec<String>,
    /// Mined height of the revoking management op, or `None` if unmined.
    pub height: Option<u32>,
    /// Block timestamp (unix seconds) of the revocation, or `None`.
    pub timestamp: Option<u32>,
    /// Compressed-hex of the owner that signed the revocation, if recoverable.
    pub revoked_by: Option<String>,
}

/// Derive the set of revoked roles from an [`AuditResult`] by replaying its
/// *applied* management ops in chain order: every pubkey that was granted
/// authority (`INIT`/`OWNERSET`/`WRITERSET`) and later revoked
/// (`OWNERDEL`/`WRITERDEL`) without being re-granted. Newest revocation first.
///
/// Only `Applied` rows count (a pending/below-threshold or dropped management
/// op confers nothing), so this agrees with [`AuditResult::auth`]: a pubkey is
/// either current there or revoked here, never both.
pub fn revoked_roles(audit: &AuditResult) -> Vec<RevokedRole> {
    /// The live role a target currently holds as we replay forward.
    struct Held {
        was_owner: bool,
        capabilities: Vec<String>,
    }
    let mut held: BTreeMap<String, Held> = BTreeMap::new();
    let mut tombstones: BTreeMap<String, RevokedRole> = BTreeMap::new();
    for row in &audit.rows {
        if !matches!(row.outcome, RowOutcome::Applied) {
            continue;
        }
        let Some(op) = row.op else { continue };
        match op {
            // INIT makes the root key (the row's signer) owner #1.
            Op::Init => {
                if let Some(by) = row.signer.as_deref() {
                    held.insert(
                        by.to_owned(),
                        Held {
                            was_owner: true,
                            capabilities: Vec::new(),
                        },
                    );
                    tombstones.remove(by);
                }
            }
            Op::OwnerSet => {
                if let Some(target) = row.key.as_deref() {
                    held.insert(
                        target.to_owned(),
                        Held {
                            was_owner: true,
                            capabilities: Vec::new(),
                        },
                    );
                    tombstones.remove(target);
                }
            }
            Op::WriterSet => {
                if let Some(target) = row.key.as_deref() {
                    let capabilities = row
                        .value
                        .as_deref()
                        .and_then(Scope::parse)
                        .map(|s| s.capabilities().map(|c| c.as_str().to_owned()).collect())
                        .unwrap_or_default();
                    held.insert(
                        target.to_owned(),
                        Held {
                            was_owner: false,
                            capabilities,
                        },
                    );
                    tombstones.remove(target);
                }
            }
            Op::OwnerDel | Op::WriterDel => {
                if let Some(target) = row.key.as_deref() {
                    if let Some(prev) = held.remove(target) {
                        tombstones.insert(
                            target.to_owned(),
                            RevokedRole {
                                pubkey: target.to_owned(),
                                was_owner: prev.was_owner,
                                capabilities: prev.capabilities,
                                height: row.mined_height,
                                timestamp: row.timestamp,
                                revoked_by: row.signer.clone(),
                            },
                        );
                    }
                }
            }
            // Data ops, FINALIZE, and VERSION never grant or revoke a role.
            Op::Set | Op::SetL | Op::Del | Op::Finalize | Op::Version => {}
        }
    }
    let mut out: Vec<RevokedRole> = tombstones.into_values().collect();
    out.sort_by(|a, b| {
        b.height
            .unwrap_or(u32::MAX)
            .cmp(&a.height.unwrap_or(u32::MAX))
            .then(a.pubkey.cmp(&b.pubkey))
    });
    out
}

/// A pubkey that currently holds authority, paired with the provenance of the
/// grant that established its present role: the `INIT` (for the creator) or the
/// most recent `OWNERSET`/`WRITERSET` that set it.
///
/// The complement of [`revoked_roles`]: these are the survivors (the
/// tombstones are revoked), so together they partition every pubkey that ever
/// held authority. Owners first, then writers, each sorted by pubkey (the same
/// order [`AuthRegistry`] iterates), so a roles view can match these against the
/// registry for per-role "added when / by whom" provenance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantedRole {
    /// The signer's canonical `zkvid1…` public key.
    pub pubkey: String,
    /// `true` = owner (full authority), `false` = scoped writer.
    pub is_owner: bool,
    /// Capability tokens for a writer (`CREATE`/`UPDATE`/`DESTROY`, canonical
    /// order). Empty for an owner.
    pub capabilities: Vec<String>,
    /// Mined height of the granting op, or `None` if it's still unmined.
    pub height: Option<u32>,
    /// Block timestamp (unix seconds) of the grant, or `None`.
    pub timestamp: Option<u32>,
    /// The owner that signed the grant. For the creator this is itself (a
    /// self-signed `INIT`); `None` if it couldn't be recovered.
    pub granted_by: Option<String>,
    /// `true` when the grant was the database's `INIT`, i.e. this is the
    /// creator (owner #1), so `height`/`timestamp` are the database's birth.
    pub via_init: bool,
}

/// Derive the currently-authorized roles and their grant provenance from an
/// [`AuditResult`] by replaying its *applied* management ops in chain order:
/// every pubkey granted authority (`INIT`/`OWNERSET`/`WRITERSET`) and not since
/// revoked, carrying the height/timestamp/signer of the grant that set its
/// current role. See [`GrantedRole`].
///
/// Only `Applied` rows count (a pending/below-threshold or dropped management op
/// confers nothing), so the survivors here agree with [`AuditResult::auth`] and
/// are exactly the complement of [`revoked_roles`].
pub fn granted_roles(audit: &AuditResult) -> Vec<GrantedRole> {
    let mut held: BTreeMap<String, GrantedRole> = BTreeMap::new();
    for row in &audit.rows {
        if !matches!(row.outcome, RowOutcome::Applied) {
            continue;
        }
        let Some(op) = row.op else { continue };
        match op {
            // INIT makes the root key (the row's signer) owner #1, the creator.
            Op::Init => {
                if let Some(by) = row.signer.as_deref() {
                    held.insert(
                        by.to_owned(),
                        GrantedRole {
                            pubkey: by.to_owned(),
                            is_owner: true,
                            capabilities: Vec::new(),
                            height: row.mined_height,
                            timestamp: row.timestamp,
                            granted_by: row.signer.clone(),
                            via_init: true,
                        },
                    );
                }
            }
            Op::OwnerSet => {
                if let Some(target) = row.key.as_deref() {
                    held.insert(
                        target.to_owned(),
                        GrantedRole {
                            pubkey: target.to_owned(),
                            is_owner: true,
                            capabilities: Vec::new(),
                            height: row.mined_height,
                            timestamp: row.timestamp,
                            granted_by: row.signer.clone(),
                            via_init: false,
                        },
                    );
                }
            }
            Op::WriterSet => {
                if let Some(target) = row.key.as_deref() {
                    let capabilities = row
                        .value
                        .as_deref()
                        .and_then(Scope::parse)
                        .map(|s| s.capabilities().map(|c| c.as_str().to_owned()).collect())
                        .unwrap_or_default();
                    held.insert(
                        target.to_owned(),
                        GrantedRole {
                            pubkey: target.to_owned(),
                            is_owner: false,
                            capabilities,
                            height: row.mined_height,
                            timestamp: row.timestamp,
                            granted_by: row.signer.clone(),
                            via_init: false,
                        },
                    );
                }
            }
            Op::OwnerDel | Op::WriterDel => {
                if let Some(target) = row.key.as_deref() {
                    held.remove(target);
                }
            }
            // VERSION announces a protocol epoch and FINALIZE seals the
            // database; neither grants authority, so they contribute nothing to
            // the granted-roles projection.
            Op::Set | Op::SetL | Op::Del | Op::Version | Op::Finalize => {}
        }
    }
    let mut out: Vec<GrantedRole> = held.into_values().collect();
    // Owners before writers, each by pubkey (the registry's iteration order).
    out.sort_by(|a, b| b.is_owner.cmp(&a.is_owner).then(a.pubkey.cmp(&b.pubkey)));
    out
}
