//! Price oracle: the canonical zkv use case.
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
//! # Redundant fleets (`--stale-after`)
//!
//! N oracle nodes can run the same feed with no node-to-node links: the
//! chain itself is the coordinator. Every node restores the same database
//! (same recovery phrase) and runs with `--stale-after <minutes>`: each
//! tick it syncs (chain + live mempool) and posts ONLY if the newest
//! authorized post for [`PRICE_KEYS`]`[0]` is older than its threshold.
//! Stagger the thresholds and the lowest-threshold node that is alive does
//! all the posting; the others stand by and fill gaps when it goes down:
//!
//! ```text
//! node-a$ ZKV_DB=oracle cargo run -p zkv --example oracle -- --stale-after 15
//! node-b$ ZKV_DB=oracle cargo run -p zkv --example oracle -- --stale-after 30
//! node-c$ ZKV_DB=oracle cargo run -p zkv --example oracle -- --stale-after 50
//! ```
//!
//! With node-a alive the feed updates every ~15 minutes; if it dies, node-b
//! takes over at 30 minutes of staleness, then node-c at 50. Launching a new
//! node never touches the existing nodes' config: restore the phrase, pick a
//! threshold, start it.
//!
//! How the gate stays honest:
//! - A sibling's still-unmined post is visible (the probe syncs the mempool
//!   and reads at `Confirmations::Mempool`), and counts as fresh, so a
//!   standby doesn't double-post while the primary's tx waits for a block.
//! - If an in-flight post never mines, it expires out of view after ~40
//!   blocks (~50 minutes) and the gate reopens; a stuck tx can't silence the
//!   fleet forever.
//! - Forged memos are skipped: anyone holding the public zkv address can
//!   aim an unauthorized `SET` at the database, but the probe ignores
//!   entries that fail signature/authorization verification.
//! - If two nodes do race onto the same tick (e.g. the primary comes back
//!   the instant a standby fires), the per-key version CAS and the shared
//!   wallet's note selection mean at most one duplicate transaction; replay
//!   keeps the state consistent either way.
//!
//! Replace [`fetch_prices`] with your real price source (CoinGecko, an
//! L1 feed, an aggregator, …). Everything else stays the same.

use std::{
    env,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use zkv::{
    config::Role,
    db::{install_default_subscriber, Confirmations, Database, ZkvError},
    remote::ConnectionArgs,
};

/// The keys this oracle publishes each tick (one shielded memo output per key,
/// all in a single sendmany transaction).
const PRICE_KEYS: &[&str] = &["zec_usd", "btc_usd"];
const DEFAULT_INTERVAL_SECS: u64 = 15 * 60;
/// Watcher-mode (`--stale-after`) default tick: the freshness probe is a
/// cheap incremental sync + local read, so re-check well below the
/// staleness threshold.
const DEFAULT_PROBE_INTERVAL_SECS: u64 = 60;
/// How many newest history rows the freshness probe skims. Unverified rows
/// are skipped (see [`last_post_age`]), so look a few entries deep before
/// concluding there is no authorized post.
const PROBE_DEPTH: u32 = 16;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    install_default_subscriber();

    let db_name = env::var("ZKV_DB").map_err(|_| anyhow::anyhow!("set $ZKV_DB"))?;
    let (once, interval, stale_after) = parse_args()?;

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
    match (once, stale_after) {
        (true, None) => eprintln!("  mode       : one-shot"),
        (true, Some(t)) => eprintln!(
            "  mode       : one-shot, only if the last post is older than {}",
            fmt_dur(t)
        ),
        (false, None) => eprintln!("  mode       : looping every {}s", interval.as_secs()),
        (false, Some(t)) => eprintln!(
            "  mode       : watcher; posting when the last post is older than {} \
             (probing every {}s)",
            fmt_dur(t),
            interval.as_secs()
        ),
    }
    eprintln!();

    let mut ticker = tokio::time::interval(interval);
    loop {
        // Don't wait for the first tick; publish immediately on startup.
        ticker.tick().await;

        // Redundancy gate: coordinate through the chain, not through
        // node-to-node links. Pull the chain plus the live mempool (so a
        // sibling node's still-unmined post is visible), then stand down
        // unless the newest authorized post is older than the threshold.
        if let Some(threshold) = stale_after {
            if let Err(e) = db.sync_with_mempool().await {
                eprintln!("⚠ skipping tick: sync failed: {e}");
                if once {
                    return Err(e.into());
                }
                continue;
            }
            match last_post_age(&db, PRICE_KEYS[0]) {
                Ok(Some(age)) if age < threshold => {
                    eprintln!(
                        "· last post {} ago (threshold {}); standing by",
                        fmt_dur(age),
                        fmt_dur(threshold)
                    );
                    if once {
                        return Ok(());
                    }
                    continue;
                }
                Ok(Some(age)) => {
                    eprintln!("→ last post {} ago; taking over", fmt_dur(age))
                }
                Ok(None) => eprintln!("→ no post for {:?} yet; publishing", PRICE_KEYS[0]),
                Err(e) => {
                    eprintln!("⚠ skipping tick: freshness probe failed: {e:#}");
                    if once {
                        return Err(e);
                    }
                    continue;
                }
            }
        }

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

/// Age of the newest authorized post for `key`, read from the chain: `None`
/// when the key has never been posted, `Some(Duration::ZERO)` when a post is
/// still in flight (ours or a sibling node's; visible because the caller
/// synced via `sync_with_mempool`).
///
/// Entries that failed verification (`verified == Some(false)`) are skipped:
/// anyone holding the public zkv address can aim an unauthorized memo at the
/// database, and a forged post must not silence the real oracle.
fn last_post_age(db: &Database, key: &str) -> anyhow::Result<Option<Duration>> {
    let page = db.history(Some(key), Confirmations::Mempool, Some(PROBE_DEPTH), 0)?;
    for entry in &page.entries {
        if entry.key != key || entry.verified == Some(false) {
            continue;
        }
        // Newest-first, so the first authorized entry is the latest post. An
        // unmined entry has no block timestamp yet: treat it as fresh now (if
        // it never mines it expires out of the wallet's view after ~40 blocks
        // and the gate reopens).
        let Some(ts) = entry.timestamp else {
            return Ok(Some(Duration::ZERO));
        };
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        return Ok(Some(Duration::from_secs(now.saturating_sub(u64::from(ts)))));
    }
    Ok(None)
}

fn parse_args() -> anyhow::Result<(bool, Duration, Option<Duration>)> {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut once = false;
    let mut interval: Option<Duration> = None;
    let mut stale_after: Option<Duration> = None;
    let mut iter = args.into_iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--once" => once = true,
            "--interval" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--interval needs a value"))?;
                interval = Some(Duration::from_secs(v.parse()?));
            }
            "--stale-after" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--stale-after needs a value (minutes)"))?;
                let mins: u64 = v.parse()?;
                anyhow::ensure!(mins > 0, "--stale-after must be at least 1 minute");
                stale_after = Some(Duration::from_secs(mins * 60));
            }
            other => anyhow::bail!("unknown arg {other:?}"),
        }
    }
    // In watcher mode each tick is a cheap freshness probe, so default it
    // well below the staleness threshold; a plain oracle posts on every tick.
    let interval = interval.unwrap_or(Duration::from_secs(if stale_after.is_some() {
        DEFAULT_PROBE_INTERVAL_SECS
    } else {
        DEFAULT_INTERVAL_SECS
    }));
    Ok((once, interval, stale_after))
}

fn fmt_dur(d: Duration) -> String {
    let secs = d.as_secs();
    if secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

fn short(txid: &str) -> String {
    if txid.len() <= 12 {
        return txid.to_owned();
    }
    format!("{}…{}", &txid[..6], &txid[txid.len() - 6..])
}
