use clap::Args;
use zcash_client_backend::data_api::{wallet::ConfirmationsPolicy, WalletRead};

use crate::{
    config::{pool_label, Role, WalletConfig},
    data::{self, get_db_paths, open_wallet_db},
    internal::{
        protocol::InitState,
        state::{load_state, INIT_CONFIRMATIONS},
    },
    ui::format_zec,
};

#[derive(Debug, Args)]
pub(crate) struct Command {
    /// Also show each database's last-known wallet balance (no sync; run
    /// `zkv sync` first for an up-to-date figure). Watch-only databases hold
    /// no spending key and report no balance.
    #[arg(long)]
    balances: bool,
}

impl Command {
    pub(crate) fn run(self, _db: Option<String>) -> anyhow::Result<()> {
        let dbs = data::list_dbs()?;
        if dbs.is_empty() {
            eprintln!("(no databases. Run `zkv init` to create one.)");
            return Ok(());
        }
        let current = data::current_db()?;
        let balances = self.balances;

        // Each database is an independent set of sqlite files (wallet DB +
        // snapshot cache), and describing one is dominated by sqlite opens and a
        // local state replay. Run them concurrently instead of strictly
        // serially so `zkv list` doesn't slow down linearly with the number of
        // databases. Output order is preserved by joining the handles in order.
        let lines: Vec<String> = std::thread::scope(|scope| {
            let handles: Vec<_> = dbs
                .iter()
                .map(|name| {
                    let current = current.as_deref();
                    scope.spawn(move || describe_db(name, current, balances))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        for line in lines {
            println!("{line}");
        }
        Ok(())
    }
}

/// Build one `zkv list` output line for a single database. Reads local state
/// only (no sync); an unreadable config or state degrades gracefully rather
/// than failing the whole listing.
fn describe_db(name: &str, current: Option<&str>, balances: bool) -> String {
    let marker = if current == Some(name) { "*" } else { " " };
    let (role, network, pool, creator) = match WalletConfig::read(name) {
        Ok(cfg) => (
            format!("{:?}", cfg.role).to_lowercase(),
            cfg.network.name().to_owned(),
            pool_label(cfg.pool).to_owned(),
            // Admin = we hold the seed whose UFVK roots this address, i.e. we
            // hold the root signing key that bootstraps the db via INIT.
            // Watch-only dbs never do.
            cfg.role == Role::Admin,
        ),
        Err(_) => ("?".to_owned(), "?".to_owned(), "?".to_owned(), false),
    };
    let mut tags = Vec::new();
    if creator {
        tags.push("creator".to_owned());
    }
    tags.push(role);
    tags.push(network);
    tags.push(pool);
    // Init state is read from local cache only (no sync). Anything that isn't a
    // clean `Initialized` is surfaced so uninitialized (or still-confirming)
    // databases stand out; an unreadable state is skipped silently rather than
    // failing the whole listing.
    if let Ok(state) = load_state(name, INIT_CONFIRMATIONS, false) {
        match state.init {
            InitState::Initialized => {}
            InitState::Initializing { done, required } => {
                tags.push(format!("initializing {done}/{required}"));
            }
            // Only an admin database (one we created) is meaningfully
            // "uninitialized": it exists locally but hasn't been INIT'd yet. A
            // watch-only database we merely imported has no INIT of its own, and
            // before its first sync we simply don't know its state, so don't
            // mislabel it.
            InitState::Uninitialized if creator => tags.push("uninitialized".to_owned()),
            InitState::Uninitialized => {}
        }
    }
    if balances {
        if let Some(bal) = last_known_balance(name) {
            tags.push(bal);
        }
    }
    format!("{marker} {name}  ({})", tags.join(", "))
}

/// Last-known wallet balance for a database, formatted as e.g. `1.23 TAZ`,
/// or `None` for watch-only / unreadable databases (which hold no spending
/// key and thus no balance). Reads the local wallet DB only; no sync, so the
/// figure reflects the last completed scan.
fn last_known_balance(name: &str) -> Option<String> {
    let cfg = WalletConfig::read(name).ok()?;
    if cfg.role != Role::Admin {
        return None;
    }
    let (_, db_data_path) = get_db_paths(name).ok()?;
    let db_data = open_wallet_db(db_data_path, cfg.network).ok()?;
    let summary = db_data
        .get_wallet_summary(ConfirmationsPolicy::default())
        .ok()??;
    let total_zat: u64 = summary
        .account_balances()
        .values()
        .map(|b| u64::from(b.total()))
        .sum();
    let network = cfg.network;
    Some(
        format_zec(total_zat as i64, network)
            .trim_start()
            .to_owned(),
    )
}
