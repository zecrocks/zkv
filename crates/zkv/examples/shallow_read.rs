//! Shallow-read the latest oracle values from a bare zkv address: no local
//! database, no wallet, no full sync.
//!
//! The library-only equivalent of:
//!
//! ```text
//! zkv shallow get "rates/*" --address zkvtest1… --no-verify-init
//! ```
//!
//! A [`ShallowClient`] built from an address scans only a recent block
//! window, trial-decrypting compact blocks with the address's viewing key and
//! verifying each memo's signature against the address-derived root key.
//! [`ShallowClient::find_where`] walks backward from the tip, one block at a
//! time, and stops as soon as the caller's predicate is satisfied; here the
//! predicate is "some key under the prefix has a verified winner", standing
//! in for the CLI's `rates/*` glob.
//!
//! Defaults to the bundled demo-oracles testnet address and the `rates/`
//! prefix, so it runs out of the box:
//!
//! ```text
//! cargo run -p zkv --example shallow_read
//! # any other database / prefix:
//! ZKV_ADDRESS=zkv1… ZKV_PREFIX=prices/ cargo run -p zkv --example shallow_read
//! # also verify the INIT anchor (slower first call, stronger):
//! ZKV_VERIFY_INIT=1 cargo run -p zkv --example shallow_read
//! ```

use std::env;

use zkv::{
    db::install_default_subscriber,
    remote::ConnectionArgs,
    shallow::{ShallowClient, ShallowOptions, DEFAULT_SCAN_DEPTH},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    install_default_subscriber();

    let address = env::var("ZKV_ADDRESS").unwrap_or_else(|_| zkv::demo::DEMO_ZKV_ADDRESS.into());
    let prefix = env::var("ZKV_PREFIX").unwrap_or_else(|_| "rates/".into());
    // Mirrors the CLI's --no-verify-init: skip the INIT-anchor walk. Only do
    // this for addresses you already know are genuine, initialized databases;
    // the anchor check is what pins the database identity on chain.
    let verify_init = env::var("ZKV_VERIFY_INIT").is_ok();

    // Identity (viewing key, birthday, network, pool, root signing key) comes
    // entirely from the address; nothing is read from or written to disk.
    let mut client = ShallowClient::from_address(&address, &ConnectionArgs::default()).await?;

    let opts = ShallowOptions {
        verify_init,
        ..ShallowOptions::default()
    };

    // Walk back from the tip until a verified update under the prefix
    // appears, plus a grace window below the first match so sibling keys
    // written a few blocks earlier are caught too. Bounded by
    // `opts.max_depth` (default ~1 hour of blocks): shallow stays shallow,
    // and a quiet database simply yields no matches.
    let prefix_for_match = prefix.clone();
    let state = client
        .find_where(
            &opts,
            move |latest| latest.keys().any(|k| k.starts_with(&prefix_for_match)),
            DEFAULT_SCAN_DEPTH,
        )
        .await?;

    // Trust-model caveats the validator surfaced (unverified signers,
    // rebroadcast tells, …) are worth showing even in a happy-path example.
    for warning in &state.warnings {
        eprintln!("warning: {warning:?}");
    }

    let mut found = false;
    for (key, update) in &state.latest {
        if !key.starts_with(&prefix) {
            continue;
        }
        // A DEL winner means "confirmed deleted in the window"; skip it here.
        if let Some(value) = &update.value {
            println!("{key} = {value}");
            found = true;
        }
    }
    if !found {
        eprintln!(
            "no verified {prefix}* update in blocks {}..={} (tip {})",
            state.scanned.from, state.scanned.to, state.tip,
        );
        std::process::exit(1);
    }
    Ok(())
}
