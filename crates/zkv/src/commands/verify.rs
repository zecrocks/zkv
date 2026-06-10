use std::io::Read as _;

use clap::{Args, ValueEnum};
use serde::Serialize;

use crate::{
    commands::connection_args::ConnectionCliArgs,
    data::resolve_db,
    db::Database,
    internal::protocol::{
        self, parse_zkv_addr, pubkey_bech32, receiver_domain, zkv_verifying_pubkey, MemoReject,
        MemoVerification, Op, RowOutcome,
    },
    ui,
};

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub(crate) enum OutputFormat {
    /// Human-readable, styled report on stderr.
    #[default]
    Friendly,
    /// Machine-readable JSON on stdout.
    Json,
}

/// `zkv verify [MEMO]`: verify the signature on a raw zkv memo.
///
/// Two modes:
///
/// - **Against an imported database** (the default, the current database, or
///   `--db`): verifies *everything*. The signature must recover to a signer,
///   that signer must be authorized for the op, and the write must be in order
///   (replay / version protection) against the database's current confirmed
///   state.
/// - **Against a raw address** (`--address <zkv1…>`): a database we haven't
///   imported. Verifies only that the *signature* is valid and recovers the
///   signer. It explicitly does **not** verify that the signer is authorized to
///   write, nor that the write occurred in the correct order; that needs the
///   database's replayed on-chain state.
///
/// The memo is taken from the positional argument, or read from stdin when it
/// is omitted or `-` (handy for the multi-line wire form).
#[derive(Debug, Args)]
pub(crate) struct Command {
    /// The raw memo text (the two-line `ZKV0 …` wire form). Omit, or pass `-`,
    /// to read it from stdin.
    memo: Option<String>,

    /// Verify against this zkv address rather than an imported database. Checks
    /// the signature only, not authorization or ordering (we have no synced
    /// state for an address we haven't imported).
    #[arg(long)]
    address: Option<String>,

    #[command(flatten)]
    connection: ConnectionCliArgs,

    /// Don't sync first; verify against the last-known confirmed state
    /// (imported-database mode only).
    #[arg(long)]
    offline: bool,

    /// Minimum confirmations a write must have to count toward the state the
    /// memo is verified against (imported-database mode only).
    #[arg(short = 'c', long, default_value_t = 3)]
    confirmations: u32,

    /// Output format: `friendly` (default, styled) or `json`.
    #[arg(long, value_enum, default_value_t = OutputFormat::Friendly)]
    output: OutputFormat,
}

impl Command {
    pub(crate) async fn run(self, db: Option<String>) -> anyhow::Result<()> {
        let memo = read_memo(self.memo.as_deref())?;
        let imported = self.address.is_none();

        // The inner `Result` is the *verification verdict*: a memo (or address)
        // that didn't parse far enough to verify is a normal "verification
        // failed" outcome reported with the same styling, not a program error.
        // The outer `?` covers only genuine I/O / sync / db failures.
        let outcome = if let Some(address) = self.address.as_deref() {
            self.verify_raw(address, &memo)
        } else {
            self.verify_imported(db, &memo).await?
        };

        match outcome {
            Ok(v) => {
                match self.output {
                    OutputFormat::Json => print_json(&v, imported),
                    OutputFormat::Friendly => report(&v, imported),
                }
                // A verify tool must be scriptable: exit non-zero when the memo
                // doesn't hold up. The report already explains why.
                if !verified_ok(&v) {
                    std::process::exit(1);
                }
            }
            Err(fail) => {
                match self.output {
                    OutputFormat::Json => print_fail_json(&fail),
                    OutputFormat::Friendly => report_fail(&fail),
                }
                std::process::exit(1);
            }
        }
        Ok(())
    }

    /// Signature-only verification against a zkv address we haven't imported. A
    /// malformed `--address` is surfaced as a styled verification failure, the
    /// same as a malformed memo, not a raw error.
    fn verify_raw(&self, address: &str, memo: &str) -> Result<MemoVerification, VerifyFail> {
        let parsed = parse_zkv_addr(address).map_err(VerifyFail::bad_input)?;
        let receiver = receiver_domain(&parsed.ufvk, parsed.pool, parsed.network)
            .map_err(VerifyFail::bad_input)?;
        let root =
            pubkey_bech32(&zkv_verifying_pubkey(&parsed.ufvk).map_err(VerifyFail::bad_input)?);
        protocol::verify_signature(memo, &receiver, &root).map_err(VerifyFail::Reject)
    }

    /// Full verification (signature + authorization + ordering) against an
    /// imported database's replayed state.
    async fn verify_imported(
        &self,
        db: Option<String>,
        memo: &str,
    ) -> anyhow::Result<Result<MemoVerification, VerifyFail>> {
        let name = resolve_db(db.as_deref())?;
        let connection = self.connection.clone().into_inner();
        let database = Database::open(&name, connection)?;

        if !self.offline && !crate::commands::blocksync_skip(&name)? {
            database.sync().await?;
        }
        crate::commands::gate_read(&database.version(self.confirmations)?, &name)?;

        let state = database.read(self.confirmations)?;
        let receiver = database.receiver()?;
        let root = database.signer()?;
        Ok(protocol::verify_memo(memo, &receiver, &root, &state).map_err(VerifyFail::Reject))
    }
}

/// Why a memo couldn't be verified at all (vs. a parsed-but-rejected verdict).
enum VerifyFail {
    /// The memo didn't parse into a signed command (foreign / malformed / newer
    /// protocol version).
    Reject(MemoReject),
    /// Bad input that isn't the memo (e.g. a malformed `--address`).
    BadInput(String),
}

impl VerifyFail {
    fn bad_input(e: anyhow::Error) -> Self {
        VerifyFail::BadInput(e.to_string())
    }
}

/// Read the memo from the argument, or stdin when omitted / `-`.
fn read_memo(arg: Option<&str>) -> anyhow::Result<String> {
    match arg {
        Some(s) if s != "-" => Ok(s.to_owned()),
        _ => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            if buf.trim().is_empty() {
                anyhow::bail!(
                    "no memo provided: pass the raw memo as an argument or pipe it on stdin"
                );
            }
            Ok(buf)
        }
    }
}

/// Whether the verification passed: the signature recovered and (when verified
/// against an imported database) the write would be applied.
fn verified_ok(v: &MemoVerification) -> bool {
    v.signature_valid
        && v.outcome
            .as_ref()
            .is_none_or(|o| matches!(o, RowOutcome::Applied))
}

/// A friendly, actionable description of why a memo couldn't be verified at
/// all; it never reached a signature, so there's nothing to check.
fn fail_message(f: &VerifyFail) -> String {
    match f {
        VerifyFail::BadInput(msg) => msg.clone(),
        VerifyFail::Reject(MemoReject::NotZkv) => {
            "this isn't a zkv memo: it doesn't start with the `ZKV0` wire marker".to_owned()
        }
        VerifyFail::Reject(MemoReject::Malformed(fmt)) => {
            format!("this is a zkv memo but it's malformed: {fmt}")
        }
        VerifyFail::Reject(MemoReject::UnsupportedVersion(n)) => format!(
            "this memo is from a newer zkv protocol version ({n}) than this build understands; \
             update zkv"
        ),
    }
}

/// Styled report for a memo (or address) that couldn't be verified at all.
fn report_fail(f: &VerifyFail) {
    eprintln!();
    eprintln!("  {}", ui::bold("zkv verify"));
    eprintln!();
    ui::failure(format!("Cannot verify: {}", fail_message(f)));
    eprintln!();
}

/// Machine-readable shape for a verification that couldn't be performed.
fn print_fail_json(f: &VerifyFail) {
    let json = serde_json::json!({
        "signature_valid": false,
        "error": fail_message(f),
    });
    println!("{json}");
}

/// The label for the wire key, by opcode family.
fn key_label(op: Op) -> &'static str {
    match op {
        Op::Init => "Address",
        Op::OwnerAdd | Op::OwnerDel | Op::WriterAdd | Op::WriterDel => "Target",
        Op::Version => "Version",
        _ => "Key",
    }
}

/// Print the styled human report to stderr.
fn report(v: &MemoVerification, imported: bool) {
    eprintln!();
    eprintln!("  {}", ui::bold("zkv verify"));
    eprintln!();

    // Aligned detail block. Labels are dimmed; only relevant rows are shown.
    let mut rows: Vec<(&str, String)> = vec![("Op", v.op.as_str().to_owned())];
    if !v.key.is_empty() {
        rows.push((key_label(v.op), v.key.clone()));
    }
    if let Some(value) = &v.value {
        rows.push(("Value", value.clone()));
    }
    if v.seq != 0 {
        rows.push(("Sequence", v.seq.to_string()));
    }
    match &v.signer {
        Some(signer) => {
            let mut s = signer.clone();
            if v.is_root == Some(true) {
                s.push_str(&ui::dim("  · database root key"));
            }
            rows.push(("Signer", s));
        }
        None => rows.push(("Signer", ui::dim("(unrecoverable)").to_string())),
    }
    let width = rows.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
    for (label, value) in &rows {
        eprintln!("  {}  {value}", ui::dim(&format!("{label:<width$}")));
    }
    eprintln!();

    // Signature verdict.
    if v.signature_valid {
        ui::success("Signature valid: the message verifies");
    } else {
        ui::failure(
            "Signature invalid: does not recover to any signer over this database's signing domain",
        );
    }

    if imported {
        report_imported(v);
    } else {
        report_raw(v);
    }
    eprintln!();
}

/// The authorization/ordering verdict for an imported database.
fn report_imported(v: &MemoVerification) {
    // Only meaningful once the signature recovered; a bad signature already
    // failed above and has no signer to authorize.
    if !v.signature_valid {
        return;
    }
    match &v.outcome {
        Some(RowOutcome::Applied) => {
            ui::success("Authorized and in order: this write would be applied");
        }
        Some(RowOutcome::Dropped(reason)) => {
            ui::failure(format!("Would be rejected: {reason}"));
        }
        // verify_memo judges the memo as a confirmed write, so Pending/None
        // don't arise; report defensively rather than claim success.
        _ => ui::warn("Could not determine authorization for this write"),
    }
}

/// The "signature only" caveats for raw-address mode.
fn report_raw(v: &MemoVerification) {
    if !v.signature_valid {
        return;
    }
    ui::warn("Signature only. This does NOT verify:");
    ui::hint("• that the signer is authorized to write");
    ui::hint("• that the write is in order (replay / version protection)");
    ui::hint("Import the database to check those: zkv watch <zkv-address>");
}

#[derive(Serialize)]
struct VerifyJson<'a> {
    op: &'static str,
    key: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<&'a str>,
    seq: u64,
    /// Verification mode: `imported` (full) or `signature-only` (raw address).
    mode: &'static str,
    signature_valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    signer: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_root: Option<bool>,
    /// Present only in `imported` mode: whether the write would be applied
    /// (authorized and in order).
    #[serde(skip_serializing_if = "Option::is_none")]
    applied: Option<bool>,
    /// Present only in `imported` mode when the write would be dropped: why.
    #[serde(skip_serializing_if = "Option::is_none")]
    rejected_reason: Option<String>,
}

/// Print the stable machine-readable shape on stdout.
fn print_json(v: &MemoVerification, imported: bool) {
    let (applied, rejected_reason) = match (imported, &v.outcome) {
        (true, Some(RowOutcome::Applied)) => (Some(true), None),
        (true, Some(RowOutcome::Dropped(reason))) => (Some(false), Some(reason.to_string())),
        (true, _) => (Some(false), None),
        (false, _) => (None, None),
    };
    let json = VerifyJson {
        op: v.op.as_str(),
        key: &v.key,
        value: v.value.as_deref(),
        seq: v.seq,
        mode: if imported {
            "imported"
        } else {
            "signature-only"
        },
        signature_valid: v.signature_valid,
        signer: v.signer.as_deref(),
        is_root: v.is_root,
        applied,
        rejected_reason,
    };
    match serde_json::to_string(&json) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("failed to serialize verify result: {e}"),
    }
}
