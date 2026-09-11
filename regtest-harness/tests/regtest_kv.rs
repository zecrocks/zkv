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
//!  8. batch write: several ops in ONE transaction (one txid, one fee), with
//!     two writes to the same key taking consecutive replay versions. Needs
//!     `ZKV_BATCH_BIN`; skipped without it.
//!  9. `sync --rebuild`: refused on a member of the shared scan, which has no
//!     files of its own to wipe, and exercised on an admin and on a
//!     `--standalone` watch database, where the cache is wiped (proved with a
//!     canary in the block cache), the wallet is re-created and re-migrated
//!     under `zec/lrz/`, and every read comes back identical.
//! 10. `remove` deletes the whole database directory, seed included, on a
//!     database in the post-sync (nested) layout. Last, since it destroys the
//!     database the phases above build.
//!
//! Skips cleanly unless `ZEBRAD_BIN`, `LIGHTWALLETD_BIN` and `DEVTOOL_BIN` are
//! all set (see README.md).

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use zkv_regtest_harness::{resolve_bin, FundedStack, Zkv};

/// 0.1 TAZ: covers the INIT fee plus every write below with lots of headroom
/// (each write costs only the ZIP-317 fee, ~0.0001).
const FUND_ZATOSHIS: u64 = 10_000_000;
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
    let stack = FundedStack::up(&zebrad_bin, &lwd_bin, &devtool_bin)
        .await
        .expect("bring up a funded regtest stack");
    let FundedStack { zebrad, lwd, .. } = &stack;

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

    stack
        .fund(&funding_ua, FUND_ZATOSHIS)
        .await
        .expect("fund the zkv wallet");

    zkv.init_until_confirmed(DB, zebrad, Duration::from_secs(300))
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

    // ---- Phase 8: batch writes (one transaction, many memos) ---------------------------
    // `Database::write_many` packs an op per output into ONE transaction: one
    // fee, one txid. Nothing in the CLI or GUI surfaces it, so the coverage
    // runs the `batch_write` example as a subprocess. Skipped, not failed, when
    // the example was not built: a lean local run should still get everything
    // else in this file.
    if let Some(batch_bin) = resolve_bin("ZKV_BATCH_BIN") {
        let fee_before = zkv.balance(DB).expect("balance before the batch");

        // Two distinct keys plus two writes to the SAME key: the same-key pair
        // must take consecutive replay versions, with the later one winning.
        let txid = zkv
            .batch_write(&batch_bin, DB, "batch-a=1;batch-b=2;batch-a=3")
            .expect("batch write");
        assert_eq!(txid.len(), 64, "expected one txid, got {txid:?}");
        zebrad.generate_blocks(CONFIRM_BLOCKS).await.expect("mine");

        wait_for_value(&zkv, DB, "batch-a", Some("3")).await;
        assert_eq!(
            zkv.get(DB, "batch-b", 1).expect("read batch-b"),
            Some("2".to_owned()),
            "every op in the batch must land, not just the last",
        );

        // One transaction, so one ZIP-317 fee: three separate writes would have
        // cost three. The exact fee is the node's business; what matters is
        // that the batch did not pay per op.
        let spent = fee_before - zkv.balance(DB).expect("balance after the batch");
        assert!(
            spent > 0.0,
            "the batch should have paid a fee, but the balance did not move",
        );

        // All three ops share the transaction, and the same-key pair must have
        // taken consecutive sequences.
        let hist = zkv.history_json(DB, 1).expect("history after the batch");
        let entries = hist["entries"].as_array().expect("history entries");
        let in_batch: Vec<&Value> = entries
            .iter()
            .filter(|e| e["txid"] == Value::String(txid.clone()))
            .collect();
        assert_eq!(
            in_batch.len(),
            3,
            "all three ops should be outputs of the one transaction, got {in_batch:#?}",
        );
        let mut a_seqs: Vec<u64> = in_batch
            .iter()
            .filter(|e| e["key"] == "batch-a")
            .filter_map(|e| e["seq"].as_u64())
            .collect();
        a_seqs.sort_unstable();
        assert_eq!(
            a_seqs.len(),
            2,
            "both writes to batch-a should be recorded, got {a_seqs:?}",
        );
        assert_eq!(
            a_seqs[1],
            a_seqs[0] + 1,
            "same-key ops in one batch must take consecutive replay versions, got {a_seqs:?}",
        );
    } else {
        eprintln!(
            "SKIP the batch-write phase: set ZKV_BATCH_BIN to the built \
             `cargo build -p zcash_zkv --example batch_write` binary."
        );
    }

    // ---- Phase 9: sync --rebuild ------------------------------------------------------
    // The wipe-and-rebootstrap path lost its automatic trigger when the scan
    // moved into the node, so it is now an explicit escape hatch: the thing a
    // user runs when a database will not sync. Its layout handling is
    // unit-tested; the live round trip was manual QA.
    //
    // Both variants run here rather than in a binary of their own: this needs a
    // funded, INITed database with keys written *and* a watch-only replica of
    // it, which is exactly what Phases 2 to 6 leave behind. Each rebuild
    // re-scans only from a near-tip birthday, so the cost is a node restart
    // rather than a chain scan.
    rebuild_and_compare(&zkv, DB, true).await;

    // A watch-only database is a member of the shared scan by default, and a
    // member has no wallet files of its own to rebuild: the ones it reads are
    // the shard's, shared with every other member, so wiping them here would
    // rescan everybody. `zkv sync --rebuild` refuses and names the command that
    // does mean this for a fleet.
    let refusal = reader_zkv
        .run_online(Some(READER), &["sync", "--rebuild"])
        .expect("run sync --rebuild on a member");
    assert!(
        !refusal.status_ok,
        "a member must not rebuild the shard out from under its shard-mates",
    );
    assert!(
        refusal.stderr.contains("fleet rebuild"),
        "the refusal should name the command that does mean this: {}",
        refusal.stderr,
    );

    // The rebuild path itself still needs covering for a watch-only database,
    // so this uses one that has files of its own. Its own home, because zkv
    // refuses to import a database it already holds and the reader home holds
    // this address already.
    let solo_zkv = Zkv::new(lwd.grpc_port).expect("set up a standalone reader home");
    solo_zkv
        .ok_online(None, &["watch", &zkv_addr, READER, "--standalone"])
        .expect("watch with a wallet engine of its own");
    wait_for_value(&solo_zkv, READER, "greeting", Some("world")).await;
    rebuild_and_compare(&solo_zkv, READER, false).await;

    // ---- Phase 10: remove destroys the whole database ---------------------------------
    // Last, because it deletes the database every phase above needed.
    //
    // `zkv remove` warns that it destroys the seed, so "the directory is gone"
    // is the whole contract. It is asserted here rather than only offline
    // because the layout is what makes it interesting: a database that has
    // synced keeps its wallet files under `zec/lrz/` while `keys.toml` and the
    // age identity stay at the root, so a remove that reached only for the
    // engine directory would take the wallet and leave the seed, having told
    // the user otherwise. An offline fixture cannot reach this state; only a
    // real node start produces it.
    let db_dir = zkv.db_dir(DB);
    assert!(
        db_dir.join("zec").join("lrz").join("data.sqlite").is_file(),
        "precondition: the database should be in the post-sync layout",
    );
    assert!(
        db_dir.join("keys.toml").is_file(),
        "precondition: keys.toml should be at the database root",
    );

    zkv.ok_local(None, &["remove", DB, "--yes"])
        .expect("zkv remove");

    assert!(
        !db_dir.exists(),
        "remove must delete the whole database directory, but {} still holds {:?}",
        db_dir.display(),
        std::fs::read_dir(&db_dir)
            .map(|d| d
                .filter_map(|e| e.ok().map(|e| e.file_name()))
                .collect::<Vec<_>>())
            .unwrap_or_default(),
    );
    assert!(
        !zkv.ok_local(None, &["list"])
            .expect("zkv list")
            .contains(DB),
        "a removed database must not still be listed",
    );
}

/// Run `zkv sync --rebuild` and assert the database comes back identical.
///
/// The interesting part is proving the cache was actually wiped, and after the
/// fact a rebuilt `data.sqlite` is indistinguishable from an untouched one. So
/// this plants a canary inside the block cache first: `wipe_sidecars` removes
/// that directory wholesale, making the canary's absence unambiguous evidence
/// rather than an inference from timestamps. It is inert, because nothing in
/// the wallet stack ever lists that directory.
async fn rebuild_and_compare(zkv: &Zkv, db: &str, admin: bool) {
    let role = if admin { "admin" } else { "watch-only" };
    let db_dir = zkv.db_dir(db);
    // `<db>/zec/lrz` is where the node relocates librustzcash's files on its
    // first start; a database that has synced is always in this layout.
    let engine_dir = db_dir.join("zec").join("lrz");
    assert!(
        engine_dir.join("data.sqlite").is_file(),
        "{role}: expected the wallet database under zec/lrz before the rebuild",
    );
    assert!(
        !db_dir.join("data.sqlite").exists(),
        "{role}: the pre-engine location must be empty once the node has run",
    );

    let canary = engine_dir.join("blocks").join(".zkv-harness-canary");
    std::fs::create_dir_all(engine_dir.join("blocks")).expect("create the block cache dir");
    std::fs::write(&canary, b"canary").expect("plant the canary");

    let keys_before = zkv.keys(db, "*", 1).expect("keys before");
    let greeting_before = zkv.get(db, "greeting", 1).expect("greeting before");
    let temp_before = zkv.get(db, "temp", 1).expect("temp before");
    let roles_before = zkv.roles_raw(db, 1).expect("roles before");
    let addr_before = zkv.address(db).expect("address before");
    let history_before = history_shape(&zkv.history_json(db, 1).expect("history before"));
    let keys_toml = db_dir.join("keys.toml");
    let keys_toml_before = std::fs::read_to_string(&keys_toml).expect("keys.toml before");

    let out = zkv
        .run_online(Some(db), &["sync", "--rebuild"])
        .expect("spawn sync --rebuild");
    assert!(
        out.status_ok,
        "{role}: sync --rebuild should succeed, got stderr:\n{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("re-scanning from the birthday"),
        "{role}: it should announce the rebuild, got stderr:\n{}",
        out.stderr
    );

    assert!(
        !canary.exists(),
        "{role}: the block cache should have been wiped, but the canary survived",
    );
    // Rebootstrap re-creates the wallet at the database root (nothing is
    // nested at that moment), and the node's next start migrates it back. Both
    // halves matter: a `data.sqlite` left in *both* places makes every later
    // read refuse outright rather than pick one.
    assert!(
        engine_dir.join("data.sqlite").is_file(),
        "{role}: the rebuilt wallet should end up back under zec/lrz",
    );
    assert!(
        !db_dir.join("data.sqlite").exists(),
        "{role}: the rebuilt wallet must be moved, not copied",
    );
    assert!(
        db_dir.join("zkv_state.sqlite").is_file(),
        "{role}: the snapshot sidecar should have been rebuilt",
    );
    assert_eq!(
        std::fs::read_to_string(&keys_toml).expect("keys.toml after"),
        keys_toml_before,
        "{role}: a rebuild must not touch keys.toml",
    );
    if admin {
        // The age identity exists only to wrap the seed, so only an admin
        // database has one: `init_watch_at` stores `mnemonic: None` and never
        // writes the file. Losing it on an admin database would make the seed
        // permanently unreadable, which is why it is worth asserting there.
        assert!(
            db_dir.join("security-theater-key").is_file(),
            "{role}: the age identity must survive, or the seed becomes unreadable",
        );
    }

    assert_eq!(zkv.keys(db, "*", 1).expect("keys after"), keys_before);
    assert_eq!(
        zkv.get(db, "greeting", 1).expect("greeting after"),
        greeting_before
    );
    assert_eq!(
        zkv.get(db, "temp", 1).expect("temp after"),
        temp_before,
        "{role}: the DEL tombstone must survive a rebuild",
    );
    assert_eq!(zkv.roles_raw(db, 1).expect("roles after"), roles_before);
    assert_eq!(zkv.address(db).expect("address after"), addr_before);
    assert_eq!(
        history_shape(&zkv.history_json(db, 1).expect("history after")),
        history_before,
        "{role}: the signed write log must replay identically",
    );
}

/// The parts of `zkv history --output json` that a rebuild must reproduce
/// exactly.
///
/// Deliberately a projection rather than the whole value: each entry carries a
/// `confirmations` count measured against the current tip, so comparing the
/// raw JSON would fail the moment a block lands between the two reads.
fn history_shape(v: &Value) -> Value {
    let entries: Vec<Value> = v["entries"]
        .as_array()
        .expect("history entries")
        .iter()
        .map(|e| {
            json!({
                "op": e["op"],
                "key": e["key"],
                "value": e["value"],
                "txid": e["txid"],
                "output_index": e["output_index"],
                "seq": e["seq"],
                "signer": e["signer"],
                "verified": e["verified"],
                "height": e["height"],
            })
        })
        .collect();
    json!({ "creator": v["creator"], "entries": entries })
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
