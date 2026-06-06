//! Flat command surface: one file per subcommand under src/commands/.

pub(crate) mod address;
pub(crate) mod admin;
pub(crate) mod balance;
pub(crate) mod connection_args;
pub(crate) mod del;
pub(crate) mod get;
#[cfg(feature = "gui")]
pub(crate) mod gui;
#[cfg(feature = "gui")]
pub(crate) mod gui_browser;
pub(crate) mod history;
pub(crate) mod init;
pub(crate) mod inspect;
pub(crate) mod keys;
pub(crate) mod list;
pub(crate) mod owner;
pub(crate) mod remove;
pub(crate) mod restore;
pub(crate) mod roles;
pub(crate) mod send;
pub(crate) mod set;
pub(crate) mod show;
pub(crate) mod sign;
pub(crate) mod sync;
pub(crate) mod use_db;
pub(crate) mod verify;
pub(crate) mod watch;
pub(crate) mod writer;

/// After loading state, gate a read-path command on the database's required
/// protocol version. Bails if the controlling `VERSION` memo blocks reads
/// (`blockread`/`blockall`) for this out-of-date client; otherwise prints an
/// upgrade warning to stderr when the database is merely newer than this build.
pub(crate) fn gate_read(
    version: &crate::internal::protocol::VersionState,
    db_name: &str,
) -> anyhow::Result<()> {
    if version.blocks_read() {
        anyhow::bail!(
            "database {db_name:?} has been upgraded to version {} and blocks reads for this \
             client (which supports up to version {}); update zkv to the latest version",
            version.version,
            crate::internal::protocol::MAX_DB_VERSION,
        );
    }
    if let Some(warning) = version.upgrade_warning() {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

/// Should a read/sync command skip the network scan because the database
/// disabled syncing for this client version (`blocksync`/`blockall`)? Prints a
/// notice and returns `true` when so. Consults only the promoted snapshot
/// version (cheap, no sync), so a freshly-broadcast `blocksync` still allows
/// one more scan until it buries past `SAFE_DEPTH`.
pub(crate) fn blocksync_skip(db_name: &str) -> anyhow::Result<bool> {
    let cached = crate::internal::state::cached_version(db_name)?;
    if cached.blocks_sync() {
        eprintln!(
            "# note: this database disabled syncing for clients older than version {}; \
             showing cached state, update zkv",
            cached.version,
        );
        Ok(true)
    } else {
        Ok(false)
    }
}
