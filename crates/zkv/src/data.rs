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

use rand::rngs::OsRng;
use tracing::error;
use zcash_client_sqlite::{
    chain::{init::init_blockmeta_db, BlockMeta},
    util::SystemClock,
    wallet::init::init_wallet_db,
    FsBlockDb, WalletDb,
};
use zcash_protocol::consensus::{self, Parameters};

use crate::error;

const BLOCKS_FOLDER: &str = "blocks";
const DATA_DB: &str = "data.sqlite";
const ZKV_STATE_DB: &str = "zkv_state.sqlite";
const CURRENT_MARKER: &str = "current";
/// Marker file (at the data-dir root) recording that the user has gone through
/// (or dismissed) the GUI's first-run onboarding. State lives in the data dir,
/// not the browser: localStorage is per-origin (every zkv install shares
/// `http://127.0.0.1:<port>`), so a browser-side flag suppressed onboarding
/// across unrelated installs and data-dir resets. The leading dot keeps it out
/// of [`list_dbs`] (which skips dotfiles and non-directories).
const ONBOARDED_MARKER: &str = ".onboarded";

#[derive(Clone, Copy, Debug, Default)]
pub enum Network {
    #[default]
    Main,
    Test,
}

impl Network {
    pub fn parse(name: &str) -> Result<Network, String> {
        match name {
            // Canonical names: "mainnet" / "testnet". Short forms accepted
            // for legacy/CLI brevity.
            "mainnet" | "main" => Ok(Network::Main),
            "testnet" | "test" => Ok(Network::Test),
            other => Err(format!(
                "Unsupported network: {other:?} (use \"mainnet\" or \"testnet\")",
            )),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Network::Test => "testnet",
            Network::Main => "mainnet",
        }
    }

    pub fn ticker(&self) -> &'static str {
        match self {
            Network::Main => "ZEC",
            Network::Test => "TAZ",
        }
    }
}

impl From<Network> for consensus::Network {
    fn from(value: Network) -> Self {
        match value {
            Network::Test => consensus::Network::TestNetwork,
            Network::Main => consensus::Network::MainNetwork,
        }
    }
}

impl From<consensus::Network> for Network {
    fn from(value: consensus::Network) -> Self {
        match value {
            consensus::Network::TestNetwork => Network::Test,
            consensus::Network::MainNetwork => Network::Main,
        }
    }
}

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

/// Returns (db_root, data.sqlite path) for a named database. Does NOT create
/// the directory; callers that need a writable directory go via `ensure_db_dir`.
pub fn get_db_paths(name: &str) -> anyhow::Result<(PathBuf, PathBuf)> {
    let root = db_dir(name)?;
    let data = root.join(DATA_DB);
    Ok((root, data))
}

/// Path to the per-database KV-state snapshot sidecar (`zkv_state.sqlite`).
/// Does NOT create the parent directory; callers reading the snapshot already
/// have a wallet DB open under `db_dir`, and `ensure_db_dir` is the right
/// preparation for the write side.
pub fn zkv_state_path(name: &str) -> anyhow::Result<PathBuf> {
    Ok(db_dir(name)?.join(ZKV_STATE_DB))
}

pub fn get_block_path(fsblockdb_root: &Path, meta: &BlockMeta) -> PathBuf {
    meta.block_file_path(&fsblockdb_root.join(BLOCKS_FOLDER))
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

pub async fn erase_wallet_state(name: &str) {
    let (root, _) = match get_db_paths(name) {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to resolve {name}: {e}");
            return;
        }
    };
    if let Err(e) = tokio::fs::remove_dir_all(&root).await {
        error!("Failed to remove {:?}: {}", root, e);
    }
}

pub fn init_dbs<P: Parameters + 'static>(
    params: P,
    name: &str,
) -> anyhow::Result<WalletDb<rusqlite::Connection, P, SystemClock, OsRng>> {
    ensure_db_dir(name)?;
    let (db_cache, db_data) = get_db_paths(name)?;
    let mut db_cache = FsBlockDb::for_path(db_cache).map_err(error::Error::from)?;
    let mut db_data = WalletDb::for_path(db_data, params, SystemClock, OsRng)?;
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
) -> anyhow::Result<WalletDb<rusqlite::Connection, P, SystemClock, OsRng>> {
    let mut db_data = WalletDb::for_path(path, params, SystemClock, OsRng)?;
    init_wallet_db(&mut db_data, None)?;
    Ok(db_data)
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
