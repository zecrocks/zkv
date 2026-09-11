//! `zkv fleet`: report and manage the shared scan.
//!
//! Watch-only databases share one wallet engine per network instead of running
//! one each: one upstream connection, one set of shard databases, one pass over
//! the blocks for every viewing key. This command is where that arrangement is
//! visible and where it is changed by hand.

use clap::{Args, Subcommand};

use crate::{
    commands::connection_args::ConnectionCliArgs,
    data::{list_dbs, resolve_db},
    ui,
};
use zkv::config::{WalletConfig, WalletEngine};
use zkv::network::Network;

#[derive(Debug, Args)]
pub(crate) struct Command {
    #[command(subcommand)]
    action: Option<Action>,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Show which databases share a scan, and how far along they are.
    Status(StatusArgs),
    /// Move a database onto the shared scan.
    Join(JoinArgs),
    /// Give a database a wallet engine of its own again.
    Leave(LeaveArgs),
    /// Delete and re-import the shard databases from the manifests.
    Rebuild(RebuildArgs),
}

#[derive(Debug, Args)]
struct StatusArgs {
    /// Only this network's shared scan.
    #[arg(long)]
    network: Option<String>,

    /// `text` (default) or `json`.
    #[arg(long, default_value = "text")]
    output: String,
}

#[derive(Debug, Args)]
struct JoinArgs {
    /// The database to move. Defaults to the current one.
    db: Option<String>,

    #[command(flatten)]
    connection: ConnectionCliArgs,
}

#[derive(Debug, Args)]
struct LeaveArgs {
    /// The database to move. Defaults to the current one.
    db: Option<String>,

    #[command(flatten)]
    connection: ConnectionCliArgs,
}

#[derive(Debug, Args)]
struct RebuildArgs {
    /// Which network's shards to rebuild. Defaults to every network that has
    /// members.
    #[arg(long)]
    network: Option<String>,

    /// Skip the confirmation prompt.
    #[arg(short = 'y', long)]
    yes: bool,
}

impl Command {
    pub(crate) async fn run(self, db: Option<String>) -> anyhow::Result<()> {
        match self.action {
            None => status(&StatusArgs {
                network: None,
                output: "text".into(),
            }),
            Some(Action::Status(a)) => status(&a),
            Some(Action::Join(a)) => join(a, db).await,
            Some(Action::Leave(a)) => leave(a, db),
            Some(Action::Rebuild(a)) => rebuild(a),
        }
    }
}

/// One database's place in the arrangement.
struct Row {
    name: String,
    network: Network,
    engine: WalletEngine,
    /// The shard its account is in, or `None` while the scan is still importing
    /// it. Meaningless for a database that is not enrolled.
    shard: Option<String>,
    /// Whether a manifest exists for it, which is what the scan reads.
    manifest: bool,
}

fn rows(filter: Option<Network>) -> anyhow::Result<Vec<Row>> {
    let mut out = Vec::new();
    for name in list_dbs()? {
        let Ok(cfg) = WalletConfig::read(&name) else {
            continue;
        };
        if cfg.engine.is_standalone() {
            continue;
        }
        if filter.is_some_and(|n| n != cfg.network) {
            continue;
        }
        let shard = zkv::fleet::locate_member(&cfg, &name)
            .ok()
            .flatten()
            .and_then(|(dir, _)| dir.file_name().and_then(|n| n.to_str()).map(str::to_owned));
        let manifest = zkv::fleet::read_manifest(cfg.network, &name)
            .ok()
            .flatten()
            .is_some();
        out.push(Row {
            name,
            network: cfg.network,
            engine: cfg.engine,
            shard,
            manifest,
        });
    }
    out.sort_by(|a, b| (a.network.name(), &a.name).cmp(&(b.network.name(), &b.name)));
    Ok(out)
}

fn state_of(row: &Row) -> &'static str {
    match (row.engine, row.shard.is_some(), row.manifest) {
        // Enrolled, imported, reading from its shard.
        (WalletEngine::Fleet, true, _) => "shared",
        // A member whose account is not in a shard: either the scan has not
        // imported it yet, or its manifest is gone and never will.
        (WalletEngine::Fleet, false, true) => "importing",
        (WalletEngine::Fleet, false, false) => "no manifest",
        // Converting: still reading from its own files.
        (WalletEngine::FleetPending, true, _) => "joining (shard ready)",
        (WalletEngine::FleetPending, false, true) => "joining",
        (WalletEngine::FleetPending, false, false) => "joining (no manifest)",
        _ => "own",
    }
}

fn status(args: &StatusArgs) -> anyhow::Result<()> {
    let filter = args
        .network
        .as_deref()
        .map(Network::parse)
        .transpose()
        .map_err(|_| anyhow::anyhow!("unknown network"))?;
    let rows = rows(filter)?;

    if args.output == "json" {
        let items: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "database": r.name,
                    "network": r.network.name(),
                    "state": state_of(r),
                    "shard": r.shard,
                    "manifest": r.manifest,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }

    if rows.is_empty() {
        ui::hint(
            "No databases are on the shared scan. New watch-only databases join it \
             automatically unless you pass --standalone.",
        );
        return Ok(());
    }
    for r in &rows {
        eprintln!(
            "{:<24} {:<8} {:<22} {}",
            r.name,
            r.network.name(),
            state_of(r),
            r.shard.as_deref().unwrap_or("-"),
        );
    }
    // Say where the files are, since the whole point is that they are not where
    // a database directory would suggest.
    if let Some(net) = rows.first().map(|r| r.network) {
        if let Ok(dir) = zkv::fleet::fleet_dir(net) {
            ui::hint(format!("Shared scan directory: {}", dir.display()));
        }
    }
    Ok(())
}

async fn join(args: JoinArgs, db: Option<String>) -> anyhow::Result<()> {
    let name = resolve_db(args.db.as_deref().or(db.as_deref()))?;
    let mut cfg = WalletConfig::read(&name)?;
    if cfg.role == zkv::config::Role::Admin {
        anyhow::bail!(
            "{name:?} holds a spending key, and the shared scan is watch-only. \
             Only watch-only databases can join it."
        );
    }
    if !cfg.engine.is_standalone() {
        ui::hint(format!("{name:?} is already on the shared scan."));
        return Ok(());
    }
    if name == "default" {
        anyhow::bail!(
            "the shared scan cannot serve a database named \"default\" (the wallet engine \
             reserves that name)"
        );
    }

    // Enrolling is offline: it writes a manifest and a line of keys.toml. The
    // database keeps reading from its own files until its shard has caught up
    // with them, which is what makes this safe to do at any moment.
    zkv::fleet::join(&mut cfg, &name)?;
    ui::success(format!("{name:?} is joining the shared scan"));
    ui::hint(
        "It keeps reading from its own files until the shared scan has caught up. \
         Run `zkv sync` a few times, or leave the GUI open, to get there.",
    );
    let _ = args.connection;
    Ok(())
}

fn leave(args: LeaveArgs, db: Option<String>) -> anyhow::Result<()> {
    let name = resolve_db(args.db.as_deref().or(db.as_deref()))?;
    let mut cfg = WalletConfig::read(&name)?;
    if cfg.engine.is_standalone() {
        ui::hint(format!("{name:?} already has a wallet engine of its own."));
        return Ok(());
    }
    let was_member = cfg.engine.is_fleet_member();
    zkv::fleet::leave(&mut cfg, &name)?;
    ui::success(format!("{name:?} left the shared scan"));
    if was_member {
        // Its wallet files were deleted when it joined, so it has to rebuild
        // them from its birthday. The snapshot survives, so its reads stay
        // correct while that happens.
        ui::hint("Run `zkv sync` to rebuild its own wallet files from the birthday.");
    }
    let _ = args.connection;
    Ok(())
}

fn rebuild(args: RebuildArgs) -> anyhow::Result<()> {
    let filter = args
        .network
        .as_deref()
        .map(Network::parse)
        .transpose()
        .map_err(|_| anyhow::anyhow!("unknown network"))?;
    let rows = rows(filter)?;
    if rows.is_empty() {
        ui::hint("No databases are on the shared scan; nothing to rebuild.");
        return Ok(());
    }
    if !args.yes {
        ui::hint(
            "Rebuilding deletes the shared scan's databases and re-imports every member \
             from its viewing key, so every member rescans from its birthday. Reads keep \
             working from each database's snapshot meanwhile. Pass -y to proceed.",
        );
        return Ok(());
    }

    let mut networks: Vec<Network> = rows.iter().map(|r| r.network).collect();
    networks.dedup();
    for net in networks {
        let shards = zkv::fleet::shards_dir(net)?;
        if shards.exists() {
            std::fs::remove_dir_all(&shards)?;
        }
        // Manifests are rebuildable here, unlike in the wallet engine: their
        // two fields are copies of what keys.toml already holds. Rewriting them
        // repairs a torn or hand-edited one at the same time.
        for row in rows.iter().filter(|r| r.network == net) {
            let mut cfg = WalletConfig::read(&row.name)?;
            zkv::fleet::remove_manifest(net, &row.name)?;
            zkv::fleet::write_manifest(net, &row.name, &zkv::fleet::manifest_for(&cfg)?)?;
            // The placement hint pointed into the shards that are now gone.
            cfg.forget_shard()?;
        }
        ui::success(format!("Rebuilt the {} shared scan", net.name()));
    }
    ui::hint("Run `zkv sync`, or open the GUI, to start the rescan.");
    Ok(())
}
