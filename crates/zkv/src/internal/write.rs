//! Shared SET/DEL helpers: sign a memo, build a payment-to-self, optionally broadcast.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use anyhow::anyhow;
use zcash_address::ZcashAddress;
use zcash_client_backend::data_api::{
    error::Error as WalletError, wallet::ConfirmationsPolicy, WalletRead,
};
use zcash_protocol::{
    memo::{Memo, MemoBytes},
    value::Zatoshis,
};
use zip321::{Payment, TransactionRequest};

use crate::{
    config::{Role, WalletConfig},
    data::{get_db_paths, open_wallet_db},
    error,
    internal::{
        account::{account_keys, signing_key},
        pending::{self, PendingEntry},
        protocol::{
            build_init_memo, build_memo, parse_pubkey, pubkey_bech32, sign_command,
            signed_init_payload, signed_payload, signing_domain, InitState, Op, PendingOp,
            ReplayResult, VersionState,
        },
        send::pay,
        state::{load_state, INIT_CONFIRMATIONS},
        sync::run_sync,
    },
    remote::ConnectionArgs,
    ui::format_zec,
};

/// Typed write-path failures that the [`Database`](crate::db::Database) facade
/// maps to structured [`ZkvError`](crate::db::ZkvError) variants.
///
/// Carried as the source of an [`anyhow::Error`] (via `bail!`/`.into()`), so the
/// `anyhow::Result` signatures and the human-readable CLI output are unchanged,
/// while the facade recovers the structured form with `downcast_ref` instead of
/// parsing the message text. Keep each `Display` arm byte-for-byte in sync with
/// what the facade and the producer-side tests expect.
#[derive(Debug)]
pub enum WriteError {
    /// This machine holds only a viewing key (config role is `Watch`).
    WatchOnly { db: String },
    /// The signing key isn't authorized for this data op on this key.
    UnauthorizedData { op: String, key: String },
    /// A management op (`OWNER*`/`WRITER*`/`VERSION`) was attempted by a non-owner.
    OwnerOnly { op: String },
    /// No `INIT` memo has been broadcast yet.
    NotInitialized { db: String },
    /// `INIT` is broadcast but hasn't reached the write-confirmation threshold.
    Initializing {
        db: String,
        done: u32,
        required: u32,
    },
    /// A `VERSION` memo blocks writes from this (older) client build.
    ClientUpgradeRequired { required: u32, supported: u32 },
    /// The funding wallet can't cover the ZIP-317 fee.
    InsufficientFunds {
        available: u64,
        required: u64,
        pending: u64,
        network: crate::data::Network,
    },
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::WatchOnly { db } => {
                write!(f, "{db:?} is a watch-only database; you can't sign writes")
            }
            WriteError::UnauthorizedData { op, key } => write!(
                f,
                "this database's signing key is not authorized to {op} {key:?} \
                 (no owner/writer entry, or out of scope)",
            ),
            WriteError::OwnerOnly { op } => write!(
                f,
                "only an owner can run {op}; this database's signing key is not an owner",
            ),
            WriteError::NotInitialized { db } => write!(
                f,
                "database {db:?} is not yet initialized; broadcast INIT via `zkv init` \
                 (or fund this wallet so `zkv sync` can)",
            ),
            WriteError::Initializing { db, done, required } => write!(
                f,
                "database {db:?} is not yet initialized (INIT seen at {done}/{required} \
                 confirmations; wait {} more block(s))",
                required.saturating_sub(*done),
            ),
            WriteError::ClientUpgradeRequired {
                required,
                supported,
            } => write!(
                f,
                "this database has been upgraded to version {required} and blocks writes for \
                 clients older than that (this build supports up to version {supported}); \
                 update zkv to the latest version",
            ),
            WriteError::InsufficientFunds {
                available,
                required,
                pending,
                network,
            } => {
                if *pending > 0 {
                    write!(
                        f,
                        "Insufficient balance: {available} zats spendable, {required} zats needed \
                         (including fee).\n{pending} zats incoming ({}, confirming). Wait for \
                         confirmations and try again.",
                        format_zec(*pending as i64, *network).trim(),
                    )
                } else {
                    write!(
                        f,
                        "Insufficient balance: {available} zats spendable, {required} zats needed \
                         (including fee).\nSend funds to this database's funding UA (see `zkv show`) \
                         and re-run after they confirm.",
                    )
                }
            }
        }
    }
}

impl std::error::Error for WriteError {}

/// Best-effort append to the per-db pending cache after a successful broadcast.
/// Failure here doesn't fail the user-facing command; the broadcast already
/// succeeded; we'd just lose the "# mempool:" annotation in subsequent reads
/// until either `zkv get --confirmations=0` runs or the tx mines.
fn record_pending(db_name: &str, txid: &str, op: Op, key: &str, value: Option<&str>, memo: &str) {
    let entry = PendingEntry {
        txid: txid.to_owned(),
        op: op.as_str().to_owned(),
        key: key.to_owned(),
        value: value.map(str::to_owned),
        memo: Some(memo.to_owned()),
        broadcast_at_unix: pending::now_unix(),
    };
    if let Err(e) = pending::append(db_name, entry) {
        tracing::warn!("recording pending tx {txid}: {e:#}");
    }
}

pub struct PreparedWrite {
    pub zkv_addr: String,
    pub recipient_ua: String,
    pub memo_text: String,
    pub request: TransactionRequest,
}

/// Common setup: read config, decrypt seed, derive signing key + recipient UA.
struct Prep {
    zkv_addr: String,
    /// The database's [`receiver_domain`](crate::internal::protocol::receiver_domain)
    /// (what `ZKV0` signatures bind to; see [`signing_domain`]).
    receiver_hex: String,
    recipient_ua: String,
    sk: secp256k1::SecretKey,
}

fn prepare_common(db_name: &str) -> anyhow::Result<Prep> {
    let cfg = WalletConfig::read(db_name)?;
    if cfg.role != Role::Admin {
        anyhow::bail!(WriteError::WatchOnly {
            db: db_name.to_owned(),
        });
    }
    let keys = account_keys(&cfg, db_name)?;
    let account_index = keys
        .account_index
        .ok_or_else(|| anyhow!("watch-only account can't sign"))?;
    let sk = signing_key(&cfg, account_index)?;
    Ok(Prep {
        zkv_addr: keys.zkv_addr,
        receiver_hex: keys.receiver_hex,
        recipient_ua: keys.recipient_ua,
        sk,
    })
}

/// Count this client's distinct in-flight (broadcast, unconfirmed) writes to
/// `key` whose op is in `ops`, so the write path can sign over the *next*
/// version and a writer's own rapid successive same-key writes don't
/// self-conflict (each lands on a fresh version rather than all colliding on
/// `confirmed_version`).
///
/// Sources, deduped by txid (mirroring `db::merge_pending`): the wallet's
/// mempool view (`result.state[key].pending`, data ops only; management ops
/// aren't projected into key-state) ∪ `pending.toml` rows for this key whose op
/// matches. Trade-off: an in-flight op that never confirms keeps inflating the
/// count until it ages out of `pending.toml` (~1h), stalling later same-key
/// writes; that is the documented optimistic-concurrency wart.
fn inflight_count(db_name: &str, result: &ReplayResult, key: &str, ops: &[&str]) -> u64 {
    let mut txids: HashSet<String> = HashSet::new();
    if let Some(ks) = result.state.get(key) {
        for p in &ks.pending {
            let txid = match p {
                PendingOp::Set { txid, .. } | PendingOp::Del { txid, .. } => txid,
            };
            if !txid.is_empty() {
                txids.insert(txid.clone());
            }
        }
    }
    for e in pending::load(db_name).unwrap_or_default() {
        if e.key == key && ops.contains(&e.op.as_str()) && !e.txid.is_empty() {
            txids.insert(e.txid);
        }
    }
    txids.len() as u64
}

/// Data ops that share the per-key replay-protection version.
const DATA_OPS: &[&str] = &["SET", "SETL", "DEL"];
/// Management ops that share the per-target replay-protection version.
const MGMT_OPS: &[&str] = &["OWNERADD", "OWNERDEL", "WRITERADD", "WRITERDEL"];

/// Build a single zero-value memo output (the unit of a zkv write). Shared by
/// the single-write path ([`build_request`]) and the batch path
/// ([`prepare_batch`]), which collects many of these into one request.
fn build_payment(recipient_ua: &str, memo_text: &str) -> anyhow::Result<Payment> {
    let recipient =
        ZcashAddress::from_str(recipient_ua).map_err(|e| anyhow!("bad recipient UA: {e}"))?;
    Payment::new(
        recipient,
        // Memo-only write: a zero-value shielded output (in the database's
        // pool). Zcash imposes no dust threshold on shielded outputs, so the
        // tx only ever costs the fee.
        Some(Zatoshis::ZERO),
        Some(MemoBytes::from(Memo::from_str(memo_text)?)),
        None,
        None,
        vec![],
    )
    .map_err(|e| anyhow!("failed to build payment: {e}"))
}

fn build_request(recipient_ua: &str, memo_text: &str) -> anyhow::Result<TransactionRequest> {
    let payment = build_payment(recipient_ua, memo_text)?;
    TransactionRequest::new(vec![payment]).map_err(|e| anyhow!("bad tx request: {e}"))
}

/// Sign + build a SET/DEL memo + assemble a `TransactionRequest`. Bails if
/// the database is not yet initialized (the INIT memo has not confirmed at
/// the write threshold) or if this database's signing key lacks the authority
/// to perform `op` on `key`. Does NOT broadcast.
pub fn prepare(
    db_name: &str,
    op: Op,
    key: &str,
    value: Option<&str>,
) -> anyhow::Result<PreparedWrite> {
    // Refuse writes until INIT has reached the write threshold. Readers will
    // silently drop pre-INIT writes anyway (per the replay rules); failing
    // fast here saves the user a wasted tx fee.
    let result = load_state(db_name, INIT_CONFIRMATIONS, false)?;
    ensure_initialized(db_name, &result.init)?;
    ensure_not_finalized(db_name, result.finalized)?;
    ensure_client_supported(&result.version)?;

    let mut prep = prepare_common(db_name)?;
    // Authorization pre-check: refuse to broadcast a data write that readers
    // would drop. Owners may write anything; writers are gated by scope and
    // the create-vs-update distinction (judged against confirmed state).
    let signer_hex = pubkey_bech32(&crate::internal::protocol::pubkey_of(&prep.sk));
    let key_exists = result
        .state
        .get(key)
        .is_some_and(|ks| ks.confirmed.is_some());
    if !result.auth.may_write(&signer_hex, op, key_exists) {
        anyhow::bail!(WriteError::UnauthorizedData {
            op: op.as_str().to_owned(),
            key: key.to_owned(),
        });
    }

    // Sign over the receiver-bound, version-stamped domain. The version is the
    // confirmed per-key count plus this client's own in-flight writes to `key`,
    // so back-to-back same-key writes each land on a fresh version (readers
    // advance `kv_versions` on pending ops too, so all stay visible).
    let version = result.kv_versions.get(key).copied().unwrap_or(0)
        + inflight_count(db_name, &result, key, DATA_OPS);
    let domain = signing_domain(&prep.receiver_hex, op, version);
    let payload = signed_payload(&domain, op, key, value);
    let sig = sign_command(&prep.sk, &payload);
    // Wipe the signing key as soon as we're done with it (best-effort; secp256k1
    // has no zeroize-on-drop) so it doesn't linger in process memory.
    prep.sk.non_secure_erase();
    // The sequence rides on the wire (compact prefix on the signature line) so
    // readers know which version this write referenced without guessing.
    let memo = build_memo(op, key, value, version, &sig)?;
    let memo_text = match Memo::try_from(memo)? {
        Memo::Text(t) => t.to_string(),
        _ => unreachable!("we just built a text memo"),
    };
    let request = build_request(&prep.recipient_ua, &memo_text)?;
    Ok(PreparedWrite {
        zkv_addr: prep.zkv_addr,
        recipient_ua: prep.recipient_ua,
        memo_text,
        request,
    })
}

/// One data op (`SET`/`SETL`/`DEL`) in a batch write, as handed to the
/// internal layer. Borrows from the caller; the facade builds these from its
/// public `WriteOp`.
pub struct BatchItem<'a> {
    pub op: Op,
    pub key: &'a str,
    pub value: Option<&'a str>,
}

/// One planned batch op with its resolved replay version and the key-existence
/// flag (as of this op, accounting for earlier ops in the same batch) used for
/// the authorization check.
#[derive(Clone, Debug, PartialEq, Eq)]
struct BatchPlanItem {
    op: Op,
    key: String,
    value: Option<String>,
    version: u64,
    key_exists: bool,
}

/// Resolve the per-key replay version (and running key-existence) for each op in
/// a batch. Pure: no I/O, so it is unit-testable in isolation.
///
/// `base(key)` is the version the *first* batch op touching `key` should sign
/// over: the confirmed per-key count plus this client's distinct in-flight
/// txids for the key (the same value the single-write path uses). Each
/// subsequent op on the same key advances a local counter, so two ops on one
/// key in one tx get consecutive versions, exactly as a reader advances
/// `kv_versions` across the outputs of that tx. Distinct keys are independent.
///
/// `key_exists` mirrors how a reader judges create-vs-update *within* the tx: a
/// `SET`/`SETL` earlier in the batch makes a later op on that key see the key as
/// existing; a `DEL` makes a later op see it as absent. Seeded from
/// `confirmed_exists` (keys with a confirmed value at batch start).
fn sequence_versions(
    items: &[BatchItem<'_>],
    mut base: impl FnMut(&str) -> u64,
    confirmed_exists: &HashSet<String>,
) -> Vec<BatchPlanItem> {
    let mut local_count: HashMap<&str, u64> = HashMap::new();
    let mut exists: HashMap<&str, bool> = HashMap::new();
    let mut plan = Vec::with_capacity(items.len());
    for item in items {
        let seen = local_count.get(item.key).copied().unwrap_or(0);
        let version = base(item.key) + seen;
        local_count.insert(item.key, seen + 1);

        let key_exists = exists
            .get(item.key)
            .copied()
            .unwrap_or_else(|| confirmed_exists.contains(item.key));
        // Advance running existence for the next op on this key.
        exists.insert(item.key, matches!(item.op, Op::Set | Op::SetL));

        plan.push(BatchPlanItem {
            op: item.op,
            key: item.key.to_owned(),
            value: item.value.map(str::to_owned),
            version,
            key_exists,
        });
    }
    plan
}

/// A prepared batch write: one [`TransactionRequest`] carrying N zero-value memo
/// outputs (one per op), plus the per-op memo texts so the broadcaster can
/// record one `pending.toml` row per op (all sharing the one txid).
pub struct PreparedBatch {
    pub zkv_addr: String,
    pub recipient_ua: String,
    pub plan_ops: Vec<(Op, String, Option<String>)>,
    pub memo_texts: Vec<String>,
    pub request: TransactionRequest,
}

/// Sign + build N SET/DEL memos + assemble ONE `TransactionRequest` (a
/// "sendmany"). Amortizes the expensive setup: one `load_state`, one seed
/// decrypt, and one `inflight_count` per distinct key. Same init/finalized/
/// version-write gates as [`prepare`], checked once for the whole batch. Bails
/// on an empty batch or if this signing key lacks authority for any op. Does NOT
/// broadcast.
pub fn prepare_batch(db_name: &str, ops: &[BatchItem<'_>]) -> anyhow::Result<PreparedBatch> {
    if ops.is_empty() {
        anyhow::bail!("empty batch: nothing to write");
    }

    let result = load_state(db_name, INIT_CONFIRMATIONS, false)?;
    ensure_initialized(db_name, &result.init)?;
    ensure_not_finalized(db_name, result.finalized)?;
    ensure_client_supported(&result.version)?;

    let mut prep = prepare_common(db_name)?;
    let signer_hex = pubkey_bech32(&crate::internal::protocol::pubkey_of(&prep.sk));

    // Base version per distinct key (confirmed count + in-flight txids). Memoize
    // so `inflight_count` (which reads pending.toml) runs at most once per key.
    let mut base_cache: HashMap<String, u64> = HashMap::new();
    let base = |key: &str| -> u64 {
        if let Some(v) = base_cache.get(key) {
            return *v;
        }
        let v = result.kv_versions.get(key).copied().unwrap_or(0)
            + inflight_count(db_name, &result, key, DATA_OPS);
        base_cache.insert(key.to_owned(), v);
        v
    };

    let confirmed_exists: HashSet<String> = ops
        .iter()
        .map(|i| i.key)
        .filter(|k| {
            result
                .state
                .get(*k)
                .is_some_and(|ks| ks.confirmed.is_some())
        })
        .map(str::to_owned)
        .collect();

    let plan = sequence_versions(ops, base, &confirmed_exists);

    let mut payments = Vec::with_capacity(plan.len());
    let mut memo_texts = Vec::with_capacity(plan.len());
    let mut plan_ops = Vec::with_capacity(plan.len());
    for item in plan {
        // Authorization pre-check per op, judged against existence as of this op
        // (so a create earlier in the batch makes a later write an update).
        if !result.auth.may_write(&signer_hex, item.op, item.key_exists) {
            anyhow::bail!(WriteError::UnauthorizedData {
                op: item.op.as_str().to_owned(),
                key: item.key.clone(),
            });
        }
        let domain = signing_domain(&prep.receiver_hex, item.op, item.version);
        let payload = signed_payload(&domain, item.op, &item.key, item.value.as_deref());
        let sig = sign_command(&prep.sk, &payload);
        let memo = build_memo(
            item.op,
            &item.key,
            item.value.as_deref(),
            item.version,
            &sig,
        )?;
        let memo_text = match Memo::try_from(memo)? {
            Memo::Text(t) => t.to_string(),
            _ => unreachable!("we just built a text memo"),
        };
        payments.push(build_payment(&prep.recipient_ua, &memo_text)?);
        memo_texts.push(memo_text);
        plan_ops.push((item.op, item.key, item.value));
    }
    // Wipe the signing key once every op in the batch is signed (best-effort;
    // secp256k1 has no zeroize-on-drop).
    prep.sk.non_secure_erase();

    let request = TransactionRequest::new(payments).map_err(|e| anyhow!("bad tx request: {e}"))?;
    Ok(PreparedBatch {
        zkv_addr: prep.zkv_addr,
        recipient_ua: prep.recipient_ua,
        plan_ops,
        memo_texts,
        request,
    })
}

/// Sign + build an owner/writer management memo (`OWNERADD`/`OWNERDEL`/
/// `WRITERADD`/`WRITERDEL`). `target` is the affected pubkey (`zkvid1…` or
/// hex); `scope` is the capability string for `WRITERADD` (ignored
/// otherwise). Bails if the database isn't initialized or this signing key is
/// not a current owner. Does NOT broadcast.
pub fn prepare_management(
    db_name: &str,
    op: Op,
    target: &str,
    scope: Option<&str>,
) -> anyhow::Result<PreparedWrite> {
    debug_assert!(op.is_management(), "prepare_management on a non-mgmt op");
    let result = load_state(db_name, INIT_CONFIRMATIONS, false)?;
    ensure_initialized(db_name, &result.init)?;
    ensure_not_finalized(db_name, result.finalized)?;
    ensure_client_supported(&result.version)?;

    let mut prep = prepare_common(db_name)?;
    let signer_hex = pubkey_bech32(&crate::internal::protocol::pubkey_of(&prep.sk));
    if !result.auth.is_owner(&signer_hex) {
        anyhow::bail!(WriteError::OwnerOnly {
            op: op.as_str().to_owned(),
        });
    }
    // Accept the target as either the canonical `zkvid1…` Bech32m form or raw
    // compressed/uncompressed hex, and normalize to the canonical form. This is
    // the one place CLI/GUI input crosses into the protocol; the wire memo, the
    // signed payload, and the authorization registry all key on the canonical
    // string (`pubkey_bech32`), so we pin it here and use it for everything
    // downstream. Normalizing also means we never broadcast a memo that replay
    // would silently drop as an unparseable target. FINALIZE carries no target,
    // so there's nothing to parse there.
    let target_str = if op == Op::Finalize {
        String::new()
    } else {
        let target_pk = parse_pubkey(target).ok_or_else(|| {
            anyhow!(
                "target {target:?} is not a valid pubkey (expected a zkvid1… key or compressed hex)"
            )
        })?;
        pubkey_bech32(&target_pk)
    };
    let value = match op {
        Op::WriterAdd => Some(scope.ok_or_else(|| anyhow!("WRITERADD requires a scope"))?),
        _ => None,
    };

    // Sign over the receiver-bound domain stamped with the target's version
    // (confirmed count + this client's in-flight management ops to the target),
    // so a verbatim re-broadcast of a stale OWNER*/WRITER* memo recovers the
    // wrong signer once the target's version has advanced.
    let version = result
        .target_versions
        .get(&target_str)
        .copied()
        .unwrap_or(0)
        + inflight_count(db_name, &result, &target_str, MGMT_OPS);
    let domain = signing_domain(&prep.receiver_hex, op, version);
    let payload = signed_payload(&domain, op, &target_str, value);
    let sig = sign_command(&prep.sk, &payload);
    // Wipe the signing key as soon as we're done with it (best-effort).
    prep.sk.non_secure_erase();
    let memo = build_memo(op, &target_str, value, version, &sig)?;
    let memo_text = match Memo::try_from(memo)? {
        Memo::Text(t) => t.to_string(),
        _ => unreachable!("we just built a text memo"),
    };
    let request = build_request(&prep.recipient_ua, &memo_text)?;
    Ok(PreparedWrite {
        zkv_addr: prep.zkv_addr,
        recipient_ua: prep.recipient_ua,
        memo_text,
        request,
    })
}

/// Shared init-state guard: bail with a typed, actionable error unless the
/// database is initialized.
fn ensure_initialized(db_name: &str, init: &InitState) -> anyhow::Result<()> {
    match init {
        InitState::Initialized => Ok(()),
        InitState::Initializing { done, required } => Err(WriteError::Initializing {
            db: db_name.to_owned(),
            done: *done,
            required: *required,
        }
        .into()),
        InitState::Uninitialized => Err(WriteError::NotInitialized {
            db: db_name.to_owned(),
        }
        .into()),
    }
}

/// Refuse any write to a sealed database. A confirmed FINALIZE is a one-way
/// latch; readers drop every subsequent write. We fail fast here instead
/// of wasting a tx fee on a memo nobody will honor.
fn ensure_not_finalized(db_name: &str, finalized: bool) -> anyhow::Result<()> {
    if finalized {
        anyhow::bail!("database {db_name:?} is finalized; no further writes are possible");
    }
    Ok(())
}

/// Shared write-side version guard. Bails if the database has been upgraded
/// past this build's [`MAX_DB_VERSION`](crate::internal::protocol::MAX_DB_VERSION)
/// and the controlling `VERSION` memo blocks writes (`blockwrite`/`blockall`),
/// so an out-of-date client doesn't broadcast a write the new epoch's rules may
/// misinterpret. The facade downcasts the typed
/// [`WriteError::ClientUpgradeRequired`] and surfaces it as
/// `db::ZkvError::ClientUpgradeRequired`.
fn ensure_client_supported(version: &VersionState) -> anyhow::Result<()> {
    if version.blocks_write() {
        return Err(WriteError::ClientUpgradeRequired {
            required: version.version,
            supported: crate::internal::protocol::MAX_DB_VERSION,
        }
        .into());
    }
    Ok(())
}

/// Sign + build the INIT memo + assemble a `TransactionRequest`. Skips the
/// init-state guard (this is what gets you out of `Uninitialized`). Does NOT
/// broadcast. The caller is responsible for not double-broadcasting; the next
/// sync will surface the in-flight INIT as `Initializing`.
pub fn prepare_init(db_name: &str) -> anyhow::Result<PreparedWrite> {
    let mut prep = prepare_common(db_name)?;
    // INIT binds only the receiver; the wire memo still echoes the human address
    // (advisory, unsigned) so `zkv history` can show the database it claims.
    let payload = signed_init_payload(&prep.receiver_hex);
    let sig = sign_command(&prep.sk, &payload);
    // Wipe the signing key as soon as we're done with it (best-effort).
    prep.sk.non_secure_erase();
    let memo = build_init_memo(&prep.zkv_addr, &sig)?;
    let memo_text = match Memo::try_from(memo)? {
        Memo::Text(t) => t.to_string(),
        _ => unreachable!("we just built a text memo"),
    };
    let request = build_request(&prep.recipient_ua, &memo_text)?;
    Ok(PreparedWrite {
        zkv_addr: prep.zkv_addr,
        recipient_ua: prep.recipient_ua,
        memo_text,
        request,
    })
}

/// Broadcast a freshly-built INIT. Caller has already established (a) the DB
/// is in Uninitialized state, and (b) the wallet has spendable funds.
pub async fn broadcast_init(db_name: &str, connection: &ConnectionArgs) -> anyhow::Result<String> {
    // Exclude any other zkv process touching this database while we build and
    // broadcast the spend (reentrant with the sync path's lock).
    let _lock = crate::internal::lock::DbLock::acquire(db_name)?;
    let prepared = prepare_init(db_name)?;
    tracing::debug!(
        zkv_addr = %prepared.zkv_addr,
        recipient_ua = %prepared.recipient_ua,
        "preparing INIT",
    );
    let txid = pay(db_name, connection, prepared.request).await?;
    // INIT's "key" is the zkv address (matching the confirmed kv_history row),
    // so the History view's per-key filter excludes it but the genesis entry
    // still renders.
    record_pending(
        db_name,
        &txid,
        Op::Init,
        &prepared.zkv_addr,
        None,
        &prepared.memo_text,
    );
    Ok(txid)
}

/// Sync (unless `no_sync`), prepare, broadcast. Returns the broadcast txid.
///
/// `no_sync` only skips the pre-broadcast sync; the memo is still signed
/// and broadcast immediately. No stdout/stderr output; the caller decides
/// how to surface the txid (CLI commands print it; library consumers use
/// the returned string).
pub async fn write_and_broadcast(
    db_name: &str,
    connection: &ConnectionArgs,
    no_sync: bool,
    op: Op,
    key: &str,
    value: Option<&str>,
) -> anyhow::Result<String> {
    // Hold the database lock across the pre-broadcast sync and the spend so the
    // two stay atomic against another zkv process (reentrant with run_sync).
    let _lock = crate::internal::lock::DbLock::acquire(db_name)?;
    if !no_sync {
        run_sync(db_name, connection, false).await?;
    }

    let prepared = prepare(db_name, op, key, value)?;
    tracing::debug!(
        zkv_addr = %prepared.zkv_addr,
        recipient_ua = %prepared.recipient_ua,
        "preparing {} {}", op.as_str(), key,
    );

    let txid = match pay(db_name, connection, prepared.request).await {
        Ok(t) => t,
        Err(e) => return Err(augment_insufficient_funds(e, db_name)),
    };
    record_pending(db_name, &txid, op, key, value, &prepared.memo_text);
    Ok(txid)
}

/// Sync (unless `no_sync`), prepare, and broadcast a batch of data writes as
/// ONE transaction (a "sendmany"): N zero-value memo outputs, one ZIP-317 fee,
/// one txid. Returns the single broadcast txid. `no_sync` only skips the
/// pre-broadcast sync; the batch is still broadcast immediately.
///
/// Records one `pending.toml` row per op, all sharing the one txid (matching
/// `merge_pending`'s dedup-by-txid and the read path's per-output scan).
pub async fn write_many_and_broadcast(
    db_name: &str,
    connection: &ConnectionArgs,
    no_sync: bool,
    ops: &[BatchItem<'_>],
) -> anyhow::Result<String> {
    // Hold the database lock across the pre-broadcast sync and the spend so the
    // two stay atomic against another zkv process (reentrant with run_sync).
    let _lock = crate::internal::lock::DbLock::acquire(db_name)?;
    if !no_sync {
        run_sync(db_name, connection, false).await?;
    }

    let prepared = prepare_batch(db_name, ops)?;
    tracing::debug!(
        zkv_addr = %prepared.zkv_addr,
        recipient_ua = %prepared.recipient_ua,
        "preparing sendmany of {} ops", prepared.plan_ops.len(),
    );

    let txid = match pay(db_name, connection, prepared.request).await {
        Ok(t) => t,
        Err(e) => return Err(augment_insufficient_funds(e, db_name)),
    };
    for ((op, key, value), memo_text) in prepared.plan_ops.iter().zip(&prepared.memo_texts) {
        record_pending(db_name, &txid, *op, key, value.as_deref(), memo_text);
    }
    Ok(txid)
}

/// Sync (unless `no_sync`), prepare, and broadcast an owner/writer management
/// memo. `target` is the affected pubkey (zkvid1… or hex); `scope` is the
/// capability string for `WRITERADD`. Returns the broadcast txid. `no_sync`
/// only skips the pre-broadcast sync; the memo is still broadcast immediately.
///
/// Management ops are recorded in `pending.toml` with the op name, the target
/// pubkey as the "key", and the scope as the "value"; `merge_pending`
/// only synthesizes data ops, so a pending management op never masquerades as
/// a key/value (its effect only shows once confirmed, like any registry op).
pub async fn manage_and_broadcast(
    db_name: &str,
    connection: &ConnectionArgs,
    no_sync: bool,
    op: Op,
    target: &str,
    scope: Option<&str>,
) -> anyhow::Result<String> {
    // Hold the database lock across the pre-broadcast sync and the spend so the
    // two stay atomic against another zkv process (reentrant with run_sync).
    let _lock = crate::internal::lock::DbLock::acquire(db_name)?;
    if !no_sync {
        run_sync(db_name, connection, false).await?;
    }

    let prepared = prepare_management(db_name, op, target, scope)?;
    tracing::debug!(
        zkv_addr = %prepared.zkv_addr,
        recipient_ua = %prepared.recipient_ua,
        "preparing {} {target}", op.as_str(),
    );

    let txid = match pay(db_name, connection, prepared.request).await {
        Ok(t) => t,
        Err(e) => return Err(augment_insufficient_funds(e, db_name)),
    };
    record_pending(db_name, &txid, op, target, scope, &prepared.memo_text);
    Ok(txid)
}

/// Pending zatoshis from the wallet summary: in-flight incoming notes, change
/// pending confirmation, and unshielded transparent funds (which would need
/// shielding before they could fund a write).
fn pending_zats(db_name: &str) -> anyhow::Result<u64> {
    let cfg = WalletConfig::read(db_name)?;
    let (_, db_data_path) = get_db_paths(db_name)?;
    let db_data = open_wallet_db(db_data_path, cfg.network)?;
    let summary = match db_data.get_wallet_summary(ConfirmationsPolicy::default())? {
        Some(s) => s,
        None => return Ok(0),
    };
    let mut pending: u64 = 0;
    for b in summary.account_balances().values() {
        pending += u64::from(b.value_pending_spendability());
        pending += u64::from(b.change_pending_confirmation());
        pending += u64::from(b.unshielded_balance().total());
    }
    Ok(pending)
}

/// If `err` is the `zcash_client_backend` "insufficient funds" error, rewrite
/// it to mention any pending (confirming) balance the wallet is aware of.
/// Otherwise return it unchanged.
pub(crate) fn augment_insufficient_funds(err: anyhow::Error, db_name: &str) -> anyhow::Error {
    let Some(error::Error::Wallet(WalletError::InsufficientFunds {
        available,
        required,
    })) = err.downcast_ref::<error::Error>()
    else {
        return err;
    };
    let network = WalletConfig::read(db_name)
        .map(|c| c.network)
        .unwrap_or_default();
    WriteError::InsufficientFunds {
        available: u64::from(*available),
        required: u64::from(*required),
        pending: pending_zats(db_name).unwrap_or(0),
        network,
    }
    .into()
}

/// Sign and print a data memo (`SET`/`SETL`/`DEL`) only, no sync, no
/// broadcast. Backs `zkv sign set` / `zkv sign del`.
pub fn write_and_print(
    db_name: &str,
    op: Op,
    key: &str,
    value: Option<&str>,
) -> anyhow::Result<()> {
    let prepared = prepare(db_name, op, key, value)?;
    print_prepared(op, key, &prepared);
    Ok(())
}

/// Sign and print an owner/writer management memo (`OWNER*`/`WRITER*`) only;
/// no sync, no broadcast. Backs `zkv sign owner …` / `zkv sign writer …`.
/// `target` is the affected pubkey; `scope` is the capability string for
/// `WRITERADD` (ignored otherwise).
pub fn manage_and_print(
    db_name: &str,
    op: Op,
    target: &str,
    scope: Option<&str>,
) -> anyhow::Result<()> {
    let prepared = prepare_management(db_name, op, target, scope)?;
    print_prepared(op, target, &prepared);
    Ok(())
}

/// Print a prepared (but un-broadcast) memo: human-readable framing on stderr,
/// the raw memo text on stdout so it can be piped to a relaying wallet.
fn print_prepared(op: Op, key: &str, prepared: &PreparedWrite) {
    eprintln!("zkv {} {} → {}", op.as_str(), key, prepared.zkv_addr);
    eprintln!("  recipient (single-pool UA): {}", prepared.recipient_ua);
    eprintln!();
    eprintln!("--- begin zkv memo ---");
    println!("{}", prepared.memo_text);
    eprintln!("--- end zkv memo ---");
    eprintln!();
    eprintln!("Send a zero-value (memo-only) shielded payment to the recipient UA above with this exact memo.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::protocol::{BlockSet, MAX_DB_VERSION};

    fn item<'a>(op: Op, key: &'a str, value: Option<&'a str>) -> BatchItem<'a> {
        BatchItem { op, key, value }
    }

    fn exists(keys: &[&str]) -> HashSet<String> {
        keys.iter().map(|k| k.to_string()).collect()
    }

    /// Each (version, key_exists) pair, for terse assertions.
    fn vx(plan: &[BatchPlanItem]) -> Vec<(u64, bool)> {
        plan.iter().map(|p| (p.version, p.key_exists)).collect()
    }

    #[test]
    fn empty_batch_yields_empty_plan() {
        let plan = sequence_versions(&[], |_| 0, &exists(&[]));
        assert!(plan.is_empty());
    }

    #[test]
    fn distinct_keys_use_independent_bases() {
        // base(a)=0, base(b)=5; each first-touch op signs over its own base.
        let items = [item(Op::Set, "a", Some("1")), item(Op::Set, "b", Some("2"))];
        let plan = sequence_versions(&items, |k| if k == "b" { 5 } else { 0 }, &exists(&[]));
        assert_eq!(
            plan.iter().map(|p| p.version).collect::<Vec<_>>(),
            vec![0, 5]
        );
    }

    #[test]
    fn same_key_gets_consecutive_versions() {
        // Two ops on one key in one tx advance the local counter from the base.
        let items = [
            item(Op::Set, "a", Some("1")),
            item(Op::Set, "a", Some("2")),
            item(Op::Del, "a", None),
        ];
        let plan = sequence_versions(&items, |_| 2, &exists(&["a"]));
        assert_eq!(
            plan.iter().map(|p| p.version).collect::<Vec<_>>(),
            vec![2, 3, 4],
        );
    }

    #[test]
    fn in_batch_create_makes_later_op_an_update() {
        // confirmed_exists empty: first Set is a create (key_exists=false), the
        // following Del sees the key created earlier in the batch.
        let items = [item(Op::Set, "a", Some("1")), item(Op::Del, "a", None)];
        let plan = sequence_versions(&items, |_| 0, &exists(&[]));
        assert_eq!(vx(&plan), vec![(0, false), (1, true)]);
    }

    #[test]
    fn in_batch_del_makes_later_set_a_create() {
        // confirmed_exists {a}: first Del sees it (key_exists=true); the later
        // Set re-creates it (key_exists=false).
        let items = [item(Op::Del, "a", None), item(Op::Set, "a", Some("1"))];
        let plan = sequence_versions(&items, |_| 0, &exists(&["a"]));
        assert_eq!(vx(&plan), vec![(0, true), (1, false)]);
    }

    #[test]
    fn interleaved_keys_track_per_key_state() {
        // a, b, a: a's counter advances across the gap, b is independent.
        let items = [
            item(Op::Set, "a", Some("1")),
            item(Op::Set, "b", Some("2")),
            item(Op::Set, "a", Some("3")),
        ];
        let plan = sequence_versions(&items, |_| 0, &exists(&[]));
        assert_eq!(
            plan.iter()
                .map(|p| (p.key.as_str(), p.version))
                .collect::<Vec<_>>(),
            vec![("a", 0), ("b", 0), ("a", 1)],
        );
    }

    // These pin the *producer* side of the error contract the facade's
    // classifiers depend on (see the matching tests in `db.rs`): if any of
    // these messages is reworded, the structured `ZkvError` the facade returns
    // silently degrades to `ZkvError::Other`.

    #[test]
    fn ensure_initialized_initializing_message() {
        let init = InitState::Initializing {
            done: 1,
            required: 3,
        };
        let msg = format!("{:#}", ensure_initialized("demo", &init).unwrap_err());
        assert!(msg.contains("is not yet initialized"), "{msg}");
        assert!(msg.contains("INIT seen at 1/3"), "{msg}");
    }

    #[test]
    fn ensure_initialized_uninitialized_message() {
        let msg = format!(
            "{:#}",
            ensure_initialized("demo", &InitState::Uninitialized).unwrap_err()
        );
        assert!(msg.contains("is not yet initialized"), "{msg}");
        assert!(!msg.contains("INIT seen at"), "{msg}");
    }

    #[test]
    fn ensure_initialized_passes_when_initialized() {
        assert!(ensure_initialized("demo", &InitState::Initialized).is_ok());
    }

    #[test]
    fn ensure_client_supported_blockwrite_message() {
        let v = VersionState {
            version: MAX_DB_VERSION + 1,
            blocks: BlockSet::parse("blockwrite").unwrap(),
        };
        let msg = format!("{:#}", ensure_client_supported(&v).unwrap_err());
        assert!(msg.contains("blocks writes for clients older"), "{msg}");
        assert!(
            msg.contains(&format!("upgraded to version {}", MAX_DB_VERSION + 1)),
            "{msg}"
        );
        assert!(
            msg.contains(&format!("supports up to version {MAX_DB_VERSION}")),
            "{msg}"
        );
    }

    #[test]
    fn ensure_client_supported_passes_when_not_blocking() {
        let v = VersionState {
            version: MAX_DB_VERSION,
            blocks: BlockSet::parse("warn").unwrap(),
        };
        assert!(ensure_client_supported(&v).is_ok());
    }

    #[test]
    fn build_request_rejects_bad_recipient() {
        let err = build_request("not-a-zcash-address", "ZKV0 SET k v").unwrap_err();
        assert!(format!("{err:#}").contains("bad recipient UA"));
    }
}
