//! `zkv shallow`: chain-window reads without the full wallet sync.
//!
//! Thin CLI over [`zkv::shallow::ShallowClient`]; see that module for the
//! pipeline and the trust model. Three subcommands:
//!
//! - `zkv shallow scan [--depth N]`: every validated update in the last N
//!   blocks (default 48 ≈ 1 hour).
//! - `zkv shallow get <key>... [--max-depth N]`: walk back from the tip until
//!   the key(s) are found and validated. Key arguments may be glob patterns
//!   (`*` wildcard, Redis `KEYS`-style); a pattern reports every matching key.
//! - `zkv shallow follow <key>... [--depth N] [--interval S]`: print the
//!   current value of each matching key, then keep polling the tip and print
//!   new validated updates as they confirm (until Ctrl-C). Patterns work here
//!   too, so `shallow follow 'prices/*'` follows every key under `prices/`.
//!
//! All three work against the current database (read-only) or, with
//! `--address zkv1…`, against a bare address with no local database at all.

use std::collections::BTreeMap;

use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use crate::{
    commands::{
        connection_args::ConnectionCliArgs,
        glob::{glob_match, has_wildcard},
    },
    data::resolve_db,
    internal::protocol::Op,
    shallow::{
        InitAnchor, ShallowClient, ShallowCursor, ShallowOptions, ShallowUpdate, ShallowWarning,
        DEFAULT_SCAN_DEPTH,
    },
};

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum OutputFormat {
    /// Human-oriented: values on stdout, metadata/warnings on stderr.
    #[default]
    Friendly,
    /// Machine-readable JSON on stdout (`scan`: one object; `get`/`follow`:
    /// one object per matching key/update per line).
    Json,
}

#[derive(Debug, Args)]
pub(crate) struct Command {
    #[command(subcommand)]
    cmd: Sub,
}

#[derive(Debug, Subcommand)]
enum Sub {
    /// All validated updates within the last N blocks.
    Scan(ScanCmd),
    /// Walk back from the tip until the given key(s) / pattern(s) resolve.
    Get(GetCmd),
    /// Print current values, then watch the tip for new updates (Ctrl-C to stop).
    Follow(FollowCmd),
}

#[derive(Debug, Args)]
struct Shared {
    /// Read this zkv address directly (no local database needed). Defaults to
    /// the current database, opened read-only.
    #[arg(long)]
    address: Option<String>,

    #[command(flatten)]
    connection: ConnectionCliArgs,

    /// Minimum confirmations for an update to count (shallow has no mempool
    /// path; 0 behaves as 1).
    #[arg(short = 'c', long, default_value_t = 3)]
    confirmations: u32,

    /// Skip the root-signed INIT anchor check (faster, weaker: state may
    /// belong to a never-initialized database).
    #[arg(long)]
    no_verify_init: bool,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Friendly)]
    output: OutputFormat,
}

#[derive(Debug, Args)]
struct ScanCmd {
    #[command(flatten)]
    shared: Shared,

    /// Window size in blocks (~75s each; 48 ≈ 1 hour).
    #[arg(long, default_value_t = DEFAULT_SCAN_DEPTH)]
    depth: u32,
}

#[derive(Debug, Args)]
struct GetCmd {
    /// The key(s) or glob pattern(s) to resolve, newest first. Quote a pattern
    /// (e.g. 'prices/*') so your shell doesn't expand it.
    #[arg(required = true)]
    keys: Vec<String>,

    #[command(flatten)]
    shared: Shared,

    /// How far back from the tip to search before giving up, in blocks
    /// (~75s each; the default 48 ≈ 1 hour keeps shallow shallow; raise it
    /// explicitly for keys updated less often).
    #[arg(long, default_value_t = DEFAULT_SCAN_DEPTH)]
    max_depth: u32,
}

#[derive(Debug, Args)]
struct FollowCmd {
    /// The key(s) or glob pattern(s) to follow. Quote a pattern (e.g.
    /// 'prices/*') so your shell doesn't expand it.
    #[arg(required = true)]
    keys: Vec<String>,

    #[command(flatten)]
    shared: Shared,

    /// Initial look-back window in blocks for the current values printed
    /// before watching begins (~75s each; 48 ≈ 1 hour).
    #[arg(long, default_value_t = DEFAULT_SCAN_DEPTH)]
    depth: u32,

    /// Poll interval in seconds (minimum 10).
    #[arg(long, default_value_t = 60)]
    interval: u64,
}

impl Command {
    pub(crate) async fn run(self, db: Option<String>, verbose: bool) -> anyhow::Result<()> {
        match self.cmd {
            Sub::Scan(c) => c.run(db, verbose).await,
            Sub::Get(c) => c.run(db, verbose).await,
            Sub::Follow(c) => c.run(db, verbose).await,
        }
    }
}

/// Build the client: `--address` is fully db-less; otherwise the current
/// database, read-only. The `# Database:` progress line is verbose-only; the
/// `--no-verify-init` notice is a real warning and always shown.
async fn client_for(
    shared: &Shared,
    db: Option<String>,
    verbose: bool,
) -> anyhow::Result<ShallowClient> {
    let connection = shared.connection.clone().into_inner();
    let client = match &shared.address {
        Some(addr) => ShallowClient::from_address(addr, &connection).await?,
        None => {
            let name = resolve_db(db.as_deref())?;
            note(
                verbose,
                format_args!("Database: {name} (shallow, read-only)"),
            );
            ShallowClient::from_db(&name, &connection).await?
        }
    };
    if shared.no_verify_init {
        eprintln!(
            "# Warning: Only use --no-verify-init with zkv1 databases addresses that you know are correct."
        );
    }
    Ok(client)
}

fn options_for(shared: &Shared) -> ShallowOptions {
    ShallowOptions {
        min_confirmations: shared.confirmations,
        verify_init: !shared.no_verify_init,
        ..ShallowOptions::default()
    }
}

impl ScanCmd {
    async fn run(self, db: Option<String>, verbose: bool) -> anyhow::Result<()> {
        let mut client = client_for(&self.shared, db, verbose).await?;
        let opts = ShallowOptions {
            depth: self.depth,
            ..options_for(&self.shared)
        };
        let state = client.scan(&opts).await?;

        match self.shared.output {
            OutputFormat::Json => println!("{}", serde_json::to_string(&state)?),
            OutputFormat::Friendly => {
                note(
                    verbose,
                    format_args!(
                        "shallow scan: blocks {}..={} (tip {}, {} update(s), {} key(s))",
                        state.scanned.from,
                        state.scanned.to,
                        state.tip,
                        state.updates.len(),
                        state.latest.len(),
                    ),
                );
                print_init_line(verbose, &state.init);
                print_warnings(&state.warnings);
                // A full window dump always labels each value with its key.
                for (key, update) in &state.latest {
                    print_value(true, key, update);
                }
            }
        }
        Ok(())
    }
}

impl GetCmd {
    async fn run(self, db: Option<String>, verbose: bool) -> anyhow::Result<()> {
        let mut client = client_for(&self.shared, db, verbose).await?;
        let opts = ShallowOptions {
            max_depth: self.max_depth,
            ..options_for(&self.shared)
        };

        // Walk back newest-first until every argument resolves: an exact key
        // resolves when it has a verified winner; a pattern resolves when at
        // least one key matches it. Patterns add a grace window below the
        // first match (DEFAULT_SCAN_DEPTH ≈ 1h) so sibling keys written a
        // little earlier are still caught; keys whose last update is older
        // than that window need `shallow scan --depth` or a full sync.
        let any_wildcard = self.keys.iter().any(|k| has_wildcard(k));
        let grace = if any_wildcard { DEFAULT_SCAN_DEPTH } else { 0 };
        if matches!(self.shared.output, OutputFormat::Friendly) {
            note(
                verbose,
                format_args!(
                    "searching back from the tip (up to {} blocks)...",
                    self.max_depth
                ),
            );
        }
        let patterns = self.keys.clone();
        let state = client
            .find_where(
                &opts,
                move |latest| {
                    patterns.iter().all(|p| {
                        if has_wildcard(p) {
                            latest.keys().any(|k| glob_match(p, k))
                        } else {
                            latest.contains_key(p)
                        }
                    })
                },
                grace,
            )
            .await?;

        let matches = matched_sorted(&state.latest, &self.keys);
        // Bare value (machine-readable, like `zkv get`) only for a single
        // exact key; otherwise label each line with its key.
        let labeled = self.keys.len() > 1 || any_wildcard;

        match self.shared.output {
            OutputFormat::Json => {
                for (key, update) in &matches {
                    println!("{}", serde_json::to_string(&KeyResult::found(key, update))?);
                }
                // Report exact keys that resolved to nothing (a pattern that
                // matched nothing has no specific key to report).
                for key in self.keys.iter().filter(|k| !has_wildcard(k)) {
                    if !matches.iter().any(|(k, _)| *k == key) {
                        println!("{}", serde_json::to_string(&KeyResult::missing(key))?);
                    }
                }
            }
            OutputFormat::Friendly => {
                print_init_line(verbose, &state.init);
                for (key, update) in &matches {
                    found_meta_line(verbose, key, update);
                    print_value(labeled, key, update);
                }
                for key in self.keys.iter().filter(|k| !has_wildcard(k)) {
                    if !matches.iter().any(|(k, _)| *k == key) {
                        note(
                            verbose,
                            format_args!(
                                "{key}: no verified update found in blocks {}..={}",
                                state.scanned.from, state.scanned.to
                            ),
                        );
                    }
                }
                print_warnings(&state.warnings);
            }
        }

        // Exit nonzero (like `zkv get`) when nothing usable came back: an
        // exact key with no value, or a pattern that matched nothing.
        let value_matches = matches.iter().filter(|(_, u)| u.value.is_some()).count();
        let exact_missing = self
            .keys
            .iter()
            .filter(|k| !has_wildcard(k))
            .any(|k| !matches.iter().any(|(mk, u)| *mk == k && u.value.is_some()));
        if value_matches == 0 || exact_missing {
            std::process::exit(1);
        }
        Ok(())
    }
}

impl FollowCmd {
    async fn run(self, db: Option<String>, verbose: bool) -> anyhow::Result<()> {
        let mut client = client_for(&self.shared, db, verbose).await?;
        let opts = ShallowOptions {
            depth: self.depth,
            ..options_for(&self.shared)
        };

        // Bootstrap: one scan to print current values and seed the cursor.
        let state = client.scan(&opts).await?;
        if matches!(self.shared.output, OutputFormat::Friendly) {
            print_init_line(verbose, &state.init);
            note(
                verbose,
                format_args!(
                    "following {} from tip {} (every {}s, Ctrl-C to stop)",
                    self.keys.join(", "),
                    state.tip,
                    self.interval.max(10),
                ),
            );
            print_warnings(&state.warnings);
        }
        for (key, update) in matched_sorted(&state.latest, &self.keys) {
            self.emit(verbose, key, update);
        }

        self.follow_loop(client, opts, state.cursor, verbose).await
    }

    /// Poll the tip and print new validated updates for the matching keys.
    /// Runs until interrupted; deliberately never exits nonzero (the whole
    /// point is to wait for updates).
    async fn follow_loop(
        &self,
        mut client: ShallowClient,
        opts: ShallowOptions,
        mut cursor: ShallowCursor,
        verbose: bool,
    ) -> anyhow::Result<()> {
        let interval = std::time::Duration::from_secs(self.interval.max(10));
        loop {
            tokio::time::sleep(interval).await;
            let state = match client.poll(&cursor, &opts).await {
                Ok(s) => s,
                Err(e) => {
                    // Transient network trouble shouldn't kill a watcher;
                    // report and retry on the next tick.
                    eprintln!("# poll failed (will retry): {e}");
                    continue;
                }
            };
            cursor = state.cursor.clone();
            note(
                verbose,
                format_args!("polled: tip {} ({} new)", state.tip, state.updates.len()),
            );
            for u in state
                .updates
                .iter()
                .filter(|u| u.verified && self.matches(&u.key))
            {
                self.emit(verbose, &u.key, u);
            }
            if matches!(self.shared.output, OutputFormat::Friendly) {
                print_warnings(&state.warnings);
            }
        }
    }

    fn matches(&self, key: &str) -> bool {
        self.keys.iter().any(|p| glob_match(p, key))
    }

    /// Print one update. Follow always labels with the key (you may be
    /// following several), matching the `key = value` form `zkv get` uses for
    /// a full dump.
    fn emit(&self, verbose: bool, key: &str, update: &ShallowUpdate) {
        match self.shared.output {
            OutputFormat::Json => {
                if let Ok(s) = serde_json::to_string(update) {
                    println!("{s}");
                }
            }
            OutputFormat::Friendly => {
                found_meta_line(verbose, key, update);
                print_value(true, key, update);
            }
        }
    }
}

/// The matching `(key, update)` pairs from a window's per-key winners, sorted
/// by key (the `latest` map is already sorted). A key matches when any of the
/// requested patterns glob-matches it; an exact key is a pattern that matches
/// only itself.
fn matched_sorted<'a>(
    latest: &'a BTreeMap<String, ShallowUpdate>,
    patterns: &[String],
) -> Vec<(&'a String, &'a ShallowUpdate)> {
    latest
        .iter()
        .filter(|(k, _)| patterns.iter().any(|p| glob_match(p, k)))
        .collect()
}

/// Print a value to stdout: `key = value` when `labeled`, else the bare value
/// (the machine-readable single-key form). A deleted winner has no value, so
/// it goes to stderr as a note instead.
fn print_value(labeled: bool, key: &str, update: &ShallowUpdate) {
    match &update.value {
        Some(v) if labeled => println!("{key} = {v}"),
        Some(v) => println!("{v}"),
        None => eprintln!("# {key}: deleted at height {}", update.height),
    }
}

/// One key's outcome in `--output json` mode (one line each).
#[derive(Serialize)]
struct KeyResult<'a> {
    key: &'a str,
    found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    update: Option<&'a ShallowUpdate>,
}

impl<'a> KeyResult<'a> {
    fn found(key: &'a str, update: &'a ShallowUpdate) -> Self {
        KeyResult {
            key,
            found: true,
            update: Some(update),
        }
    }

    fn missing(key: &'a str) -> Self {
        KeyResult {
            key,
            found: false,
            update: None,
        }
    }
}

/// A verbose-only `#` status line to stderr. Default output is just the values
/// (and trust warnings); `-v` surfaces this per-step detail. Keeps the stdout
/// machine-readable channel clean either way.
fn note(verbose: bool, args: std::fmt::Arguments<'_>) {
    if verbose {
        eprintln!("# {args}");
    }
}

fn found_meta_line(verbose: bool, key: &str, u: &ShallowUpdate) {
    note(
        verbose,
        format_args!(
            "{key}: {} at height {} ({} confirmation(s)), seq {}, signer {}",
            match u.op {
                Op::Del => "deleted",
                _ => "found",
            },
            u.height,
            u.confirmations,
            u.seq,
            u.signer.as_deref().unwrap_or("(unrecovered)"),
        ),
    );
}

fn print_init_line(verbose: bool, init: &Option<InitAnchor>) {
    match init {
        Some(a) => note(
            verbose,
            format_args!("INIT verified at height {} (txid {})", a.height, a.txid),
        ),
        None => note(verbose, format_args!("INIT not verified")),
    }
}

fn print_warnings(warnings: &[ShallowWarning]) {
    for w in warnings {
        match w {
            ShallowWarning::UnverifiedSigner {
                key,
                signer,
                height,
                ..
            } => eprintln!(
                "# warning: unverified signer {signer} wrote {key:?} at height {height} \
                 (possibly a delegated writer; run a full sync to verify)"
            ),
            ShallowWarning::SeqOrderMismatch {
                key,
                chain_winner_seq,
                max_seq,
            } => eprintln!(
                "# warning: {key:?}: chain-order winner has seq {chain_winner_seq} but the \
                 window saw seq {max_seq}; possible rebroadcast replay, full sync to be sure"
            ),
            ShallowWarning::ManagementSeen { op, height } => eprintln!(
                "# warning: {} memo at height {height} not applied (shallow cannot apply \
                 role/lifecycle changes; run a full sync)",
                op.as_str()
            ),
            ShallowWarning::Malformed {
                height,
                txid,
                detail,
            } => eprintln!("# warning: malformed zkv memo at height {height} ({txid}): {detail}"),
        }
    }
}
