//! `zkv-version-oracle` — watches Docker Hub for project release tags and
//! publishes the current latest/stable version of each into a zkv database.
//!
//! ```text
//! # one pass, no broadcast — print desired-vs-current for every key:
//! zkv-version-oracle --db oracle-admin --once --dry-run -v
//!
//! # publish once, then exit:
//! zkv-version-oracle --db oracle-admin --once
//!
//! # long-running: poll on the default interval, publishing only on change:
//! zkv-version-oracle --db oracle-admin
//! ```
//!
//! The database named by `--db` must be an existing, INIT'd, funded **admin**
//! zkv database; its network is read from its `keys.toml`. Each write costs a
//! Zcash fee, so the oracle publishes a key only when the computed version
//! differs from what it has already published — including a value still pending
//! in the mempool (see [`effective_value`]).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::{Args, Parser};
use tracing_subscriber::{layer::SubscriberExt, Layer};

use zkv::{
    config::Role,
    data::set_data_dir_override,
    db::{Confirmations, Database, ZkvError},
    protocol::PendingOp,
    remote::{parse_connection_mode, ConnectionArgs, ConnectionMode, Servers},
};

mod config;
mod dockerhub;
mod select;

use config::{Channel, Project};

/// Default poll interval: versions change rarely, and an hour is gentle on
/// Docker Hub's anonymous rate limits.
const DEFAULT_INTERVAL_SECS: u64 = 60 * 60;

/// CLI-side wrapper around [`ConnectionArgs`] (the library type is clap-free so
/// external crates can build connections without a clap dependency).
#[derive(Debug, Clone, Args)]
struct ConnectionCliArgs {
    /// The lightwalletd server to use: "ecc", "ywallet", "zecrocks", or a
    /// comma-separated list of `host:port`.
    #[arg(short, long, default_value = "zecrocks", value_parser = Servers::parse)]
    server: Servers,

    /// Connection mode: "direct" (default) or "socks5://<host>:<port>".
    #[arg(long, default_value = "direct", value_parser = parse_connection_mode)]
    connection: ConnectionMode,
}

impl ConnectionCliArgs {
    fn into_inner(self) -> ConnectionArgs {
        ConnectionArgs {
            server: self.server,
            mainnet_server: None,
            testnet_server: None,
            connection: self.connection,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "zkv-version-oracle",
    about = "Polls Docker Hub for release tags and publishes latest/stable versions into a zkv database."
)]
struct Cli {
    /// Admin zkv database to publish into. Its network is read from keys.toml.
    #[arg(long)]
    db: String,

    /// Data directory (overrides $ZKV_DATA and the ~/.zkv fallback).
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Poll interval, in seconds.
    #[arg(long, default_value_t = DEFAULT_INTERVAL_SECS)]
    interval: u64,

    /// Run a single poll and exit.
    #[arg(long)]
    once: bool,

    /// Replace the baked-in project list with a TOML config file.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Compute and print desired-vs-current values without syncing or
    /// broadcasting (works on a watch-only database too).
    #[arg(long)]
    dry_run: bool,

    /// More-verbose stderr logging.
    #[arg(short, long)]
    verbose: bool,

    #[command(flatten)]
    connection: ConnectionCliArgs,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(run(cli))
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    if let Some(dir) = cli.data_dir.clone() {
        set_data_dir_override(dir);
    }

    let projects = config::load(cli.config.as_deref())?.projects;
    let http = dockerhub::build_client()?;
    let conn = cli.connection.clone().into_inner();
    let db =
        Database::open(&cli.db, conn).with_context(|| format!("open zkv database {:?}", cli.db))?;

    print_dashboard(&db, &projects, &cli)?;

    if !cli.dry_run && db.role() != Role::Admin {
        anyhow::bail!(
            "{:?} is watch-only; the oracle needs an admin database",
            cli.db
        );
    }

    let mut ticker = tokio::time::interval(Duration::from_secs(cli.interval));
    loop {
        // Don't wait for the first tick — poll immediately on startup.
        ticker.tick().await;

        // One sync per tick; the per-key reads below are pure-local. Skipped
        // in dry-run (which never touches the chain).
        if !cli.dry_run {
            if let Err(e) = db.sync().await {
                eprintln!("⚠ skipping tick: sync failed: {e:#}");
                if cli.once {
                    return Err(anyhow::Error::new(e));
                }
                continue;
            }
        }

        for project in &projects {
            let tags =
                match dockerhub::fetch_tags(&http, dockerhub::DEFAULT_BASE, &project.repo).await {
                    Ok(tags) => tags,
                    Err(e) => {
                        eprintln!("⚠ {}: tag fetch failed: {e:#}", project.name);
                        if cli.once {
                            return Err(e);
                        }
                        continue;
                    }
                };
            for &channel in &project.channels {
                if let Err(e) = reconcile(&db, project, channel, &tags, cli.dry_run).await {
                    eprintln!("⚠ {}/{}: {e:#}", project.name, channel.key_suffix());
                    if cli.once {
                        return Err(e);
                    }
                }
            }
        }

        if cli.once {
            return Ok(());
        }
    }
}

/// Compute the desired version for `(project, channel)`, compare it to the
/// value already published (including the oracle's own pending write), and
/// broadcast a SET only when it differs.
async fn reconcile(
    db: &Database,
    project: &Project,
    channel: Channel,
    tags: &[String],
    dry_run: bool,
) -> anyhow::Result<()> {
    let key = project.key_for(channel);
    let Some(desired) = select::select(tags, channel, &project.ignore) else {
        // e.g. stable requested but only pre-releases exist — skip silently.
        tracing::debug!(
            "{key}: no qualifying {} version yet; skipping",
            channel.key_suffix()
        );
        return Ok(());
    };
    let want = select::value_string(&desired);

    let current = effective_value(db, &key)?;
    if current.as_deref() == Some(want.as_str()) {
        tracing::info!("{key} = {want} (unchanged)");
        return Ok(());
    }

    if dry_run {
        eprintln!(
            "• {key}: {} -> {want} (dry-run, not broadcasting)",
            current.as_deref().unwrap_or("∅")
        );
        return Ok(());
    }

    // We already synced once at the top of the tick; skip the redundant
    // pre-broadcast sync.
    match db.set_no_sync(&key, &want).await {
        Ok(txid) => eprintln!(
            "✓ {key} = {want}  (was {}; txid {})",
            current.as_deref().unwrap_or("∅"),
            short(&txid)
        ),
        Err(ZkvError::InsufficientFunds {
            available,
            required,
            pending,
        }) => {
            let tkr = db.network().ticker();
            eprintln!(
                "⚠ insufficient funds for {key}: have {:.8} {tkr}, need {:.8} {tkr} \
                 (pending {:.8} {tkr}); will retry next tick",
                available as f64 / 1e8,
                required as f64 / 1e8,
                pending as f64 / 1e8,
            );
        }
        Err(ZkvError::Initializing { done, required }) => {
            eprintln!("⚠ database is still initializing ({done}/{required}); will retry next tick");
        }
        Err(e) => return Err(anyhow::Error::new(e)),
    }
    Ok(())
}

/// The current value of `key` as this client sees it, **including its own
/// in-flight (`pending.toml`) writes**. Reading at [`Confirmations::Mempool`]
/// makes the facade surface pending SET/DEL ops, so a value the oracle just
/// broadcast (but that hasn't confirmed) is not re-broadcast next tick. The
/// last pending op wins over the confirmed value; a pending DEL clears it.
fn effective_value(db: &Database, key: &str) -> anyhow::Result<Option<String>> {
    let replay = db.read(Confirmations::Mempool)?;
    let Some(ks) = replay.state.get(key) else {
        return Ok(None);
    };
    if let Some(last) = ks.pending.last() {
        return Ok(match last {
            PendingOp::Set { value, .. } => Some(value.clone()),
            PendingOp::Del { .. } => None,
        });
    }
    Ok(ks.confirmed.clone())
}

fn print_dashboard(db: &Database, projects: &[Project], cli: &Cli) -> anyhow::Result<()> {
    eprintln!("zkv version-oracle");
    eprintln!(
        "  database   : {} ({:?}, {:?})",
        db.name(),
        db.role(),
        db.network()
    );
    eprintln!("  zkv address: {}", db.zkv_address()?);
    if !cli.dry_run {
        match db.balance() {
            Ok(bal) => eprintln!(
                "  balance    : {:.8} {}",
                bal as f64 / 1e8,
                db.network().ticker()
            ),
            Err(e) => eprintln!("  balance    : (unavailable: {e})"),
        }
    }
    eprintln!(
        "  projects   : {}",
        projects
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mode = if cli.dry_run {
        "dry-run (no broadcast)".to_owned()
    } else if cli.once {
        "one-shot".to_owned()
    } else {
        format!("looping every {}s", cli.interval)
    };
    eprintln!("  mode       : {mode}");
    eprintln!();
    Ok(())
}

fn init_logging(verbose: bool) {
    let level = std::env::var("RUST_LOG").unwrap_or_else(|_| {
        if verbose {
            "info".to_owned()
        } else {
            "warn".to_owned()
        }
    });
    let filter = tracing_subscriber::EnvFilter::from(level);
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(filter);
    let subscriber = tracing_subscriber::registry().with(layer);
    tracing::subscriber::set_global_default(subscriber).ok();
}

fn short(txid: &str) -> String {
    if txid.len() <= 12 {
        txid.to_owned()
    } else {
        format!("{}…{}", &txid[..6], &txid[txid.len() - 6..])
    }
}
