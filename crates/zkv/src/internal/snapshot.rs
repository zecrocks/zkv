//! Persistent projection of confirmed memos into a sidecar SQLite DB
//! (`zkv_state.sqlite`), plus an append-only `kv_history` audit log.
//!
//! Why this exists: every read in zkv replays the full history of shielded
//! memos addressed to the account, parsing each one and verifying its
//! ECDSA signature. That's `O(total writes ever)` work per read, which
//! grows without bound for any long-lived database (e.g., a ZEC price
//! oracle writing `SET price/zec_usd` every 30 minutes). This module
//! holds the materialized result so we only re-process the recent tail.
//!
//! Lagged-snapshot model: we promote a row from the wallet DB into this
//! snapshot only after it is at least [`SAFE_DEPTH`] blocks deep. The
//! wallet itself rewinds at most 10 blocks on reorg
//! (`src/internal/sync.rs:448`); a 100-block buffer means the snapshot
//! watermark is always behind any plausible rewind and no rollback path
//! is needed. The recent tail (last `SAFE_DEPTH` blocks plus mempool) is
//! always re-replayed in memory by [`super::state::load_state`].

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{anyhow, Result};
use rusqlite::{named_params, types::Value, Connection, OptionalExtension, Transaction};
use zcash_primitives::transaction::TxId;

use crate::internal::protocol::{
    bump_hw, decide, parse_text_memo, payload_for, pubkey_bech32, recover_signer, seq_in_window,
    AuthRegistry, BlockSet, InitState, KeyState, Op, ReplayResult, RowOutcome, Scope, VersionState,
    WriteStatus, GENESIS_DB_VERSION,
};

/// Convert a raw little-endian txid blob into conventional display-order
/// hex (matching `pay()` / the read path). `None` if not 32 bytes.
fn txid_display(txid: &[u8]) -> Option<String> {
    <[u8; 32]>::try_from(txid)
        .ok()
        .map(|arr| TxId::from_bytes(arr).to_string())
}

/// Promote a memo into the snapshot only once it is at least this many
/// blocks deep. Zcash mainnet has never seen a reorg anywhere close to
/// this depth; picking 100 leaves ~10× headroom over the wallet code's
/// own 10-block rewind cap on continuity errors.
pub const SAFE_DEPTH: u32 = 100;

/// Sidecar schema version. Bump and add a migration arm when changing
/// table layout. On a mismatch [`open`] wipes the file so the next read
/// rebuilds from scratch (the chain remains authoritative).
///
/// v2 added the `signature` column to `kv_history` so `zkv history` can
/// surface each write's raw signed memo without re-scanning the chain.
/// v3 also records the database's INIT memo as a `kv_history` row so it
/// appears in the history. v4 caches each write's `block_time` on
/// `kv_history` and the latest write's `(mined_height, txid, block_time)`
/// on `kv`, so "last update" is a per-key lookup rather than a full
/// history scan. v5 adds the owner/writer `auth` table (registry
/// projection) alongside those history columns. v6 was claimed independently
/// on two branches: one switched the registry's stored pubkey form from
/// compressed hex to the canonical `zkvid1…` Bech32m encoding, the other
/// recorded the database's required protocol version (`db_version`) and the
/// under-versioned-client block flags (`db_version_blocks`) in `meta`. v7
/// folds both in and adds the recovered `signer` column to `kv_history` so
/// `zkv history` can attribute each write to the actual delegated signer
/// (owner or scoped writer), not just the root key; the bump forces any v6
/// cache (either variant) to rebuild with this combined layout. v8 adds the
/// `kv_version` and `target_version` tables that persist the per-key /
/// per-target replay-protection counters folded into the `ZKV0` signing domain
/// (see [`super::protocol::signing_domain`]). v9 adds the `seq` column to
/// `kv_history` so a deep row's raw memo can be reconstructed byte-for-byte now
/// that the replay-protection sequence rides on the wire (the compact prefix on
/// the signature line). v10 records the `finalized` latch in `meta` so a sealed
/// (FINALIZE'd) database is remembered without a chain re-scan. v11 adds the
/// `comment` column to `kv_history` so a deep row carrying a first-line comment
/// (the signed `#…` line) still round-trips byte-for-byte. v12 carries no
/// layout change: it forces a wipe after the wire/signing magic was renumbered
/// from `ZKV1` to `ZKV0`. Caches promoted by a `ZKV1` build trusted their deep
/// `INIT`/`SET` rows as confirmed; under `ZKV0` those same on-chain memos parse
/// as `UnsupportedVersion` (ver 1 > our epoch 0) and must be dropped, so the
/// stale projection has to be rebuilt from the chain rather than trusted.
const SCHEMA_VERSION: u32 = 12;

const META_INIT_STATE: &str = "init_state";
const META_WATERMARK_HEIGHT: &str = "watermark_height";
const META_WATERMARK_TXID: &str = "watermark_txid";
const META_WATERMARK_OUTPUT_INDEX: &str = "watermark_output_index";
/// `meta` key for the one-way FINALIZE latch. Present (value
/// [`FINALIZED_YES`]) once a confirmed FINALIZE has sealed the database.
const META_FINALIZED: &str = "finalized";
/// Required protocol epoch, as a decimal `u32` (default [`GENESIS_DB_VERSION`]).
const META_DB_VERSION: &str = "db_version";
/// Block flags ([`BlockSet::to_wire`]) of the controlling `VERSION` memo
/// (default `warn`).
const META_DB_VERSION_BLOCKS: &str = "db_version_blocks";

const INIT_STATE_INITIALIZED: &str = "initialized";
const FINALIZED_YES: &str = "finalized";

/// `auth.role` value for an owner row. Writers store their scope string.
const AUTH_ROLE_OWNER: &str = "owner";

/// Position of the last confirmed memo applied to the snapshot. Ordering
/// is `(height, txid, output_index)` lexicographic, the same total order
/// the wallet read path uses (`src/internal/state.rs:74`).
#[derive(Clone, Debug, Default)]
pub struct Watermark {
    pub height: u32,
    /// Raw 32-byte txid (little-endian, matching `v_tx_outputs.txid`).
    pub txid: Vec<u8>,
    pub output_index: u32,
}

/// One promotable row: a memo from `v_tx_outputs` that is mined deep
/// enough to be reorg-safe and past the current watermark. Caller is
/// responsible for partitioning rows; [`promote`] does no depth check.
pub struct PromoteRow {
    pub mined_height: u32,
    pub txid: Vec<u8>,
    pub output_index: u32,
    /// Block timestamp (unix seconds) of the mined height, if known.
    pub block_time: Option<u32>,
    pub memo_text: String,
}

/// Open the sidecar at `path`, creating it (and the schema) if missing.
///
/// On schema-version mismatch we wipe the file and treat the next read
/// as a full rebuild from the wallet DB; the snapshot is a cache, not a
/// source of truth.
pub fn open(path: &Path) -> Result<Connection> {
    let mut conn = Connection::open(path)
        .map_err(|e| anyhow!("open zkv_state.sqlite at {}: {e}", path.display()))?;
    let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    match version {
        0 => init_schema(&mut conn)?,
        SCHEMA_VERSION => {}
        other => {
            tracing::warn!(
                "zkv_state.sqlite at {} has unknown schema version {other}; wiping",
                path.display()
            );
            wipe(&mut conn)?;
        }
    }
    Ok(conn)
}

fn init_schema(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (
            k TEXT PRIMARY KEY,
            v TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS kv (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            mined_height INTEGER,
            txid TEXT,
            block_time INTEGER
        );
        CREATE TABLE IF NOT EXISTS kv_history (
            mined_height INTEGER NOT NULL,
            txid BLOB NOT NULL,
            output_index INTEGER NOT NULL,
            key TEXT NOT NULL,
            op TEXT NOT NULL,
            value TEXT,
            signature TEXT NOT NULL,
            block_time INTEGER,
            signer TEXT NOT NULL,
            seq INTEGER NOT NULL DEFAULT 0,
            comment TEXT,
            PRIMARY KEY (mined_height, txid, output_index)
        );
        CREATE INDEX IF NOT EXISTS kv_history_by_key
            ON kv_history (key, mined_height, txid, output_index);
        -- Owner/writer registry projection. `pubkey_bech32` is a signer's
        -- canonical `zkvid1…` Bech32m identity. `role` is 'owner' for owners,
        -- or the canonical comma-joined capability scope for writers (e.g.
        -- 'CREATE,UPDATE'). The chain is authoritative; this is a cache.
        CREATE TABLE IF NOT EXISTS auth (
            pubkey_bech32 TEXT PRIMARY KEY,
            role TEXT NOT NULL
        );
        -- Per-key replay-protection version: the count of honored data writes
        -- (SET/SETL/DEL) to each key, folded into the ZKV0 signing domain.
        -- Survives a DEL as a tombstone (the `kv` row is gone but the version
        -- stays), so a replayed original creation can't recreate the key.
        CREATE TABLE IF NOT EXISTS kv_version (
            key TEXT PRIMARY KEY,
            version INTEGER NOT NULL
        );
        -- Per-target replay-protection version for OWNER*/WRITER* management
        -- ops, keyed by the canonical `zkvid1…` target pubkey. Survives
        -- revocation (tombstone) so a replayed revoke can't re-fire post-regrant.
        CREATE TABLE IF NOT EXISTS target_version (
            target TEXT PRIMARY KEY,
            version INTEGER NOT NULL
        );",
    )?;
    tx.commit()?;
    // `PRAGMA user_version` is not bindable; the value is fixed by us.
    conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
    Ok(())
}

/// Drop and recreate all snapshot tables. Used on version mismatch or
/// when the wallet has rewound past the watermark (impossible barring
/// catastrophic corruption; see [`super::state::load_state`]).
pub fn wipe(conn: &mut Connection) -> Result<()> {
    conn.execute_batch(
        "DROP TABLE IF EXISTS target_version;
         DROP TABLE IF EXISTS kv_version;
         DROP TABLE IF EXISTS auth;
         DROP TABLE IF EXISTS kv_history;
         DROP TABLE IF EXISTS kv;
         DROP TABLE IF EXISTS meta;",
    )?;
    init_schema(conn)
}

/// Read the current watermark; returns the default `(0, [], 0)` if unset
/// (fresh snapshot; every wallet row is past the watermark).
pub fn read_watermark(conn: &Connection) -> Result<Watermark> {
    let height: u32 = read_meta_u32(conn, META_WATERMARK_HEIGHT)?.unwrap_or(0);
    let txid: Vec<u8> = read_meta_blob(conn, META_WATERMARK_TXID)?.unwrap_or_default();
    let output_index: u32 = read_meta_u32(conn, META_WATERMARK_OUTPUT_INDEX)?.unwrap_or(0);
    Ok(Watermark {
        height,
        txid,
        output_index,
    })
}

/// Read the snapshot's confirmed init flag without materializing the whole
/// seed. `Uninitialized` until a promoted batch has folded in a valid INIT.
pub fn read_init_state(conn: &Connection) -> Result<InitState> {
    Ok(
        if read_meta_text(conn, META_INIT_STATE)?.as_deref() == Some(INIT_STATE_INITIALIZED) {
            InitState::Initialized
        } else {
            InitState::Uninitialized
        },
    )
}

/// Load the snapshot as a `ReplayResult` suitable for seeding
/// [`super::protocol::replay_with_seed`]. The init flag is loaded from
/// `meta`; `state` carries the confirmed projection from `kv`; `auth` carries
/// the owner/writer registry from `auth`. Pending queues are always empty
/// here (they're a live-tail concept).
pub fn load_seed(conn: &Connection) -> Result<ReplayResult> {
    let init = if read_meta_text(conn, META_INIT_STATE)?.as_deref() == Some(INIT_STATE_INITIALIZED)
    {
        InitState::Initialized
    } else {
        InitState::Uninitialized
    };

    let mut stmt = conn.prepare("SELECT key, value, txid, block_time FROM kv")?;
    let mut state: BTreeMap<String, KeyState> = BTreeMap::new();
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let key: String = row.get(0)?;
        let value: String = row.get(1)?;
        let last_txid: Option<String> = row.get(2)?;
        let updated_at: Option<u32> = row
            .get::<_, Option<i64>>(3)?
            .map(|t| u32::try_from(t).unwrap_or(u32::MAX));
        state.insert(
            key,
            KeyState {
                confirmed: Some(value),
                pending: Vec::new(),
                updated_at,
                last_txid,
            },
        );
    }

    let auth = load_auth(conn)?;
    let finalized = read_meta_text(conn, META_FINALIZED)?.as_deref() == Some(FINALIZED_YES);
    let version = read_version(conn)?;
    let kv_versions = load_versions(conn, "kv_version", "key")?;
    let target_versions = load_versions(conn, "target_version", "target")?;
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

/// Load a `(name -> version)` map from one of the replay-protection version
/// tables (`kv_version` keyed by `key`, `target_version` keyed by `target`).
/// `id_col` is the table's id column name. Only non-zero rows are stored, so an
/// absent entry reads as version 0, the same convention the in-memory maps use.
fn load_versions(conn: &Connection, table: &str, id_col: &str) -> Result<BTreeMap<String, u64>> {
    let mut out = BTreeMap::new();
    let mut stmt = conn.prepare(&format!("SELECT {id_col}, version FROM {table}"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let version: i64 = row.get(1)?;
        out.insert(id, version.max(0) as u64);
    }
    Ok(out)
}

/// Rewrite a replay-protection version table to match `versions`. Like
/// [`persist_auth`], a full replace inside the promote transaction keeps the
/// table exactly in sync with the projection (the maps are small).
fn persist_versions(
    tx: &Transaction<'_>,
    table: &str,
    id_col: &str,
    versions: &BTreeMap<String, u64>,
) -> Result<()> {
    tx.execute(&format!("DELETE FROM {table}"), [])?;
    let mut stmt = tx.prepare(&format!(
        "INSERT INTO {table} ({id_col}, version) VALUES (:id, :v)"
    ))?;
    for (id, v) in versions {
        stmt.execute(named_params! { ":id": id, ":v": *v as i64 })?;
    }
    Ok(())
}

/// Rebuild an [`AuthRegistry`] from the `auth` table. Rows with `role =
/// 'owner'` are owners; any other `role` is parsed as a writer scope (an
/// unparseable scope is skipped; the table is a cache and the chain wins).
fn load_auth(conn: &Connection) -> Result<AuthRegistry> {
    let mut auth = AuthRegistry::default();
    let mut stmt = conn.prepare("SELECT pubkey_bech32, role FROM auth")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let pubkey_bech32: String = row.get(0)?;
        let role: String = row.get(1)?;
        if role == AUTH_ROLE_OWNER {
            auth.insert_owner(pubkey_bech32);
        } else if let Some(scope) = Scope::parse(&role) {
            // Re-affirm via the public mutator so owner-precedence invariants
            // hold even if the table somehow contains a writer row for a key
            // also listed as owner (owners load first only by query order, so
            // be defensive).
            let _ = auth.apply_management(Op::WriterSet, &pubkey_bech32, Some(&scope.to_wire()));
        }
    }
    Ok(auth)
}

/// One row of `kv_history`: a confirmed SET/DEL already folded into the
/// snapshot. `txid` is the raw little-endian storage blob (the caller
/// converts to display hex); `signature` is the 130-char hex recorded at
/// promote time, so the raw signed memo can be reconstructed without a
/// chain re-scan.
pub struct HistRow {
    pub mined_height: u32,
    pub txid: Vec<u8>,
    pub output_index: u32,
    pub key: String,
    pub op: String,
    pub value: Option<String>,
    pub signature: String,
    pub block_time: Option<u32>,
    /// Compressed-hex of the signer recovered from `signature` at promote
    /// time; the delegated owner/writer that authored this write, not
    /// necessarily the root key.
    pub signer: String,
    /// The replay-protection sequence this write referenced, so the raw signed
    /// memo can be reconstructed byte-for-byte (the sequence rides on the wire
    /// as a compact prefix on the signature line). 0 for INIT.
    pub seq: u64,
    /// The signed first-line comment (`#…`) this write carried, if any, so the
    /// raw memo round-trips byte-for-byte. `None` when the memo had no comment.
    pub comment: Option<String>,
}

/// Escape `%`, `_`, and `\` so a user filter is matched literally under a
/// `LIKE … ESCAPE '\'` clause (keys often contain `_`, e.g. `zec_usd`).
fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Build the `WHERE …` clause (and its positional bind values, in order) for a
/// `kv_history` query restricted by an optional key substring `filter` and an
/// optional set of wire `op` strings (e.g. `["SET", "SETL"]`). Returns an empty
/// string + no binds when neither is set. The op tokens are bound as
/// parameters, so they need not be pre-sanitised against SQL injection.
fn history_where(filter: Option<&str>, ops: Option<&[String]>) -> (String, Vec<Value>) {
    let mut conds: Vec<String> = Vec::new();
    let mut binds: Vec<Value> = Vec::new();
    if let Some(f) = filter {
        conds.push("key LIKE ? ESCAPE '\\'".to_owned());
        binds.push(Value::Text(format!("%{}%", like_escape(f))));
    }
    if let Some(ops) = ops.filter(|o| !o.is_empty()) {
        let placeholders = vec!["?"; ops.len()].join(", ");
        conds.push(format!("op IN ({placeholders})"));
        binds.extend(ops.iter().map(|o| Value::Text(o.clone())));
    }
    if conds.is_empty() {
        (String::new(), binds)
    } else {
        (format!("WHERE {}", conds.join(" AND ")), binds)
    }
}

/// Read a page of confirmed history rows from `kv_history`. Ordered newest-first
/// (`mined_height DESC, txid DESC, output_index DESC`, index-backed) unless
/// `ascending`, which flips it to oldest-first. `filter` restricts to keys
/// containing it (substring `LIKE`); `ops`, when set, restricts to those wire
/// opcodes. `limit = None` returns all matching rows; `offset` skips the leading
/// N (in the chosen order).
pub fn history_page(
    conn: &Connection,
    filter: Option<&str>,
    ops: Option<&[String]>,
    ascending: bool,
    limit: Option<u32>,
    offset: u32,
) -> Result<Vec<HistRow>> {
    const COLS: &str =
        "mined_height, txid, output_index, key, op, value, signature, block_time, signer, seq, comment";
    let map = |row: &rusqlite::Row<'_>| -> rusqlite::Result<HistRow> {
        Ok(HistRow {
            mined_height: row.get::<_, i64>(0)? as u32,
            txid: row.get(1)?,
            output_index: row.get::<_, i64>(2)? as u32,
            key: row.get(3)?,
            op: row.get(4)?,
            value: row.get(5)?,
            signature: row.get(6)?,
            block_time: row
                .get::<_, Option<i64>>(7)?
                .map(|t| u32::try_from(t).unwrap_or(u32::MAX)),
            signer: row.get(8)?,
            seq: row.get::<_, i64>(9)?.max(0) as u64,
            comment: row.get(10)?,
        })
    };
    let order = if ascending {
        "ORDER BY mined_height ASC, txid ASC, output_index ASC"
    } else {
        "ORDER BY mined_height DESC, txid DESC, output_index DESC"
    };
    // SQLite treats LIMIT -1 as "no limit"; OFFSET still applies.
    let lim: i64 = limit.map(|l| l as i64).unwrap_or(-1);
    let off: i64 = offset as i64;
    let (where_sql, mut binds) = history_where(filter, ops);
    binds.push(Value::Integer(lim));
    binds.push(Value::Integer(off));
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM kv_history {where_sql} {order} LIMIT ? OFFSET ?"
    ))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(binds), map)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Count confirmed history rows (optionally key- and op-filtered), for
/// pagination.
pub fn history_count(
    conn: &Connection,
    filter: Option<&str>,
    ops: Option<&[String]>,
) -> Result<u64> {
    let (where_sql, binds) = history_where(filter, ops);
    let n: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM kv_history {where_sql}"),
        rusqlite::params_from_iter(binds),
        |r| r.get(0),
    )?;
    Ok(n.max(0) as u64)
}

/// Locate the newest `kv_history` row carrying `txid` (respecting an optional
/// key `filter`) and return its **0-based rank** in the newest-first order
/// used by [`history_page`]: how many rows sort before it. `None` if no
/// matching row exists. Lets a caller page straight to a specific write.
pub fn history_locate(conn: &Connection, filter: Option<&str>, txid: &[u8]) -> Result<Option<u64>> {
    let like = filter.map(|f| format!("%{}%", like_escape(f)));
    // The target's sort key: the newest output sharing this txid (and filter).
    let target: Option<(i64, i64)> = match &like {
        Some(like) => conn
            .query_row(
                "SELECT mined_height, output_index FROM kv_history
                 WHERE txid = :t AND key LIKE :like ESCAPE '\\'
                 ORDER BY mined_height DESC, output_index DESC LIMIT 1",
                named_params! { ":t": txid, ":like": like },
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?,
        None => conn
            .query_row(
                "SELECT mined_height, output_index FROM kv_history
                 WHERE txid = :t
                 ORDER BY mined_height DESC, output_index DESC LIMIT 1",
                named_params! { ":t": txid },
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?,
    };
    let Some((h, oi)) = target else {
        return Ok(None);
    };
    // Count rows that sort strictly before it. In the DESC ordering a row
    // precedes the target when its (mined_height, txid, output_index) tuple is
    // lexicographically greater.
    let before = "mined_height > :h
        OR (mined_height = :h AND txid > :t)
        OR (mined_height = :h AND txid = :t AND output_index > :oi)";
    let rank: i64 = match &like {
        Some(like) => conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM kv_history
                 WHERE key LIKE :like ESCAPE '\\' AND ({before})"
            ),
            named_params! { ":like": like, ":h": h, ":t": txid, ":oi": oi },
            |r| r.get(0),
        )?,
        None => conn.query_row(
            &format!("SELECT COUNT(*) FROM kv_history WHERE {before}"),
            named_params! { ":h": h, ":t": txid, ":oi": oi },
            |r| r.get(0),
        )?,
    };
    Ok(Some(rank.max(0) as u64))
}

/// Apply a chain-ordered batch of confirmed rows to the snapshot in a
/// single transaction: parse, recover the signer, gate on INIT, enforce
/// owner/writer authorization, update `kv` / the `auth` registry, append
/// `kv_history`, advance the watermark.
///
/// `rows` must be in chain order (mined_height ASC, txid ASC, output_index
/// ASC) and strictly past the current watermark. Caller guarantees each
/// row is at least `SAFE_DEPTH` deep, so every row here is *confirmed*;
/// management ops are therefore applied directly (a pending management op
/// never reaches the snapshot).
///
/// `pk` is the root (UFVK-derived) signer that may broadcast INIT. Malformed
/// memos, unrecoverable signatures, and unauthorized writes are silently
/// dropped; only honored entries reach `kv` / `kv_history` / `auth`.
pub fn promote(
    conn: &mut Connection,
    rows: &[PromoteRow],
    receiver: &str,
    pk: &secp256k1::PublicKey,
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let tx = conn.transaction()?;
    let mut init_state = read_init_state_in_tx(&tx)?;
    let started_uninitialized = matches!(init_state, InitState::Uninitialized);
    // The registry, finalized latch, required version, and per-entity
    // replay-protection counters are materialized projections of every confirmed
    // op so far. Load them, fold this batch on top (so the rules compose across
    // promote batches), and persist them back below. (Transaction derefs to
    // Connection.)
    let mut auth = load_auth(&tx)?;
    let mut finalized = read_meta_text(&tx, META_FINALIZED)?.as_deref() == Some(FINALIZED_YES);
    let mut version = read_version(&tx)?;
    let mut kv_versions = load_versions(&tx, "kv_version", "key")?;
    let mut target_versions = load_versions(&tx, "target_version", "target")?;
    let root_hex = pubkey_bech32(pk);
    for row in rows {
        apply_row(
            &tx,
            row,
            receiver,
            &root_hex,
            &mut init_state,
            &mut auth,
            &mut finalized,
            &mut version,
            &mut kv_versions,
            &mut target_versions,
        )?;
    }
    // Genesis safety: never advance the watermark while the database is still
    // uninitialized. If this batch started uninitialized and contained no valid
    // INIT, do not bury these blocks behind the watermark: the wallet scans
    // tip-first and backfills older ranges later, so the genesis INIT may live
    // in a range that hasn't been scanned yet. Advancing past it would strand it
    // below the watermark forever, leaving the database permanently stuck
    // "uninitialized" with every later write dropped as unauthorized (the
    // sidecar's older incarnation of this bug). Roll back (drop the tx) and
    // leave the batch to the live tail; a later promote that actually captures
    // the INIT advances the watermark atomically with initialization. While
    // uninitialized every data/management op is dropped as `NotInitialized`, so
    // nothing was written to `kv`/`kv_history` to lose here.
    if started_uninitialized && matches!(init_state, InitState::Uninitialized) {
        return Ok(());
    }
    // Watermark advances to the last row regardless of whether it was a
    // recognized zkv memo. The SQL filter rejected it for a reason
    // (non-text, wrong-pool, etc.) won't be true here (it already
    // passed), but if it parses as garbage we still must not re-process
    // it on the next read.
    let last = rows.last().expect("non-empty checked above");
    write_meta_u32(&tx, META_WATERMARK_HEIGHT, last.mined_height)?;
    write_meta_blob(&tx, META_WATERMARK_TXID, &last.txid)?;
    write_meta_u32(&tx, META_WATERMARK_OUTPUT_INDEX, last.output_index)?;
    if matches!(init_state, InitState::Initialized) {
        write_meta_text(&tx, META_INIT_STATE, INIT_STATE_INITIALIZED)?;
    }
    if finalized {
        write_meta_text(&tx, META_FINALIZED, FINALIZED_YES)?;
    }
    persist_auth(&tx, &auth)?;
    persist_versions(&tx, "kv_version", "key", &kv_versions)?;
    persist_versions(&tx, "target_version", "target", &target_versions)?;
    write_meta_u32(&tx, META_DB_VERSION, version.version)?;
    write_meta_text(&tx, META_DB_VERSION_BLOCKS, &version.blocks.to_wire())?;
    tx.commit()?;
    Ok(())
}

// The persisted twin of `apply_in_memory`: a SQL transaction plus the same
// registry / version / finalized projections and the per-entity version maps.
// Its key/value state lives in SQLite (not an in-memory map), so it can't share
// `apply_in_memory`'s accumulator; the shared decision (decide + seq_in_window)
// is already factored out, leaving these store-specific args intrinsic.
#[allow(clippy::too_many_arguments)]
fn apply_row(
    tx: &Transaction<'_>,
    row: &PromoteRow,
    receiver: &str,
    root_hex: &str,
    init_state: &mut InitState,
    auth: &mut AuthRegistry,
    finalized: &mut bool,
    version: &mut VersionState,
    kv_versions: &mut BTreeMap<String, u64>,
    target_versions: &mut BTreeMap<String, u64>,
) -> Result<()> {
    let Some(cmd) = parse_text_memo(&row.memo_text) else {
        return Ok(());
    };
    // Reconstruct the writer's signed payload exactly as the in-memory replay
    // does (INIT binds only the receiver; every other op binds the receiver plus
    // the wire sequence; a first-line comment folds into the domain).
    let payload = payload_for(receiver, &cmd);
    let Some(signer) = recover_signer(&payload, &cmd.sig_hex) else {
        return Ok(());
    };
    let signer_hex = pubkey_bech32(&signer);

    // version-CAS (bounded-forward) via the shared `seq_in_window`, so this
    // persisted path can't drift from the in-memory replay: a versioned op whose
    // wire `seq` falls outside the entity's accepted window is a defeated replay
    // / lost CAS (or a desync beyond tolerance) and is dropped without applying
    // or advancing the projection.
    if !seq_in_window(cmd.op, &cmd.key, cmd.seq, kv_versions, target_versions) {
        tracing::debug!(
            key = %cmd.key, seq = cmd.seq,
            "snapshot promote dropped out-of-window memo"
        );
        return Ok(());
    }

    // Every promotable row is buried >= SAFE_DEPTH, hence Confirmed. Compute
    // key_exists from the snapshot's `kv` projection and route the gate through
    // the shared `decide`, so this persisted path can't drift from the
    // in-memory replay. The recording below (signature + block_time columns,
    // INIT logged to kv_history) is the snapshot's richer audit projection.
    let key_exists: bool = tx
        .query_row(
            "SELECT 1 FROM kv WHERE key = :k",
            named_params! { ":k": &cmd.key },
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    let outcome = decide(
        cmd.op,
        &cmd.key,
        &signer_hex,
        root_hex,
        &WriteStatus::Confirmed,
        init_state,
        auth,
        *finalized,
        key_exists,
    );
    match outcome {
        RowOutcome::Dropped(reason) => {
            tracing::debug!(%reason, "snapshot promote dropped memo");
        }
        // Promotable rows are all confirmed, so `decide` never yields Pending.
        RowOutcome::Pending => {}
        RowOutcome::Applied => match cmd.op {
            Op::Init => {
                // The root key becomes owner #1. The INIT is also recorded in
                // kv_history (key = the zkv address) so `zkv history` can show
                // the database's genesis event; it never touches `kv`.
                *init_state = InitState::Initialized;
                auth.insert_owner(root_hex.to_owned());
                tx.execute(
                    "INSERT INTO kv_history (mined_height, txid, output_index, key, op, value, signature, block_time, signer, seq, comment)
                     VALUES (:h, :t, :i, :k, 'INIT', NULL, :sig, :bt, :signer, 0, :comment)",
                    named_params! {
                        ":h": row.mined_height,
                        ":t": &row.txid,
                        ":i": row.output_index,
                        ":k": &cmd.key,
                        ":sig": &cmd.sig_hex,
                        ":bt": row.block_time,
                        ":signer": &signer_hex,
                        ":comment": &cmd.comment,
                    },
                )?;
            }
            Op::OwnerSet | Op::OwnerDel | Op::WriterSet | Op::WriterDel => {
                let result = auth.apply_management(cmd.op, &cmd.key, cmd.value.as_deref());
                // Advance the target's high-water for EVERY owner-authorized,
                // in-window management op, even a policy no-op
                // (LastOwnerProtected), so an unbumped on-chain memo can't be
                // replayed once a state-dependent rejection later flips. Mirrors
                // `apply_in_memory` exactly so the snapshot and live tail stay in
                // lockstep (closing the OWNERDEL-replay gap identically in both).
                bump_hw(target_versions, cmd.key.clone(), cmd.seq);
                if let Err(reason) = result {
                    tracing::debug!(%reason, "snapshot promote dropped management op");
                }
            }
            Op::Version => {
                // db-global meta change; not a per-key or registry event, so
                // nothing is written to `kv`/`kv_history`. The version + block
                // flags are persisted to `meta` at the end of `promote`.
                if let Err(reason) = version.apply_version(&cmd.key, cmd.value.as_deref()) {
                    tracing::debug!(%reason, "snapshot promote dropped version op");
                }
            }
            // FINALIZE seals the database. Like the management ops it is not
            // written to kv_history (only INIT/SET/SETL/DEL are); the latch is
            // persisted to `meta` after the batch.
            Op::Finalize => {
                *finalized = true;
            }
            Op::Set | Op::SetL => {
                let value = cmd.value.ok_or_else(|| anyhow!("SET without value"))?;
                let txid_hex = txid_display(&row.txid);
                // Audit log records the wire op (`SET` vs `SETL`) so the
                // history shows exactly what was confirmed on chain. The `kv`
                // projection collapses them; `SETL` is just a wire detail.
                let op_str = cmd.op.as_str();
                tx.execute(
                    "INSERT INTO kv (key, value, mined_height, txid, block_time)
                     VALUES (:k, :v, :h, :tx, :bt)
                     ON CONFLICT(key) DO UPDATE SET
                         value = excluded.value,
                         mined_height = excluded.mined_height,
                         txid = excluded.txid,
                         block_time = excluded.block_time",
                    named_params! {
                        ":k": &cmd.key,
                        ":v": &value,
                        ":h": row.mined_height,
                        ":tx": &txid_hex,
                        ":bt": row.block_time,
                    },
                )?;
                tx.execute(
                    "INSERT INTO kv_history (mined_height, txid, output_index, key, op, value, signature, block_time, signer, seq, comment)
                     VALUES (:h, :t, :i, :k, :op, :v, :sig, :bt, :signer, :seq, :comment)",
                    named_params! {
                        ":h": row.mined_height,
                        ":t": &row.txid,
                        ":i": row.output_index,
                        ":k": &cmd.key,
                        ":op": op_str,
                        ":v": &value,
                        ":sig": &cmd.sig_hex,
                        ":bt": row.block_time,
                        ":signer": &signer_hex,
                        ":seq": cmd.seq as i64,
                        ":comment": &cmd.comment,
                    },
                )?;
                bump_hw(kv_versions, cmd.key.clone(), cmd.seq);
            }
            Op::Del => {
                tx.execute(
                    "DELETE FROM kv WHERE key = :k",
                    named_params! { ":k": &cmd.key },
                )?;
                tx.execute(
                    "INSERT INTO kv_history (mined_height, txid, output_index, key, op, value, signature, block_time, signer, seq, comment)
                     VALUES (:h, :t, :i, :k, 'DEL', NULL, :sig, :bt, :signer, :seq, :comment)",
                    named_params! {
                        ":h": row.mined_height,
                        ":t": &row.txid,
                        ":i": row.output_index,
                        ":k": &cmd.key,
                        ":sig": &cmd.sig_hex,
                        ":bt": row.block_time,
                        ":signer": &signer_hex,
                        ":seq": cmd.seq as i64,
                        ":comment": &cmd.comment,
                    },
                )?;
                // Tombstone: the `kv` row is gone but the high-water persists, so
                // a replayed original creation can't recreate the key.
                bump_hw(kv_versions, cmd.key.clone(), cmd.seq);
            }
        },
    }
    Ok(())
}

/// Rewrite the `auth` table to match `auth`. The registry is small (a handful
/// of owners/writers), so a full replace inside the promote transaction is
/// simplest and keeps the table exactly in sync with the projection.
fn persist_auth(tx: &Transaction<'_>, auth: &AuthRegistry) -> Result<()> {
    tx.execute("DELETE FROM auth", [])?;
    for owner in auth.owners() {
        tx.execute(
            "INSERT INTO auth (pubkey_bech32, role) VALUES (:k, :r)",
            named_params! { ":k": owner, ":r": AUTH_ROLE_OWNER },
        )?;
    }
    for (writer, scope) in auth.writers() {
        tx.execute(
            "INSERT INTO auth (pubkey_bech32, role) VALUES (:k, :r)",
            named_params! { ":k": writer, ":r": scope.to_wire() },
        )?;
    }
    Ok(())
}

fn read_init_state_in_tx(tx: &Transaction<'_>) -> Result<InitState> {
    let v: Option<String> = tx
        .query_row(
            "SELECT v FROM meta WHERE k = :k",
            named_params! { ":k": META_INIT_STATE },
            |r| r.get(0),
        )
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    Ok(match v.as_deref() {
        Some(INIT_STATE_INITIALIZED) => InitState::Initialized,
        _ => InitState::Uninitialized,
    })
}

/// Read the projected [`VersionState`] from `meta`. Accepts `&Connection` or
/// `&Transaction` (which derefs to it). Defaults to genesis (no blocks) when the
/// keys are absent; an unparseable block string falls back to empty (the chain
/// is authoritative; this is a cache).
fn read_version(conn: &Connection) -> Result<VersionState> {
    let version = read_meta_u32(conn, META_DB_VERSION)?.unwrap_or(GENESIS_DB_VERSION);
    let blocks = match read_meta_text(conn, META_DB_VERSION_BLOCKS)? {
        Some(s) => BlockSet::parse(&s).unwrap_or_default(),
        None => BlockSet::default(),
    };
    Ok(VersionState { version, blocks })
}

/// Read the database's required version from the snapshot cache alone, without sync
/// no full replay. Reflects only memos already promoted into the snapshot (at
/// least [`SAFE_DEPTH`] deep); a recent `VERSION` memo still in the live tail is
/// not visible here. Used for the pre-sync `blocksync` decision. A missing
/// snapshot reads as genesis (no blocks).
pub fn cached_version(path: &Path) -> Result<VersionState> {
    if !path.exists() {
        return Ok(VersionState::default());
    }
    let conn = open(path)?;
    read_version(&conn)
}

fn read_meta_text(conn: &Connection, k: &str) -> Result<Option<String>> {
    match conn.query_row(
        "SELECT v FROM meta WHERE k = :k",
        named_params! { ":k": k },
        |r| r.get::<_, String>(0),
    ) {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn read_meta_u32(conn: &Connection, k: &str) -> Result<Option<u32>> {
    read_meta_text(conn, k)?
        .map(|s| s.parse::<u32>())
        .transpose()
        .map_err(|e| anyhow!("meta[{k}] not a u32: {e}"))
}

fn read_meta_blob(conn: &Connection, k: &str) -> Result<Option<Vec<u8>>> {
    match conn.query_row(
        "SELECT v FROM meta WHERE k = :k",
        named_params! { ":k": k },
        |r| r.get::<_, Vec<u8>>(0),
    ) {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn write_meta_text(tx: &Transaction<'_>, k: &str, v: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO meta (k, v) VALUES (:k, :v)
         ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        named_params! { ":k": k, ":v": v },
    )?;
    Ok(())
}

fn write_meta_u32(tx: &Transaction<'_>, k: &str, v: u32) -> Result<()> {
    write_meta_text(tx, k, &v.to_string())
}

fn write_meta_blob(tx: &Transaction<'_>, k: &str, v: &[u8]) -> Result<()> {
    tx.execute(
        "INSERT INTO meta (k, v) VALUES (:k, :v)
         ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        named_params! { ":k": k, ":v": v },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::protocol::{
        build_init_memo, build_memo, parse_text_memo, pubkey_bech32, render_memo_text,
        replay_audit, sign_command, signed_init_payload, signed_payload, signing_domain,
        verify_command, AuditEntry, RowOutcome, VERSION_WINDOW,
    };
    use zcash_protocol::memo::Memo;

    fn keypair() -> (secp256k1::SecretKey, secp256k1::PublicKey) {
        let secp = secp256k1::Secp256k1::new();
        let sk = secp256k1::SecretKey::from_slice(&[0x42u8; 32]).unwrap();
        let pk = sk.public_key(&secp);
        (sk, pk)
    }

    fn init_memo_text(sk: &secp256k1::SecretKey, addr: &str) -> String {
        let payload = signed_init_payload(addr);
        let sig = sign_command(sk, &payload);
        let memo = build_init_memo(addr, &sig).unwrap();
        match Memo::try_from(memo).unwrap() {
            Memo::Text(t) => t.to_string(),
            _ => unreachable!(),
        }
    }

    /// Signed memo at the entity's version 0 (first/only write to a key/target).
    /// Same-key/target follow-ups use [`op_memo_text_v`] with the next version.
    fn op_memo_text(
        sk: &secp256k1::SecretKey,
        addr: &str,
        op: Op,
        k: &str,
        v: Option<&str>,
    ) -> String {
        op_memo_text_v(sk, addr, op, k, v, 0)
    }

    /// Signed memo over the `ZKV0` versioned domain. `addr` is the receiver
    /// domain; `version` is the key/target's current replay-protection version.
    fn op_memo_text_v(
        sk: &secp256k1::SecretKey,
        addr: &str,
        op: Op,
        k: &str,
        v: Option<&str>,
        version: u64,
    ) -> String {
        let domain = signing_domain(addr, op, version);
        let payload = signed_payload(&domain, op, k, v);
        let sig = sign_command(sk, &payload);
        let memo = build_memo(op, k, v, version, &sig).unwrap();
        match Memo::try_from(memo).unwrap() {
            Memo::Text(t) => t.to_string(),
            _ => unreachable!(),
        }
    }

    /// Tiny synthetic txid generator. Real txids come from the wallet
    /// row as a 32-byte BLOB; tests only need them to be unique and to
    /// sort the way the real ordering does.
    fn synth_txid(seed: u8) -> Vec<u8> {
        let mut t = vec![0u8; 32];
        t[0] = seed;
        t
    }

    fn fresh() -> Connection {
        // In-memory SQLite: open() inspects user_version, which on a
        // brand-new in-memory database is 0, so the schema gets
        // initialized identically to a real file.
        let mut conn = Connection::open_in_memory().unwrap();
        let v: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 0);
        init_schema(&mut conn).unwrap();
        conn
    }

    #[test]
    fn init_schema_sets_user_version_and_tables() {
        let conn = fresh();
        let v: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        // All three tables exist and are empty.
        for t in &["meta", "kv", "kv_history"] {
            let n: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{t} should start empty");
        }
    }

    #[test]
    fn promote_init_then_sets_and_dels_populates_kv_and_history() {
        let (sk, pk) = keypair();
        let addr = "zkv1test1";
        let mut conn = fresh();

        let rows = vec![
            PromoteRow {
                mined_height: 100,
                txid: synth_txid(1),
                output_index: 0,
                block_time: None,
                memo_text: init_memo_text(&sk, addr),
            },
            PromoteRow {
                mined_height: 101,
                txid: synth_txid(2),
                output_index: 0,
                block_time: None,
                memo_text: op_memo_text(&sk, addr, Op::Set, "a", Some("1")),
            },
            PromoteRow {
                mined_height: 102,
                txid: synth_txid(3),
                output_index: 0,
                block_time: None,
                memo_text: op_memo_text(&sk, addr, Op::Set, "b", Some("stable")),
            },
            PromoteRow {
                mined_height: 103,
                txid: synth_txid(4),
                output_index: 0,
                block_time: None,
                // Second write to "a", version 1.
                memo_text: op_memo_text_v(&sk, addr, Op::Set, "a", Some("2"), 1),
            },
            PromoteRow {
                mined_height: 104,
                txid: synth_txid(5),
                output_index: 0,
                block_time: None,
                // Second write to "b" (after its create), version 1.
                memo_text: op_memo_text_v(&sk, addr, Op::Del, "b", None, 1),
            },
        ];
        promote(&mut conn, &rows, addr, &pk).unwrap();

        // Final kv: a=2, b deleted.
        let mut kv: Vec<(String, String)> = conn
            .prepare("SELECT key, value FROM kv ORDER BY key")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        kv.sort();
        assert_eq!(kv, vec![("a".into(), "2".into())]);

        // History: 5 rows (INIT, 3 SETs, 1 DEL), in chain order. INIT is
        // recorded with the zkv address as its key.
        let hist: Vec<(i64, String, String, Option<String>)> = conn
            .prepare(
                "SELECT mined_height, key, op, value FROM kv_history
                 ORDER BY mined_height, txid, output_index",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            hist,
            vec![
                (100, addr.into(), "INIT".into(), None),
                (101, "a".into(), "SET".into(), Some("1".into())),
                (102, "b".into(), "SET".into(), Some("stable".into())),
                (103, "a".into(), "SET".into(), Some("2".into())),
                (104, "b".into(), "DEL".into(), None),
            ]
        );

        // Watermark advanced to the last applied row.
        let wm = read_watermark(&conn).unwrap();
        assert_eq!(wm.height, 104);
        assert_eq!(wm.txid, synth_txid(5));
        assert_eq!(wm.output_index, 0);

        // Init state recorded.
        assert!(
            matches!(load_seed(&conn).unwrap().init, InitState::Initialized),
            "INIT should have persisted",
        );
    }

    #[test]
    fn promote_drops_stale_sequence() {
        // The persisted twin of `replay_drops_stale_sequence_set`: a verbatim
        // re-broadcast of the original create (sequence 0) after the key has
        // advanced is dropped by promote's version-CAS (the shared
        // `seq_in_window`), so it neither reverts the value nor records a row.
        let (sk, pk) = keypair();
        let addr = "zkv1test1";
        let mut conn = fresh();
        promote(
            &mut conn,
            &[
                promote_row(1, 100, init_memo_text(&sk, addr)),
                promote_row(
                    2,
                    101,
                    op_memo_text_v(&sk, addr, Op::Set, "k", Some("v1"), 0),
                ),
                promote_row(
                    3,
                    102,
                    op_memo_text_v(&sk, addr, Op::Set, "k", Some("v2"), 1),
                ),
                // Replay the original create (seq 0) after "k" advanced to 2.
                promote_row(
                    4,
                    103,
                    op_memo_text_v(&sk, addr, Op::Set, "k", Some("v1"), 0),
                ),
            ],
            addr,
            &pk,
        )
        .unwrap();

        let value: String = conn
            .query_row("SELECT value FROM kv WHERE key = 'k'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            value, "v2",
            "stale-sequence promote must not revert the value"
        );

        // The dropped replay left no extra history row: only the two honored SETs.
        let history_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM kv_history WHERE key = 'k'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(history_rows, 2, "the dropped replay must not be recorded");

        // The per-key high-water held at 2 (it never regressed to the replayed 0).
        assert_eq!(
            load_seed(&conn).unwrap().kv_versions.get("k").copied(),
            Some(2)
        );
    }

    #[test]
    fn promote_drops_sequence_beyond_window() {
        // The persisted twin of `replay_drops_sequence_beyond_window`: a SET whose
        // wire sequence is past the bounded-forward window (a desync larger than
        // tolerated, or a counter-jump freeze attempt) is dropped by promote, so
        // it never lands in `kv` or advances the high-water.
        let (sk, pk) = keypair();
        let addr = "zkv1test1";
        let mut conn = fresh();
        promote(
            &mut conn,
            &[
                promote_row(1, 100, init_memo_text(&sk, addr)),
                promote_row(
                    2,
                    101,
                    op_memo_text_v(&sk, addr, Op::Set, "k", Some("v"), VERSION_WINDOW + 1),
                ),
            ],
            addr,
            &pk,
        )
        .unwrap();

        let k_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM kv WHERE key = 'k'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(k_rows, 0, "a beyond-window sequence must not land in kv");
        assert_eq!(
            load_seed(&conn).unwrap().kv_versions.get("k").copied(),
            None
        );
    }

    #[test]
    fn promote_sendmany_multiple_memos_one_tx_parsed_in_output_order() {
        // A single "sendmany" transaction can carry many shielded outputs,
        // each with its own memo. The read path keys memos by
        // `(mined_height, txid, output_index)` and orders by `output_index`
        // ASC within a txid (see `state.rs` scan query), so all of a tx's
        // memos must be parsed, applied in output order (last-write-wins for
        // a repeated key), and recorded individually in kv_history. The
        // `output_index` zkv sees is the position within the pool's bundle
        // (Sapling shielded outputs / Orchard actions), so it is ascending
        // but may be sparse (other recipients' / non-zkv outputs sit between
        // ours); the watermark must still advance past the whole tx.
        let (sk, pk) = keypair();
        let addr = "zkv1test1";
        let mut conn = fresh();

        // INIT lands in its own earlier transaction.
        // The sendmany is one txid at one height, fanning out over several
        // bundle outputs. Indices are deliberately sparse: output 2 is a
        // foreign (non-zkv) memo that scanning still surfaces but replay
        // ignores. Two writes target key "a"; the later output (index 3)
        // must win over the earlier one (index 0).
        let txid = synth_txid(7);
        let rows = vec![
            PromoteRow {
                mined_height: 100,
                txid: synth_txid(1),
                output_index: 0,
                block_time: None,
                memo_text: init_memo_text(&sk, addr),
            },
            PromoteRow {
                mined_height: 101,
                txid: txid.clone(),
                output_index: 0,
                block_time: Some(1_700_000_101),
                memo_text: op_memo_text(&sk, addr, Op::Set, "a", Some("first")),
            },
            PromoteRow {
                mined_height: 101,
                txid: txid.clone(),
                output_index: 1,
                block_time: Some(1_700_000_101),
                memo_text: op_memo_text(&sk, addr, Op::Set, "b", Some("x")),
            },
            PromoteRow {
                mined_height: 101,
                txid: txid.clone(),
                output_index: 2,
                block_time: Some(1_700_000_101),
                // Foreign memo riding in the same sendmany: not ZKV0, so
                // replay skips it, but its slot must not derail the others.
                memo_text: "just a normal memo".to_string(),
            },
            PromoteRow {
                mined_height: 101,
                txid: txid.clone(),
                output_index: 3,
                block_time: Some(1_700_000_101),
                // Second write to "a" in the SAME tx, version 1. Being at a
                // higher output_index, it must win.
                memo_text: op_memo_text_v(&sk, addr, Op::Set, "a", Some("second"), 1),
            },
            PromoteRow {
                mined_height: 101,
                txid: txid.clone(),
                output_index: 4,
                block_time: Some(1_700_000_101),
                // DEL "b" in the same tx (b's version 1): removes the create
                // that happened two outputs earlier.
                memo_text: op_memo_text_v(&sk, addr, Op::Del, "b", None, 1),
            },
        ];
        promote(&mut conn, &rows, addr, &pk).unwrap();

        // Final kv: a wins from the later output ("second"); b created then
        // deleted within the one tx, so it is absent.
        let seed = load_seed(&conn).unwrap();
        assert_eq!(
            seed.state.get("a").and_then(|k| k.confirmed.as_deref()),
            Some("second"),
            "the higher-output_index write to a must win within the tx",
        );
        assert!(
            !seed.state.contains_key("b"),
            "b was created and deleted in the same sendmany",
        );

        // kv_history records every zkv memo of the tx individually, keyed by
        // its output_index; the foreign output (index 2) is not recorded.
        let hist: Vec<(u32, String, String, Option<String>)> = conn
            .prepare(
                "SELECT output_index, key, op, value FROM kv_history
                 WHERE txid = ?1 ORDER BY output_index",
            )
            .unwrap()
            .query_map([&txid], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            hist,
            vec![
                (0, "a".into(), "SET".into(), Some("first".into())),
                (1, "b".into(), "SET".into(), Some("x".into())),
                (3, "a".into(), "SET".into(), Some("second".into())),
                (4, "b".into(), "DEL".into(), None),
            ],
            "all four zkv outputs recorded in output order; foreign index 2 skipped",
        );

        // Watermark advances to the tx's last output (4), so a re-sync
        // resumes strictly after the whole sendmany.
        let wm = read_watermark(&conn).unwrap();
        assert_eq!(wm.height, 101);
        assert_eq!(wm.txid, txid);
        assert_eq!(wm.output_index, 4);
    }

    /// Signed memo carrying a first-line `#…` comment, at version 0.
    fn op_memo_text_commented(
        sk: &secp256k1::SecretKey,
        addr: &str,
        op: Op,
        k: &str,
        v: Option<&str>,
        comment: &str,
    ) -> String {
        use crate::internal::protocol::{build_memo_with_comment, payload_for, ZkvCommand};
        let cmd = ZkvCommand {
            op,
            key: k.to_owned(),
            value: v.map(str::to_owned),
            seq: 0,
            sig_hex: String::new(),
            comment: Some(comment.to_owned()),
        };
        let sig = sign_command(sk, &payload_for(addr, &cmd));
        let memo = build_memo_with_comment(op, k, v, 0, &sig, Some(comment)).unwrap();
        match Memo::try_from(memo).unwrap() {
            Memo::Text(t) => t.to_string(),
            _ => unreachable!(),
        }
    }

    #[test]
    fn promote_persists_comment_and_memo_round_trips() {
        use crate::internal::protocol::render_memo_with_comment;
        let (sk, pk) = keypair();
        let addr = "zkv1test1";
        let mut conn = fresh();

        let commented = op_memo_text_commented(&sk, addr, Op::Set, "a", Some("1"), " just testing");
        let rows = vec![
            PromoteRow {
                mined_height: 100,
                txid: synth_txid(1),
                output_index: 0,
                block_time: None,
                memo_text: init_memo_text(&sk, addr),
            },
            PromoteRow {
                mined_height: 101,
                txid: synth_txid(2),
                output_index: 0,
                block_time: None,
                memo_text: commented.clone(),
            },
        ];
        // A commented write that recovers an authorized signer must apply; if
        // promote reconstructed the payload without the comment it would recover
        // a different key and silently drop the write.
        promote(&mut conn, &rows, addr, &pk).unwrap();
        let v: Option<String> = conn
            .query_row("SELECT value FROM kv WHERE key = 'a'", [], |r| r.get(0))
            .optional()
            .unwrap();
        assert_eq!(v.as_deref(), Some("1"));

        // The comment is persisted and the deep row reconstructs byte-for-byte.
        let rows_back = history_page(&conn, Some("a"), None, true, None, 0).unwrap();
        assert_eq!(rows_back.len(), 1);
        let row = &rows_back[0];
        assert_eq!(row.comment.as_deref(), Some(" just testing"));
        let rebuilt = render_memo_with_comment(
            Op::Set,
            &row.key,
            row.value.as_deref(),
            row.seq,
            &row.signature,
            row.comment.as_deref(),
        );
        assert_eq!(rebuilt, commented);
    }

    #[test]
    fn promote_drops_pre_init_writes() {
        // A SET that lands before the INIT memo in chain order must be
        // dropped; that is the same security guarantee replay enforces.
        let (sk, pk) = keypair();
        let addr = "zkv1test1";
        let mut conn = fresh();

        let rows = vec![
            PromoteRow {
                mined_height: 100,
                txid: synth_txid(1),
                output_index: 0,
                block_time: None,
                memo_text: op_memo_text(&sk, addr, Op::Set, "pre", Some("ghost")),
            },
            PromoteRow {
                mined_height: 101,
                txid: synth_txid(2),
                output_index: 0,
                block_time: None,
                memo_text: init_memo_text(&sk, addr),
            },
            PromoteRow {
                mined_height: 102,
                txid: synth_txid(3),
                output_index: 0,
                block_time: None,
                memo_text: op_memo_text(&sk, addr, Op::Set, "post", Some("kept")),
            },
        ];
        promote(&mut conn, &rows, addr, &pk).unwrap();

        let seed = load_seed(&conn).unwrap();
        assert!(matches!(seed.init, InitState::Initialized));
        assert!(
            !seed.state.contains_key("pre"),
            "pre-INIT write must not land"
        );
        assert_eq!(
            seed.state.get("post").and_then(|k| k.confirmed.as_deref()),
            Some("kept"),
        );
        // The dropped row is absent from history; INIT (keyed by the zkv
        // address) and the post-INIT write are present, in chain order.
        let hist_keys: Vec<String> = conn
            .prepare("SELECT key FROM kv_history ORDER BY mined_height")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(hist_keys, vec![addr.to_string(), "post".to_string()]);
    }

    #[test]
    fn promote_pre_init_batch_does_not_advance_watermark() {
        // Genesis safety: a batch that never reaches a valid INIT must leave the
        // watermark untouched, so the not-yet-scanned genesis INIT (in an
        // earlier range the tip-first wallet hasn't backfilled) can still be
        // picked up later. Advancing here is exactly the bug that strands the
        // INIT below the watermark and leaves the database stuck uninitialized.
        let (sk, pk) = keypair();
        let addr = "zkv1test1";
        let mut conn = fresh();

        let rows = vec![
            PromoteRow {
                mined_height: 200,
                txid: synth_txid(7),
                output_index: 0,
                block_time: None,
                memo_text: op_memo_text(&sk, addr, Op::Set, "early", Some("ghost")),
            },
            PromoteRow {
                mined_height: 201,
                txid: synth_txid(8),
                output_index: 0,
                block_time: None,
                memo_text: "plain text, not a zkv memo".to_owned(),
            },
        ];
        promote(&mut conn, &rows, addr, &pk).unwrap();

        // Watermark stayed at genesis (0); nothing initialized, nothing logged.
        let wm = read_watermark(&conn).unwrap();
        assert_eq!(
            wm.height, 0,
            "uninitialized batch must not advance watermark"
        );
        assert!(matches!(
            read_init_state(&conn).unwrap(),
            InitState::Uninitialized
        ));
        let hist: i64 = conn
            .query_row("SELECT count(*) FROM kv_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(hist, 0);

        // A later batch that finally carries the INIT recovers everything from
        // genesis (the earlier blocks are re-presented because the watermark
        // never moved past them).
        let rows2 = vec![
            PromoteRow {
                mined_height: 200,
                txid: synth_txid(7),
                output_index: 0,
                block_time: None,
                memo_text: op_memo_text(&sk, addr, Op::Set, "early", Some("ghost")),
            },
            PromoteRow {
                mined_height: 202,
                txid: synth_txid(9),
                output_index: 0,
                block_time: None,
                memo_text: init_memo_text(&sk, addr),
            },
        ];
        promote(&mut conn, &rows2, addr, &pk).unwrap();
        assert!(matches!(
            read_init_state(&conn).unwrap(),
            InitState::Initialized
        ));
        assert_eq!(read_watermark(&conn).unwrap().height, 202);
    }

    #[test]
    fn promote_advances_watermark_even_for_garbage_rows() {
        // Non-zkv memos (or zkv memos with bad signatures) still cost us
        // a row scan in the wallet DB. Advancing the watermark past them
        // is what keeps subsequent reads cheap.
        let (sk, pk) = keypair();
        let addr = "zkv1test1";
        let mut conn = fresh();

        let rows = vec![
            PromoteRow {
                mined_height: 100,
                txid: synth_txid(1),
                output_index: 0,
                block_time: None,
                memo_text: init_memo_text(&sk, addr),
            },
            PromoteRow {
                mined_height: 101,
                txid: synth_txid(2),
                output_index: 0,
                block_time: None,
                memo_text: "hello, plain text, not zkv".to_owned(),
            },
        ];
        promote(&mut conn, &rows, addr, &pk).unwrap();
        let wm = read_watermark(&conn).unwrap();
        assert_eq!(wm.height, 101);
        assert_eq!(wm.txid, synth_txid(2));
    }

    #[test]
    fn promote_is_atomic_per_call() {
        // promote() runs each batch in a transaction; if a row's kv_history
        // INSERT hits the primary key of an already-promoted row, the whole
        // batch must roll back and the prior state stay intact. (Re-promoting
        // the *identical* batch no longer collides (version-CAS drops the
        // duplicate INIT and the stale-version SET before they reach
        // kv_history), so we collide with a *fresh*, validly-signed write that
        // reuses an existing (height, txid, output_index).)
        let (sk, pk) = keypair();
        let addr = "zkv1test1";
        let mut conn = fresh();

        let rows = vec![
            PromoteRow {
                mined_height: 100,
                txid: synth_txid(1),
                output_index: 0,
                block_time: None,
                memo_text: init_memo_text(&sk, addr),
            },
            PromoteRow {
                mined_height: 101,
                txid: synth_txid(2),
                output_index: 0,
                block_time: None,
                memo_text: op_memo_text(&sk, addr, Op::Set, "a", Some("1")),
            },
        ];
        promote(&mut conn, &rows, addr, &pk).unwrap();
        // A new key "b" (version 0, root-signed → valid) reusing the a-row's
        // (height, txid, output_index): it passes auth + version-CAS and so
        // reaches the kv_history INSERT, where it collides on the PK.
        let conflict = vec![PromoteRow {
            mined_height: 101,
            txid: synth_txid(2),
            output_index: 0,
            block_time: None,
            memo_text: op_memo_text(&sk, addr, Op::Set, "b", Some("1")),
        }];
        let err = promote(&mut conn, &conflict, addr, &pk)
            .expect_err("a fresh write reusing an existing kv_history PK must conflict");
        let msg = format!("{err}");
        assert!(
            msg.to_ascii_lowercase().contains("unique")
                || msg.to_ascii_lowercase().contains("constraint"),
            "unexpected error: {msg}",
        );
        // State from the first promote is intact (rollback worked).
        let seed = load_seed(&conn).unwrap();
        assert_eq!(
            seed.state.get("a").and_then(|k| k.confirmed.as_deref()),
            Some("1")
        );
    }

    #[test]
    fn reopen_in_memory_via_init_schema_is_idempotent() {
        // init_schema uses CREATE TABLE IF NOT EXISTS, so running it on
        // an already-initialized DB must be a no-op (the path we hit on
        // open() with version 0 on an empty file vs. a file with the
        // schema already created out-of-band).
        let mut conn = fresh();
        init_schema(&mut conn).unwrap();
        let v: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn wipe_clears_state_and_resets_meta() {
        let (sk, pk) = keypair();
        let addr = "zkv1test1";
        let mut conn = fresh();

        promote(
            &mut conn,
            &[
                PromoteRow {
                    mined_height: 100,
                    txid: synth_txid(1),
                    output_index: 0,
                    block_time: None,
                    memo_text: init_memo_text(&sk, addr),
                },
                PromoteRow {
                    mined_height: 101,
                    txid: synth_txid(2),
                    output_index: 0,
                    block_time: None,
                    memo_text: op_memo_text(&sk, addr, Op::Set, "a", Some("1")),
                },
            ],
            addr,
            &pk,
        )
        .unwrap();
        assert!(!load_seed(&conn).unwrap().state.is_empty());

        wipe(&mut conn).unwrap();
        let seed = load_seed(&conn).unwrap();
        assert!(seed.state.is_empty());
        assert!(matches!(seed.init, InitState::Uninitialized));
        let wm = read_watermark(&conn).unwrap();
        assert_eq!(wm.height, 0);
        assert!(wm.txid.is_empty());
    }

    #[test]
    fn promote_empty_batch_is_noop() {
        let (_, pk) = keypair();
        let mut conn = fresh();
        promote(&mut conn, &[], "zkv1test1", &pk).unwrap();
        let wm = read_watermark(&conn).unwrap();
        assert_eq!(wm.height, 0);
    }

    #[test]
    fn promote_records_signature_and_history_query_round_trips() {
        let (sk, pk) = keypair();
        let addr = "zkv1test1";
        let mut conn = fresh();

        let set_a = op_memo_text(&sk, addr, Op::Set, "a", Some("1"));
        let set_b = op_memo_text(&sk, addr, Op::Set, "b", Some("x"));
        // DEL "a" is the second write to "a" → version 1.
        let del_a = op_memo_text_v(&sk, addr, Op::Del, "a", None, 1);
        let rows = vec![
            PromoteRow {
                mined_height: 100,
                txid: synth_txid(1),
                output_index: 0,
                block_time: None,
                memo_text: init_memo_text(&sk, addr),
            },
            PromoteRow {
                mined_height: 101,
                txid: synth_txid(2),
                output_index: 0,
                block_time: Some(1_700_000_101),
                memo_text: set_a.clone(),
            },
            PromoteRow {
                mined_height: 102,
                txid: synth_txid(3),
                output_index: 1,
                block_time: Some(1_700_000_102),
                memo_text: set_b.clone(),
            },
            PromoteRow {
                mined_height: 103,
                txid: synth_txid(4),
                output_index: 0,
                block_time: None,
                memo_text: del_a.clone(),
            },
        ];
        promote(&mut conn, &rows, addr, &pk).unwrap();

        // Full page, newest-first (DESC); INIT (oldest) is last.
        let all = history_page(&conn, None, None, false, None, 0).unwrap();
        assert_eq!(all.len(), 4);
        assert_eq!((all[0].key.as_str(), all[0].op.as_str()), ("a", "DEL"));
        assert_eq!((all[1].key.as_str(), all[1].op.as_str()), ("b", "SET"));
        assert_eq!((all[2].key.as_str(), all[2].op.as_str()), ("a", "SET"));
        assert_eq!((all[3].key.as_str(), all[3].op.as_str()), (addr, "INIT"));
        assert_eq!(all[1].output_index, 1);
        assert_eq!(all[0].value, None, "DEL stores a NULL value");
        // block_time is cached on the row (no per-row block lookup).
        assert_eq!(all[2].block_time, Some(1_700_000_101));
        assert_eq!(all[1].block_time, Some(1_700_000_102));

        // The stored SET-a signature matches the memo, re-verifies, renders.
        let cmd_a = parse_text_memo(&set_a).unwrap();
        assert_eq!(all[2].signature, cmd_a.sig_hex);
        // set_a was signed at version 0, so verify over the versioned domain.
        let domain = signing_domain(addr, Op::Set, 0);
        let payload = signed_payload(&domain, Op::Set, "a", Some("1"));
        assert!(verify_command(&pk, &payload, &all[2].signature));
        // The recovered signer is persisted per row (here the root key).
        let root_hex = pubkey_bech32(&pk);
        assert!(all.iter().all(|r| r.signer == root_hex));
        assert_eq!(
            render_memo_text(Op::Set, "a", Some("1"), all[2].seq, &all[2].signature),
            set_a
        );

        // Pagination: newest row only, then the next page.
        let page0 = history_page(&conn, None, None, false, Some(1), 0).unwrap();
        assert_eq!(page0.len(), 1);
        assert_eq!(page0[0].op, "DEL");
        let page1 = history_page(&conn, None, None, false, Some(1), 1).unwrap();
        assert_eq!(page1.len(), 1);
        assert_eq!((page1[0].key.as_str(), page1[0].op.as_str()), ("b", "SET"));
        assert_eq!(history_count(&conn, None, None).unwrap(), 4);

        // Ascending flips to oldest-first: INIT leads, the DEL is last.
        let asc = history_page(&conn, None, None, true, None, 0).unwrap();
        assert_eq!(asc.first().map(|r| r.op.as_str()), Some("INIT"));
        assert_eq!(asc.last().map(|r| r.op.as_str()), Some("DEL"));

        // Op filter restricts to the requested wire opcodes (count + page).
        let only_set = vec!["SET".to_owned()];
        assert_eq!(history_count(&conn, None, Some(&only_set)).unwrap(), 2);
        let sets = history_page(&conn, None, Some(&only_set), false, None, 0).unwrap();
        assert!(sets.iter().all(|r| r.op == "SET"));
        assert_eq!(sets.len(), 2);

        // Per-key substring filter returns only that key's writes, newest-first.
        let only_a = history_page(&conn, Some("a"), None, false, None, 0).unwrap();
        assert_eq!(only_a.len(), 2);
        assert!(only_a.iter().all(|r| r.key == "a"));
        assert_eq!(only_a[0].op, "DEL");
        assert_eq!(only_a[1].op, "SET");
        assert_eq!(history_count(&conn, Some("a"), None).unwrap(), 2);

        // The `kv` cache carries the latest write's time + txid per key; "a"
        // was deleted so only "b" remains, with its SET-b metadata.
        let seed = load_seed(&conn).unwrap();
        assert!(!seed.state.contains_key("a"), "deleted key absent from kv");
        let b = seed.state.get("b").expect("b present");
        assert_eq!(b.updated_at, Some(1_700_000_102));
        assert_eq!(
            b.last_txid.as_deref(),
            txid_display(&synth_txid(3)).as_deref()
        );
    }

    #[test]
    fn promote_records_delegated_writer_signer() {
        // A scoped writer's promoted write is attributed to the *writer* in
        // kv_history.signer, not the root key.
        let (root_sk, root_pk) = keypair();
        let (w_sk, w_pk) = keypair_from(0x33);
        let addr = "zkv1test1";
        let mut conn = fresh();

        let rows = vec![
            PromoteRow {
                mined_height: 100,
                txid: synth_txid(1),
                output_index: 0,
                block_time: None,
                memo_text: init_memo_text(&root_sk, addr),
            },
            PromoteRow {
                mined_height: 101,
                txid: synth_txid(2),
                output_index: 0,
                block_time: None,
                memo_text: op_memo_text(
                    &root_sk,
                    addr,
                    Op::WriterSet,
                    &pubkey_bech32(&w_pk),
                    Some("CREATE,UPDATE"),
                ),
            },
            PromoteRow {
                mined_height: 102,
                txid: synth_txid(3),
                output_index: 0,
                block_time: None,
                memo_text: op_memo_text(&w_sk, addr, Op::Set, "price", Some("v")),
            },
        ];
        promote(&mut conn, &rows, addr, &root_pk).unwrap();

        // Only the data write is in kv_history (WRITERSET is a management op),
        // attributed to the writer.
        let all = history_page(&conn, Some("price"), None, false, None, 0).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].signer, pubkey_bech32(&w_pk));
        assert_ne!(all[0].signer, pubkey_bech32(&root_pk));
    }

    #[test]
    fn history_locate_ranks_newest_first() {
        let (sk, pk) = keypair();
        let addr = "zkv1test1";
        let mut conn = fresh();
        let rows = vec![
            PromoteRow {
                mined_height: 100,
                txid: synth_txid(1),
                output_index: 0,
                block_time: None,
                memo_text: init_memo_text(&sk, addr),
            },
            PromoteRow {
                mined_height: 101,
                txid: synth_txid(2),
                output_index: 0,
                block_time: None,
                memo_text: op_memo_text(&sk, addr, Op::Set, "a", Some("1")),
            },
            PromoteRow {
                mined_height: 102,
                txid: synth_txid(3),
                output_index: 0,
                block_time: None,
                memo_text: op_memo_text(&sk, addr, Op::Set, "b", Some("x")),
            },
            PromoteRow {
                mined_height: 103,
                txid: synth_txid(4),
                output_index: 0,
                block_time: None,
                // DEL "a" is the second write to "a" → version 1.
                memo_text: op_memo_text_v(&sk, addr, Op::Del, "a", None, 1),
            },
        ];
        promote(&mut conn, &rows, addr, &pk).unwrap();

        // Newest-first ranks across the full (unfiltered) history:
        // DEL a (0), SET b (1), SET a (2), INIT (3); unknown txid -> None.
        assert_eq!(
            history_locate(&conn, None, &synth_txid(4)).unwrap(),
            Some(0)
        );
        assert_eq!(
            history_locate(&conn, None, &synth_txid(3)).unwrap(),
            Some(1)
        );
        assert_eq!(
            history_locate(&conn, None, &synth_txid(2)).unwrap(),
            Some(2)
        );
        assert_eq!(
            history_locate(&conn, None, &synth_txid(1)).unwrap(),
            Some(3)
        );
        assert_eq!(history_locate(&conn, None, &synth_txid(9)).unwrap(), None);

        // The rank is consistent with history_page's paging: rank 2 (SET a)
        // is the first row of the page starting at offset 2.
        let page = history_page(&conn, None, None, false, Some(2), 2).unwrap();
        assert_eq!((page[0].key.as_str(), page[0].op.as_str()), ("a", "SET"));

        // Filtered ranks are relative to the filtered ordering: key "a" has
        // DEL a (0) then SET a (1); key "b" is excluded by the "a" filter.
        assert_eq!(
            history_locate(&conn, Some("a"), &synth_txid(4)).unwrap(),
            Some(0)
        );
        assert_eq!(
            history_locate(&conn, Some("a"), &synth_txid(2)).unwrap(),
            Some(1)
        );
        assert_eq!(
            history_locate(&conn, Some("a"), &synth_txid(3)).unwrap(),
            None
        );
    }

    #[test]
    fn promote_setl_records_wire_op_in_history() {
        // `kv_history` keeps the wire-form op so the audit log distinguishes
        // SET from SETL writes (the `kv` projection collapses them).
        let (sk, pk) = keypair();
        let addr = "zkv1test1";
        let mut conn = fresh();
        let rows = vec![
            PromoteRow {
                mined_height: 100,
                txid: synth_txid(1),
                output_index: 0,
                block_time: None,
                memo_text: init_memo_text(&sk, addr),
            },
            PromoteRow {
                mined_height: 101,
                txid: synth_txid(2),
                output_index: 0,
                block_time: None,
                memo_text: op_memo_text(&sk, addr, Op::Set, "a", Some("plain")),
            },
            PromoteRow {
                mined_height: 102,
                txid: synth_txid(3),
                output_index: 0,
                block_time: None,
                // Second write to "a" (SET and SETL share the counter) → v1.
                memo_text: op_memo_text_v(&sk, addr, Op::SetL, "a", Some("multi\nline"), 1),
            },
        ];
        promote(&mut conn, &rows, addr, &pk).unwrap();

        // `kv` shows the latest value regardless of wire form.
        let value: String = conn
            .query_row("SELECT value FROM kv WHERE key = 'a'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(value, "multi\nline");

        // `kv_history` records each op under the wire opcode it arrived in.
        // Scope to the data key: the INIT memo is also recorded (keyed by the
        // address) and is asserted separately in the history-query test.
        let hist: Vec<(String, String, Option<String>)> = conn
            .prepare(
                "SELECT key, op, value FROM kv_history
                 WHERE key = 'a'
                 ORDER BY mined_height, txid, output_index",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            hist,
            vec![
                (
                    "a".to_string(),
                    "SET".to_string(),
                    Some("plain".to_string())
                ),
                (
                    "a".to_string(),
                    "SETL".to_string(),
                    Some("multi\nline".to_string()),
                ),
            ],
        );
    }

    #[test]
    fn promote_setl_with_empty_value_lands_as_empty_string() {
        // Empty SETL values must reach the `kv` table as `Some("")` (the
        // value column is NOT NULL TEXT; storing `""` is the right
        // representation for "key exists, value is empty").
        let (sk, pk) = keypair();
        let addr = "zkv1test1";
        let mut conn = fresh();
        promote(
            &mut conn,
            &[
                PromoteRow {
                    mined_height: 100,
                    txid: synth_txid(1),
                    output_index: 0,
                    block_time: None,
                    memo_text: init_memo_text(&sk, addr),
                },
                PromoteRow {
                    mined_height: 101,
                    txid: synth_txid(2),
                    output_index: 0,
                    block_time: None,
                    memo_text: op_memo_text(&sk, addr, Op::SetL, "k", Some("")),
                },
            ],
            addr,
            &pk,
        )
        .unwrap();
        let value: String = conn
            .query_row("SELECT value FROM kv WHERE key = 'k'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(value, "");
        let seed = load_seed(&conn).unwrap();
        assert_eq!(
            seed.state.get("k").and_then(|ks| ks.confirmed.as_deref()),
            Some(""),
        );
    }

    #[test]
    fn incremental_promote_with_mixed_set_setl_round_trips_through_load_seed() {
        // End-to-end snapshot pipeline against mixed SET/SETL history:
        // promote a batch, load_seed, promote a second batch on top, and
        // assert the final `kv` state matches what a single full promote
        // of the concatenated batch would have produced. This is the
        // snapshot-layer analog of
        // `protocol::replay_with_seed_matches_full_replay_at_every_split`.
        let (sk, pk) = keypair();
        let addr = "zkv1test1";

        // The full chain we will check against: SET, SETL with empty
        // value, SETL with newline value, SETL overwriting SET, DEL.
        let chain = vec![
            (100u32, 1u8, init_memo_text(&sk, addr)),
            (101, 2, op_memo_text(&sk, addr, Op::Set, "a", Some("1"))),
            (102, 3, op_memo_text(&sk, addr, Op::SetL, "blank", Some(""))),
            (
                103,
                4,
                op_memo_text(&sk, addr, Op::SetL, "multi", Some("x\ny")),
            ),
            (
                104,
                5,
                op_memo_text(&sk, addr, Op::SetL, "a", Some("setl-wins\n")),
            ),
            (105, 6, op_memo_text(&sk, addr, Op::Set, "b", Some("keep"))),
            (
                106,
                7,
                op_memo_text(&sk, addr, Op::SetL, "doomed", Some("byebye")),
            ),
            (107, 8, op_memo_text(&sk, addr, Op::Del, "doomed", None)),
        ];
        let make_rows = |slice: &[(u32, u8, String)]| -> Vec<PromoteRow> {
            slice
                .iter()
                .map(|(h, t, m)| PromoteRow {
                    mined_height: *h,
                    txid: synth_txid(*t),
                    output_index: 0,
                    block_time: None,
                    memo_text: m.clone(),
                })
                .collect()
        };

        // Reference: single-shot promote of the whole chain.
        let mut single_conn = fresh();
        promote(&mut single_conn, &make_rows(&chain), addr, &pk).unwrap();
        let reference: Vec<(String, String)> = single_conn
            .prepare("SELECT key, value FROM kv ORDER BY key")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        // Incremental: promote at every prefix boundary, then promote the
        // tail on top. After each round, load_seed and the live `kv` table
        // must agree with the reference run.
        for split in 1..=chain.len() {
            let mut conn = fresh();
            promote(&mut conn, &make_rows(&chain[..split]), addr, &pk).unwrap();

            // load_seed reflects what's persisted between promote calls.
            // We don't directly inject it back (promote operates on the
            // raw db connection) but exercising it here proves the
            // serialized form round-trips through SQLite without
            // mangling SETL values (empty strings, newlines, etc.).
            let mid_seed = load_seed(&conn).unwrap();
            for (k, ks) in &mid_seed.state {
                // Confirm that what load_seed returns equals what the
                // kv table has at this point.
                let direct: String = conn
                    .query_row("SELECT value FROM kv WHERE key = ?", [k], |r| r.get(0))
                    .unwrap();
                assert_eq!(
                    ks.confirmed.as_deref(),
                    Some(direct.as_str()),
                    "load_seed disagrees with kv at split={split}, key={k:?}",
                );
            }

            // Now apply the tail on top.
            promote(&mut conn, &make_rows(&chain[split..]), addr, &pk).unwrap();
            let incremental: Vec<(String, String)> = conn
                .prepare("SELECT key, value FROM kv ORDER BY key")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            assert_eq!(
                incremental, reference,
                "split={split}: incremental promote diverged from single-shot",
            );
        }
    }

    #[test]
    fn open_wipes_on_schema_version_mismatch() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "zkv-snap-mismatch-{}-{nanos}.sqlite",
            std::process::id(),
        ));

        // Simulate an old snapshot file: a stale table + a non-current
        // schema version persisted in the header.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE kv_history (x INTEGER);
                 INSERT INTO kv_history (x) VALUES (1);
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }

        // open() must detect the mismatch, wipe, and rebuild at the current
        // version with the new (signature-carrying) schema.
        let conn = open(&path).unwrap();
        let v: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM kv_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "stale rows must be wiped");
        let has_sig: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('kv_history') WHERE name = 'signature'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            has_sig, 1,
            "rebuilt kv_history must have the signature column"
        );

        drop(conn);
        std::fs::remove_file(&path).ok();
    }

    // ===== Owner / Writer registry persistence =====

    fn keypair_from(seed: u8) -> (secp256k1::SecretKey, secp256k1::PublicKey) {
        let secp = secp256k1::Secp256k1::new();
        let sk = secp256k1::SecretKey::from_slice(&[seed; 32]).unwrap();
        let pk = sk.public_key(&secp);
        (sk, pk)
    }

    fn auth_rows(conn: &Connection) -> Vec<(String, String)> {
        conn.prepare("SELECT pubkey_bech32, role FROM auth ORDER BY pubkey_bech32")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn schema_version_current_with_auth_table() {
        let conn = fresh();
        let v: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            v, SCHEMA_VERSION,
            "auth-table schema is at the current version"
        );
        // The auth table exists and starts empty.
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM auth", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn promote_seats_root_owner_at_init() {
        let (sk, pk) = keypair();
        let addr = "zkv1test1";
        let mut conn = fresh();
        promote(
            &mut conn,
            &[PromoteRow {
                mined_height: 100,
                txid: synth_txid(1),
                output_index: 0,
                block_time: None,
                memo_text: init_memo_text(&sk, addr),
            }],
            addr,
            &pk,
        )
        .unwrap();
        let rows = auth_rows(&conn);
        assert_eq!(rows, vec![(pubkey_bech32(&pk), "owner".to_string())]);
        // And load_seed reflects it.
        let seed = load_seed(&conn).unwrap();
        assert!(seed.auth.is_owner(&pubkey_bech32(&pk)));
    }

    #[test]
    fn promote_persists_owner_and_writer_grants() {
        let (root_sk, root_pk) = keypair();
        let (owner2_sk, owner2_pk) = keypair_from(0x55);
        let _ = owner2_sk;
        let (w_sk, w_pk) = keypair_from(0x33);
        let _ = w_sk;
        let addr = "zkv1test1";
        let mut conn = fresh();

        promote(
            &mut conn,
            &[
                PromoteRow {
                    mined_height: 100,
                    txid: synth_txid(1),
                    output_index: 0,
                    block_time: None,
                    memo_text: init_memo_text(&root_sk, addr),
                },
                PromoteRow {
                    mined_height: 101,
                    txid: synth_txid(2),
                    output_index: 0,
                    block_time: None,
                    memo_text: op_memo_text(
                        &root_sk,
                        addr,
                        Op::OwnerSet,
                        &pubkey_bech32(&owner2_pk),
                        None,
                    ),
                },
                PromoteRow {
                    mined_height: 102,
                    txid: synth_txid(3),
                    output_index: 0,
                    block_time: None,
                    memo_text: op_memo_text(
                        &root_sk,
                        addr,
                        Op::WriterSet,
                        &pubkey_bech32(&w_pk),
                        Some("CREATE,UPDATE"),
                    ),
                },
            ],
            addr,
            &root_pk,
        )
        .unwrap();

        let seed = load_seed(&conn).unwrap();
        assert!(seed.auth.is_owner(&pubkey_bech32(&root_pk)));
        assert!(seed.auth.is_owner(&pubkey_bech32(&owner2_pk)));
        match seed.auth.authority_of(&pubkey_bech32(&w_pk)) {
            Some(crate::internal::protocol::Authority::Writer(scope)) => {
                assert_eq!(scope.to_wire(), "CREATE,UPDATE");
            }
            other => panic!("expected writer, got {other:?}"),
        }
    }

    #[test]
    fn promote_finalize_seals_and_persists_and_drops_later_writes() {
        let (root_sk, root_pk) = keypair();
        let addr = "zkv1test1";
        let mut conn = fresh();

        promote(
            &mut conn,
            &[
                PromoteRow {
                    mined_height: 100,
                    txid: synth_txid(1),
                    output_index: 0,
                    block_time: None,
                    memo_text: init_memo_text(&root_sk, addr),
                },
                PromoteRow {
                    mined_height: 101,
                    txid: synth_txid(2),
                    output_index: 0,
                    block_time: None,
                    memo_text: op_memo_text(&root_sk, addr, Op::Set, "k", Some("v")),
                },
                // Seal the database.
                PromoteRow {
                    mined_height: 102,
                    txid: synth_txid(3),
                    output_index: 0,
                    block_time: None,
                    memo_text: op_memo_text(&root_sk, addr, Op::Finalize, "", None),
                },
                // A post-FINALIZE write must be dropped by the promote path too.
                PromoteRow {
                    mined_height: 103,
                    txid: synth_txid(4),
                    output_index: 0,
                    block_time: None,
                    memo_text: op_memo_text(&root_sk, addr, Op::Set, "k", Some("after")),
                },
            ],
            addr,
            &root_pk,
        )
        .unwrap();

        // The latch is persisted and reloaded.
        let seed = load_seed(&conn).unwrap();
        assert!(seed.finalized, "promoted FINALIZE must persist the seal");
        // The pre-FINALIZE value survives; the post-FINALIZE SET never applied.
        let v: String = conn
            .query_row("SELECT value FROM kv WHERE key = 'k'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, "v");
    }

    #[test]
    fn promote_remembers_finalize_across_calls() {
        // A FINALIZE promoted in one batch must still seal writes promoted in a
        // later batch (the flag is read back from `meta`, not just in-memory).
        let (root_sk, root_pk) = keypair();
        let addr = "zkv1test1";
        let mut conn = fresh();

        promote(
            &mut conn,
            &[
                PromoteRow {
                    mined_height: 100,
                    txid: synth_txid(1),
                    output_index: 0,
                    block_time: None,
                    memo_text: init_memo_text(&root_sk, addr),
                },
                PromoteRow {
                    mined_height: 101,
                    txid: synth_txid(2),
                    output_index: 0,
                    block_time: None,
                    memo_text: op_memo_text(&root_sk, addr, Op::Finalize, "", None),
                },
            ],
            addr,
            &root_pk,
        )
        .unwrap();

        // Second batch: a write after the seal, in a separate promote call.
        promote(
            &mut conn,
            &[PromoteRow {
                mined_height: 102,
                txid: synth_txid(3),
                output_index: 0,
                block_time: None,
                memo_text: op_memo_text(&root_sk, addr, Op::Set, "k", Some("v")),
            }],
            addr,
            &root_pk,
        )
        .unwrap();

        assert!(load_seed(&conn).unwrap().finalized);
        let exists: Option<i64> = conn
            .query_row("SELECT 1 FROM kv WHERE key = 'k'", [], |r| r.get(0))
            .optional()
            .unwrap();
        assert!(exists.is_none(), "post-seal write must be dropped");
    }

    #[test]
    fn promote_enforces_writer_scope() {
        // A CREATE-only writer's overwrite of an owner-seeded key is dropped
        // at the snapshot layer, exactly as in replay.
        let (root_sk, root_pk) = keypair();
        let (w_sk, w_pk) = keypair_from(0x33);
        let addr = "zkv1test1";
        let mut conn = fresh();

        promote(
            &mut conn,
            &[
                PromoteRow {
                    mined_height: 100,
                    txid: synth_txid(1),
                    output_index: 0,
                    block_time: None,
                    memo_text: init_memo_text(&root_sk, addr),
                },
                PromoteRow {
                    mined_height: 101,
                    txid: synth_txid(2),
                    output_index: 0,
                    block_time: None,
                    memo_text: op_memo_text(
                        &root_sk,
                        addr,
                        Op::WriterSet,
                        &pubkey_bech32(&w_pk),
                        Some("CREATE"),
                    ),
                },
                PromoteRow {
                    mined_height: 102,
                    txid: synth_txid(3),
                    output_index: 0,
                    block_time: None,
                    memo_text: op_memo_text(&root_sk, addr, Op::Set, "k", Some("owner")),
                },
                // Create-only writer tries to overwrite: dropped.
                PromoteRow {
                    mined_height: 103,
                    txid: synth_txid(4),
                    output_index: 0,
                    block_time: None,
                    memo_text: op_memo_text(&w_sk, addr, Op::Set, "k", Some("hijack")),
                },
                // But can create a new key.
                PromoteRow {
                    mined_height: 104,
                    txid: synth_txid(5),
                    output_index: 0,
                    block_time: None,
                    memo_text: op_memo_text(&w_sk, addr, Op::Set, "fresh", Some("ok")),
                },
            ],
            addr,
            &root_pk,
        )
        .unwrap();

        let seed = load_seed(&conn).unwrap();
        assert_eq!(
            seed.state.get("k").and_then(|ks| ks.confirmed.as_deref()),
            Some("owner"),
            "create-only writer must not overwrite at snapshot layer",
        );
        assert_eq!(
            seed.state
                .get("fresh")
                .and_then(|ks| ks.confirmed.as_deref()),
            Some("ok"),
        );
    }

    #[test]
    fn promote_drops_unauthorized_writes() {
        // A signer with no registry entry cannot write through promote.
        let (root_sk, root_pk) = keypair();
        let (attacker_sk, _) = keypair_from(0x99);
        let addr = "zkv1test1";
        let mut conn = fresh();

        promote(
            &mut conn,
            &[
                PromoteRow {
                    mined_height: 100,
                    txid: synth_txid(1),
                    output_index: 0,
                    block_time: None,
                    memo_text: init_memo_text(&root_sk, addr),
                },
                PromoteRow {
                    mined_height: 101,
                    txid: synth_txid(2),
                    output_index: 0,
                    block_time: None,
                    memo_text: op_memo_text(&attacker_sk, addr, Op::Set, "k", Some("evil")),
                },
            ],
            addr,
            &root_pk,
        )
        .unwrap();

        let seed = load_seed(&conn).unwrap();
        assert!(seed.state.is_empty(), "unauthorized write must not land");
        // And the unauthorized write isn't in history either (the genesis
        // INIT row, keyed by the address, is expected and excluded here).
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM kv_history WHERE key = 'k'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn promote_drops_management_from_non_owner() {
        // A writer's attempt to grant itself owner is dropped by promote.
        let (root_sk, root_pk) = keypair();
        let (w_sk, w_pk) = keypair_from(0x33);
        let addr = "zkv1test1";
        let mut conn = fresh();

        promote(
            &mut conn,
            &[
                PromoteRow {
                    mined_height: 100,
                    txid: synth_txid(1),
                    output_index: 0,
                    block_time: None,
                    memo_text: init_memo_text(&root_sk, addr),
                },
                PromoteRow {
                    mined_height: 101,
                    txid: synth_txid(2),
                    output_index: 0,
                    block_time: None,
                    memo_text: op_memo_text(
                        &root_sk,
                        addr,
                        Op::WriterSet,
                        &pubkey_bech32(&w_pk),
                        Some("CREATE"),
                    ),
                },
                // Writer self-promotes: dropped.
                PromoteRow {
                    mined_height: 102,
                    txid: synth_txid(3),
                    output_index: 0,
                    block_time: None,
                    memo_text: op_memo_text(&w_sk, addr, Op::OwnerSet, &pubkey_bech32(&w_pk), None),
                },
            ],
            addr,
            &root_pk,
        )
        .unwrap();

        let seed = load_seed(&conn).unwrap();
        assert!(
            !seed.auth.is_owner(&pubkey_bech32(&w_pk)),
            "writer must not self-promote at snapshot layer",
        );
    }

    #[test]
    fn incremental_promote_matches_single_shot_for_registry() {
        // The registry must survive being built across multiple promote calls
        // (the snapshot analog of the replay seed-split equivalence test).
        let (root_sk, root_pk) = keypair();
        let (owner2_sk, owner2_pk) = keypair_from(0x55);
        let (w_sk, w_pk) = keypair_from(0x33);
        let addr = "zkv1test1";

        let chain: Vec<(u32, u8, String)> = vec![
            (100, 1, init_memo_text(&root_sk, addr)),
            (
                101,
                2,
                op_memo_text(
                    &root_sk,
                    addr,
                    Op::OwnerSet,
                    &pubkey_bech32(&owner2_pk),
                    None,
                ),
            ),
            (
                102,
                3,
                op_memo_text(
                    &owner2_sk,
                    addr,
                    Op::WriterSet,
                    &pubkey_bech32(&w_pk),
                    Some("CREATE,UPDATE"),
                ),
            ),
            (103, 4, op_memo_text(&w_sk, addr, Op::Set, "k", Some("v1"))),
            (
                104,
                5,
                op_memo_text(&root_sk, addr, Op::WriterDel, &pubkey_bech32(&w_pk), None),
            ),
        ];
        let make_rows = |slice: &[(u32, u8, String)]| -> Vec<PromoteRow> {
            slice
                .iter()
                .map(|(h, t, m)| PromoteRow {
                    mined_height: *h,
                    txid: synth_txid(*t),
                    output_index: 0,
                    block_time: None,
                    memo_text: m.clone(),
                })
                .collect()
        };

        let mut single = fresh();
        promote(&mut single, &make_rows(&chain), addr, &root_pk).unwrap();
        let reference = auth_rows(&single);

        for split in 1..chain.len() {
            let mut conn = fresh();
            promote(&mut conn, &make_rows(&chain[..split]), addr, &root_pk).unwrap();
            promote(&mut conn, &make_rows(&chain[split..]), addr, &root_pk).unwrap();
            assert_eq!(
                auth_rows(&conn),
                reference,
                "registry diverged at split={split}",
            );
        }
    }

    /// The snapshot promote path and the in-memory `replay_audit` must agree
    /// on the resulting confirmed state, init flag, and registry for the same
    /// memo stream, including which rows are dropped. This is the cross-path
    /// regression guard: both route through the shared `decide` + apply, so
    /// they cannot diverge on what is applied vs. dropped.
    #[test]
    fn promote_agrees_with_replay_audit() {
        let (root_sk, root_pk) = keypair();
        let (w_sk, w_pk) = keypair_from(6);
        let (stranger_sk, _) = keypair_from(9);
        let addr = "zkv1test1";
        let root_hex = pubkey_bech32(&root_pk);
        let w_hex = pubkey_bech32(&w_pk);

        // A stream mixing applied ops, an unauthorized write, and a last-owner
        // OWNERDEL (both of which must drop identically on both paths).
        let memos = vec![
            init_memo_text(&root_sk, addr),
            op_memo_text(
                &root_sk,
                addr,
                Op::Version,
                "1",
                Some("blocksync,blockwrite"),
            ),
            op_memo_text(&root_sk, addr, Op::WriterSet, &w_hex, Some("CREATE,UPDATE")),
            // "k": CREATE (v0), then UPDATE (v1). The stranger's write is
            // unauthorized: signed at the correct version 2, it drops on
            // authority (identically on both paths), not on signature.
            op_memo_text(&w_sk, addr, Op::Set, "k", Some("v1")),
            op_memo_text_v(&w_sk, addr, Op::Set, "k", Some("v2"), 1),
            op_memo_text_v(&stranger_sk, addr, Op::Set, "k", Some("evil"), 2),
            op_memo_text(&root_sk, addr, Op::OwnerDel, &root_hex, None),
            op_memo_text(&root_sk, addr, Op::Set, "other", Some("x")),
        ];

        // Snapshot path.
        let mut conn = fresh();
        let rows: Vec<PromoteRow> = memos
            .iter()
            .enumerate()
            .map(|(i, memo_text)| PromoteRow {
                mined_height: 100 + i as u32,
                txid: synth_txid(i as u8 + 1),
                output_index: 0,
                memo_text: memo_text.clone(),
                block_time: None,
            })
            .collect();
        promote(&mut conn, &rows, addr, &root_pk).unwrap();
        let from_snapshot = load_seed(&conn).unwrap();

        // In-memory history path (all rows confirmed).
        let hist = replay_audit(
            memos.into_iter().map(|text| AuditEntry {
                mined_height: Some(100),
                timestamp: None,
                txid: String::new(),
                text,
                status: WriteStatus::Confirmed,
            }),
            addr,
            &root_pk,
        );

        assert_eq!(from_snapshot.init, hist.init);
        assert_eq!(from_snapshot.auth, hist.auth);
        // The VERSION projection must agree across both paths too.
        assert_eq!(from_snapshot.version, hist.version);
        assert_eq!(from_snapshot.version.version, 1);
        // Compare the *decision* (confirmed value per key), not the presentation
        // metadata (`updated_at`/`last_txid`, which the snapshot records from the
        // real block but the audit primitive leaves unset). load_seed's state is
        // the `kv` projection (deleted keys absent); the audit keeps emptied keys.
        let snap_confirmed: BTreeMap<_, _> = from_snapshot
            .state
            .iter()
            .filter_map(|(k, ks)| ks.confirmed.clone().map(|v| (k.clone(), v)))
            .collect();
        let hist_confirmed: BTreeMap<_, _> = hist
            .state
            .iter()
            .filter_map(|(k, ks)| ks.confirmed.clone().map(|v| (k.clone(), v)))
            .collect();
        assert_eq!(snap_confirmed, hist_confirmed);

        // Sanity: the unauthorized write and last-owner OWNERDEL were dropped.
        assert!(hist
            .rows
            .iter()
            .any(|r| matches!(r.outcome, RowOutcome::Dropped(_))));
        assert_eq!(
            hist.state.get("k").unwrap().confirmed.as_deref(),
            Some("v2")
        );
    }

    /// A signed VERSION memo for the promote tests.
    fn version_memo_text(sk: &secp256k1::SecretKey, addr: &str, n: u32, flags: &str) -> String {
        op_memo_text(sk, addr, Op::Version, &n.to_string(), Some(flags))
    }

    fn promote_row(seed: u8, height: u32, memo_text: String) -> PromoteRow {
        PromoteRow {
            mined_height: height,
            txid: synth_txid(seed),
            output_index: 0,
            block_time: None,
            memo_text,
        }
    }

    #[test]
    fn promote_drops_unsupported_version_init() {
        // Regression: a snapshot rebuilt from memos a *future*-version (ZKV1)
        // build broadcast must not mark the database initialized. The INIT
        // parses as UnsupportedVersion (ver 1 > our epoch 0) and is dropped, so
        // after the schema-bump wipe these databases consistently show as
        // uninitialized rather than trusting a stale ZKV1-era projection.
        let (sk, pk) = keypair();
        let addr = "zkv1test1";
        let zkv1_init = init_memo_text(&sk, addr).replacen("ZKV0", "ZKV1", 1);
        assert!(zkv1_init.starts_with("ZKV1 INIT"));
        let mut conn = fresh();
        promote(&mut conn, &[promote_row(1, 100, zkv1_init)], addr, &pk).unwrap();
        assert!(
            matches!(load_seed(&conn).unwrap().init, InitState::Uninitialized),
            "a ZKV1 INIT must not initialize a ZKV0 build"
        );
    }

    #[test]
    fn promote_folds_version_into_meta() {
        let (sk, pk) = keypair();
        let addr = "zkv1test1";
        let mut conn = fresh();
        let rows = vec![
            promote_row(1, 100, init_memo_text(&sk, addr)),
            promote_row(2, 101, version_memo_text(&sk, addr, 1, "blockwrite")),
        ];
        promote(&mut conn, &rows, addr, &pk).unwrap();

        let version = load_seed(&conn).unwrap().version;
        assert_eq!(version.version, 1);
        assert_eq!(version.blocks.to_wire(), "blockwrite");
        // `cached_version`-equivalent read of the meta projection.
        assert_eq!(read_version(&conn).unwrap(), version);
        // VERSION is db-global meta; it must not leak into kv / kv_history.
        let kv: i64 = conn
            .query_row("SELECT COUNT(*) FROM kv", [], |r| r.get(0))
            .unwrap();
        let hist: i64 = conn
            .query_row("SELECT COUNT(*) FROM kv_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kv, 0, "VERSION must not write kv");
        assert_eq!(hist, 1, "only the INIT row is logged to history");
    }

    #[test]
    fn promote_drops_version_jump_and_non_owner() {
        let (root_sk, root_pk) = keypair();
        let (stranger_sk, _) = keypair_from(9);
        let addr = "zkv1test1";

        // Multi-step jump from genesis is dropped; version stays at 0.
        let mut conn = fresh();
        promote(
            &mut conn,
            &[
                promote_row(1, 100, init_memo_text(&root_sk, addr)),
                promote_row(2, 101, version_memo_text(&root_sk, addr, 2, "warn")),
            ],
            addr,
            &root_pk,
        )
        .unwrap();
        assert_eq!(load_seed(&conn).unwrap().version.version, 0);

        // A VERSION signed by a non-owner is dropped too.
        let mut conn = fresh();
        promote(
            &mut conn,
            &[
                promote_row(1, 100, init_memo_text(&root_sk, addr)),
                promote_row(2, 101, version_memo_text(&stranger_sk, addr, 1, "blockall")),
            ],
            addr,
            &root_pk,
        )
        .unwrap();
        assert_eq!(load_seed(&conn).unwrap().version.version, 0);
    }

    #[test]
    fn promote_version_composes_across_split_batches() {
        let (root_sk, root_pk) = keypair();
        let addr = "zkv1test1";
        // One-step-at-a-time climb spanning what may be two promote batches: the
        // running version persisted in `meta` is the start point of the next
        // batch, so the step rule composes across the watermark boundary.
        let chain = [
            init_memo_text(&root_sk, addr),
            version_memo_text(&root_sk, addr, 1, "warn"),
            version_memo_text(&root_sk, addr, 2, "blockall"),
        ];
        let make_rows = |memos: &[String], base: u8| -> Vec<PromoteRow> {
            memos
                .iter()
                .enumerate()
                .map(|(i, m)| promote_row(base + i as u8, 100 + i as u32, m.clone()))
                .collect()
        };

        for split in 1..chain.len() {
            let mut conn = fresh();
            promote(&mut conn, &make_rows(&chain[..split], 1), addr, &root_pk).unwrap();
            promote(
                &mut conn,
                &make_rows(&chain[split..], 1 + split as u8),
                addr,
                &root_pk,
            )
            .unwrap();
            let v = load_seed(&conn).unwrap().version;
            assert_eq!(v.version, 2, "split={split}");
            assert_eq!(v.blocks, BlockSet::all(), "split={split}");
        }
    }
}
