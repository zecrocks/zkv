use std::collections::{BTreeMap, HashSet};

use clap::{Args, ValueEnum};
use serde::Serialize;

use crate::{
    commands::connection_args::ConnectionCliArgs,
    config::{pool_label, WalletConfig},
    data::{self, resolve_db},
    internal::{
        pending,
        protocol::{InitState, KeyState, PendingOp},
        state::load_state,
        sync::run_sync_read,
    },
};

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub(crate) enum OutputFormat {
    /// Human-readable lines: `key = value`, with `# mempool:` annotations.
    #[default]
    Friendly,
    /// Machine-readable JSON. Shape: full dump → `{ "keys": { ... } }`,
    /// single key → `{ "key": "...", "value": ..., "pending": [...] }`.
    Json,
}

#[derive(Debug, Args)]
pub(crate) struct Command {
    /// A specific key to look up. If omitted, all keys are printed.
    key: Option<String>,

    #[command(flatten)]
    connection: ConnectionCliArgs,

    /// Don't sync first; replay over last-known memos.
    #[arg(long)]
    offline: bool,

    /// Error on malformed memos or invalid signatures instead of skipping them.
    #[arg(long)]
    strict: bool,

    /// Minimum confirmations required to display an externally-received memo.
    /// Self-sent writes below this still show, tagged "unconfirmed (done/required)".
    /// `--confirmations 0` additionally pulls the current mempool from
    /// lightwalletd, so arbitrary off-wire mempool entries appear as
    /// "# mempool: …". Under any `--confirmations >= 1`, only your own
    /// locally-broadcast txs surface as mempool (from `pending.toml`).
    #[arg(short = 'c', long, default_value_t = 3)]
    confirmations: u32,

    /// Output format. `friendly` (default) is human-oriented; `json` emits
    /// a stable machine-readable shape on stdout.
    #[arg(long, value_enum, default_value_t = OutputFormat::Friendly)]
    output: OutputFormat,
}

impl Command {
    pub(crate) async fn run(self, db: Option<String>) -> anyhow::Result<()> {
        let name = resolve_db(db.as_deref())?;
        if let Ok(cfg) = WalletConfig::read(&name) {
            eprintln!(
                "# Database: {name} ({}, {}, {})",
                format!("{:?}", cfg.role).to_lowercase(),
                data::Network::from(cfg.network).name(),
                pool_label(cfg.pool),
            );
        }
        let connection = self.connection.into_inner();

        if !self.offline && !crate::commands::blocksync_skip(&name)? {
            let fetch_mempool_too = self.confirmations == 0;
            run_sync_read(&name, &connection, fetch_mempool_too).await?;
        }

        let min_confs = self.confirmations;
        let mut result = load_state(&name, min_confs, self.strict)?;

        // Version gate: warn if the database is newer than this build, and
        // refuse to display state when the controlling VERSION memo blocks reads.
        crate::commands::gate_read(&result.version, &name)?;

        match &result.init {
            InitState::Uninitialized => {
                anyhow::bail!(
                    "database {name:?} is not initialized, no confirmed INIT memo at the \
                     configured confirmation depth (--confirmations={min_confs}). \
                     If you just sent funds, wait a few blocks and re-run.",
                );
            }
            InitState::Initializing { done, required } => {
                eprintln!(
                    "# initializing ({done}/{required}): INIT memo seen, waiting on confirmations",
                );
                anyhow::bail!(
                    "database {name:?} is initializing, try again after {} more blocks",
                    required.saturating_sub(*done),
                );
            }
            InitState::Initialized => {}
        }

        let local_pending = pending::load(&name).unwrap_or_default();
        let local_txids: HashSet<String> = local_pending.iter().map(|e| e.txid.clone()).collect();

        // Drop mempool ops sourced from the wire under --confirmations >= 1.
        // (Mempool == done == 0 in this layer; mined txs are reported with
        // done >= 1 by load_state.) Always keep our own broadcasts, identified
        // by txid match against pending.toml.
        if min_confs >= 1 {
            for ks in result.state.values_mut() {
                ks.pending
                    .retain(|op| op.done() >= 1 || local_txids.contains(op.txid()));
            }
            // A key may have had only a wire-mempool op; drop it if the retain
            // emptied its pending and there's no confirmed value either.
            result.state.retain(|_, ks| {
                ks.confirmed.is_some()
                    || ks
                        .pending
                        .iter()
                        .any(|op| matches!(op, PendingOp::Set { .. }))
            });
        }

        // Merge in pending.toml entries the wallet DB hasn't surfaced yet
        // (the gap between `pay()` returning and the next sync indexing the
        // tx). We collect the set of txids already represented in `result`
        // and synthesize a PendingOp for each pending.toml entry not yet
        // present. INIT entries don't appear in state; they're handled by
        // the init-status block above.
        let seen_txids: HashSet<String> = result
            .state
            .values()
            .flat_map(|ks| ks.pending.iter().map(|op| op.txid().to_owned()))
            .collect();
        for entry in &local_pending {
            if entry.op == "INIT" || seen_txids.contains(&entry.txid) {
                continue;
            }
            // Compute the synthesized op before touching `state` so a non-data
            // op (e.g. an OWNER*/WRITER* management memo whose "key" is a
            // pubkey) never inserts a phantom key.
            let op = match entry.op.as_str() {
                // "SET" and "SETL" are the two wire encodings of the same
                // semantic op; pending state doesn't care which was used.
                "SET" | "SETL" => PendingOp::Set {
                    value: entry.value.clone().unwrap_or_default(),
                    done: 0,
                    required: min_confs.max(1),
                    txid: entry.txid.clone(),
                },
                "DEL" => PendingOp::Del {
                    done: 0,
                    required: min_confs.max(1),
                    txid: entry.txid.clone(),
                },
                // Management ops confer no per-key pending state.
                _ => continue,
            };
            result
                .state
                .entry(entry.key.clone())
                .or_default()
                .pending
                .push(op);
        }

        let state = &result.state;
        match (&self.key, self.output) {
            (Some(k), OutputFormat::Friendly) => match state.get(k) {
                Some(ks) => match &ks.confirmed {
                    Some(v) => {
                        println!("{v}");
                        if let Some(line) = pending_line(k, ks) {
                            eprintln!("{line}");
                        }
                    }
                    None => {
                        // No confirmed value. Surface the pending info to stderr
                        // so the user knows something is in flight, then exit 1
                        // so stdout stays a clean signal of confirmed state.
                        if let Some(line) = pending_line(k, ks) {
                            eprintln!("{line}");
                        }
                        std::process::exit(1);
                    }
                },
                None => std::process::exit(1),
            },
            (Some(k), OutputFormat::Json) => {
                let ks = state.get(k);
                let json = SingleKeyJson::from_state(k, ks);
                println!("{}", serde_json::to_string(&json)?);
                if ks.and_then(|ks| ks.confirmed.as_ref()).is_none() {
                    std::process::exit(1);
                }
            }
            (None, OutputFormat::Friendly) => {
                if state.is_empty() {
                    eprintln!("(empty)");
                } else {
                    for (k, ks) in state {
                        if let Some(line) = pending_line(k, ks) {
                            println!("{line}");
                        }
                        if let Some(v) = &ks.confirmed {
                            println!("{k} = {v}");
                        }
                    }
                }
            }
            (None, OutputFormat::Json) => {
                let dump = FullDumpJson::from_state(state);
                println!("{}", serde_json::to_string(&dump)?);
            }
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct FullDumpJson<'a> {
    keys: BTreeMap<&'a str, KeyStateJson<'a>>,
}

impl<'a> FullDumpJson<'a> {
    fn from_state(state: &'a BTreeMap<String, KeyState>) -> Self {
        Self {
            keys: state
                .iter()
                .map(|(k, ks)| (k.as_str(), KeyStateJson::from(ks)))
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct SingleKeyJson<'a> {
    key: &'a str,
    value: Option<&'a str>,
    pending: Vec<PendingOpJson<'a>>,
}

impl<'a> SingleKeyJson<'a> {
    fn from_state(key: &'a str, ks: Option<&'a KeyState>) -> Self {
        Self {
            key,
            value: ks.and_then(|ks| ks.confirmed.as_deref()),
            pending: ks
                .map(|ks| ks.pending.iter().map(PendingOpJson::from).collect())
                .unwrap_or_default(),
        }
    }
}

#[derive(Serialize)]
struct KeyStateJson<'a> {
    value: Option<&'a str>,
    pending: Vec<PendingOpJson<'a>>,
}

impl<'a> From<&'a KeyState> for KeyStateJson<'a> {
    fn from(ks: &'a KeyState) -> Self {
        Self {
            value: ks.confirmed.as_deref(),
            pending: ks.pending.iter().map(PendingOpJson::from).collect(),
        }
    }
}

#[derive(Serialize)]
struct PendingOpJson<'a> {
    op: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<&'a str>,
    done: u32,
    required: u32,
    txid: &'a str,
}

impl<'a> From<&'a PendingOp> for PendingOpJson<'a> {
    fn from(op: &'a PendingOp) -> Self {
        match op {
            PendingOp::Set {
                value,
                done,
                required,
                txid,
            } => Self {
                op: "SET",
                value: Some(value.as_str()),
                done: *done,
                required: *required,
                txid: txid.as_str(),
            },
            PendingOp::Del {
                done,
                required,
                txid,
            } => Self {
                op: "DEL",
                value: None,
                done: *done,
                required: *required,
                txid: txid.as_str(),
            },
        }
    }
}

/// Format the pending-annotation comment line for a key, if it has any
/// pending ops. `done == 0` means mempool (no block yet); higher values mean
/// mined but below the caller's `--confirmations` threshold.
fn pending_line(key: &str, ks: &KeyState) -> Option<String> {
    if ks.pending.len() > 1 {
        Some(format!("# multiple pending: {key}"))
    } else {
        ks.pending.first().map(|op| match op {
            PendingOp::Set { value, done: 0, .. } => {
                format!("# mempool: {key} = {value}")
            }
            PendingOp::Set {
                value,
                done,
                required,
                ..
            } => {
                format!("# unconfirmed ({done}/{required}): {key} = {value}")
            }
            PendingOp::Del { done: 0, .. } => {
                format!("# mempool: {key} → (deleted)")
            }
            PendingOp::Del { done, required, .. } => {
                format!("# unconfirmed ({done}/{required}): {key} → (deleted)")
            }
        })
    }
}
