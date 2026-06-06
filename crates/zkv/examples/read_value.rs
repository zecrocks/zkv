//! Read a value from a zkv database, demonstrating the facade's
//! "sync once, then schedule it yourself" model.
//!
//! Reads the database name from `$ZKV_DB` and the key from `$ZKV_KEY`. The
//! data directory follows the usual zkv precedence: `$ZKV_DATA`, then the
//! per-OS default (`$HOME/.zkv` on Linux, `$HOME/Library/Application Support/zkv`
//! on macOS, `%APPDATA%\zkv` on Windows).
//!
//! A read ([`Database::read_at`]) is pure-local: it never touches the
//! network and reports the chain height the local state reflects
//! (`as_of_height`). Rather than syncing unconditionally, this example
//! checks that height against the live chain tip (one cheap RPC) and only
//! runs the expensive block scan when the local state has drifted more than
//! `$ZKV_MAX_LAG` blocks behind (default 3). A scheduled reader can run this
//! on a tight loop and mostly serve from local state, syncing only when it
//! actually falls behind.
//!
//! ```text
//! ZKV_DB=mydb ZKV_KEY=hello cargo run -p zkv --example read_value
//! # tolerate up to 10 blocks of staleness before forcing a sync:
//! ZKV_MAX_LAG=10 ZKV_DB=mydb ZKV_KEY=hello cargo run -p zkv --example read_value
//! ```

use std::env;

use zkv::{
    db::{install_default_subscriber, Confirmations, Database},
    remote::ConnectionArgs,
};

/// Blocks behind the chain tip we tolerate before forcing a sync.
const DEFAULT_MAX_LAG: u32 = 3;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    install_default_subscriber();

    let db_name = env::var("ZKV_DB").map_err(|_| anyhow::anyhow!("set $ZKV_DB"))?;
    let key = env::var("ZKV_KEY").map_err(|_| anyhow::anyhow!("set $ZKV_KEY"))?;
    let max_lag = env::var("ZKV_MAX_LAG")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_LAG);

    let db = Database::open(&db_name, ConnectionArgs::default())?;

    // Pure-local read: replays whatever the wallet has already scanned and
    // tells us the height that state reflects. No network I/O.
    let confs = Confirmations::OneBlock;
    let mut read = db.read_at(confs)?;

    // One cheap RPC for the live tip, so we can measure how stale we are.
    let tip = db.chain_tip().await?;
    let lag = tip.saturating_sub(read.as_of_height.unwrap_or(0));

    if read.as_of_height.is_none() || lag >= max_lag {
        // Never synced, or drifted too far: pay for the full scan, then
        // re-read the now-fresh local state.
        eprintln!("{lag} block(s) behind tip {tip} (>= {max_lag}); syncing…");
        let height = db.sync().await?;
        read = db.read_at(confs)?;
        eprintln!("synced to height {height}");
    } else {
        eprintln!(
            "state as-of height {} is fresh ({lag} < {max_lag} behind tip {tip}); skipping sync",
            read.as_of_height.unwrap_or(0),
        );
    }

    // Pull the value out of the read we already have (no second query).
    match read
        .replay
        .state
        .get(&key)
        .and_then(|ks| ks.confirmed.clone())
    {
        Some(value) => println!("{value}"),
        None => {
            eprintln!("(no confirmed value for {key:?})");
            std::process::exit(1);
        }
    }
    Ok(())
}
