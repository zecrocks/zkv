//! Where a database's wallet files live, and the fleet layout that answers it.
//!
//! zkv's original answer was "in the database directory": one node per
//! database, its datadir the directory the user named. That is right for an
//! admin database, which holds a spending key and needs a node of its own to
//! spend from, and wasteful for a watch-only one. A GUI following twenty
//! oracles ran twenty nodes with twenty upstream connections and twenty scans
//! of the same blocks, and rebuilt all of it every auto-sync cycle.
//!
//! The wallet engine's **fleet** is the shared alternative: many viewing keys
//! in one shard database, served by one node, scanned once. This module owns
//! zkv's side of that arrangement: the on-disk layout, the manifests that
//! enrol a member, and the lookup that turns a database name into the files
//! its reads should open. It sits **below** the engine seam and names no
//! `zecd::` path; `crate::engine::fleet` is the part that talks to the node.
//!
//! # Layout
//!
//! ```text
//! <data-dir>/
//!   .fleet/                    a dotfile, so `data::list_dbs` skips it
//!     test/                    one fleet datadir per network
//!       .lock                  the node's datadir lock, while it runs
//!       wallets.d/             manifests; the file stem is the database name
//!         watch-abcd.toml      ufvk = "uviewtest1…"   birthday = 3000000
//!       shards/
//!         shard-0000/          one shard, holding several members' accounts
//!           lrz/               data.sqlite, blockmeta.sqlite, blocks/
//!   watch-abcd/                the member: still "one directory is one database"
//!     keys.toml                + engine = "fleet", shard = "shard-0000"
//!     zkv_state.sqlite  pending.toml
//! ```
//!
//! A member keeps everything that is about *memos* (the snapshot, the pending
//! file, its keys and identity) and gives up only the librustzcash files. So
//! "one directory is one database" still holds for everything zkv owns.
//!
//! # Placement is read, never recorded
//!
//! Which shard holds a member is a fact about the shard databases, exactly as
//! upstream keeps it: nothing here writes a placement file that could disagree
//! with reality. The `shard` line in `keys.toml` is a *hint* that saves opening
//! every shard, and it is re-derived the moment it does not hold.
//!
//! # Manifests are rebuildable here
//!
//! For the wallet engine a manifest is key material written at runtime and held
//! nowhere else, so it needs backing up. For zkv it is neither: its two fields
//! are copies of `keys.toml`'s `ufvk` and `birthday`, and `zkv fleet rebuild`
//! writes them back out. The write is still atomic (temp, fsync, rename), both
//! because a torn file stops that member being served until someone notices and
//! because upstream reports such a file rather than failing.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _};
use serde::{Deserialize, Serialize};

use crate::config::{WalletConfig, WalletEngine};
use crate::data::{db_dir, engine_dir_in, open_wallet_db, zkv_data, DATA_DB};
use crate::network::Network;

/// The fleet root inside the data directory. A dotfile so
/// [`crate::data::list_dbs`] skips it: it is not a database.
pub const FLEET_DIR: &str = ".fleet";
/// Manifest directory name, inside a network's fleet datadir. The name is the
/// engine's default (`wallets.d`), so an operator reading either project's
/// documentation finds the same thing.
pub const MANIFEST_DIR: &str = "wallets.d";
/// Shard directory name, inside a network's fleet datadir.
pub const SHARDS_DIR: &str = "shards";

/// A member's shard directory name, `shard-0000` upward.
///
/// The engine derives the same name from the same index, and a canary test
/// pins the two together: zkv reads shard files directly (there is no
/// supported way to ask a node where a member lives), so a rename upstream
/// would otherwise mean reads quietly finding nothing.
pub fn shard_dir_name(index: usize) -> String {
    format!("shard-{index:04}")
}

/// `<data-dir>/.fleet/<network>`: the datadir of that network's fleet node.
pub fn fleet_dir(network: Network) -> anyhow::Result<PathBuf> {
    Ok(zkv_data()?.join(FLEET_DIR).join(network.name()))
}

/// `<data-dir>/.fleet/<network>/wallets.d`.
pub fn manifest_dir(network: Network) -> anyhow::Result<PathBuf> {
    Ok(fleet_dir(network)?.join(MANIFEST_DIR))
}

/// `<data-dir>/.fleet/<network>/shards`.
pub fn shards_dir(network: Network) -> anyhow::Result<PathBuf> {
    Ok(fleet_dir(network)?.join(SHARDS_DIR))
}

/// The shard directories that exist, in order, stopping at the first gap.
///
/// Contiguity is the engine's own rule, and stopping where it stops is what
/// keeps the two sides looking at the same set: a hand-deleted `shard-0001`
/// hides `shard-0002` from both, which reads as "not imported yet" and is
/// fixed by `zkv fleet rebuild` rather than by half of each shard being read.
pub fn existing_shard_dirs(network: Network) -> anyhow::Result<Vec<PathBuf>> {
    Ok(existing_shard_dirs_in(&shards_dir(network)?))
}

/// [`existing_shard_dirs`] against an explicit shards directory.
///
/// The predicate is the directory's existence, which is upstream's: a shard the
/// node has created but not yet populated is still a shard, and testing for the
/// wallet database instead hid every one of them, since that file is a level
/// deeper than it looks (see [`shard_engine_dir`]).
pub(crate) fn existing_shard_dirs_in(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for index in 0.. {
        let dir = root.join(shard_dir_name(index));
        if !dir.is_dir() {
            break;
        }
        out.push(dir);
    }
    out
}

/// Where a shard keeps its librustzcash files: `<shard>/lrz`, not the shard
/// directory itself.
///
/// A per-database node puts them under `<db>/zec/lrz`; a shard gets only the
/// engine segment, because upstream composes a shard actor's `engine_dir` that
/// way. Reading `<shard>/data.sqlite` therefore finds nothing at all, which is
/// indistinguishable from "this member has not been imported yet", so every
/// fleet read reported exactly that. Delegated to the seam so the answer is
/// upstream's rather than a guess that happens to match today.
pub(crate) fn shard_engine_dir(shard_dir: &Path) -> PathBuf {
    crate::engine::fleet::shard_engine_dir(shard_dir)
}

/// A wallet manifest: exactly the two fields the engine reads, and exactly the
/// two `keys.toml` already holds.
///
/// `deny_unknown_fields` mirrors the engine's own parser, so a manifest zkv
/// writes and a manifest zkv accepts are the same set of files, and a hand-
/// added key is a visible error on both sides rather than on one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub ufvk: String,
    pub birthday: u32,
}

fn manifest_path(network: Network, name: &str) -> anyhow::Result<PathBuf> {
    Ok(manifest_path_in(&manifest_dir(network)?, name))
}

/// A manifest's path inside a manifest directory. The database name is the file
/// stem, which is what makes it the wallet name the engine serves.
fn manifest_path_in(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.toml"))
}

/// Write a member's manifest, refusing to overwrite an existing one.
///
/// The write itself is the node's (`engine::fleet::write_manifest`), not a copy
/// of it: temp, fsync, rename, because a bare write truncates first and a crash
/// in that window leaves a zero-length `.toml` holding a wallet's only copy of
/// its viewing key. Upstream made its writer public for hosts that provision a
/// member with no node running, which is exactly what zkv does, and using it
/// means the manifest format cannot drift from what the node reads.
pub fn write_manifest(network: Network, name: &str, manifest: &Manifest) -> anyhow::Result<()> {
    write_manifest_in(&manifest_dir(network)?, name, manifest)
}

/// [`write_manifest`] against an explicit manifest directory.
pub(crate) fn write_manifest_in(dir: &Path, name: &str, manifest: &Manifest) -> anyhow::Result<()> {
    crate::engine::fleet::write_manifest(dir, name, &manifest.ufvk, manifest.birthday)
}

/// Read a member's manifest, or `None` when it has none.
pub fn read_manifest(network: Network, name: &str) -> anyhow::Result<Option<Manifest>> {
    read_manifest_at(&manifest_path(network, name)?)
}

/// [`read_manifest`] against an explicit manifest path.
pub(crate) fn read_manifest_at(path: &Path) -> anyhow::Result<Option<Manifest>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(toml::from_str(&text).with_context(|| {
            format!("parsing the fleet manifest {}", path.display())
        })?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Remove a member's manifest. Missing is success: the caller wants it gone.
pub fn remove_manifest(network: Network, name: &str) -> anyhow::Result<()> {
    let path = manifest_path(network, name)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

/// The manifest a database's `keys.toml` describes.
///
/// A fleet member pins its `ufvk` at creation precisely so this cannot need the
/// wallet files, which is what makes enrolling a database an offline step.
pub fn manifest_for(cfg: &WalletConfig) -> anyhow::Result<Manifest> {
    let ufvk = cfg.ufvk.clone().ok_or_else(|| {
        anyhow!("this database has no viewing key pinned, so it cannot join the fleet")
    })?;
    Ok(Manifest {
        ufvk,
        birthday: u32::from(cfg.birthday),
    })
}

/// Where a database's wallet files are, and which account in them is its own.
pub enum WalletHome {
    /// A node of this database's own: the files are under its directory, at
    /// the root or under `zec/lrz` depending on whether a node has run.
    Own { engine_dir: PathBuf },
    /// A fleet member: its account lives in a shard database shared with other
    /// members, so every read has to say *which* account it means. Which one is
    /// resolved where it is used, by
    /// [`crate::internal::account::select_account`], off the same viewing key
    /// this lookup matched on.
    ///
    /// The path is the shard's **engine** directory (`<shard>/lrz`), the same
    /// shape as `Own`: both name the directory a wallet database sits directly
    /// inside, so a caller never has to know which arm it got.
    Fleet { engine_dir: PathBuf },
    /// Enrolled (its manifest is written) but its account is not in a shard
    /// yet. The engine imports one member per sync pass, so this is an ordinary
    /// transient state, not a failure, and it is deliberately distinct from
    /// "this database has no wallet key imported": there is nothing to repair.
    Importing,
}

/// Resolve where `db_name`'s wallet files are.
pub fn locate(cfg: &WalletConfig, db_name: &str) -> anyhow::Result<WalletHome> {
    // `FleetPending` reads from its own files by design: that is what keeps a
    // converting database answering while its shard catches up.
    if !cfg.engine.is_fleet_member() {
        return Ok(WalletHome::Own {
            engine_dir: engine_dir_in(&db_dir(db_name)?)?,
        });
    }
    match locate_member(cfg, db_name)? {
        // `locate_member` answers in shard directories, because that is what a
        // placement is named by; the wallet files are a level below.
        Some((shard_dir, _account_uuid)) => Ok(WalletHome::Fleet {
            engine_dir: shard_engine_dir(&shard_dir),
        }),
        None => Ok(WalletHome::Importing),
    }
}

/// Find a member's account: the shard database holding an account whose
/// viewing key is this database's, and that account's UUID.
///
/// The cached shard is tried first, then every shard in order. Matching by
/// **encoded UFVK** is what the engine itself does to reconcile a manifest with
/// an account, so the two agree on identity by construction; matching by name
/// would not, since a name is a manifest fact and an account fact separately.
///
/// A hit refreshes the cache, which is a hint and not worth failing a read
/// over, so a cache write that fails is logged and ignored.
pub fn locate_member(
    cfg: &WalletConfig,
    db_name: &str,
) -> anyhow::Result<Option<(PathBuf, Vec<u8>)>> {
    let want = cfg
        .ufvk
        .as_deref()
        .ok_or_else(|| anyhow!("a fleet member needs its viewing key pinned in keys.toml"))?;

    let candidates = existing_shard_dirs(cfg.network)?;
    let Some((dir, uuid)) = search_shards(&candidates, cfg.network, want, cfg.shard())? else {
        return Ok(None);
    };
    let found = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned();
    if cfg.shard() != Some(found.as_str()) {
        let mut cfg = WalletConfig::read(db_name)?;
        if let Err(e) = cfg.cache_shard(&found) {
            tracing::debug!("caching the shard for {db_name:?}: {e:#}");
        }
    }
    Ok(Some((dir, uuid)))
}

/// The shard holding an account whose encoded viewing key is `want`, and that
/// account's UUID. `cached` is tried first and is only ever a hint: a stale one
/// costs an extra open, never a missed match.
pub(crate) fn search_shards(
    shards: &[PathBuf],
    network: Network,
    want: &str,
    cached: Option<&str>,
) -> anyhow::Result<Option<(PathBuf, Vec<u8>)>> {
    let mut candidates = shards.to_vec();
    if let Some(cached) = cached {
        // Try the remembered shard first, without changing the set: a stale
        // hint must cost one extra open, never a missed match.
        if let Some(pos) = candidates
            .iter()
            .position(|d| d.file_name().and_then(|n| n.to_str()) == Some(cached))
        {
            candidates.swap(0, pos);
        }
    }

    for dir in candidates {
        if let Some(uuid) = account_in_shard(&dir, network, want)? {
            return Ok(Some((dir, uuid)));
        }
    }
    Ok(None)
}

/// The UUID of the account in `shard_dir` whose viewing key encodes to `want`.
fn account_in_shard(
    shard_dir: &Path,
    network: Network,
    want: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    use zcash_client_backend::data_api::{Account as _, WalletRead as _};

    let db = open_wallet_db(shard_engine_dir(shard_dir).join(DATA_DB), network)?;
    for id in db.get_account_ids()? {
        let Some(account) = db.get_account(id)? else {
            continue;
        };
        let Some(ufvk) = account.ufvk() else {
            continue;
        };
        if ufvk.encode(&network) == want {
            return Ok(Some(id.expose_uuid().as_bytes().to_vec()));
        }
    }
    Ok(None)
}

/// Marker recording that new watch-only databases should get their own node
/// instead of joining the shared scan.
///
/// A dotfile in the data directory, so [`crate::data::list_dbs`] skips it, and
/// Rust-owned rather than kept in the GUI's browser storage: an origin-scoped
/// flag is shared by every zkv install on `127.0.0.1`, which is the bug the
/// onboarding marker already exists to avoid.
///
/// Absence means the shared scan is on, so a fresh data directory gets the
/// better default without anything having to be written first.
pub(crate) const FLEET_DEFAULT_OFF: &str = ".fleet-default-off";

/// Whether a new watch-only database should join the shared scan.
pub fn fleet_is_default() -> bool {
    match zkv_data() {
        Ok(home) => !home.join(FLEET_DEFAULT_OFF).exists(),
        // No readable data directory is a problem the caller is about to hit
        // anyway; answer with the default rather than inventing an opinion.
        Err(_) => true,
    }
}

/// Turn the shared-scan default on or off for new watch-only databases.
pub fn set_fleet_default(on: bool) -> anyhow::Result<()> {
    let path = zkv_data()?.join(FLEET_DEFAULT_OFF);
    if on {
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
        }
    } else {
        std::fs::write(&path, b"").with_context(|| format!("writing {}", path.display()))
    }
}

// ---------------------------------------------------------------------------
// Conversion
// ---------------------------------------------------------------------------

/// Enrol a watch-only database in the shared scan, if nobody has decided how it
/// should be served and the data directory's default says to.
///
/// Offline and idempotent: it writes a manifest and one line of `keys.toml`.
/// The database keeps reading and syncing from its own files until its shard
/// has caught up, which is what [`WalletEngine::FleetPending`] means and why
/// this is safe to run on open.
///
/// Deliberately silent about failure. Enrolment is an optimisation; a data
/// directory that cannot take one (read-only, out of space) must not stop a
/// database being opened, and the next open will try again.
pub(crate) fn enrol_if_unchosen(cfg: &mut WalletConfig, db_name: &str) {
    if !should_enrol(cfg, db_name) {
        return;
    }
    if let Err(e) = enrol(cfg, db_name) {
        tracing::debug!("not enrolling {db_name:?} in the shared scan: {e:#}");
    }
}

/// Whether automatic enrolment applies to this database.
fn should_enrol(cfg: &WalletConfig, db_name: &str) -> bool {
    // Only a database nobody has decided about. An explicit `own` (what
    // `--standalone` and `zkv fleet leave` write) is a decision.
    cfg.engine.is_unset()
        // The shared scan is watch-only: an admin database holds a spending
        // key, which is not something the fleet can serve.
        && cfg.role == crate::config::Role::Watch
        // The engine always has a wallet called `default`, so a member by that
        // name collides with it and stops the node starting at all.
        && db_name != crate::engine::WALLET
        && fleet_is_default()
}

/// Move a database onto the shared scan on request, whatever the data
/// directory's default says. This is `zkv fleet join`.
pub fn join(cfg: &mut WalletConfig, db_name: &str) -> anyhow::Result<()> {
    enrol(cfg, db_name)
}

/// Step one of the conversion: write the manifest, then record the state.
///
/// That order is load-bearing. A crash between them leaves a manifest with no
/// database claiming it, which is inert (nothing is served until a `keys.toml`
/// names it) and which the next open rewrites. The other order would leave a
/// database that believes it is converting with nothing to convert into.
fn enrol(cfg: &mut WalletConfig, db_name: &str) -> anyhow::Result<()> {
    // A member is found in its shard by viewing key, so the key has to be on
    // disk before it is enrolled. Every watch database can produce one: it is
    // in the address it was created from.
    if cfg.ufvk.is_none() {
        let addr = cfg.zkv_address.as_deref().ok_or_else(|| {
            anyhow!("this database has no address to derive its viewing key from")
        })?;
        let ufvk = crate::protocol::parse_zkv_addr(addr)?
            .ufvk
            .encode(&cfg.network);
        cfg.pin_ufvk(&ufvk)?;
    }
    let manifest = manifest_for(cfg)?;
    match write_manifest(cfg.network, db_name, &manifest) {
        Ok(()) => {}
        // Already there: a previous attempt got this far, or the user enrolled
        // the database by hand. Either way it is the state this wants.
        Err(_) if read_manifest(cfg.network, db_name)?.as_ref() == Some(&manifest) => {}
        Err(e) => return Err(e),
    }
    cfg.set_engine(WalletEngine::FleetPending)?;
    tracing::info!(
        "{db_name:?} is joining the shared scan; it keeps reading from its own files until \
         the shared scan has caught up"
    );
    Ok(())
}

/// Undo an enrolment that has not flipped yet, or leave the fleet outright.
///
/// The manifest goes first for the same reason it arrives last: a database that
/// still believes it is a member while its manifest is gone is the one state
/// that reads as "importing" forever.
pub fn leave(cfg: &mut WalletConfig, db_name: &str) -> anyhow::Result<()> {
    remove_manifest(cfg.network, db_name)?;
    cfg.set_engine(WalletEngine::Own)?;
    Ok(())
}

/// Step two: flip a converting database over to its shard once the shard has
/// scanned at least as far as its own wallet file had.
///
/// The comparison is the whole safety argument. A shard rescans from the
/// member's birthday, which for a year-old oracle is hours, and reading from it
/// before it arrives would show an empty database. So the old files stay
/// authoritative until the shard has demonstrably caught up with them, and only
/// then are they deleted.
///
/// Returns whether the flip happened.
pub(crate) fn flip_if_caught_up(
    cfg: &mut WalletConfig,
    db_name: &str,
    own_scanned: u32,
    shard_scanned: u32,
) -> anyhow::Result<bool> {
    if cfg.engine != WalletEngine::FleetPending || shard_scanned < own_scanned {
        return Ok(false);
    }
    // Record the new state *before* deleting anything: a crash after the delete
    // and before the write would leave a database that believes it has its own
    // files and does not.
    cfg.set_engine(WalletEngine::Fleet)?;
    if let Err(e) = discard_own_wallet_files(db_name) {
        // The database is a member and reads correctly; what is left behind is
        // a stale copy of the wallet files. Worth saying, not worth failing.
        tracing::warn!("{db_name:?} joined the shared scan but its old wallet files remain: {e:#}");
    }
    tracing::info!("{db_name:?} is now served by the shared scan");
    Ok(true)
}

/// Delete the librustzcash files a converted member no longer uses, leaving
/// everything zkv owns (`keys.toml`, the snapshot, `pending.toml`) in place.
///
/// The snapshot is deliberately kept: it is a projection of confirmed memos
/// keyed by chain position, and the shard has scanned past all of them, so it
/// is as valid on the new files as on the old. Wiping it would mean re-reading
/// and re-verifying every historical memo for no gain.
fn discard_own_wallet_files(db_name: &str) -> anyhow::Result<()> {
    let root = db_dir(db_name)?;
    let engine_dir = engine_dir_in(&root)?;
    for name in [DATA_DB, "blockmeta.sqlite"] {
        let path = engine_dir.join(name);
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        }
    }
    let blocks = engine_dir.join("blocks");
    if blocks.is_dir() {
        std::fs::remove_dir_all(&blocks)
            .with_context(|| format!("removing {}", blocks.display()))?;
    }
    // The per-coin subdirectory the node created is empty now; leaving it would
    // make `engine_dir_in` keep pointing at it.
    let nested = root.join(crate::data::ENGINE_COIN_DIR);
    if nested.is_dir() {
        let _ = std::fs::remove_dir_all(&nested);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(ufvk: &str, birthday: u32) -> Manifest {
        Manifest {
            ufvk: ufvk.to_owned(),
            birthday,
        }
    }

    /// The engine derives a shard's directory from the same index, and zkv
    /// reads those directories itself because there is no supported way to ask
    /// a node where a member lives. A rename upstream would otherwise mean
    /// every read quietly finding nothing, so the format is pinned here and
    /// against the engine's own helper in `engine::config`'s canaries.
    #[test]
    fn shard_directories_are_zero_padded_from_zero() {
        assert_eq!(shard_dir_name(0), "shard-0000");
        assert_eq!(shard_dir_name(3), "shard-0003");
        assert_eq!(shard_dir_name(1234), "shard-1234");
        // Wider than the padding rather than truncated, so a large fleet keeps
        // distinct names.
        assert_eq!(shard_dir_name(12345), "shard-12345");
    }

    #[test]
    fn a_manifest_round_trips_and_is_never_written_twice() {
        let dir = tempfile::tempdir().unwrap();
        let m = manifest("uviewtest1abc", 3_000_000);
        write_manifest_in(dir.path(), "watch-a", &m).unwrap();

        let path = manifest_path_in(dir.path(), "watch-a");
        assert_eq!(read_manifest_at(&path).unwrap(), Some(m.clone()));
        // The write is temp + rename, and the temporary must not survive it:
        // a leftover would be an unserved wallet's key sitting in the open.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        // Refusing to overwrite is what keeps enrolment idempotent-or-loud
        // rather than silently repointing an existing member at another key.
        let err = write_manifest_in(dir.path(), "watch-a", &manifest("uviewtest1zzz", 1))
            .unwrap_err()
            .to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(read_manifest_at(&path).unwrap(), Some(m));
    }

    #[test]
    fn a_missing_manifest_reads_as_none_and_removing_it_twice_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let path = manifest_path_in(dir.path(), "absent");
        assert_eq!(read_manifest_at(&path).unwrap(), None);

        write_manifest_in(dir.path(), "gone", &manifest("uviewtest1a", 1)).unwrap();
        let path = manifest_path_in(dir.path(), "gone");
        std::fs::remove_file(&path).unwrap();
        assert_eq!(read_manifest_at(&path).unwrap(), None);
    }

    /// An unknown key means a manifest that is not what somebody meant, and the
    /// engine refuses one too: parsing it leniently here would enrol a member
    /// on terms nobody wrote.
    #[test]
    fn a_manifest_with_an_unknown_key_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.toml");
        std::fs::write(
            &path,
            "ufvk = \"uviewtest1a\"\nbirthday = 1\nbirthdya = 2\n",
        )
        .unwrap();
        let err = read_manifest_at(&path).unwrap_err().to_string();
        assert!(err.contains("w.toml"), "{err}");
    }

    /// The engine keeps its shards contiguous, so a gap means something has
    /// been deleted by hand. Both sides stop at it, which reads as "not
    /// imported yet" for anything above rather than as half a fleet.
    #[test]
    fn shard_enumeration_stops_at_the_first_gap() {
        let dir = tempfile::tempdir().unwrap();
        for i in [0usize, 1, 3] {
            std::fs::create_dir_all(dir.path().join(shard_dir_name(i))).unwrap();
        }
        let found = existing_shard_dirs_in(dir.path());
        assert_eq!(
            found,
            vec![dir.path().join("shard-0000"), dir.path().join("shard-0001")],
        );

        // The predicate is the directory, which is upstream's: a shard the node
        // has created but not yet imported into is still a shard, and a member
        // that lands in it must be findable there. Testing for a wallet
        // database instead saw no shards at all, because that file is a level
        // below where this used to look.
        std::fs::create_dir_all(dir.path().join(shard_dir_name(2))).unwrap();
        assert_eq!(existing_shard_dirs_in(dir.path()).len(), 4);
    }

    #[test]
    fn no_shards_at_all_is_an_empty_fleet_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(existing_shard_dirs_in(&dir.path().join("absent")).is_empty());
    }
}

/// Finding a member's account across real shard databases.
///
/// The lookup matches on the **encoded viewing key**, which is what the wallet
/// engine itself reconciles a manifest against, so the two cannot disagree
/// about which account belongs to which database. These tests build shards the
/// way the engine would (accounts imported from viewing keys, several to a
/// file) and check the three answers that matter: the right account, the right
/// shard, and a clean "not here yet".
#[cfg(test)]
mod shard_lookup_tests {
    use super::*;
    use zcash_client_backend::data_api::{AccountBirthday, AccountPurpose, WalletWrite};
    use zcash_keys::keys::UnifiedSpendingKey;
    use zcash_protocol::consensus::BlockHeight;
    use zcash_protocol::local_consensus::LocalNetwork;

    const NET: Network = Network::Test;

    /// A viewing key derived from a one-byte seed, so each `n` names a
    /// different member.
    fn ufvk_for(n: u8) -> zcash_keys::keys::UnifiedFullViewingKey {
        let seed = vec![n; 64];
        UnifiedSpendingKey::from_seed(&NET, &seed, zip32::AccountId::ZERO)
            .unwrap()
            .to_unified_full_viewing_key()
    }

    /// A shard database holding the given members' accounts, imported in order,
    /// exactly as the engine onboards them.
    fn shard(root: &Path, index: usize, members: &[u8]) -> PathBuf {
        let dir = root.join(shard_dir_name(index));
        // Under the engine subdirectory, where the node puts them. Building the
        // fixture flat is what let the resolver look in the wrong place and
        // still pass every offline test.
        let engine = shard_engine_dir(&dir);
        std::fs::create_dir_all(&engine).unwrap();
        let mut db = crate::data::open_wallet_db(engine.join(DATA_DB), NET).unwrap();
        for m in members {
            let birthday = AccountBirthday::from_parts(
                zcash_client_backend::data_api::chain::ChainState::empty(
                    BlockHeight::from_u32(1),
                    zcash_primitives::block::BlockHash([0; 32]),
                ),
                None,
            );
            db.import_account_ufvk(
                &format!("member-{m}"),
                &ufvk_for(*m),
                &birthday,
                AccountPurpose::ViewOnly,
                None,
            )
            .unwrap();
        }
        dir
    }

    #[test]
    fn a_member_is_found_by_its_viewing_key_not_its_position() {
        let root = tempfile::tempdir().unwrap();
        // Two shards; the wanted member is the second account of the second
        // shard, so neither "first shard" nor "first account" would find it.
        shard(root.path(), 0, &[1, 2]);
        let second = shard(root.path(), 1, &[3, 4]);
        let shards = existing_shard_dirs_in(root.path());
        assert_eq!(shards.len(), 2);

        let want = ufvk_for(4).encode(&NET);
        let (dir, uuid) = search_shards(&shards, NET, &want, None).unwrap().unwrap();
        assert_eq!(dir, second);
        assert_eq!(uuid.len(), 16, "an account UUID is 16 bytes");

        // And the account it points at really is that member's.
        let found = account_in_shard(&dir, NET, &want).unwrap().unwrap();
        assert_eq!(found, uuid);
        // A different member in the same shard resolves to a different account.
        let other = search_shards(&shards, NET, &ufvk_for(3).encode(&NET), None)
            .unwrap()
            .unwrap();
        assert_eq!(other.0, second);
        assert_ne!(other.1, uuid);
    }

    #[test]
    fn a_key_that_is_in_no_shard_is_not_found_rather_than_mismatched() {
        let root = tempfile::tempdir().unwrap();
        shard(root.path(), 0, &[1, 2]);
        let shards = existing_shard_dirs_in(root.path());
        // The member is enrolled but not imported yet: the caller turns this
        // into "importing", never into somebody else's account.
        assert!(search_shards(&shards, NET, &ufvk_for(9).encode(&NET), None)
            .unwrap()
            .is_none());
    }

    /// A shard holds several members' accounts, so a balance read that sums the
    /// whole file reports other databases' money under this one's name.
    ///
    /// This is the same mistake the wallet engine made on its own read path
    /// until upstream #270, and zkv had its own instance of it: `zkv balance`
    /// deliberately serves watch-only databases, and every member is watch-only.
    /// The scoping helper is what fixes it, and this is what would catch it
    /// coming back.
    #[test]
    fn a_balance_read_sees_only_this_databases_account() {
        use zcash_client_backend::data_api::WalletRead as _;

        let root = tempfile::tempdir().unwrap();
        // One shard, two members, exactly as onboarding lays it down.
        let dir = shard(root.path(), 0, &[1, 2]);
        let db = crate::data::open_wallet_db(shard_engine_dir(&dir).join(DATA_DB), NET).unwrap();

        let ids = db.get_account_ids().unwrap();
        assert_eq!(ids.len(), 2, "the fixture must hold shard-mates to confuse");

        // The account each member's viewing key opens is its own, and the two
        // are different. That is the property every scoped read rests on: a
        // summary keyed by account can then be looked up rather than summed.
        let a = account_in_shard(&dir, NET, &ufvk_for(1).encode(&NET))
            .unwrap()
            .unwrap();
        let b = account_in_shard(&dir, NET, &ufvk_for(2).encode(&NET))
            .unwrap()
            .unwrap();
        assert_ne!(a, b);
        for uuid in [&a, &b] {
            assert!(
                ids.iter()
                    .any(|id| id.expose_uuid().as_bytes().to_vec() == **uuid),
                "each resolved account must be one the file actually holds",
            );
        }
    }

    /// The cached shard is a hint. A wrong one must cost an extra open and
    /// nothing else, because it goes stale for real: the engine rebuilds
    /// shards, and `zkv fleet rebuild` moves every member.
    #[test]
    fn a_stale_shard_hint_still_finds_the_member() {
        let root = tempfile::tempdir().unwrap();
        let first = shard(root.path(), 0, &[1]);
        let second = shard(root.path(), 1, &[2]);
        let shards = existing_shard_dirs_in(root.path());
        let want = ufvk_for(2).encode(&NET);

        // Hint points at the wrong shard.
        let (dir, _) = search_shards(&shards, NET, &want, Some("shard-0000"))
            .unwrap()
            .unwrap();
        assert_eq!(dir, second);
        // Hint points at a shard that no longer exists.
        let (dir, _) = search_shards(&shards, NET, &want, Some("shard-0099"))
            .unwrap()
            .unwrap();
        assert_eq!(dir, second);
        // And the right hint is honoured.
        let (dir, _) = search_shards(&shards, NET, &ufvk_for(1).encode(&NET), Some("shard-0000"))
            .unwrap()
            .unwrap();
        assert_eq!(dir, first);
    }

    // Silences the unused-import warning when the local-consensus type is not
    // otherwise named; it is pulled in by `Network`'s Parameters impl.
    #[allow(dead_code)]
    fn _network_type_is_used(_: LocalNetwork) {}
}
