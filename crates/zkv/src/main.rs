//! `zkv`: Redis-style key-value store on Zcash shielded memos.

use std::env;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, Layer};

mod commands;

// The wallet/send/sync plumbing lives in the library crate so the bundled
// `zkv-faucet` binary (a separate crate) can reach it. Re-bind it at the
// binary's crate root so existing `crate::config::*`, `crate::internal::*`
// etc. references in `commands/*` continue to compile.
use zkv::{config, data, db, demo, internal, remote, shallow, ui};

/// `<pkg-version> (<git-sha>)`, e.g. `0.0.1 (eda2d7f)`. The SHA is captured at
/// build time by `build.rs` (`-dirty` suffix for an unclean tree, `unknown`
/// outside a git checkout).
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("ZKV_GIT_SHA"), ")");

#[derive(Debug, Parser)]
#[command(
    name = "zkv",
    version = VERSION,
    about = "A decentralized key-value store powered by Zcash memos."
)]
struct Cli {
    /// Override the current database.
    #[arg(long, global = true)]
    db: Option<String>,

    /// Data directory. Overrides `$ZKV_DATA` and the per-OS default
    /// (`$HOME/.zkv` on Linux, `$HOME/Library/Application Support/zkv` on
    /// macOS, `%APPDATA%\zkv` on Windows).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    /// More-verbose stderr logging.
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Print the bundled third-party license texts to stdout and exit.
    #[cfg(feature = "gui")]
    #[arg(long)]
    licenses: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a new zkv database (a new wallet) with a fresh recovery phrase.
    Init(commands::init::Command),

    /// Restore an admin database from an existing 24-word recovery phrase.
    Restore(commands::restore::Command),

    /// Watch a zkv database in view-only mode using its zkv address.
    Watch(commands::watch::Command),

    /// List all local databases.
    List(commands::list::Command),

    /// Switch the current database.
    #[command(name = "use")]
    UseDb(commands::use_db::Command),

    /// Remove a local database (the seed is destroyed).
    Remove(commands::remove::Command),

    /// Show the current database's address, funding UA, balance, and sync state.
    Show(commands::show::Command),

    /// Print just the zkv address (for scripting).
    Address(commands::address::Command),

    /// Decode a zkv address: network, pool, birthday, funding address, keys.
    Inspect(commands::inspect::Command),

    /// Read the current key-value state (one key, or all).
    Get(commands::get::Command),

    /// List key names matching a glob pattern (`*` wildcard, Redis KEYS-style).
    Keys(commands::keys::Command),

    /// Show the append-only signed write history (one key, or all).
    History(commands::history::Command),

    /// Set a key to a value.
    Set(commands::set::Command),

    /// Delete a key.
    Del(commands::del::Command),

    /// Send ZEC/TAZ to any Zcash address (optionally with a memo).
    Send(commands::send::Command),

    /// Inspect and manage owners and scoped writers.
    Roles(commands::roles::Command),

    /// Owner-only administration: finalize, plus the `sign`/`verify` memo tools.
    Admin(commands::admin::Command),

    /// Read recent updates by scanning only a shallow block window (no full sync).
    Shallow(commands::shallow::Command),

    /// Force a sync (commands that need fresh chain state auto-sync by default).
    Sync(commands::sync::Command),

    /// Show the current balance (zatoshi to stdout, formatted to stderr).
    Balance(commands::balance::Command),

    /// Launch the native desktop GUI (needs `--features desktop`).
    #[cfg(feature = "gui")]
    Gui(commands::gui::Command),

    /// Serve the web database browser on localhost (opens your browser).
    #[cfg(feature = "gui")]
    #[command(name = "gui-browser")]
    GuiBrowser(commands::gui_browser::Command),
}

fn main() -> Result<(), anyhow::Error> {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    // Out-of-date notice: the very first line printed by every command. Goes to
    // stderr so it never pollutes machine-readable stdout (see `## Conventions`).
    // Clock-only here: this runs before any chain access, so there is no live
    // tip to pass. The GUI (always connected) also enforces the height-based
    // gate that resists clock tampering.
    if zkv::freshness::build_out_of_date(None) {
        eprintln!("{}", zkv::freshness::OUT_OF_DATE_MESSAGE);
    }

    // `--licenses`: dump the bundled third-party license texts and exit. No
    // database, runtime, or subcommand needed; just print and go.
    #[cfg(feature = "gui")]
    if cli.licenses {
        use std::io::Write as _;
        std::io::stdout().write_all(zkv::gui::licenses_text().as_bytes())?;
        return Ok(());
    }

    if let Some(p) = cli.data_dir {
        data::set_data_dir_override(p);
    }

    // A subcommand is required for every action; without one (and without
    // `--licenses` above) just print help, like a bare `zkv`.
    let Some(command) = cli.command else {
        use clap::CommandFactory as _;
        Cli::command().print_help()?;
        return Ok(());
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    // The desktop GUI's webview event loop must own the main thread, so it
    // can't run inside `block_on` on a runtime worker. Dispatch it here,
    // before the runtime takes over, and let it drive Tauri on this thread.
    #[cfg(feature = "desktop")]
    if matches!(command, Command::Gui(_)) {
        let db = cli.db;
        if let Command::Gui(c) = command {
            return commands::gui::run_desktop(runtime, c, db);
        }
        unreachable!();
    }

    runtime.block_on(async move {
        let db = cli.db;
        let verbose = cli.verbose;

        // First run: provision the bundled "demo-oracles" watch-only database
        // once (best effort, never fatal). The GUI commands provision it via
        // the Engine's auto-sync loop instead, so skip them here to avoid a
        // duplicate attempt; everything else gets it through this path.
        #[cfg(feature = "gui")]
        let is_gui_cmd = matches!(command, Command::Gui(_) | Command::GuiBrowser(_));
        #[cfg(not(feature = "gui"))]
        let is_gui_cmd = false;
        if !is_gui_cmd {
            if let Err(e) = zkv::demo::ensure(remote::ConnectionArgs::default()).await {
                tracing::debug!("demo database not provisioned: {e}");
            }
        }

        match command {
            Command::Init(c) => c.run(db).await,
            Command::Restore(c) => c.run(db).await,
            Command::Watch(c) => c.run(db).await,
            Command::List(c) => c.run(db),
            Command::UseDb(c) => c.run(db),
            Command::Remove(c) => c.run(db).await,
            Command::Show(c) => c.run(db).await,
            Command::Address(c) => c.run(db),
            Command::Inspect(c) => c.run(db),
            Command::Get(c) => c.run(db).await,
            Command::Keys(c) => c.run(db).await,
            Command::History(c) => c.run(db).await,
            Command::Set(c) => c.run(db).await,
            Command::Del(c) => c.run(db).await,
            Command::Send(c) => c.run(db).await,
            Command::Roles(c) => c.run(db).await,
            Command::Admin(c) => c.run(db).await,
            Command::Shallow(c) => c.run(db, verbose).await,
            Command::Sync(c) => c.run(db).await,
            Command::Balance(c) => c.run(db).await,
            #[cfg(feature = "gui")]
            Command::Gui(c) => c.run(db).await,
            #[cfg(feature = "gui")]
            Command::GuiBrowser(c) => c.run(db).await,
        }
    })
}

fn init_logging(verbose: bool) {
    let level = env::var("RUST_LOG").unwrap_or_else(|_| {
        if verbose {
            "info".to_owned()
        } else {
            "warn".to_owned()
        }
    });
    let filter = tracing_subscriber::EnvFilter::from(level);
    // Match the log colouring to the same stderr terminal/ANSI decision the
    // status lines use; on Windows, also enable Virtual Terminal processing so
    // the console renders the escape codes instead of printing them raw.
    let layer = tracing_subscriber::fmt::layer()
        .with_ansi(ui::color_enabled())
        .with_writer(std::io::stderr)
        .with_filter(filter);
    let subscriber = tracing_subscriber::registry().with(layer);
    tracing::subscriber::set_global_default(subscriber).ok();
}
