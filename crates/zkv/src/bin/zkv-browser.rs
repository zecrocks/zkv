// In release builds, link this GUI binary against the Windows "windows"
// subsystem so double-clicking the .exe doesn't pop a console window next to
// the webview. Debug builds keep the console for `cargo run` diagnostics; the
// attribute is a no-op on non-Windows targets.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! `zkv-browser`: the native desktop database browser for zkv.
//!
//! A thin launcher around [`zkv::gui::desktop::run`]: it parses the
//! lightwalletd connection flags, builds a multi-thread tokio runtime, then
//! hands the main thread to Tauri (the webview event loop must own it). This
//! is the same desktop GUI reachable via `zkv gui`, packaged as its own
//! binary so it can be shipped and launched on its own.
//!
//! Requires the `desktop` feature (Tauri) plus `cli` (clap, for flag
//! parsing; `cli` also pulls the default `tracing` subscriber). On a plain
//! `cargo build` this binary is skipped because its `required-features`
//! aren't met; build it with `--features desktop`.

use std::path::PathBuf;

use clap::Parser;

use zkv::remote::{parse_connection_mode, ConnectionArgs, ConnectionMode, Servers};

/// `<pkg-version> (<git-sha>)`: the git SHA is captured at build time by
/// `build.rs`. Matches the `zkv` CLI's `--version` format.
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("ZKV_GIT_SHA"), ")");

#[derive(Debug, Parser)]
#[command(
    name = "zkv-browser",
    version = VERSION,
    about = "Native desktop browser for zkv databases."
)]
struct Cli {
    /// The lightwalletd server to use. One of "ecc", "ywallet", "zecrocks", or a
    /// comma-separated list of `host:port`. Used for any network without a more
    /// specific override below.
    #[arg(short, long, default_value = "zecrocks", value_parser = Servers::parse)]
    server: Servers,

    /// Override the lightwalletd server for mainnet only (same format as
    /// `--server`); falls back to `--server` when unset.
    #[arg(long, value_parser = Servers::parse)]
    mainnet_server: Option<Servers>,

    /// Override the lightwalletd server for testnet only (same format as
    /// `--server`); falls back to `--server` when unset.
    #[arg(long, value_parser = Servers::parse)]
    testnet_server: Option<Servers>,

    /// Connection mode: "direct" (default) or "socks5://<host>:<port>".
    #[arg(long, default_value = "direct", value_parser = parse_connection_mode)]
    connection: ConnectionMode,

    /// Data directory. Overrides `$ZKV_DATA` and the per-OS default
    /// (`$HOME/.zkv` on Linux, `$HOME/Library/Application Support/zkv` on
    /// macOS, `%APPDATA%\zkv` on Windows).
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// More-verbose stderr logging.
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Mirror the `zkv` CLI's `-v/--verbose`: bump the default filter to
    // `info` unless the caller already set `RUST_LOG`. The subscriber is
    // available because the `cli` feature (a required-feature of this
    // binary) pulls in `default-subscriber`.
    if cli.verbose && std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "info");
    }
    zkv::db::install_default_subscriber();

    if let Some(p) = cli.data_dir {
        zkv::data::set_data_dir_override(p);
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    zkv::gui::desktop::run(
        runtime,
        ConnectionArgs {
            server: cli.server,
            mainnet_server: cli.mainnet_server,
            testnet_server: cli.testnet_server,
            connection: cli.connection,
        },
    )
}
