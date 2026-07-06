use std::io::IsTerminal;
use std::time::{Duration, Instant};

use anyhow::anyhow;
use bip0039::{Count, English, Mnemonic};
use clap::Args;
use secrecy::{SecretVec, Zeroize};
use zcash_client_backend::data_api::{wallet::ConfirmationsPolicy, WalletRead, WalletWrite};
use zcash_protocol::ShieldedProtocol;

use crate::{
    commands::connection_args::ConnectionCliArgs,
    config::{parse_pool, WalletConfig},
    data::{db_dir, get_db_paths, init_dbs, open_wallet_db, Network},
    internal::{
        protocol::{encode_zkv_addr, zkv_verifying_pubkey, InitState},
        state::{load_state, INIT_CONFIRMATIONS},
        sync::run_sync_with_status,
        write::{broadcast_init, prepare_init},
    },
    remote::ConnectionArgs,
    ui,
};

/// Default database name when none is supplied.
const DEFAULT_DB: &str = "default";

/// Minimum funding for a new wallet before INIT can broadcast (in the
/// chain's smallest unit). Covers the ZIP-317 fee for a single-output
/// shielded transaction (~0.00005 ZEC) with headroom. The memo output is
/// zero-value, so the wallet only spends the fee.
const MIN_INIT_ZATS: u64 = 10_000;

/// Re-sync + retry cadence inside the init poll loop.
const POLL_INTERVAL: Duration = Duration::from_secs(15);

/// Default cap on how long `zkv init` will block waiting for funds + INIT
/// confirmation. Overridable via `--init-timeout`.
const DEFAULT_INIT_TIMEOUT_SECS: u64 = 30 * 60;

#[derive(Debug, Args)]
pub(crate) struct Command {
    /// Database name. Defaults to "default".
    pub(crate) name: Option<String>,

    /// Network: "mainnet" (default), "testnet", or "regtest".
    #[arg(long, default_value = "mainnet", value_parser = Network::parse)]
    pub(crate) network: Network,

    /// Shielded pool for this database: "orchard" (default) or "sapling".
    /// Fixed at creation; every memo is read from and written to this pool.
    #[arg(long, default_value = "orchard", value_parser = parse_pool)]
    pub(crate) pool: ShieldedProtocol,

    /// Skip the type-it-back confirmation prompt AND the funding/INIT poll
    /// loop. Prints the funding instructions (no QR), the exact INIT memo
    /// text, and a machine-readable summary; exits 0. Re-run `zkv init <name>`
    /// later (once funded) to detect the funds and broadcast INIT.
    #[arg(long)]
    pub(crate) non_interactive: bool,

    /// Print the 24-word recovery phrase to stdout in `--non-interactive`
    /// mode. The phrase is full spending authority, so it is hidden by
    /// default: without this flag `zkv init --non-interactive` prints only the
    /// address (the seed stays wrapped under the data dir), so redirecting
    /// stdout to a file can't silently capture the seed. Pass this only when
    /// you mean to record the phrase (e.g. piping into a password manager),
    /// and treat the output like a private key.
    #[arg(long)]
    pub(crate) dangerously_show_seed_phrase: bool,

    /// In interactive mode, skip the write-the-phrase-back verification
    /// step. The recovery phrase is still printed so you can record it,
    /// and you'll still be prompted to press Enter when you've done so.
    /// Useful if you copied the phrase to a password manager and trust
    /// that; saves the friction of writing it back.
    #[arg(long)]
    pub(crate) no_verify: bool,

    /// Max seconds to wait for funds + INIT confirmation in interactive mode.
    /// On timeout, exit with instructions to finalize later via `zkv init`.
    #[arg(long, default_value_t = DEFAULT_INIT_TIMEOUT_SECS)]
    pub(crate) init_timeout: u64,

    #[command(flatten)]
    pub(crate) connection: ConnectionCliArgs,
}

impl Command {
    pub(crate) async fn run(self, db: Option<String>) -> anyhow::Result<()> {
        // Resolve the target database: an explicit positional name wins, then
        // the global `--db` flag, then the current-db marker, and only a truly
        // fresh setup (no name, no flag, no current db) falls back to
        // "default". This makes a bare `zkv init` after `zkv use <name>` act on
        // that database rather than silently targeting "default" (and possibly
        // broadcasting its INIT on the wrong network).
        let name = match self.name.clone() {
            Some(n) => n,
            None => db
                .or(crate::data::current_db()?)
                .unwrap_or_else(|| DEFAULT_DB.to_owned()),
        };
        let params = self.network;

        let dir = db_dir(&name)?;
        if dir.join("keys.toml").exists() {
            // The database already exists locally. Don't recreate it; resume:
            // sync, confirm it isn't already initialized/finalized on-chain,
            // and (if still uninitialized + funded) broadcast INIT. This is the
            // canonical "finalize a funded-but-uninitialized database" path now
            // that `zkv sync` is read-only.
            return self.resume(&name).await;
        }

        // Mnemonic ceremony first; user sees their phrase even if the network is down.
        let mnemonic = Mnemonic::generate(Count::Words24);
        if !self.non_interactive {
            ceremony(&mnemonic, &name, self.network, self.no_verify)?;
        }

        let connection = self.connection.into_inner();

        // Pin the wallet birthday at tip − safety buffer: a brand-new wallet has
        // no history before now. Refuses a stale/unreachable tip so the birthday
        // is never anchored to a stale view of the chain.
        let mut client = connection.connect(params).await?;
        let birthday = crate::internal::sync::near_tip_birthday(&mut client, params).await?;

        WalletConfig::init_admin(&name, &mnemonic, birthday.height(), params, self.pool)?;

        let seed = {
            let mut s = mnemonic.to_seed("");
            let secret = s.to_vec();
            s.zeroize();
            SecretVec::new(secret)
        };
        let mut db_data = init_dbs(params, &name)?;
        db_data.create_account(&name, &seed, &birthday, None)?;

        crate::demo::promote_current(&name)?;

        let ids = zcash_client_backend::data_api::WalletRead::get_account_ids(&db_data)?;
        let account = zcash_client_backend::data_api::WalletRead::get_account(&db_data, ids[0])?
            .ok_or_else(|| anyhow!("account vanished"))?;
        let ufvk = zcash_client_backend::data_api::Account::ufvk(&account)
            .ok_or_else(|| anyhow!("no UFVK"))?;
        zkv_verifying_pubkey(ufvk)?;
        let zkv_addr = encode_zkv_addr(ufvk, &params, self.pool, u32::from(birthday.height()))?;
        drop(db_data);

        // Build the signed INIT memo + recipient UA up-front so we can display
        // both regardless of which path the user takes (auto-poll vs. manual).
        let prepared = prepare_init(&name)?;
        debug_assert_eq!(prepared.zkv_addr, zkv_addr);

        if self.non_interactive {
            print_funding_instructions(
                &name,
                self.network,
                &zkv_addr,
                &prepared.recipient_ua,
                &prepared.memo_text,
                /* with_qr = */ false,
            );
            // Machine-readable summary. The recovery phrase is full spending
            // authority, so it is emitted to stdout ONLY when the operator
            // explicitly opts in via --dangerously-show-seed-phrase: otherwise
            // a plain `init … > file` redirect would silently capture the seed
            // into a possibly world-readable file. The address line is always
            // safe to print.
            if self.dangerously_show_seed_phrase {
                println!("# BACK THIS UP");
                println!("zkv_wallet_seed=\"{}\"", mnemonic.phrase());
            }
            println!("zkv_database=\"{zkv_addr}\"");
            if !self.dangerously_show_seed_phrase {
                eprintln!();
                eprintln!("{}", ui::yellow("Recovery phrase not shown."));
                eprintln!(
                    "It is the only portable backup of write access. Pass \
                     --dangerously-show-seed-phrase at\ncreation to print the 24 \
                     words; otherwise keep this machine's data directory safe (the\nseed \
                     is stored there, wrapped), as it is now your only backup."
                );
            }
            return Ok(());
        }

        ui::success(format!(
            "Created database {:?} ({}, birthday {})",
            name,
            self.network.name(),
            u32::from(birthday.height()),
        ));
        eprintln!();
        eprintln!("{}", ui::bold("Your zkv address (share with readers):"));
        println!("  {zkv_addr}");

        print_funding_instructions(
            &name,
            self.network,
            &zkv_addr,
            &prepared.recipient_ua,
            &prepared.memo_text,
            /* with_qr = */ true,
        );

        let timeout = Duration::from_secs(self.init_timeout);
        poll_for_init(&name, self.network, &connection, timeout).await
    }

    /// Resume initialization of a database that already exists on disk but
    /// hasn't been initialized on-chain yet (e.g. a `--non-interactive` create,
    /// or an interactive run that timed out / was Ctrl-C'd before INIT
    /// confirmed). Always syncs first: an INIT (or FINALIZE) may already have
    /// landed, so we never broadcast against stale local state.
    async fn resume(self, name: &str) -> anyhow::Result<()> {
        let cfg = WalletConfig::read(name)?;
        if cfg.role != crate::config::Role::Admin {
            anyhow::bail!(
                "database {name:?} already exists and is watch-only, it has no spending key, \
                 so it cannot broadcast INIT"
            );
        }
        let network = cfg.network;
        let connection = self.connection.into_inner();

        eprintln!("Database {name:?} already exists; checking whether it needs initialization…");

        // Sync before deciding anything: the on-chain state is authoritative.
        run_sync_with_status(name, &connection, false).await?;
        let result = load_state(name, INIT_CONFIRMATIONS, false)?;
        if let Some(warning) = result.version.upgrade_warning() {
            eprintln!("warning: {warning}");
        }

        if result.finalized {
            anyhow::bail!("database {name:?} is finalized; no further writes are possible");
        }
        match result.init {
            InitState::Initialized => {
                eprintln!("✓ Database {name:?} is already initialized; nothing to do.");
                return Ok(());
            }
            InitState::Initializing { done, required } => {
                if self.non_interactive {
                    eprintln!(
                        "INIT in flight: {done}/{required} confirmation(s). Re-run `zkv init \
                         {name}` to wait for it to confirm.",
                    );
                    return Ok(());
                }
                eprintln!(
                    "# initializing ({done}/{required}): waiting for {} more block(s)…",
                    required.saturating_sub(done),
                );
            }
            InitState::Uninitialized => {
                // Rebuild and show the funding instructions so the user knows
                // where to send funds + the exact INIT memo. Skip the QR code
                // when the wallet is already funded: the poll loop will
                // broadcast INIT immediately, so there's nothing to scan.
                let already_funded = init_balance(name)?.spendable >= MIN_INIT_ZATS;
                let prepared = prepare_init(name)?;
                print_funding_instructions(
                    name,
                    network,
                    &prepared.zkv_addr,
                    &prepared.recipient_ua,
                    &prepared.memo_text,
                    /* with_qr = */ !self.non_interactive && !already_funded,
                );
                if self.non_interactive {
                    eprintln!("Fund the wallet, then re-run `zkv init {name}` to broadcast INIT.",);
                    return Ok(());
                }
            }
        }

        let timeout = Duration::from_secs(self.init_timeout);
        poll_for_init(name, network, &connection, timeout).await
    }
}

fn ceremony(
    mnemonic: &Mnemonic,
    db_name: &str,
    network: Network,
    skip_verify: bool,
) -> anyhow::Result<()> {
    eprintln!(
        "{}",
        ui::bold(&format!(
            "Creating {} zkv database {db_name:?} at {}",
            network.name(),
            db_dir(db_name)?.display(),
        )),
    );
    eprintln!();
    eprintln!(
        "{}",
        ui::yellow("Recovery phrase, write these 24 words down NOW.")
    );
    ui::hint("They are the only way to recover this wallet if you lose this machine.");
    eprintln!();

    let words: Vec<&str> = mnemonic.phrase().split_whitespace().collect();
    if words.len() != 24 {
        anyhow::bail!("expected 24 words, got {}", words.len());
    }
    // Single line, single-space separated; easiest possible copy-paste
    // (triple-click on most terminals selects the whole logical line even
    // if it wraps in the viewport).
    eprintln!("  {}", ui::bold(&words.join(" ")));
    eprintln!();
    eprintln!("{}", ui::dim("Press Enter when you've written them down."));

    use std::io::BufRead;
    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;

    if skip_verify {
        eprintln!();
        ui::success("Phrase recorded (verification skipped via --no-verify).");
        return Ok(());
    }

    eprintln!();
    eprintln!("{}", ui::bold("Now write the phrase back to confirm:"));

    // Accept the phrase across one or more lines so a copy-paste of the
    // grid above (which spans 6 lines) works as well as a single-line
    // re-type.
    let mut accumulated = String::new();
    loop {
        let mut buf = String::new();
        let n = stdin.lock().read_line(&mut buf)?;
        if n == 0 {
            // EOF before we collected 24 words.
            anyhow::bail!("phrase entry ended early, please re-run `zkv init` and try again.",);
        }
        accumulated.push_str(&buf);
        if accumulated.split_whitespace().count() >= 24 {
            break;
        }
    }

    let normalize = |s: &str| {
        s.to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    if normalize(&accumulated) != normalize(mnemonic.phrase()) {
        anyhow::bail!("phrase mismatch, please re-run `zkv init` and try again.");
    }
    let _: Mnemonic<English> = Mnemonic::from_phrase(normalize(&accumulated))
        .map_err(|e| anyhow!("recovery phrase did not validate: {e}"))?;
    // The re-typed phrase is a seed equivalent; wipe the owned copy.
    accumulated.zeroize();
    eprintln!();
    ui::success("Confirmed.");
    Ok(())
}

fn print_funding_instructions(
    db_name: &str,
    network: Network,
    zkv_addr: &str,
    funding_ua: &str,
    memo_text: &str,
    with_qr: bool,
) {
    eprintln!();
    eprintln!("{}", ui::bold("Step 1: fund the wallet"));
    eprintln!();
    eprintln!(
        "  Send at least {} {} ({} network) to:",
        format_zec_decimal(MIN_INIT_ZATS),
        network.ticker(),
        network.name(),
    );
    println!("    {funding_ua}");

    if with_qr {
        if let Some(qr) = render_qr_for_tty(funding_ua) {
            eprintln!();
            for line in qr.lines() {
                eprintln!("    {line}");
            }
        }
    }

    if matches!(network, Network::Test) {
        eprintln!();
        ui::hint("No TAZ? Run `zkv gui-browser` and use the GUI to pull testnet funds");
        ui::hint("from our faucet.");
    }

    eprintln!();
    ui::hint("Alternative (cold-wallet flow): send a zero-value shielded transaction to that");
    ui::hint("address from any wallet, attaching this exact text memo:");
    eprintln!();
    eprintln!("{}", ui::dim("--- begin INIT memo ---"));
    // Memo body goes to stdout unindented so it's directly pasteable into
    // another wallet (and grep-friendly when piping).
    println!("{memo_text}");
    eprintln!("{}", ui::dim("--- end INIT memo ---"));
    eprintln!();
    let _ = (db_name, zkv_addr); // reserved for future error reporting
}

/// Render `data` as a Unicode QR code suitable for a dark-background terminal,
/// or `None` when stderr isn't a TTY or the code would be too wide to scan.
pub(crate) fn render_qr_for_tty(data: &str) -> Option<String> {
    use qrcode::render::unicode::Dense1x2;
    use qrcode::QrCode;

    if !std::io::stderr().is_terminal() {
        return None;
    }
    let (terminal_size::Width(cols), terminal_size::Height(rows)) = terminal_size::terminal_size()?;

    let qr = QrCode::new(data.as_bytes()).ok()?;
    let rendered = qr
        .render::<Dense1x2>()
        // Invert so the QR is readable on dark-background terminals:
        // dark modules render as the half-block (light glyph), light
        // modules as space.
        .dark_color(Dense1x2::Light)
        .light_color(Dense1x2::Dark)
        .build();

    // Width check only: if the QR would wrap, it's unscannable. The row
    // height (~25–30 for a Zcash UA) often exceeds the default 24-row
    // terminal, but the user can scroll back to it before scanning, so we
    // don't require it to fit vertically all at once.
    let qr_cols = rendered
        .lines()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0) as u16;
    if qr_cols.saturating_add(2) > cols {
        return None;
    }
    let _ = rows;
    Some(rendered)
}

fn format_zec_decimal(zats: u64) -> String {
    // 1 ZEC/TAZ = 1e8 base units. Show enough precision for sub-cent values.
    let whole = zats / 100_000_000;
    let frac = zats % 100_000_000;
    if frac == 0 {
        format!("{whole}")
    } else {
        format!("{whole}.{:08}", frac)
            .trim_end_matches('0')
            .to_owned()
    }
}

/// What the poll loop is currently waiting on. Used to dedup status output
/// so we only print when this changes (or when a count inside it advances).
#[derive(Debug, Clone, PartialEq, Eq)]
enum PollPhase {
    /// No funds detected yet (neither spendable nor pending).
    WaitingForFunds,
    /// Funds received but not yet spendable, usually waiting on the wallet's
    /// confirmations policy. `pending` is the gross pending amount (in the
    /// chain's smallest unit).
    WaitingForMaturity { pending: u64 },
    /// INIT broadcast succeeded; waiting for the wallet to index its own
    /// in-flight tx so we see it as `Initializing`.
    WaitingForIndex { txid: String },
    /// INIT visible on-chain at depth `done`, threshold `required`.
    Initializing { done: u32, required: u32 },
}

/// Max consecutive polls we'll spend in `WaitingForIndex` before deciding the
/// broadcast was lost (mempool eviction etc.) and re-attempting.
const REBROADCAST_AFTER_POLLS: u32 = 6;

async fn poll_for_init(
    db_name: &str,
    network: Network,
    connection: &ConnectionArgs,
    timeout: Duration,
) -> anyhow::Result<()> {
    let start = Instant::now();
    let mut last_phase: Option<PollPhase> = None;
    let mut pending_init_txid: Option<String> = None;
    let mut waiting_for_index_polls: u32 = 0;

    eprintln!(
        "{}",
        ui::bold(&format!(
            "Step 2: waiting for funds + INIT confirmation (timeout {}s)",
            timeout.as_secs(),
        )),
    );
    ui::hint("Ctrl-C to exit; re-run `zkv init` later to finalize.");
    eprintln!();

    loop {
        if start.elapsed() > timeout {
            anyhow::bail!(
                "timed out after {}s waiting for INIT confirmation. Re-run `zkv init` once \
                 the wallet is funded; INIT will broadcast then.",
                timeout.as_secs(),
            );
        }

        run_sync_with_status(db_name, connection, false).await?;

        let result = load_state(db_name, INIT_CONFIRMATIONS, false)?;
        match result.init {
            InitState::Initialized => {
                eprintln!();
                ui::success("INIT confirmed.");
                ui::hint("Database is now initialized.");
                ui::hint(
                    "Note: readers using --confirmations=3 will see this database as \
                     `initializing (1/3)` until 2 more blocks land.",
                );
                ui::hint("Next: `zkv show`, then `zkv set k v` once those 2 blocks land.");
                return Ok(());
            }

            InitState::Initializing { done, required } => {
                // We've moved past the index race; clear that bookkeeping.
                pending_init_txid = None;
                waiting_for_index_polls = 0;
                let phase = PollPhase::Initializing { done, required };
                if last_phase.as_ref() != Some(&phase) {
                    ui::info(format!(
                        "initializing ({done}/{required}): waiting for {} more block(s)…",
                        required.saturating_sub(done),
                    ));
                    last_phase = Some(phase);
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }

            InitState::Uninitialized => {
                if let Some(ref txid) = pending_init_txid {
                    // We already broadcast; don't try again while the wallet
                    // is still indexing its own tx.
                    waiting_for_index_polls += 1;
                    if waiting_for_index_polls >= REBROADCAST_AFTER_POLLS {
                        ui::info(format!(
                            "broadcast tx {} not visible after {} polls; will re-attempt",
                            short_txid(txid),
                            waiting_for_index_polls,
                        ));
                        pending_init_txid = None;
                        waiting_for_index_polls = 0;
                        last_phase = None;
                        // Fall through to normal Uninitialized handling on the
                        // next iteration after a sleep.
                        tokio::time::sleep(POLL_INTERVAL).await;
                        continue;
                    }
                    let phase = PollPhase::WaitingForIndex { txid: txid.clone() };
                    if last_phase.as_ref() != Some(&phase) {
                        ui::info(format!(
                            "waiting for wallet to index broadcast tx {}…",
                            short_txid(txid),
                        ));
                        last_phase = Some(phase);
                    }
                    tokio::time::sleep(POLL_INTERVAL).await;
                    continue;
                }

                let bal = init_balance(db_name)?;
                if bal.spendable < MIN_INIT_ZATS {
                    let phase = if bal.pending > 0 {
                        PollPhase::WaitingForMaturity {
                            pending: bal.pending,
                        }
                    } else {
                        PollPhase::WaitingForFunds
                    };
                    if last_phase.as_ref() != Some(&phase) {
                        match &phase {
                            PollPhase::WaitingForMaturity { pending } => ui::info(format!(
                                "funds received ({} {} pending), waiting for confirmations…",
                                format_zec_decimal(*pending),
                                network.ticker(),
                            )),
                            PollPhase::WaitingForFunds => ui::info(format!(
                                "waiting for funds (need at least {} {})",
                                format_zec_decimal(MIN_INIT_ZATS),
                                network.ticker(),
                            )),
                            _ => unreachable!(),
                        }
                        last_phase = Some(phase);
                    }
                    tokio::time::sleep(POLL_INTERVAL).await;
                    continue;
                }

                // Funds are spendable; attempt the broadcast.
                ui::info(format!(
                    "spendable balance {} {}; broadcasting INIT memo…",
                    format_zec_decimal(bal.spendable),
                    network.ticker(),
                ));
                match broadcast_init(db_name, connection).await {
                    Ok(txid) => {
                        ui::arrow(format!("INIT broadcast: {txid}"));
                        pending_init_txid = Some(txid);
                        waiting_for_index_polls = 0;
                        last_phase = None;
                    }
                    Err(e) => {
                        ui::warn(format!(
                            "broadcast attempt failed ({e}); retrying after {}s…",
                            POLL_INTERVAL.as_secs(),
                        ));
                        last_phase = None;
                        tokio::time::sleep(POLL_INTERVAL).await;
                    }
                }
            }
        }
    }
}

fn short_txid(txid: &str) -> String {
    if txid.len() <= 12 {
        return txid.to_owned();
    }
    format!("{}…{}", &txid[..6], &txid[txid.len() - 6..])
}

/// Snapshot of what the wallet has for the init flow's purposes:
/// `spendable` is what `pay()` will see as available right now; `pending`
/// is everything in flight (mempool, immature change, unshielded
/// transparent) that is *not* yet spendable but will be soon.
struct InitBalance {
    spendable: u64,
    pending: u64,
}

fn init_balance(db_name: &str) -> anyhow::Result<InitBalance> {
    let cfg = WalletConfig::read(db_name)?;
    let (_, db_data_path) = get_db_paths(db_name)?;
    let db_data = open_wallet_db(db_data_path, cfg.network)?;
    let summary = match db_data.get_wallet_summary(ConfirmationsPolicy::default())? {
        Some(s) => s,
        None => {
            return Ok(InitBalance {
                spendable: 0,
                pending: 0,
            })
        }
    };
    let mut spendable: u64 = 0;
    let mut pending: u64 = 0;
    for b in summary.account_balances().values() {
        spendable += u64::from(b.spendable_value());
        pending += u64::from(b.value_pending_spendability());
        pending += u64::from(b.change_pending_confirmation());
        // Transparent funds count as pending; they'd need auto-shielding
        // before they could fund a shielded-pool INIT broadcast.
        pending += u64::from(b.unshielded_balance().total());
    }
    Ok(InitBalance { spendable, pending })
}
