//! Funded regtest end-to-end: the full zkv database lifecycle on a real chain.
//!
//! Regtest can't mine a coinbase straight into an Orchard note, so funds reach
//! zkv the way the protocol allows: mine a **transparent** coinbase to a
//! funding wallet (`zcash-devtool`), let it mature (100 blocks), **shield** it
//! into Orchard, then **send** TAZ to the zkv wallet's funding UA. Everything
//! runs on a **single chain**: the funder's transparent address is derived
//! offline and zebra mines straight to it, so the funder's birthday anchor is
//! taken from the same chain it spends on.
//!
//! What this proves that the offline unit tests can't: the wire protocol
//! round-trips through a real chain end to end. Signed memos survive the
//! actual broadcast path, replay + authorization run over genuinely mined
//! blocks, the version-CAS `[seq]` prefix advances across an overwrite, a
//! watch-only import bootstrapped from nothing but the `zkvregtest1...`
//! address converges on the same state, and the shallow (db-less) client
//! verifies the same writes against the same chain. `sync.rs` / `write.rs` /
//! `send.rs` have no offline coverage at all; this is their test.
//!
//! Phases:
//!  1. stack up + fund the funder (mine, mature, shield).
//!  2. `zkv init --non-interactive` (create), fund the wallet, `zkv init`
//!     resume: INIT broadcast + confirmation.
//!  3. data ops: SET (create), SET (overwrite, seq > 0 on the wire), a second
//!     key, DEL with tombstone, `keys` globbing.
//!  4. `history`: the append-only signed log (INIT genesis entry, every
//!     write verified, creator attribution).
//!  5. roles: WRITERADD/WRITERDEL management memos against a second
//!     database's key, registry + revocation tombstone.
//!  6. watch-only replica from the address alone reads the same state; a
//!     duplicate import is refused.
//!  7. shallow (db-less) read of the same key from the bare address.
//!
//! Skips cleanly unless `ZEBRAD_BIN`, `LIGHTWALLETD_BIN` and `DEVTOOL_BIN` are
//! all set (see README.md).

use std::time::{Duration, Instant};

use zkv_regtest_harness::{resolve_bin, Funder, Lightwalletd, Zebrad, Zkv};

/// Coinbase blocks mined to the funder up front. zebra finalizes blocks deeper
/// than `MAX_BLOCK_REORG_HEIGHT` (= coinbase maturity - 1 = 99) below the tip;
/// only finalized blocks are persisted and survive the miner-swap restart, so
/// mining 120 finalizes the funder's coinbases at heights ~1..21 (the
/// non-finalized rest are dropped on restart, which is what keeps the funder
/// from ever holding an immature coinbase).
const FUNDER_COINBASES: u32 = 120;
/// After restarting mining to a throwaway address, mine this many blocks: the
/// restart resets the tip to the finalized height (~21) and this tail re-grows
/// the chain so the surviving funder coinbases are well past the 100-block
/// maturity.
const MATURITY_TAIL: u32 = 130;
/// A throwaway P2SH address that mines the maturity tail (the funder does not
/// control it).
const TAIL_MINER_ADDRESS: &str = "t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v";
/// 0.1 TAZ: covers the INIT fee plus every write below with lots of headroom
/// (each write costs only the ZIP-317 fee, ~0.0001).
const FUND_ZATOSHIS: u64 = 10_000_000;
/// External (untrusted) receives are spendable at 10 confirmations under the
/// default ZIP-315 policy; a couple extra cover tip skew.
const FUNDING_CONFIRMATIONS: u32 = 12;
/// Blocks mined after each zkv broadcast: 1 confirms the write for
/// `--confirmations 1` reads, 3 make the change note spendable again for the
/// *next* write (trusted change confirms at 3), 1 spare for tip skew.
const CONFIRM_BLOCKS: u32 = 5;

const DB: &str = "db";
const READER: &str = "reader";
const DELEGATE: &str = "delegate";

#[tokio::test]
async fn regtest_kv_lifecycle() {
    let (Some(zebrad_bin), Some(lwd_bin), Some(devtool_bin)) = (
        resolve_bin("ZEBRAD_BIN"),
        resolve_bin("LIGHTWALLETD_BIN"),
        resolve_bin("DEVTOOL_BIN"),
    ) else {
        eprintln!(
            "SKIP regtest_kv_lifecycle: set ZEBRAD_BIN, LIGHTWALLETD_BIN and DEVTOOL_BIN to run \
             the funded e2e (see README.md). The harness still compiled and linked."
        );
        return;
    };

    // ---- Phase 1: chain + funder -------------------------------------------------
    let funder_taddr = Funder::derive_transparent_address(&devtool_bin)
        .expect("derive funder transparent address");
    let mut zebrad = Zebrad::start_with_miner(&zebrad_bin, &funder_taddr)
        .await
        .expect("start zebrad mining to the funder");
    zebrad
        .generate_blocks(FUNDER_COINBASES)
        .await
        .expect("mine the funder's coinbases");
    zebrad
        .restart_with_miner(TAIL_MINER_ADDRESS)
        .await
        .expect("restart zebrad mining to the throwaway address");
    zebrad
        .generate_blocks(MATURITY_TAIL)
        .await
        .expect("mine the maturity tail");

    let lwd = Lightwalletd::start(&lwd_bin, zebrad.rpc_port)
        .await
        .expect("start lightwalletd");

    let funder = Funder::init(&devtool_bin, lwd.grpc_port).expect("initialise funding wallet");
    funder.sync(lwd.grpc_port).expect("funder sync (coinbase)");
    funder
        .shield(lwd.grpc_port)
        .expect("shield transparent coinbase into Orchard");
    // The shielded note must reach the trusted confirmation depth (3) before
    // the funder can spend it; extra blocks cover tip skew.
    zebrad.generate_blocks(6).await.expect("confirm shield");
    funder.sync(lwd.grpc_port).expect("funder sync (shielded)");

    // ---- Phase 2: create + fund + INIT -------------------------------------------
    let zkv = Zkv::new(lwd.grpc_port).expect("set up zkv home");
    let (zkv_addr, funding_ua) = zkv.create_db(DB).expect("zkv init --non-interactive");
    assert!(
        zkv_addr.starts_with("zkvregtest1"),
        "expected a zkvregtest1 address, got {zkv_addr}"
    );
    assert!(
        funding_ua.starts_with("uregtest1"),
        "expected a uregtest1 funding UA, got {funding_ua}"
    );

    // The address is self-describing: `inspect` (fully offline) must recover
    // the network, pool, and the creator's signing key from the string alone.
    let info = zkv.inspect_json(&zkv_addr).expect("inspect own address");
    assert_eq!(info["network"], "regtest");
    // Regtest is testnet-flavored, so the default pool is Ironwood (see
    // `config::default_pool_for_network`); the db is created without `--pool`.
    assert_eq!(info["pool"], "ironwood");
    let creator_key = info["signing_key"]
        .as_str()
        .expect("signing_key in inspect JSON")
        .to_owned();
    assert!(
        creator_key.starts_with("zkvid1"),
        "expected a zkvid1 signing key, got {creator_key}"
    );

    funder
        .send(lwd.grpc_port, &funding_ua, FUND_ZATOSHIS)
        .expect("fund the zkv wallet");
    zebrad
        .generate_blocks(FUNDING_CONFIRMATIONS)
        .await
        .expect("confirm the funding send to spendability");

    zkv.init_until_confirmed(DB, &zebrad, Duration::from_secs(300))
        .await
        .expect("broadcast + confirm INIT");
    // INIT spent the funding note; its change must reach the trusted
    // confirmation depth (3) before the first data write can spend it.
    zebrad
        .generate_blocks(CONFIRM_BLOCKS)
        .await
        .expect("confirm INIT change");

    let balance = zkv.balance(DB).expect("balance after funding");
    assert!(
        balance > 0.0,
        "expected a positive spendable balance, got {balance}"
    );

    // ---- Phase 3: data ops --------------------------------------------------------
    // SET (create).
    zkv.set(DB, "greeting", "hello").expect("set greeting");
    zebrad.generate_blocks(CONFIRM_BLOCKS).await.expect("mine");
    wait_for_value(&zkv, DB, "greeting", Some("hello")).await;

    // Regression guard for the Ironwood self-send memo. A zkv write is a shielded
    // payment to the database's OWN address, so the writer reads its value back
    // from its own *received* note. Compact-block scanning stores that note with a
    // NULL memo; librustzcash's `backfill_self_send_memos` pass fills it in
    // post-scan from the stored raw transaction. The read path no longer carries a
    // `sent_notes` fallback (that band-aid was removed once the received note
    // became authoritative), so a regression in that backfill would surface here
    // as a NULL memo and this read would return None.
    assert_eq!(
        zkv.get(DB, "greeting", 1).expect("read own greeting"),
        Some("hello".to_owned()),
        "writer must read its own Ironwood self-send memo back from the received note, \
         not a sent_notes fallback"
    );

    // SET (overwrite): last-write-wins, and the second write must carry a
    // nonzero replay-protection sequence on the wire (asserted in Phase 4).
    zkv.set(DB, "greeting", "world")
        .expect("overwrite greeting");
    zebrad.generate_blocks(CONFIRM_BLOCKS).await.expect("mine");
    wait_for_value(&zkv, DB, "greeting", Some("world")).await;

    // A second key, then DEL it: the tombstone must win.
    zkv.set(DB, "temp", "42").expect("set temp");
    zebrad.generate_blocks(CONFIRM_BLOCKS).await.expect("mine");
    wait_for_value(&zkv, DB, "temp", Some("42")).await;

    let keys = zkv.keys(DB, "*", 1).expect("keys glob");
    assert!(
        keys.contains(&"greeting".to_owned()) && keys.contains(&"temp".to_owned()),
        "keys '*' should list both live keys, got {keys:?}"
    );

    zkv.del(DB, "temp").expect("del temp");
    zebrad.generate_blocks(CONFIRM_BLOCKS).await.expect("mine");
    wait_for_value(&zkv, DB, "temp", None).await;
    let keys = zkv.keys(DB, "*", 1).expect("keys glob after del");
    assert!(
        keys.contains(&"greeting".to_owned()) && !keys.contains(&"temp".to_owned()),
        "keys '*' should drop the deleted key, got {keys:?}"
    );

    // ---- Phase 4: history ----------------------------------------------------------
    let history = zkv.history_json(DB, 1).expect("history");
    assert_eq!(
        history["creator"].as_str(),
        Some(creator_key.as_str()),
        "history creator must be the address-derived signing key"
    );
    let entries = history["entries"].as_array().expect("history entries");
    let ops: Vec<(&str, &str)> = entries
        .iter()
        .map(|e| {
            (
                e["op"].as_str().unwrap_or(""),
                e["key"].as_str().unwrap_or(""),
            )
        })
        .collect();
    assert!(
        ops.iter().any(|(op, _)| *op == "INIT"),
        "history must show the genesis INIT entry, got {ops:?}"
    );
    assert_eq!(
        ops.iter()
            .filter(|(op, key)| (*op == "SET" || *op == "SETL") && *key == "greeting")
            .count(),
        2,
        "history must show both greeting writes, got {ops:?}"
    );
    assert!(
        ops.iter().any(|(op, key)| *op == "DEL" && *key == "temp"),
        "history must show the DEL, got {ops:?}"
    );
    // Every confirmed entry carries a valid signature by an authorized signer.
    for e in entries {
        assert_eq!(
            e["verified"].as_bool(),
            Some(true),
            "history entry not verified: {e}"
        );
    }
    // The overwrite must have consumed a fresh replay-protection sequence:
    // at least one greeting SET rides the wire with seq >= 1 (the compact
    // `[seq]` prefix on the signature line).
    let greeting_seqs: Vec<u64> = entries
        .iter()
        .filter(|e| e["key"] == "greeting")
        .filter_map(|e| e["seq"].as_u64())
        .collect();
    assert!(
        greeting_seqs.iter().any(|s| *s >= 1),
        "the greeting overwrite must carry a nonzero wire seq, got {greeting_seqs:?}"
    );

    // ---- Phase 5: roles (management opcodes) ---------------------------------------
    // A second database supplies a real foreign pubkey to delegate to (its
    // wallet needs no funds; the key exists as soon as the db does). Mine a
    // fresh block first: creating a database pins a birthday against the tip
    // and refuses a tip older than TIP_MAX_AGE (5 minutes).
    zebrad.generate_blocks(1).await.expect("freshen tip");
    let (delegate_addr, _) = zkv.create_db(DELEGATE).expect("create delegate db");
    let delegate_key = zkv.inspect_json(&delegate_addr).expect("inspect delegate")["signing_key"]
        .as_str()
        .expect("delegate signing key")
        .to_owned();
    assert_ne!(delegate_key, creator_key);

    let roles = zkv.roles_raw(DB, 1).expect("roles before grant");
    assert!(
        has_line(&roles, &format!("creator {creator_key}")),
        "roles must name the creator, got:\n{roles}"
    );
    assert!(
        has_line(&roles, &format!("owner {creator_key}")),
        "the creator must be owner #1 after INIT, got:\n{roles}"
    );

    zkv.ok_online(
        Some(DB),
        &["roles", "writer", "add", &delegate_key, "CREATE,UPDATE"],
    )
    .expect("roles writer add");
    zebrad.generate_blocks(CONFIRM_BLOCKS).await.expect("mine");
    wait_for(
        || {
            let roles = zkv.roles_raw(DB, 1)?;
            Ok(has_line(
                &roles,
                &format!("writer {delegate_key} CREATE,UPDATE"),
            ))
        },
        "writer grant visible in roles",
    )
    .await;

    zkv.ok_online(Some(DB), &["roles", "writer", "remove", &delegate_key])
        .expect("roles writer remove");
    zebrad.generate_blocks(CONFIRM_BLOCKS).await.expect("mine");
    wait_for(
        || {
            let roles = zkv.roles_raw(DB, 1)?;
            // NB: line-anchored matching; a `revoked-writer <key> ...` line
            // *contains* the substring `writer <key> ...`.
            let live_writer = roles
                .lines()
                .any(|l| l.starts_with(&format!("writer {delegate_key}")));
            let tombstone = roles
                .lines()
                .any(|l| l.starts_with(&format!("revoked-writer {delegate_key}")));
            Ok(!live_writer && tombstone)
        },
        "writer revocation + tombstone visible in roles",
    )
    .await;

    // ---- Phase 6: watch-only replica from the address alone ------------------------
    // A separate zkv home plays the independent reader: it holds no seed and
    // knows nothing but the zkvregtest1... address string. (In the writer's
    // own home the import is refused by the duplicate-identity guard, since
    // the admin database is the same database.)
    zebrad.generate_blocks(1).await.expect("freshen tip");
    let reader_zkv = Zkv::new(lwd.grpc_port).expect("set up reader zkv home");
    reader_zkv.watch(&zkv_addr, READER).expect("watch import");
    wait_for_value(&reader_zkv, READER, "greeting", Some("world")).await;
    assert_eq!(
        reader_zkv.get(READER, "temp", 1).expect("reader get temp"),
        None,
        "the DEL tombstone must hold on the watch-only replica"
    );
    // Re-importing the same database under another name is refused, both as a
    // second watch in the reader's home and as a watch beside the admin
    // database that *is* this database.
    for (home, label) in [(&reader_zkv, "reader home"), (&zkv, "writer home")] {
        let dup = home
            .run_online(None, &["watch", &zkv_addr, "reader2"])
            .expect("spawn duplicate watch");
        assert!(
            !dup.status_ok && dup.stderr.contains("already imported"),
            "duplicate watch import in the {label} must be refused, got ok={} stderr:\n{}",
            dup.status_ok,
            dup.stderr
        );
    }

    // ---- Phase 7: shallow (db-less) read --------------------------------------------
    let tip = zebrad.tip_height().await.expect("tip height") as u32;
    let value = zkv
        .shallow_get(&zkv_addr, "greeting", tip)
        .expect("shallow get");
    assert_eq!(
        value, "world",
        "shallow read from the bare address must agree with the full replay"
    );
}

/// Whether `output` has a line starting with `prefix` (roles' raw records are
/// one per line; substring matching is unsafe because `revoked-writer <key>`
/// contains `writer <key>`).
fn has_line(output: &str, prefix: &str) -> bool {
    output.lines().any(|l| l.starts_with(prefix))
}

/// Poll `zkv get` (which syncs on every call) until the key's confirmed value
/// matches, tolerating lightwalletd's ingestion lag behind freshly-mined
/// blocks.
async fn wait_for_value(zkv: &Zkv, db: &str, key: &str, expected: Option<&str>) {
    let want = expected.map(|s| s.to_owned());
    wait_for(
        || Ok(zkv.get(db, key, 1)? == want),
        &format!("{db}: {key} == {expected:?}"),
    )
    .await;
}

/// Retry `check` every 2s until it returns true, failing after 90s. The
/// condition closure may itself error transiently (e.g. a sync racing the
/// indexer) for the first half of the window.
async fn wait_for(mut check: impl FnMut() -> anyhow::Result<bool>, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut last_err: Option<anyhow::Error>;
    loop {
        match check() {
            Ok(true) => return,
            Ok(false) => last_err = None,
            // Tolerate transient errors early in the window; persist and they
            // fail the wait below.
            Err(e) => last_err = Some(e),
        }
        if Instant::now() >= deadline {
            match last_err {
                Some(e) => panic!("timed out waiting for {what}; last error: {e:#}"),
                None => panic!("timed out waiting for {what}"),
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
