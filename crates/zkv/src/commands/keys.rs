use clap::{Args, ValueEnum};
use serde::Serialize;

use crate::{
    commands::{connection_args::ConnectionCliArgs, glob::glob_match},
    data::resolve_db,
    db::Database,
};

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub(crate) enum OutputFormat {
    /// One matching key name per line.
    #[default]
    Friendly,
    /// Machine-readable JSON: `{ "keys": [ ... ] }`.
    Json,
}

/// List key names matching a glob pattern (Redis `KEYS`-style).
///
/// Only the `*` wildcard is supported (it matches any run of characters,
/// including none); a backslash escapes it for a literal match (`\*` matches a
/// literal asterisk, `\\` a literal backslash). Every other character
/// (including `?`, `[`, `]`) is matched literally, and matching is
/// case-sensitive. The default pattern is `*` (every key).
#[derive(Debug, Args)]
pub(crate) struct Command {
    /// Glob pattern; `*` is the only wildcard. Quote it to stop your shell
    /// expanding it. Defaults to every key.
    #[arg(default_value = "*")]
    pattern: String,

    #[command(flatten)]
    connection: ConnectionCliArgs,

    /// Don't sync first; match over last-known local state.
    #[arg(long)]
    offline: bool,

    /// Minimum confirmations for a key's value to count as present.
    #[arg(short = 'c', long, default_value_t = 3)]
    confirmations: u32,

    /// Output format. `friendly` (default) is one key per line on stdout;
    /// `json` emits `{ "keys": [...] }`.
    #[arg(long, value_enum, default_value_t = OutputFormat::Friendly)]
    output: OutputFormat,
}

impl Command {
    pub(crate) async fn run(self, db: Option<String>) -> anyhow::Result<()> {
        let name = resolve_db(db.as_deref())?;
        let connection = self.connection.into_inner();
        let database = Database::open(&name, connection)?;

        if !self.offline {
            database.sync().await?;
        }

        // Pure-local read at the requested depth (merges pending.toml).
        let result = database.read(self.confirmations)?;

        // A key is "present" once it has a confirmed value. The state map is
        // a BTreeMap, so iteration is already sorted; keep matching names.
        let matches: Vec<&String> = result
            .state
            .iter()
            .filter(|(_, ks)| ks.confirmed.is_some())
            .map(|(k, _)| k)
            .filter(|k| glob_match(&self.pattern, k))
            .collect();

        match self.output {
            OutputFormat::Friendly => {
                if matches.is_empty() {
                    eprintln!("(no keys match {:?})", self.pattern);
                } else {
                    for k in matches {
                        println!("{k}");
                    }
                }
            }
            OutputFormat::Json => {
                println!("{}", serde_json::to_string(&KeysJson { keys: matches })?);
            }
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct KeysJson<'a> {
    keys: Vec<&'a String>,
}
