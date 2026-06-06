use std::io::IsTerminal;

use clap::{Args, ValueEnum};
use serde::Serialize;

use crate::{
    commands::connection_args::ConnectionCliArgs,
    data::resolve_db,
    internal::{
        protocol::{
            AuditResult, AuditRow, HistoryEntry, HistoryResult, HistoryStatus, Op, RowOutcome,
        },
        state::{load_audit, load_history_page, load_state, HistoryOrder},
        sync::run_sync_read,
    },
};

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub(crate) enum OutputFormat {
    /// Human-readable, tab-separated lines (one per write) on stdout.
    #[default]
    Friendly,
    /// Machine-readable JSON: `{ "signer": "...", "entries": [ ... ] }`.
    Json,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub(crate) enum Order {
    /// Newest write first (default).
    #[default]
    Desc,
    /// Oldest write first (genesis INIT leads).
    Asc,
}

impl From<Order> for HistoryOrder {
    fn from(o: Order) -> Self {
        match o {
            Order::Desc => HistoryOrder::Desc,
            Order::Asc => HistoryOrder::Asc,
        }
    }
}

/// Normalise comma-separated `--op` tokens into the set of wire opcodes to
/// match. Case-insensitive; `SET` and `SETL` are synonyms (both encode the same
/// op), so either includes both. Errors on any unknown token.
fn normalize_ops(tokens: &[String]) -> anyhow::Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        if !out.iter().any(|x| x == s) {
            out.push(s.to_owned());
        }
    };
    for t in tokens {
        match t.trim().to_ascii_uppercase().as_str() {
            "" => continue,
            "SET" | "SETL" => {
                push("SET");
                push("SETL");
            }
            "DEL" => push("DEL"),
            "INIT" => push("INIT"),
            other => anyhow::bail!(
                "unknown --op {other:?}: expected a comma-separated subset of SET,SETL,DEL,INIT"
            ),
        }
    }
    Ok(out)
}

/// Rows shown when neither `--limit` nor `--all` is given. A long-lived
/// oracle accumulates tens of thousands of writes, so an unbounded default
/// would flood the terminal; this keeps the common case to a readable page.
const DEFAULT_LIMIT: u32 = 20;

#[derive(Debug, Args)]
pub(crate) struct Command {
    /// Restrict the history to keys containing this substring
    /// (case-insensitive). If omitted, every key's writes are shown.
    key: Option<String>,

    #[command(flatten)]
    connection: ConnectionCliArgs,

    /// Don't sync first; show history from the last-known memos.
    #[arg(long)]
    offline: bool,

    /// Max rows to print, newest first. Defaults to the most recent 20; pass
    /// `--all` to print the full history instead.
    #[arg(long, conflicts_with = "all")]
    limit: Option<u32>,

    /// Print the entire history, ignoring the default row cap (and `--limit`).
    #[arg(long)]
    all: bool,

    /// Rows to skip from the leading end, for paging through the history
    /// window.
    #[arg(long, default_value_t = 0)]
    offset: u32,

    /// Restrict to specific opcodes: a comma-separated subset of
    /// SET,SETL,DEL,INIT (case-insensitive; SET and SETL are synonyms). Omit
    /// to show every opcode.
    #[arg(long, value_delimiter = ',')]
    op: Vec<String>,

    /// Sort order: `desc` (newest first, default) or `asc` (oldest first).
    /// Honoured together with `--limit` (e.g. `--order asc --limit 5` is the
    /// five oldest writes).
    #[arg(long, value_enum, default_value_t = Order::Desc)]
    order: Order,

    /// Minimum confirmations for an externally-received write to appear.
    /// Self-sent writes below this still show, tagged `confirming`.
    /// `--confirmations 0` also pulls the current mempool from lightwalletd
    /// so arbitrary off-wire mempool entries appear as `pending`.
    #[arg(short = 'c', long, default_value_t = 3)]
    confirmations: u32,

    /// Output format. `friendly` (default) is human-oriented; `json` emits
    /// a stable machine-readable shape on stdout.
    #[arg(long, value_enum, default_value_t = OutputFormat::Friendly)]
    output: OutputFormat,

    /// Also show rows that replay dropped (malformed, bad signature,
    /// unauthorized, wrong-network/foreign INIT, unsupported version, etc.),
    /// each tagged with the reason. Switches to a full re-scan classification
    /// audit (every memo, applied and dropped) instead of the paginated
    /// applied-only view; `--offset` is ignored in this mode.
    #[arg(long)]
    include_invalid: bool,
}

impl Command {
    pub(crate) async fn run(self, db: Option<String>) -> anyhow::Result<()> {
        let name = resolve_db(db.as_deref())?;
        let connection = self.connection.into_inner();

        if !self.offline && !crate::commands::blocksync_skip(&name)? {
            let fetch_mempool_too = self.confirmations == 0;
            run_sync_read(&name, &connection, fetch_mempool_too).await?;
        }

        // Version gate (authoritative post-sync state): warn if the database is
        // newer than this build, and refuse on `blockread`/`blockall`. One extra
        // replay pass; `history` is an explicit, non-hot command.
        crate::commands::gate_read(
            &load_state(&name, self.confirmations, false)?.version,
            &name,
        )?;

        let ops = normalize_ops(&self.op)?;

        // `--all` prints everything (unbounded); otherwise cap at `--limit` or
        // the readable default page.
        let limit = if self.all {
            None
        } else {
            Some(self.limit.unwrap_or(DEFAULT_LIMIT))
        };

        // `--include-invalid` switches to the full re-scan classification
        // audit, which surfaces dropped rows with their reason (the paginated
        // view reads only *applied* writes from the snapshot). The op/order
        // filters are paginated-view features, so refuse the combination rather
        // than silently ignore them.
        if self.include_invalid {
            if !ops.is_empty() || !matches!(self.order, Order::Desc) {
                anyhow::bail!(
                    "--op / --order are not supported with --include-invalid (the audit view \
                     lists every memo in chain order); drop --include-invalid to use them"
                );
            }
            let audit = load_audit(&name, self.confirmations)?;
            match self.output {
                OutputFormat::Friendly => print_audit_friendly(&audit, self.key.as_deref(), limit),
                OutputFormat::Json => {
                    let json = AuditJson::from_audit(&audit, self.key.as_deref(), limit);
                    println!("{}", serde_json::to_string(&json)?);
                }
            }
            return Ok(());
        }

        let ops_filter = (!ops.is_empty()).then_some(ops.as_slice());
        let result = load_history_page(
            &name,
            self.confirmations,
            self.key.as_deref(),
            ops_filter,
            self.order.into(),
            limit,
            self.offset,
            None,
        )?;

        match self.output {
            OutputFormat::Friendly => {
                // Detect dropped (invalid) broadcasts so we can nudge the user
                // toward the audit view. The applied-only page can't see them,
                // so this costs one extra classification scan; `history` is an
                // explicit, non-hot command, and we only do it for the
                // human-facing output (JSON consumers use --include-invalid).
                let invalid = load_audit(&name, self.confirmations)
                    .map(|a| {
                        a.rows
                            .iter()
                            .filter(|r| matches!(r.outcome, RowOutcome::Dropped(_)))
                            .count()
                    })
                    .unwrap_or(0);
                print_friendly(&result, &name, invalid);
            }
            OutputFormat::Json => {
                println!("{}", serde_json::to_string(&HistoryJson::from(&result))?);
            }
        }
        Ok(())
    }
}

/// Render the history for a human.
///
/// An interactive terminal gets a grouped, colourised view (a title +
/// summary line, then an aligned table read newest-first, like `zkv roles`).
/// A piped/redirected stdout keeps the stable, scriptable tab-separated
/// record instead:
///
/// `<height|mempool>\t<OP>\t<key>\t<value>\t<status>\t<verified>\t<signer>\t<txid>`
///
/// The per-row signer is the key that actually authored that write (a
/// delegated owner/writer in a multi-signer database), or `-` when unknown
/// (pending). `invalid` is the count of dropped (invalid) broadcasts seen by
/// the classification audit; when non-zero we point the user at
/// `--include-invalid`.
fn print_friendly(result: &HistoryResult, db_name: &str, invalid: usize) {
    if std::io::stdout().is_terminal() {
        print_pretty(result, db_name, invalid);
        return;
    }
    // Machine-facing: meta/status to stderr so stdout stays a clean record.
    if result.limit.is_some() && result.total > result.entries.len() as u64 {
        eprintln!(
            "# showing {} of {} (offset {})",
            result.entries.len(),
            result.total,
            result.offset
        );
    }
    if invalid > 0 {
        eprintln!(
            "# {invalid} invalid broadcast(s) detected, run `zkv history --include-invalid` \
             to see them"
        );
    }
    if result.entries.is_empty() {
        eprintln!("(no history)");
        return;
    }
    print_raw_rows(result);
}

/// Seconds since the Unix epoch, right now (0 on a pre-epoch clock).
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A compact, human "age" for a block timestamp: `just now`, `5m ago`,
/// `3h ago`, `2d ago`, `4mo ago`, `1y ago`. Entries with no timestamp (not
/// yet mined) render as `pending`.
fn humanize_when(timestamp: Option<u32>) -> String {
    let Some(ts) = timestamp else {
        return "pending".to_owned();
    };
    let now = now_unix();
    let secs = now.saturating_sub(ts as u64);
    if secs < 60 {
        return "just now".to_owned();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    if days < 30 {
        return format!("{days}d ago");
    }
    let months = days / 30;
    if months < 12 {
        return format!("{months}mo ago");
    }
    format!("{}y ago", days / 365)
}

/// The grouped, colourised interactive view: a `zkv roles`-style title and
/// summary line, then an aligned newest-first table.
fn print_pretty(result: &HistoryResult, db_name: &str, invalid: usize) {
    use crate::ui::out;

    println!("{}", out::header(&format!("History of {db_name:?}")));

    // Summary line: how many writes (honouring an active --limit window) and
    // the database's creator.
    let shown = result.entries.len();
    let count = if result.limit.is_some() && result.total > shown as u64 {
        format!("showing {shown} of {} writes", result.total)
    } else {
        format!(
            "{} write{}",
            result.total,
            if result.total == 1 { "" } else { "s" }
        )
    };
    let creator = result.signer.trim();
    if creator.is_empty() {
        println!("{}", out::dim(&count));
    } else {
        println!("{}", out::dim(&format!("{count} · creator {creator}")));
    }
    if invalid > 0 {
        println!(
            "{}",
            out::yellow(&format!(
                "⚠ {invalid} invalid broadcast(s) hidden, run `zkv history --include-invalid` \
                 to see them"
            )),
        );
    }
    println!();

    if result.entries.is_empty() {
        println!("  {}", out::dim("(no writes yet)"));
        return;
    }

    print_table(result);
}

/// One status cell, e.g. `✓ confirmed 5`, `✗ confirmed 5`, `confirming 1/3`,
/// or `pending`, with its colour (red when the signature didn't verify, green
/// confirmed, yellow confirming, dim pending).
struct StatusCell {
    plain: String,
    color: fn(&str) -> String,
}

fn status_cell(e: &HistoryEntry) -> StatusCell {
    use crate::ui::out;
    let sym = match e.verified {
        Some(true) => "✓ ",
        Some(false) => "✗ ",
        None => "",
    };
    let (word, color): (String, fn(&str) -> String) = match &e.status {
        HistoryStatus::Confirmed { confirmations } => {
            // Past ~100 blocks the exact depth stops being interesting (it is
            // buried well beyond any reorg risk), so cap the display at `99+`.
            let depth = if *confirmations > 99 {
                "99+".to_owned()
            } else {
                confirmations.to_string()
            };
            (
                format!("confirmed {depth}"),
                out::green as fn(&str) -> String,
            )
        }
        HistoryStatus::Confirming { done, required } => (
            format!("confirming {done}/{required}"),
            out::yellow as fn(&str) -> String,
        ),
        HistoryStatus::Pending => ("pending".to_owned(), out::dim as fn(&str) -> String),
    };
    // An unverified row is always flagged red, regardless of depth.
    let color = if matches!(e.verified, Some(false)) {
        out::red as fn(&str) -> String
    } else {
        color
    };
    StatusCell {
        plain: format!("{sym}{word}"),
        color,
    }
}

/// Human-facing aligned, colourised table for an interactive terminal,
/// newest-first. Long opaque cells (keys, signer pubkeys) are abbreviated; the
/// full record is always available via the piped/raw form or `--output json`.
fn print_table(result: &HistoryResult) {
    use crate::ui::{out, short_hash};

    struct Row {
        when: String,
        op: &'static str,
        key: String,
        value: String,
        status: StatusCell,
        by: String,
        by_is_creator: bool,
    }

    let creator = result.signer.as_str();
    let rows: Vec<Row> = result
        .entries
        .iter()
        .map(|e| {
            let value = match e.value.as_deref() {
                Some("") | None => "—".to_owned(),
                Some(v) => ellipsize(v, 28),
            };
            let (by, by_is_creator) = match e.signer.as_deref() {
                Some(s) if s == creator => ("creator".to_owned(), true),
                Some(s) => (short_hash(s), false),
                None => ("—".to_owned(), false),
            };
            Row {
                when: humanize_when(e.timestamp),
                op: e.op.as_str(),
                key: ellipsize(&e.key, 28),
                value,
                status: status_cell(e),
                by,
                by_is_creator,
            }
        })
        .collect();

    // Column widths from the plain (uncoloured) cell text, including headers.
    let headers = ["WHEN", "OP", "KEY", "VALUE", "STATUS", "BY"];
    let mut w = headers.map(|h| h.chars().count());
    for r in &rows {
        w[0] = w[0].max(r.when.chars().count());
        w[1] = w[1].max(r.op.chars().count());
        w[2] = w[2].max(r.key.chars().count());
        w[3] = w[3].max(r.value.chars().count());
        w[4] = w[4].max(r.status.plain.chars().count());
        w[5] = w[5].max(r.by.chars().count());
    }

    // Header row, dimmed and indented to match the rows.
    println!(
        "  {}",
        out::dim(&format!(
            "{:<wn$}  {:<o$}  {:<k$}  {:<v$}  {:<s$}  {:<b$}",
            headers[0],
            headers[1],
            headers[2],
            headers[3],
            headers[4],
            headers[5],
            wn = w[0],
            o = w[1],
            k = w[2],
            v = w[3],
            s = w[4],
            b = w[5],
        )),
    );

    for r in &rows {
        let op = color_op(r.op, &format!("{:<w$}", r.op, w = w[1]));
        let status = (r.status.color)(&format!("{:<w$}", r.status.plain, w = w[4]));
        let by = if r.by_is_creator {
            out::cyan(&format!("{:<w$}", r.by, w = w[5]))
        } else {
            format!("{:<w$}", r.by, w = w[5])
        };
        println!(
            "  {}  {}  {}  {}  {}  {}",
            out::dim(&format!("{:<w$}", r.when, w = w[0])),
            op,
            format_args!("{:<w$}", r.key, w = w[2]),
            out::dim(&format!("{:<w$}", r.value, w = w[3])),
            status,
            by,
        );
    }
}

/// The stable machine record, one tab-separated line per write:
/// `<height|mempool>\t<OP>\t<key>\t<value>\t<status>\t<verified>\t<signer>\t<txid>`
fn print_raw_rows(result: &HistoryResult) {
    for e in &result.entries {
        let height = e
            .height
            .map(|h| h.to_string())
            .unwrap_or_else(|| "mempool".to_owned());
        let value = e.value.as_deref().unwrap_or("");
        let status = match &e.status {
            HistoryStatus::Confirmed { confirmations } => format!("confirmed:{confirmations}"),
            HistoryStatus::Confirming { done, required } => {
                format!("confirming:{done}/{required}")
            }
            HistoryStatus::Pending => "pending".to_owned(),
        };
        let verified = match e.verified {
            Some(true) => "ok",
            Some(false) => "UNVERIFIED",
            None => "pending",
        };
        let signer = e.signer.as_deref().unwrap_or("-");
        println!(
            "{height}\t{op}\t{key}\t{value}\t{status}\t{verified}\t{signer}\t{txid}",
            op = e.op.as_str(),
            key = e.key,
            txid = e.txid,
        );
    }
}

/// Truncate `s` to at most `max` characters, marking elision with `…`.
fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

/// Colour an opcode cell by family: INIT green, SET/SETL cyan, DEL yellow, the
/// management/version/finalize ops bold.
fn color_op(op: &str, padded: &str) -> String {
    use crate::ui::out;
    match op {
        "INIT" => out::green(padded),
        "SET" | "SETL" => out::cyan(padded),
        "DEL" => out::yellow(padded),
        _ => out::bold(padded),
    }
}

#[derive(Serialize)]
struct HistoryJson<'a> {
    /// The database's creator (the `INIT` signer). Per-write attribution is on
    /// each entry's `signer`.
    creator: &'a str,
    entries: Vec<HistoryEntryJson<'a>>,
}

impl<'a> From<&'a HistoryResult> for HistoryJson<'a> {
    fn from(r: &'a HistoryResult) -> Self {
        Self {
            creator: &r.signer,
            entries: r.entries.iter().map(HistoryEntryJson::from).collect(),
        }
    }
}

#[derive(Serialize)]
struct HistoryEntryJson<'a> {
    op: &'static str,
    key: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<u32>,
    txid: &'a str,
    output_index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signer: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verified: Option<bool>,
    status: StatusJson,
}

impl<'a> From<&'a HistoryEntry> for HistoryEntryJson<'a> {
    fn from(e: &'a HistoryEntry) -> Self {
        Self {
            op: e.op.as_str(),
            key: &e.key,
            value: e.value.as_deref(),
            height: e.height,
            timestamp: e.timestamp,
            txid: &e.txid,
            output_index: e.output_index,
            signature: e.signature.as_deref(),
            signer: e.signer.as_deref(),
            verified: e.verified,
            status: StatusJson::from(&e.status),
        }
    }
}

#[derive(Serialize)]
struct StatusJson {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    confirmations: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    done: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    required: Option<u32>,
}

impl From<&HistoryStatus> for StatusJson {
    fn from(s: &HistoryStatus) -> Self {
        match s {
            HistoryStatus::Confirmed { confirmations } => Self {
                kind: "confirmed",
                confirmations: Some(*confirmations),
                done: None,
                required: None,
            },
            HistoryStatus::Confirming { done, required } => Self {
                kind: "confirming",
                confirmations: None,
                done: Some(*done),
                required: Some(*required),
            },
            HistoryStatus::Pending => Self {
                kind: "pending",
                confirmations: None,
                done: None,
                required: None,
            },
        }
    }
}

/// Newest-first slice of the audit rows, filtered to keys containing
/// `key_filter` (case-insensitive; rows with no key, e.g. malformed memos,
/// are excluded when filtering) and capped at `limit`.
fn audit_rows<'a>(
    audit: &'a AuditResult,
    key_filter: Option<&str>,
    limit: Option<u32>,
) -> Vec<&'a AuditRow> {
    let mut rows: Vec<&AuditRow> = audit
        .rows
        .iter()
        .rev()
        .filter(|r| match key_filter {
            Some(f) if !f.is_empty() => r
                .key
                .as_deref()
                .map(|k| k.to_lowercase().contains(&f.to_lowercase()))
                .unwrap_or(false),
            _ => true,
        })
        .collect();
    if let Some(lim) = limit {
        rows.truncate(lim as usize);
    }
    rows
}

/// `APPLIED` / `PENDING` / `DROPPED: <reason>` for the friendly column.
fn audit_status(outcome: &RowOutcome) -> String {
    match outcome {
        RowOutcome::Applied => "APPLIED".to_owned(),
        RowOutcome::Pending => "PENDING".to_owned(),
        RowOutcome::Dropped(reason) => format!("DROPPED: {reason}"),
    }
}

/// Print the full classification audit (newest-first), one row per memo,
/// including those replay dropped. stdout stays scriptable:
///
/// `<height|mempool>\t<OP>\t<key>\t<value>\t<APPLIED|PENDING|DROPPED: reason>\t<txid>`
fn print_audit_friendly(audit: &AuditResult, key_filter: Option<&str>, limit: Option<u32>) {
    let rows = audit_rows(audit, key_filter, limit);
    let dropped = rows
        .iter()
        .filter(|r| matches!(r.outcome, RowOutcome::Dropped(_)))
        .count();
    eprintln!("# {} rows ({dropped} dropped)", rows.len());
    if rows.is_empty() {
        eprintln!("(no history)");
        return;
    }
    for r in rows {
        let height = r
            .mined_height
            .map(|h| h.to_string())
            .unwrap_or_else(|| "mempool".to_owned());
        let op = r.op.map(Op::as_str).unwrap_or("?");
        let key = r.key.as_deref().unwrap_or("");
        let value = r.value.as_deref().unwrap_or("");
        println!(
            "{height}\t{op}\t{key}\t{value}\t{status}\t{txid}",
            status = audit_status(&r.outcome),
            txid = r.txid,
        );
    }
}

#[derive(Serialize)]
struct AuditJson<'a> {
    rows: Vec<AuditRowJson<'a>>,
}

#[derive(Serialize)]
struct AuditRowJson<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
    txid: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    op: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<&'a str>,
    /// `applied` | `pending` | `dropped`.
    outcome: &'static str,
    /// Present only when `outcome == "dropped"`: the standardized reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    /// Compressed-hex of the recovered signer, when the signature was
    /// cryptographically valid (absent for malformed / bad-signature memos).
    #[serde(skip_serializing_if = "Option::is_none")]
    signer: Option<&'a str>,
    /// Whether the signature recovered to a valid signer; lets a consumer
    /// split "bad signature" from "valid signature, unauthorized signer."
    signature_valid: bool,
}

impl<'a> AuditJson<'a> {
    fn from_audit(audit: &'a AuditResult, key_filter: Option<&str>, limit: Option<u32>) -> Self {
        let rows = audit_rows(audit, key_filter, limit)
            .into_iter()
            .map(|r| {
                let (outcome, reason) = match &r.outcome {
                    RowOutcome::Applied => ("applied", None),
                    RowOutcome::Pending => ("pending", None),
                    RowOutcome::Dropped(reason) => ("dropped", Some(reason.to_string())),
                };
                AuditRowJson {
                    height: r.mined_height,
                    txid: &r.txid,
                    op: r.op.map(Op::as_str),
                    key: r.key.as_deref(),
                    value: r.value.as_deref(),
                    outcome,
                    reason,
                    signer: r.signer.as_deref(),
                    signature_valid: r.signer.is_some(),
                }
            })
            .collect();
        Self { rows }
    }
}

#[cfg(test)]
mod tests {
    use super::ellipsize;

    #[test]
    fn ellipsize_leaves_short_strings_untouched() {
        assert_eq!(ellipsize("rates/zec_usd", 28), "rates/zec_usd");
        // Exactly at the limit is not truncated.
        assert_eq!(ellipsize("abcde", 5), "abcde");
    }

    #[test]
    fn ellipsize_truncates_with_ellipsis() {
        // One over the limit: keep max-1 chars plus the ellipsis (max total).
        assert_eq!(ellipsize("abcdef", 5), "abcd…");
        let out = ellipsize("hello world, this is a long value", 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn ellipsize_respects_char_boundaries() {
        // Must not panic on multi-byte UTF-8, and counts chars (not bytes).
        let s = "日本語テスト鍵";
        let out = ellipsize(s, 3);
        assert_eq!(out.chars().count(), 3);
        assert_eq!(out, "日本…");
    }
}
