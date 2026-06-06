//! `zkv gui`: launch the native desktop GUI (Tauri IPC).
//!
//! The desktop window renders the same single-page app as
//! [`crate::commands::gui_browser`], but with **nothing listening on a
//! localhost port**. The actual Tauri transport (the `#[tauri::command]`
//! surface and the webview event loop) lives in the library at
//! [`zkv::gui::desktop`], shared verbatim with the standalone `zkv-browser`
//! binary; this module is just the CLI seam.
//!
//! Tauri lives behind the opt-in `desktop` cargo feature. A binary built
//! without it still exposes `zkv gui`, but [`Command::run`] just prints a
//! hint to rebuild (or use `zkv gui-browser`). Because the webview event
//! loop must own the main thread, the desktop path is dispatched from
//! `main` *before* `runtime.block_on`, not through the async `run`.

use clap::Args;

use crate::commands::connection_args::ConnectionCliArgs;

#[derive(Debug, Args)]
pub(crate) struct Command {
    #[command(flatten)]
    pub(crate) connection: ConnectionCliArgs,
}

impl Command {
    /// Reached only on binaries built *without* the `desktop` feature;
    /// the desktop path is dispatched in `main` before the async runtime.
    pub(crate) async fn run(self, _db: Option<String>) -> anyhow::Result<()> {
        anyhow::bail!(
            "`zkv gui` (desktop window) needs a build with `--features desktop`; \
             rebuild with that feature, or use `zkv gui-browser` for the browser UI"
        )
    }
}

/// Launch the desktop window on the main thread by handing off to the
/// shared library launcher ([`zkv::gui::desktop::run`]). Blocks until the
/// window closes.
#[cfg(feature = "desktop")]
pub(crate) fn run_desktop(
    runtime: tokio::runtime::Runtime,
    cmd: Command,
    _db: Option<String>,
) -> anyhow::Result<()> {
    zkv::gui::desktop::run(runtime, cmd.connection.into_inner())
}
