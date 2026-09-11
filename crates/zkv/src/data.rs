//! State layout: `~/.zkv/<dbname>/`{keys.toml, security-theater-key, data.sqlite,
//! blockmeta.sqlite, blocks/, zkv_state.sqlite}. (`security-theater-key` is the
//! age identity wrapping the seed; older databases name it `.id`, migrated on
//! first access. See `config.rs`.)
//!
//! `zkv_state.sqlite` is a sidecar holding the materialized KV projection for memos that are
//! deep enough on chain to be reorg-safe; see `internal::snapshot` for the schema and the
//! tail-replay model that sits on top of it.
//!
//! A "database" is a single Zcash wallet (admin or watch-only). One `current` marker file at
//! the root of `$ZKV_DATA` records the active database, so most commands take no `--db` flag.
//!
//! Precedence for the data directory: `--data-dir` (set by the global CLI flag at startup) >
//! `$ZKV_DATA` env var > the per-OS default (`$HOME/.zkv` on Linux,
//! `$HOME/Library/Application Support/zkv` on macOS, `%APPDATA%\zkv` on Windows).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// The OS RNG is fallible in `rand_core 0.10` (`SysRng: TryRng`) while `WalletDb`
// wants an infallible `Rng`; `UnwrapErr` is the adapter the wallet stack itself
// uses for that (it is the RNG in zecd's own `WriteDb` alias).
use rand::rand_core::UnwrapErr;
use rand::rngs::SysRng;
use tracing::error;
use zcash_client_sqlite::{
    chain::init::init_blockmeta_db, util::SystemClock, wallet::init::init_wallet_db, FsBlockDb,
    WalletDb,
};
use zcash_protocol::consensus::Parameters;

use crate::error;

/// The compact-block cache directory inside the engine directory.
pub(crate) const BLOCKS_FOLDER: &str = "blocks";
/// The wallet database's filename inside a database directory.
pub(crate) const DATA_DB: &str = "data.sqlite";
/// The per-coin and per-storage-engine subdirectories the wallet engine nests
/// librustzcash's files under: `<db>/zec/lrz/`.
///
/// These mirror zecd's `Coin::data_dir` / `Coin::engine_dir`, which are frozen
/// upstream and pinned by a test there. They are spelled out rather than read
/// from zecd because this module sits below the engine seam and has to resolve
/// paths on builds where the engine is absent (see `crate::engine`).
pub(crate) const ENGINE_COIN_DIR: &str = "zec";
pub(crate) const ENGINE_STORAGE_DIR: &str = "lrz";
pub(crate) const ZKV_STATE_DB: &str = "zkv_state.sqlite";
/// The advisory-lock file inside a database directory.
///
/// zkv no longer takes this lock itself: the wallet engine's node holds it for
/// as long as it runs, and every zkv path that touches the wallet database
/// mutably now goes through a node, so the node's lock is what serializes two
/// zkv processes on one database. The name is kept here, rather than left
/// implicit inside zecd, because `crate::engine::config` has a test asserting
/// the two agree: if upstream ever renamed it, two zkv processes would stop
/// excluding each other with nothing to notice. So it is read only by that
/// test, which is what the allow is for.
#[allow(dead_code)]
pub(crate) const LOCK_FILE: &str = ".lock";
const CURRENT_MARKER: &str = "current";
/// Marker file (at the data-dir root) recording that the user has gone through
/// (or dismissed) the GUI's first-run onboarding. State lives in the data dir,
/// not the browser: localStorage is per-origin (every zkv install shares
/// `http://127.0.0.1:<port>`), so a browser-side flag suppressed onboarding
/// across unrelated installs and data-dir resets. The leading dot keeps it out
/// of [`list_dbs`] (which skips dotfiles and non-directories).
const ONBOARDED_MARKER: &str = ".onboarded";

// The network enum lives in `crate::network` (it implements `Parameters`, so
// regtest fits the whole generic wallet stack); re-exported here, its
// historical home, so `data::Network` paths keep working.
pub use crate::network::Network;

/// Process-wide override for the data directory, set once at startup by `main` from
/// the global `--data-dir` flag. Higher precedence than `$ZKV_DATA`.
static DATA_DIR_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Install the override from the CLI's `--data-dir`. Idempotent: silently ignored
/// if called twice (the harness only calls it once, at the top of `main`).
pub fn set_data_dir_override(p: PathBuf) {
    let _ = DATA_DIR_OVERRIDE.set(p);
}

/// Returns the data directory, creating it if missing. Resolution order:
/// 1. `--data-dir <path>` global CLI flag (installed via `set_data_dir_override`)
/// 2. `$ZKV_DATA` environment variable
/// 3. the per-OS default (see `default_data_dir`): `$HOME/.zkv` on Linux,
///    `$HOME/Library/Application Support/zkv` on macOS, `%APPDATA%\zkv` on Windows.
pub fn zkv_data() -> anyhow::Result<PathBuf> {
    let path = if let Some(p) = DATA_DIR_OVERRIDE.get() {
        p.clone()
    } else if let Ok(p) = std::env::var("ZKV_DATA") {
        PathBuf::from(p)
    } else {
        default_data_dir()?
    };
    // Owner-only on Unix so the whole data dir (and the per-database secrets
    // under it) isn't world-readable. An existing dir is left as-is.
    create_private_dir(&path)?;
    Ok(path)
}

/// The resolved data directory, formatted for display in the GUI/CLI. On Unix a
/// `$HOME` prefix is collapsed to `~` (so `/home/alice/.zkv` shows as `~/.zkv`);
/// on Windows the full path is shown verbatim, since `~` is not a Windows
/// convention. Resolves (and creates) the dir via [`zkv_data`].
pub fn data_dir_display() -> anyhow::Result<String> {
    Ok(display_data_dir_from(
        &zkv_data()?,
        std::env::var_os("HOME"),
        cfg!(windows),
    ))
}

/// Pure formatter behind [`data_dir_display`], split out so the per-OS logic is
/// testable from any host. An empty `$HOME` is treated as unset.
fn display_data_dir_from(dir: &Path, home: Option<OsString>, windows: bool) -> String {
    if !windows {
        if let Some(home) = home.filter(|h| !h.is_empty()) {
            if let Ok(rest) = dir.strip_prefix(PathBuf::from(home)) {
                return if rest.as_os_str().is_empty() {
                    "~".to_owned()
                } else {
                    format!("~/{}", rest.display())
                };
            }
        }
    }
    dir.display().to_string()
}

/// The per-OS default data directory, used when neither `--data-dir` nor
/// `$ZKV_DATA` is set:
///
/// * **Windows:** `%APPDATA%\zkv`: the per-user Roaming application-data
///   directory (e.g. `C:\Users\Alice\AppData\Roaming\zkv`), falling back to
///   `%USERPROFILE%\.zkv` only if `%APPDATA%` is somehow unset.
/// * **macOS:** `$HOME/Library/Application Support/zkv`, the conventional
///   per-user application-support location.
/// * **Linux (and other Unix):** `$HOME/.zkv`.
///
/// All branches are always compiled (the live function feeds the real
/// `cfg!(windows)` / `cfg!(target_os = "macos")` and environment into
/// [`default_data_dir_from`]), so every platform path is type-checked and
/// unit-tested on every host, not just on its native OS.
fn default_data_dir() -> anyhow::Result<PathBuf> {
    default_data_dir_from(
        cfg!(windows),
        cfg!(target_os = "macos"),
        std::env::var_os("APPDATA"),
        std::env::var_os("USERPROFILE"),
        std::env::var_os("HOME"),
    )
}

/// Pure resolver behind `default_data_dir`, split out so the per-OS logic is
/// testable from any host. An empty environment variable is treated as unset.
fn default_data_dir_from(
    windows: bool,
    macos: bool,
    appdata: Option<OsString>,
    userprofile: Option<OsString>,
    home: Option<OsString>,
) -> anyhow::Result<PathBuf> {
    let present = |v: Option<OsString>| v.filter(|s| !s.is_empty());
    if windows {
        if let Some(appdata) = present(appdata) {
            return Ok(PathBuf::from(appdata).join("zkv"));
        }
        let profile = present(userprofile).ok_or_else(|| {
            anyhow::anyhow!(
                "neither %APPDATA% nor %USERPROFILE% is set; cannot locate the zkv data \
                 directory (set $ZKV_DATA or pass --data-dir)"
            )
        })?;
        Ok(PathBuf::from(profile).join(".zkv"))
    } else {
        let home = PathBuf::from(present(home).ok_or_else(|| {
            anyhow::anyhow!(
                "$HOME is not set; cannot locate the zkv data directory \
                 (set $ZKV_DATA or pass --data-dir)"
            )
        })?);
        if macos {
            return Ok(home.join("Library/Application Support").join("zkv"));
        }
        Ok(home.join(".zkv"))
    }
}

/// Validate a user-supplied database name. The name becomes a directory under
/// the data directory, so we restrict it to a tight ASCII-only character set:
/// `[A-Za-z0-9_-]`, 1-24 characters. Then reject the handful of names that
/// would still confuse us on disk: our own `current` marker file, and the
/// Windows device names (`CON`, `PRN`, etc.) that misbehave on cmd.exe.
pub fn validate_db_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        anyhow::bail!("database name cannot be empty");
    }
    if name.len() > 24 {
        anyhow::bail!("database name too long (max 24 characters)");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        anyhow::bail!("database name {name:?} may only contain ASCII letters, digits, '-' and '_'");
    }
    if name == "current" {
        anyhow::bail!("{name:?} is a reserved name");
    }
    // Windows device names: refuse case-insensitively so `~/.zkv/CON/` can't
    // poison cmd.exe.
    let upper = name.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) {
        anyhow::bail!("{name:?} is a reserved device name on Windows");
    }
    Ok(())
}

/// Validated path to a named database's directory. Does NOT create the directory.
pub fn db_dir(name: &str) -> anyhow::Result<PathBuf> {
    validate_db_name(name)?;
    Ok(zkv_data()?.join(name))
}

/// Like [`db_dir`] but also creates the directory if missing. Use for write paths
/// (init / restore / watch); read paths should use `db_dir` so a typo doesn't
/// leave an empty stub directory behind.
pub fn ensure_db_dir(name: &str) -> anyhow::Result<PathBuf> {
    let p = db_dir(name)?;
    create_private_dir(&p)?;
    Ok(p)
}

/// Create a directory (and any missing parents) with owner-only permissions
/// where the platform supports it.
///
/// On Unix each created component is `0700` (applied at creation via
/// `DirBuilderExt`), so the per-database secret files underneath
/// (`keys.toml`, `security-theater-key`, `data.sqlite`) are not exposed to
/// other local users even before their own `0600` modes take effect. An
/// already-existing directory is left untouched. On Windows the directory
/// inherits the per-user
/// `%APPDATA%` ACL; tightening further is future work, acceptable for the
/// v0.0.1 alpha.
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

/// Where this database's librustzcash-owned files live: `data.sqlite`,
/// `blockmeta.sqlite` and `blocks/`.
///
/// Two layouts are live at once, so this resolves by looking rather than by
/// assuming. Historically these sat at the database directory's root, and a
/// database no wallet-engine node has ever opened still keeps them there. The
/// engine nests them under `<db>/zec/lrz/`, moving them once, under its datadir
/// lock, the first time a node starts on that database. A read has to work on
/// either side of that move.
///
/// `keys.toml`, the age identity and `zkv_state.sqlite` are unaffected: they
/// are zkv's, not librustzcash's, and stay at the database root. Use
/// [`db_dir`] for those.
///
/// # Errors
///
/// When `data.sqlite` is present in **both** places. That is the one case the
/// engine's own migration refuses rather than choosing between, and zkv
/// silently preferring one would leave the two readers of a single directory
/// disagreeing about which database is real.
pub fn engine_dir(name: &str) -> anyhow::Result<PathBuf> {
    engine_dir_in(&db_dir(name)?)
}

/// [`engine_dir`] against an explicit database directory, for callers that
/// already hold one (and so need no name to resolve).
pub(crate) fn engine_dir_in(root: &Path) -> anyhow::Result<PathBuf> {
    let nested = root.join(ENGINE_COIN_DIR).join(ENGINE_STORAGE_DIR);
    match (root.join(DATA_DB).is_file(), nested.join(DATA_DB).is_file()) {
        (true, true) => anyhow::bail!(
            "this database has a {DATA_DB} both at {} and at {}, and zkv will not choose \
             between them. Keep the one you want (the nested path is the current layout), \
             move the other out of the database directory, and try again",
            root.join(DATA_DB).display(),
            nested.join(DATA_DB).display(),
        ),
        (_, true) => Ok(nested),
        // Nothing nested: either the files are still at the root, or this is a
        // database that has not been created yet, and a fresh one is laid down
        // at the root for the engine to migrate on its first start.
        _ => Ok(root.to_path_buf()),
    }
}

/// Returns (engine dir, `data.sqlite` path) for a named database. Does NOT
/// create the directory; callers that need a writable directory go via
/// `ensure_db_dir`.
///
/// The first element is where librustzcash's files live. For a database with a
/// wallet engine of its own that is the database root until a node has run and
/// `<name>/zec/lrz` afterwards (see [`engine_dir`]); for a member of the shared
/// scan it is a shard directory holding several members' accounts, which is why
/// every read that goes through here is also account-scoped.
///
/// Routing lives here so the callers do not each have to ask: a read path takes
/// a database name and gets the files that name means.
pub fn get_db_paths(name: &str) -> anyhow::Result<(PathBuf, PathBuf)> {
    let dir = wallet_home_dir(name)?;
    let data = dir.join(DATA_DB);
    Ok((dir, data))
}

/// The directory holding a database's librustzcash files; see
/// [`get_db_paths`], whose first element this is.
fn wallet_home_dir(name: &str) -> anyhow::Result<PathBuf> {
    // No `keys.toml` yet means a database being laid down, which is always its
    // own: enrolling in the shared scan writes that file first.
    let Ok(cfg) = crate::config::WalletConfig::read(name) else {
        return engine_dir(name);
    };
    match crate::fleet::locate(&cfg, name)? {
        crate::fleet::WalletHome::Own { engine_dir } => Ok(engine_dir),
        crate::fleet::WalletHome::Fleet { engine_dir } => Ok(engine_dir),
        // Typed, not a message: this is a transient state with a specific
        // remedy (sync, which is what causes the import), and callers have to
        // tell it apart from a genuine failure to find the files. See
        // [`ImportPending`].
        crate::fleet::WalletHome::Importing => Err(ImportPending(name.to_owned()).into()),
    }
}

/// A member of the shared scan whose account the wallet engine has not imported
/// into a shard yet, so it has no wallet files to open.
///
/// The ordinary state of a database between `zkv watch` writing its manifest
/// and the fleet node's next pass, and the reason this is a type rather than a
/// message: a caller that only wants to *measure* local progress treats it as
/// "nothing to measure" and syncs, which is precisely what makes the import
/// happen, while the facade turns it into [`ZkvError::Importing`] rather than a
/// nondescript failure. Use [`is_import_pending`] to recognise it, since it
/// reaches most callers wrapped in an `anyhow` context.
///
/// [`ZkvError::Importing`]: crate::db::ZkvError::Importing
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportPending(pub String);

impl std::fmt::Display for ImportPending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the {:?} database is joining the shared scan and has not been imported yet",
            self.0
        )
    }
}

impl std::error::Error for ImportPending {}

/// Whether this error is [`ImportPending`], anywhere in its chain.
///
/// The chain walk rather than a bare `downcast_ref` because the error passes
/// through `?` and `.context(..)` on the way up, and a caller that misses it
/// reports a routine first-run state as a hard failure.
pub fn is_import_pending(err: &anyhow::Error) -> bool {
    err.chain().any(|e| e.is::<ImportPending>())
}

/// Path to the per-database KV-state snapshot sidecar (`zkv_state.sqlite`).
/// Does NOT create the parent directory; callers reading the snapshot already
/// have a wallet DB open under `db_dir`, and `ensure_db_dir` is the right
/// preparation for the write side.
pub fn zkv_state_path(name: &str) -> anyhow::Result<PathBuf> {
    Ok(db_dir(name)?.join(ZKV_STATE_DB))
}

/// Read the "current" marker; returns None if unset. Re-validates the contents
/// so a corrupted or pre-tightening marker file can't bypass the new rules.
pub fn current_db() -> anyhow::Result<Option<String>> {
    let path = zkv_data()?.join(CURRENT_MARKER);
    if !path.exists() {
        return Ok(None);
    }
    let name = std::fs::read_to_string(&path)?.trim().to_owned();
    if name.is_empty() {
        return Ok(None);
    }
    validate_db_name(&name)
        .map_err(|e| anyhow::anyhow!("the 'current' marker contains an invalid name: {e}"))?;
    Ok(Some(name))
}

/// Write the "current" marker. Validates the name first so we never write
/// untrusted bytes (control chars, etc.) into the marker file.
pub fn set_current_db(name: &str) -> anyhow::Result<()> {
    validate_db_name(name)?;
    let path = zkv_data()?.join(CURRENT_MARKER);
    std::fs::write(&path, name)?;
    Ok(())
}

/// Set as current iff there is no current yet.
pub fn set_current_db_if_unset(name: &str) -> anyhow::Result<()> {
    if current_db()?.is_none() {
        set_current_db(name)?;
    }
    Ok(())
}

/// Whether the GUI's first-run onboarding has been completed or dismissed
/// (the marker file exists). Drives whether the welcome overlay is shown on
/// launch. Tied to the data dir, so a fresh `.zkv` shows onboarding again.
pub fn was_onboarded() -> bool {
    zkv_data()
        .map(|p| p.join(ONBOARDED_MARKER).exists())
        .unwrap_or(false)
}

/// Record that onboarding has been completed or dismissed, so it is not shown
/// again for this data dir. Best-effort: a write failure just means the
/// overlay may reappear on the next launch.
pub fn mark_onboarded() -> anyhow::Result<()> {
    std::fs::write(zkv_data()?.join(ONBOARDED_MARKER), b"")?;
    Ok(())
}

/// Resolves the database name: explicit override, else the current marker,
/// else error with a helpful hint.
pub fn resolve_db(explicit: Option<&str>) -> anyhow::Result<String> {
    if let Some(name) = explicit {
        return Ok(name.to_owned());
    }
    current_db()?.ok_or_else(|| {
        anyhow::anyhow!(
            "no current database. Run `zkv init` to create one, or `zkv use <name>` to select one."
        )
    })
}

/// List all database directories under the data dir (anything with a keys.toml inside).
pub fn list_dbs() -> anyhow::Result<Vec<String>> {
    let home = zkv_data()?;
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&home)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = match entry.file_name().into_string() {
            Ok(n) => n,
            Err(_) => continue,
        };
        if name.starts_with('.') {
            continue;
        }
        if entry.path().join("keys.toml").exists() {
            out.push(name);
        }
    }
    out.sort();
    Ok(out)
}

/// Delete a database outright: the whole `<data-dir>/<name>` directory.
///
/// This is what `zkv remove` and the GUI's forget action mean, and both warn
/// the user that it destroys the seed. It must therefore be the database
/// *root*, not [`get_db_paths`]'s first element: that one resolves to the
/// engine directory (`<name>/zec/lrz` once a node has run), so using it would
/// delete the wallet files and leave `keys.toml`, the age identity, the
/// snapshot and `pending.toml` behind. On a migrated database the user would
/// be told the seed was gone while it was still on disk.
pub async fn erase_wallet_state(name: &str) {
    let root = match db_dir(name) {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to resolve {name}: {e}");
            return;
        }
    };
    // A member's manifest lives outside its directory, and it is what the
    // shared scan reads. Left behind, the scan would keep a deleted database's
    // viewing key on disk and keep scanning for it. Removed first, so a failure
    // to delete the directory does not leave the key enrolled either.
    //
    // The shard keeps the account regardless: the wallet engine has no way to
    // remove one, which is what `zkv fleet rebuild` is for. Deliberately quiet
    // about it here, since this path is already destroying the database.
    if let Ok(cfg) = crate::config::WalletConfig::read(name) {
        if !cfg.engine.is_standalone() {
            if let Err(e) = crate::fleet::remove_manifest(cfg.network, name) {
                error!("Failed to remove the shared-scan manifest for {name}: {e:#}");
            }
        }
    }
    if let Err(e) = tokio::fs::remove_dir_all(&root).await {
        error!("Failed to remove {:?}: {}", root, e);
    }
}

pub fn init_dbs<P: Parameters + 'static>(
    params: P,
    name: &str,
) -> anyhow::Result<WalletDb<rusqlite::Connection, P, SystemClock, UnwrapErr<SysRng>>> {
    ensure_db_dir(name)?;
    // A member of the shared scan has no wallet files of its own, and creating
    // some here would give it a second, empty account that reads would then
    // have to choose against.
    if let Ok(cfg) = crate::config::WalletConfig::read(name) {
        if cfg.engine.is_fleet_member() {
            anyhow::bail!(
                "{name:?} is served by the shared scan, so it has no wallet database of its \
                 own to create. `zkv fleet leave {name}` gives it one."
            );
        }
    }
    let (db_cache, db_data) = get_db_paths(name)?;
    let mut db_cache = FsBlockDb::for_path(db_cache).map_err(error::Error::from)?;
    let mut db_data = open_wallet_conn(db_data, params)?;
    init_blockmeta_db(&mut db_cache)?;
    init_wallet_db(&mut db_data, None)?;
    Ok(db_data)
}

/// Open an existing wallet `data.sqlite`, applying any pending schema
/// migrations before handing it back.
///
/// `WalletDb::for_path` opens the file but does *not* migrate it. A database
/// created by an older `zcash_client_sqlite` can therefore be missing columns
/// that the current version's generated queries reference, e.g.
/// `get_wallet_summary` selects `orchard_received_notes.witness_stabilized`
/// and the sync path selects `addresses.imported_transparent_receiver_script`.
/// Against a stale schema those queries fail with `no such column`, which is
/// exactly the failure the read/balance/sync paths hit when a database
/// predates a dependency bump.
///
/// Running `init_wallet_db` on open brings the schema forward and is a cheap
/// no-op once the database is already current, so every read/write path can
/// route through here instead of calling `WalletDb::for_path` directly. The
/// migrations needed to upgrade an existing, functioning database are
/// schema-only, so a `None` seed (matching [`init_dbs`]) is sufficient.
pub fn open_wallet_db<P: Parameters + 'static>(
    path: impl AsRef<Path>,
    params: P,
) -> anyhow::Result<WalletDb<rusqlite::Connection, P, SystemClock, UnwrapErr<SysRng>>> {
    let path = path.as_ref();
    let mut db_data = open_wallet_conn(path, params)?;
    init_wallet_db(&mut db_data, None)?;
    Ok(db_data)
}

/// Apply the SQLite pragmas zkv relies on for concurrent access to a
/// `data.sqlite`. Used for every handle onto the file: the [`WalletDb`]
/// connection and the short-lived read-path connections
/// ([`crate::internal::state`] / funding / sync).
///
/// - **`busy_timeout`**: the GUI syncs one database (a commitment-tree write)
///   while its UI reads another, and the read paths open their own connections,
///   so a writer and a reader briefly contend on the same file. Without a busy
///   timeout SQLite returns `SQLITE_BUSY` ("database is locked") *immediately*
///   instead of waiting; a few-second timeout lets the contending side retry
///   transparently rather than failing the whole sync.
/// - **WAL journal mode**: lets readers proceed while a write is in flight (the
///   common GUI pattern), which plain rollback journaling does not. WAL is a
///   persistent per-file property, so once set every later connection uses it.
pub(crate) fn configure_sqlite(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    // `execute_batch` (sqlite3_exec) ignores the row `PRAGMA journal_mode`
    // returns, unlike `pragma_update`.
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    Ok(())
}

/// Open a `data.sqlite` [`WalletDb`] on a connection pre-configured by
/// [`configure_sqlite`]. Mirrors `WalletDb::for_path` (open + load the `array`
/// vtab module) but lets us set the pragmas first, via `from_connection`.
fn open_wallet_conn<P: Parameters + 'static>(
    path: impl AsRef<Path>,
    params: P,
) -> anyhow::Result<WalletDb<rusqlite::Connection, P, SystemClock, UnwrapErr<SysRng>>> {
    let conn = rusqlite::Connection::open(path)?;
    configure_sqlite(&conn)?;
    rusqlite::vtab::array::load_module(&conn)?;
    Ok(WalletDb::from_connection(
        conn,
        params,
        SystemClock,
        UnwrapErr(SysRng),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(name: &str) {
        validate_db_name(name).unwrap_or_else(|e| panic!("expected {name:?} to be accepted: {e}"));
    }
    fn bad(name: &str) {
        let r = validate_db_name(name);
        assert!(r.is_err(), "expected {name:?} to be rejected");
    }

    #[test]
    fn accepts_alphanumeric_with_dashes_and_underscores() {
        ok("default");
        ok("foo");
        ok("Foo-Bar_2");
        ok("with-dashes");
        ok("under_score");
        ok("a"); // single char
        ok("name-with-digits-1234");
        ok("ABC");
        ok("0starts_with_digit");
    }

    #[test]
    fn rejects_anything_outside_the_charset() {
        bad(""); // empty
        bad("."); // dot
        bad(".."); // dot dot
        bad(".hidden"); // dotfile
        bad("../foo"); // path traversal
        bad("foo/bar"); // slash
        bad("foo\\bar"); // backslash
        bad("foo.bar"); // dot in middle
        bad("foo bar"); // space
        bad(" foo"); // leading space
        bad("foo "); // trailing space
        bad("foo@bar"); // special
        bad("naïve"); // non-ASCII
        bad("日本"); // CJK
        bad("a\0b"); // NUL
        bad("a\nb"); // newline
        bad("a\tb"); // tab
        bad("\x07bell"); // BEL
    }

    #[test]
    fn rejects_too_long() {
        let huge = "a".repeat(25);
        bad(&huge);
        // boundary: 24 chars OK
        ok(&"a".repeat(24));
    }

    #[test]
    fn rejects_reserved_names() {
        bad("current");
        bad("CON");
        bad("con");
        bad("PRN");
        bad("aux");
        bad("nul");
        bad("COM1");
        bad("lpt9");
    }

    // --- default_data_dir_from: the per-OS fallback when neither --data-dir
    // nor $ZKV_DATA is set. Paths are built with `.join`, so the assertions
    // are separator-agnostic and run on any host.

    #[test]
    fn windows_default_is_appdata_zkv() {
        let got = default_data_dir_from(
            true,
            false,
            Some(OsString::from(r"C:\Users\Alice\AppData\Roaming")),
            Some(OsString::from(r"C:\Users\Alice")),
            // A stray $HOME (e.g. from Git Bash) must NOT win on Windows.
            Some(OsString::from("/should/be/ignored")),
        )
        .expect("APPDATA resolves");
        assert_eq!(
            got,
            PathBuf::from(r"C:\Users\Alice\AppData\Roaming").join("zkv")
        );
    }

    #[test]
    fn windows_falls_back_to_userprofile_when_appdata_unset_or_empty() {
        for appdata in [None, Some(OsString::new())] {
            let got = default_data_dir_from(
                true,
                false,
                appdata,
                Some(OsString::from(r"C:\Users\Bob")),
                None,
            )
            .expect("USERPROFILE fallback resolves");
            assert_eq!(got, PathBuf::from(r"C:\Users\Bob").join(".zkv"));
        }
    }

    #[test]
    fn windows_errors_without_appdata_or_userprofile() {
        assert!(
            default_data_dir_from(true, false, None, None, Some(OsString::from(r"C:\home")))
                .is_err()
        );
        // Empty strings count as unset.
        assert!(default_data_dir_from(
            true,
            false,
            Some(OsString::new()),
            Some(OsString::new()),
            None
        )
        .is_err());
    }

    #[test]
    fn linux_default_is_home_dot_zkv() {
        let got = default_data_dir_from(
            false,
            false,
            // APPDATA / USERPROFILE are ignored off-Windows.
            Some(OsString::from(r"C:\ignored")),
            Some(OsString::from(r"C:\ignored")),
            Some(OsString::from("/home/carol")),
        )
        .expect("HOME resolves");
        assert_eq!(got, PathBuf::from("/home/carol").join(".zkv"));
    }

    #[test]
    fn macos_default_is_application_support_zkv() {
        let got =
            default_data_dir_from(false, true, None, None, Some(OsString::from("/Users/dave")))
                .expect("HOME resolves");
        assert_eq!(
            got,
            PathBuf::from("/Users/dave")
                .join("Library/Application Support")
                .join("zkv")
        );
    }

    #[test]
    fn display_collapses_home_to_tilde_on_unix() {
        let home = Some(OsString::from("/home/alice"));
        // A path under $HOME collapses to ~.
        assert_eq!(
            display_data_dir_from(&PathBuf::from("/home/alice/.zkv"), home.clone(), false),
            "~/.zkv"
        );
        // $HOME itself shows as a bare ~.
        assert_eq!(
            display_data_dir_from(&PathBuf::from("/home/alice"), home.clone(), false),
            "~"
        );
        // A path outside $HOME (e.g. --data-dir /srv/zkv) is shown verbatim.
        assert_eq!(
            display_data_dir_from(&PathBuf::from("/srv/zkv"), home.clone(), false),
            "/srv/zkv"
        );
        // No/empty $HOME: shown verbatim.
        assert_eq!(
            display_data_dir_from(&PathBuf::from("/home/alice/.zkv"), None, false),
            "/home/alice/.zkv"
        );
        assert_eq!(
            display_data_dir_from(
                &PathBuf::from("/home/alice/.zkv"),
                Some(OsString::new()),
                false
            ),
            "/home/alice/.zkv"
        );
    }

    #[test]
    fn display_shows_full_path_on_windows() {
        // Windows never abbreviates to ~, even with a (Git Bash) $HOME set.
        assert_eq!(
            display_data_dir_from(
                &PathBuf::from(r"C:\Users\Alice\AppData\Roaming\zkv"),
                Some(OsString::from(r"C:\Users\Alice")),
                true,
            ),
            r"C:\Users\Alice\AppData\Roaming\zkv"
        );
    }

    #[test]
    fn unix_errors_without_home() {
        assert!(default_data_dir_from(
            false,
            false,
            Some(OsString::from("x")),
            Some(OsString::from("y")),
            None
        )
        .is_err());
        // Empty $HOME is treated as unset.
        assert!(default_data_dir_from(false, false, None, None, Some(OsString::new())).is_err());
        // macOS with no $HOME errors too.
        assert!(default_data_dir_from(false, true, None, None, None).is_err());
    }
}

#[cfg(test)]
mod engine_layout_tests {
    use super::*;

    /// Lay down a database directory holding `data.sqlite` at the root, nested
    /// under `zec/lrz`, or both.
    fn fixture(root: bool, nested: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        if root {
            std::fs::write(dir.path().join(DATA_DB), b"").unwrap();
        }
        if nested {
            let deep = dir.path().join(ENGINE_COIN_DIR).join(ENGINE_STORAGE_DIR);
            std::fs::create_dir_all(&deep).unwrap();
            std::fs::write(deep.join(DATA_DB), b"").unwrap();
        }
        dir
    }

    #[test]
    fn a_database_no_node_has_opened_keeps_its_files_at_the_root() {
        let dir = fixture(true, false);
        assert_eq!(engine_dir_in(dir.path()).unwrap(), dir.path());
    }

    #[test]
    fn a_migrated_database_resolves_to_the_nested_directory() {
        let dir = fixture(false, true);
        assert_eq!(
            engine_dir_in(dir.path()).unwrap(),
            dir.path().join(ENGINE_COIN_DIR).join(ENGINE_STORAGE_DIR),
        );
    }

    #[test]
    fn a_database_that_does_not_exist_yet_resolves_to_the_root() {
        // Nothing to find either way: a fresh database is laid down at the
        // root, and the engine migrates it on its first node start.
        let dir = fixture(false, false);
        assert_eq!(engine_dir_in(dir.path()).unwrap(), dir.path());
    }

    #[test]
    fn a_wallet_database_in_both_places_is_refused_rather_than_guessed() {
        // The engine's own migration refuses this rather than choosing, so zkv
        // has to as well: picking one would leave the two readers of a single
        // directory disagreeing about which database is real.
        let dir = fixture(true, true);
        let err = engine_dir_in(dir.path()).unwrap_err().to_string();
        assert!(
            err.contains("will not choose"),
            "the error should say it refuses to choose: {err}",
        );
        // And it must name both paths, since resolving it means deleting one.
        assert!(
            err.contains(ENGINE_STORAGE_DIR),
            "names the nested path: {err}"
        );
    }

    /// A directory, not a file, at the nested path is not a migrated database.
    #[test]
    fn an_empty_nested_directory_does_not_count_as_migrated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(ENGINE_COIN_DIR).join(ENGINE_STORAGE_DIR)).unwrap();
        std::fs::write(dir.path().join(DATA_DB), b"").unwrap();
        assert_eq!(engine_dir_in(dir.path()).unwrap(), dir.path());
    }
}
