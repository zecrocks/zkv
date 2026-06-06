//! Per-database local cache of recently-broadcast txs.
//!
//! Why this exists: a `zkv set` / `zkv del` returns a txid the moment
//! lightwalletd accepts the tx, but the wallet DB doesn't see the tx (with a
//! decrypted memo in `v_tx_outputs`) until a later sync. Under default
//! `--confirmations >= 1`, the read path won't surface arbitrary mempool
//! entries off the wire, but it should still show *our own* just-broadcast
//! writes, because the local client has first-person knowledge they exist.
//!
//! This module is the source of truth for "what did this client just
//! broadcast and hasn't seen on chain yet." It's small (a handful of entries
//! at most), append-mostly, and survives a crash.
//!
//! File: `<data-dir>/<db>/pending.toml`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::data::db_dir;

const FILE: &str = "pending.toml";

/// Drop entries older than this. Mempool eviction on Zcash happens far inside
/// this window, so anything still here after an hour is genuinely lost.
const STALE_AFTER_SECS: u64 = 3600;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingEntry {
    /// Lowercase hex txid, matching `Transaction::txid().to_string()`.
    pub txid: String,
    /// "INIT" | "SET" | "DEL".
    pub op: String,
    /// Empty for INIT; otherwise the SET/DEL key.
    pub key: String,
    /// `Some` for SET; `None` for DEL and INIT.
    pub value: Option<String>,
    /// The exact signed wire memo as broadcast (two lines: the op line plus
    /// the signature). Lets the History view show in-flight writes with their
    /// raw signed memo before the wallet has indexed the tx. `#[serde(default)]`
    /// keeps older `pending.toml` files (written before this field existed)
    /// readable; they deserialize to `None`.
    #[serde(default)]
    pub memo: Option<String>,
    pub broadcast_at_unix: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PendingFile {
    #[serde(default)]
    pending: Vec<PendingEntry>,
}

fn path(db: &str) -> Result<PathBuf> {
    Ok(db_dir(db)?.join(FILE))
}

pub fn now_unix() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs(),
        Err(e) => {
            tracing::warn!("system clock is before the unix epoch ({e}); recording timestamp as 0");
            0
        }
    }
}

/// Load entries, lazily dropping anything older than `STALE_AFTER_SECS`.
/// Returns `[]` if the file doesn't exist.
pub fn load(db: &str) -> Result<Vec<PendingEntry>> {
    let p = path(db)?;
    if !p.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&p).map_err(|e| anyhow!("read {}: {e}", p.display()))?;
    let file: PendingFile =
        toml::from_str(&text).map_err(|e| anyhow!("parse {}: {e}", p.display()))?;
    let cutoff = now_unix().saturating_sub(STALE_AFTER_SECS);
    Ok(file
        .pending
        .into_iter()
        .filter(|e| e.broadcast_at_unix >= cutoff)
        .collect())
}

fn write_all(db: &str, entries: &[PendingEntry]) -> Result<()> {
    let p = path(db)?;
    let file = PendingFile {
        pending: entries.to_vec(),
    };
    let text = toml::to_string(&file).map_err(|e| anyhow!("serialize pending entries: {e}"))?;
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| anyhow!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &p)
        .map_err(|e| anyhow!("rename {} -> {}: {e}", tmp.display(), p.display()))?;
    Ok(())
}

/// Append one entry. Read-modify-write to a tempfile + atomic rename.
pub fn append(db: &str, entry: PendingEntry) -> Result<()> {
    let mut entries = load(db)?;
    entries.push(entry);
    write_all(db, &entries)
}

/// Drop entries whose `txid` appears in `seen` (typically: txids the wallet
/// has now indexed, mined or otherwise). No-op if the cache file is empty.
pub fn prune(db: &str, seen: &HashSet<String>) -> Result<()> {
    let entries = load(db)?;
    if entries.is_empty() {
        return Ok(());
    }
    let kept: Vec<_> = entries
        .into_iter()
        .filter(|e| !seen.contains(&e.txid))
        .collect();
    write_all(db, &kept)
}
