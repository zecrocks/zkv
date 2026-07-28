//! Per-database cross-process advisory lock.
//!
//! Two `zkv` processes operating on the *same* database at once would race on
//! the wallet's SQLite files (`data.sqlite`) and the compact-block cache
//! (`blockmeta.sqlite` + `blocks/`): two concurrent chain scans, or a scan
//! overlapping a spend, can leave those files inconsistent. [`DbLock`]
//! serializes them with an OS advisory lock (`flock(2)` on Unix,
//! `LockFileEx` on Windows, via the `fs4` crate) on a `.lock` file in the
//! database directory.
//!
//! The lock is **released automatically** when the guard is dropped or the
//! process exits, so a crashed or killed process never leaves a stale lock
//! behind; there is nothing to clean up by hand.
//!
//! Within a single process the lock is **reentrant**: a write path syncs and
//! then broadcasts, and each step takes the lock, so the nested acquisitions
//! must share one underlying OS lock (`flock` is not reentrant across separate
//! file handles; a second handle would block on the first and deadlock). A
//! process-global registry keyed by the lock-file path hands back the same
//! held lock for nested acquisitions and only performs a real `flock` for the
//! first one.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use anyhow::Context;
use fs4::TryLockError;

use crate::data::db_dir;

const LOCK_FILE: &str = ".lock";

/// Process-global table of currently-held locks, keyed by the absolute lock
/// file path. Entries are [`Weak`] so a lock evaporates from the table's point
/// of view the moment its last [`DbLock`] guard drops (the next acquisition for
/// that path then re-`flock`s). Bounded by the number of distinct databases a
/// process touches, so dead weak entries left behind are negligible.
fn registry() -> &'static Mutex<HashMap<PathBuf, Weak<LockInner>>> {
    static REG: OnceLock<Mutex<HashMap<PathBuf, Weak<LockInner>>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The held OS lock. Dropping it closes `_file`, which releases the advisory
/// lock.
struct LockInner {
    _file: File,
}

/// An acquired exclusive lock on a database directory. Hold it for the duration
/// of any operation that mutates the wallet DB or block cache (a sync or a
/// broadcast). Release is automatic on drop.
///
/// A guard may also represent "no lock needed" (see [`DbLock::acquire`] when the
/// database directory doesn't exist yet); dropping it is then a no-op.
pub struct DbLock {
    _inner: Option<Arc<LockInner>>,
}

impl DbLock {
    /// Acquire the exclusive lock for `db_name`.
    ///
    /// If another **process** already holds it, this blocks until that process
    /// releases it (emitting a single "waiting…" log line first). Within *this*
    /// process the lock is reentrant: a nested acquisition while a guard is
    /// still alive returns immediately, sharing the same OS lock.
    ///
    /// If the database directory does not exist (e.g. a mistyped `--db`), there
    /// is nothing to protect, so this returns a no-op guard and lets the caller
    /// surface its own "unknown database" error rather than creating an empty
    /// stub directory just to hold a lock file.
    pub fn acquire(db_name: &str) -> anyhow::Result<DbLock> {
        let lock_path = db_dir(db_name)?.join(LOCK_FILE);
        Self::acquire_at(&lock_path, db_name)
    }

    /// Core acquisition keyed by the explicit lock-file path. `label` only
    /// flavors the user-facing wait/error messages (the database name).
    fn acquire_at(lock_path: &Path, label: &str) -> anyhow::Result<DbLock> {
        // No database directory yet → nothing to protect. Don't create a stub
        // just to hold a lock file; let the caller hit its own error.
        match lock_path.parent() {
            Some(parent) if !parent.exists() => return Ok(DbLock { _inner: None }),
            _ => {}
        }

        // Hold the registry mutex across the whole acquisition. The fast
        // (reentrant) path is cheap; the slow path holds it across a possibly
        // blocking `flock`, which serializes first-time acquisitions in this
        // process. That only stalls when another process holds the lock, which
        // is exactly the situation in which serializing is desired; and it
        // is what makes the reentrancy deadlock-free (no second handle on the
        // same path is ever opened while one is being acquired).
        let mut reg = registry().lock().expect("db-lock registry poisoned");

        if let Some(inner) = reg.get(lock_path).and_then(Weak::upgrade) {
            // Reentrant: this process already holds the lock; share it.
            return Ok(DbLock {
                _inner: Some(inner),
            });
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .with_context(|| format!("opening lock file {}", lock_path.display()))?;

        // Fully-qualified `fs4::FileExt` calls: `std::fs::File` grew inherent
        // `try_lock`/`lock` methods in Rust 1.89, but the crate's MSRV is 1.81
        // (where only the `fs4` trait provides them), so disambiguate explicitly
        // to compile deterministically across that range.
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                tracing::warn!(
                    "another zkv process is using database {label:?}; \
                     waiting for it to finish…"
                );
                fs4::FileExt::lock(&file).with_context(|| format!("locking database {label:?}"))?;
            }
            Err(TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("locking database {label:?}"));
            }
        }

        let inner = Arc::new(LockInner { _file: file });
        reg.insert(lock_path.to_path_buf(), Arc::downgrade(&inner));
        Ok(DbLock {
            _inner: Some(inner),
        })
    }

    /// Like [`DbLock::acquire`], but never blocks: if another **process** holds
    /// the lock, returns `Ok(None)` immediately instead of waiting. Reentrant
    /// within this process (a lock already held here is shared and returns
    /// `Some`). Used by the GUI's background auto-sync, which must not stall a
    /// worker thread on a flock another process is holding — it reports the db
    /// as "in use" and retries on the next cycle instead.
    pub fn try_acquire(db_name: &str) -> anyhow::Result<Option<DbLock>> {
        let lock_path = db_dir(db_name)?.join(LOCK_FILE);
        // No database directory yet → nothing to protect (see `acquire_at`).
        match lock_path.parent() {
            Some(parent) if !parent.exists() => return Ok(Some(DbLock { _inner: None })),
            _ => {}
        }
        let mut reg = registry().lock().expect("db-lock registry poisoned");
        if let Some(inner) = reg.get(&lock_path).and_then(Weak::upgrade) {
            // Reentrant: this process already holds the lock; share it.
            return Ok(Some(DbLock {
                _inner: Some(inner),
            }));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("opening lock file {}", lock_path.display()))?;
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => {
                let inner = Arc::new(LockInner { _file: file });
                reg.insert(lock_path.clone(), Arc::downgrade(&inner));
                Ok(Some(DbLock {
                    _inner: Some(inner),
                }))
            }
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => {
                Err(e).with_context(|| format!("locking database {db_name:?}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_lock_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zkv-lock-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A second acquisition in the same process while the first guard is alive
    /// is reentrant: it shares the underlying OS lock instead of deadlocking
    /// on its own `flock`.
    #[test]
    fn reentrant_within_process() {
        let dir = temp_lock_dir("reentrant");
        let path = dir.join(LOCK_FILE);

        let g1 = DbLock::acquire_at(&path, "lockdb").expect("first acquire");
        assert!(g1._inner.is_some());
        // Would block forever on its own flock if it weren't reentrant.
        let g2 = DbLock::acquire_at(&path, "lockdb").expect("reentrant acquire");
        assert!(g2._inner.is_some());
        drop(g2);
        // g1 still alive: the OS lock is still held, registry entry still live.
        assert!(registry()
            .lock()
            .unwrap()
            .get(&path)
            .and_then(Weak::upgrade)
            .is_some());
        drop(g1);

        // After both guards drop, the weak registry entry is dead.
        assert!(registry()
            .lock()
            .unwrap()
            .get(&path)
            .and_then(Weak::upgrade)
            .is_none());
    }

    /// A nonexistent database directory yields a no-op guard and does not
    /// create a stub directory or lock file.
    #[test]
    fn missing_db_is_noop() {
        let parent = temp_lock_dir("missing").join("not-created");
        let path = parent.join(LOCK_FILE);
        assert!(!parent.exists());

        let g = DbLock::acquire_at(&path, "ghost").expect("no-op acquire");
        assert!(g._inner.is_none());
        assert!(!parent.exists(), "must not create a stub directory");
    }
}
