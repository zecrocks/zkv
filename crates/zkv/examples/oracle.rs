//! Price oracle: the canonical zkv use case in <60 lines.
//!
//! Opens a database once, then loops: sync, fetch the latest prices, and
//! publish them all in ONE "sendmany" transaction (one fee, one txid) via
//! `Database::set_many`. Watchers anywhere can read each value by syncing the
//! same UFVK and calling `Database::get`.
//!
//! Demonstrates everything a real consumer needs from the facade:
//! `Database::open`, `role`/`network`/`zkv_address` introspection,
//! repeated `sync` + batched `set_many`, and graceful Ctrl-C shutdown.
//!
//! ```text
//! # one-shot (no loop):
//! ZKV_DB=oracle-admin cargo run -p zkv --example oracle -- --once
//!
//! # default: update every 15 minutes
//! ZKV_DB=oracle-admin cargo run -p zkv --example oracle
//!
//! # custom cadence (seconds)
//! ZKV_DB=oracle-admin cargo run -p zkv --example oracle -- --interval 60
//! ```
//!
//! Replace [`fetch_prices`] with your real price source (CoinGecko, an
//! L1 feed, an aggregator, …). Everything else stays the same.

use std::{env, time::Duration};

use zkv::{
    config::Role,
    db::{install_default_subscriber, Database, ZkvError},
    remote::ConnectionArgs,
};

/// The keys this oracle publishes each tick (one shielded memo output per key,
/// all in a single sendmany transaction).
const PRICE_KEYS: &[&str] = &["zec_usd", "btc_usd"];
const DEFAULT_INTERVAL_SECS: u64 = 15 * 60;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    install_default_subscriber();

    let db_name = env::var("ZKV_DB").map_err(|_| anyhow::anyhow!("set $ZKV_DB"))?;
    let (once, interval) = parse_args()?;

    // Open the database once. The handle carries config + connection;
    // everything else is `&self` methods.
    let db = Database::open(&db_name, ConnectionArgs::default())?;

    // Print a quick dashboard so the operator can sanity-check what
    // they're publishing to.
    eprintln!("zkv oracle");
    eprintln!(
        "  database   : {} ({:?}, {:?})",
        db.name(),
        db.role(),
        db.network()
    );
    eprintln!("  zkv address: {}", db.zkv_address()?);
    if db.role() != Role::Admin {
        anyhow::bail!(
            "{:?} is watch-only; oracle needs an admin database",
            db_name
        );
    }
    if once {
        eprintln!("  mode       : one-shot");
    } else {
        eprintln!("  mode       : looping every {}s", interval.as_secs());
    }
    eprintln!();

    let mut ticker = tokio::time::interval(interval);
    loop {
        // Don't wait for the first tick; publish immediately on startup.
        ticker.tick().await;

        let prices = match fetch_prices().await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("⚠ skipping tick: price fetch failed: {e:#}");
                if once {
                    return Err(e);
                }
                continue;
            }
        };

        // Borrow the fetched prices as &str pairs for the batch API.
        let pairs: Vec<(&str, &str)> = prices.iter().map(|(k, v)| (*k, v.as_str())).collect();

        // `set_many` syncs first, then signs and broadcasts every key in ONE
        // transaction (one fee, one txid as a hex string).
        match db.set_many(&pairs).await {
            Ok(txid) => {
                let summary = prices
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                eprintln!("✓ {summary}  (txid {})", short(&txid));
            }
            Err(ZkvError::InsufficientFunds {
                available,
                required,
                pending,
            }) => {
                let tkr = db.network().ticker();
                eprintln!(
                    "⚠ insufficient funds: have {:.8} {tkr}, need {:.8} {tkr} \
                     (pending {:.8} {tkr}); will retry next tick",
                    available as f64 / 1e8,
                    required as f64 / 1e8,
                    pending as f64 / 1e8,
                );
            }
            Err(ZkvError::Initializing { done, required }) => {
                eprintln!("⚠ database is still initializing ({done}/{required}); will retry")
            }
            Err(e) => eprintln!("⚠ broadcast failed: {e}"),
        }

        if once {
            return Ok(());
        }
    }
}

/// Stub: return a plausible price per [`PRICE_KEYS`] entry, paired with its
/// key. Replace with a real fetch against your data source.
async fn fetch_prices() -> anyhow::Result<Vec<(&'static str, String)>> {
    // Fake source: a stable per-key mid-price plus a tiny wobble derived from
    // the current second so successive runs produce distinct values.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let wobble = (secs % 100) as f64 / 100.0;
    Ok(PRICE_KEYS
        .iter()
        .map(|&key| {
            let mid = match key {
                "btc_usd" => 67_250.0,
                _ => 553.88,
            };
            (key, format!("{:.2}", mid + wobble - 0.5))
        })
        .collect())
}

fn parse_args() -> anyhow::Result<(bool, Duration)> {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut once = false;
    let mut interval = Duration::from_secs(DEFAULT_INTERVAL_SECS);
    let mut iter = args.into_iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--once" => once = true,
            "--interval" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--interval needs a value"))?;
                interval = Duration::from_secs(v.parse()?);
            }
            other => anyhow::bail!("unknown arg {other:?}"),
        }
    }
    Ok((once, interval))
}

fn short(txid: &str) -> String {
    if txid.len() <= 12 {
        return txid.to_owned();
    }
    format!("{}…{}", &txid[..6], &txid[txid.len() - 6..])
}
