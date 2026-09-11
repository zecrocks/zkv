//! The shared scan, end to end on a real chain.
//!
//! Watch-only databases no longer get a wallet engine each: they join a
//! per-network shared scan, so many viewing keys sit in one shard database
//! served by one node over one connection. That rearranges where a database's
//! files live, which account inside them is its own, and which process holds
//! which lock, and none of it is visible to an offline test: the unit tests can
//! check the resolver against synthetic shards, but only a live chain shows a
//! member actually reading what the chain actually holds.
//!
//! What this proves that the offline tests cannot:
//!
//!  * A watched database with no wallet files of its own reads exactly what the
//!    admin database that wrote the memos reads.
//!  * Two members of one shard read their own memos and not each other's, which
//!    is the whole risk of putting several accounts in one file.
//!  * The lock moves: the shared scan's datadir is locked while it syncs, and
//!    no member directory is.
//!  * A database that predates the shared scan converts to it without going
//!    dark, and its reads are identical before, during and after.
//!  * Leaving gives a member its own files back, and the reads survive that too.
//!  * A torn manifest is visible and repairable rather than silently unscanned.
//!
//! Three zkv homes, because zkv refuses to import a database it already holds:
//! the writer home owns the admin databases, and a watch of one of their
//! addresses has to live somewhere else. That is also the honest shape, since a
//! reader knows nothing but the address.
//!
//! Phases:
//!  1. stack up; two admin databases INITed, each with its own writes, so the
//!     shard ends up holding two viewing keys with different memos under them.
//!  2. `watch` both, into a reader home (the shared scan is the default): each
//!     member reads its own admin's state and none of its shard-mate's, the
//!     layout is the shared one, and the lock is where it should be.
//!  3. a `--standalone` watch, in a third home, keeps its own files and is not
//!     enrolled.
//!  4. that standalone database is converted with `fleet join`: it keeps
//!     reading throughout, and converges onto the shard.
//!  5. `fleet leave` puts it back on its own files, still reading the same.
//!  6. `remove` takes the manifest with it.
//!  7. a hand-torn manifest is reported, and `fleet rebuild` repairs it.
//!
//! Skips cleanly unless `ZEBRAD_BIN`, `LIGHTWALLETD_BIN` and `DEVTOOL_BIN` are
//! all set (see README.md).

use std::time::Duration;

use serde_json::Value;
use zkv_regtest_harness::{datadir_lock_held, resolve_bin, FundedStack, Zkv};

/// What each admin database is funded with. Deliberately different: a member's
/// balance has to be distinguishable from its shard-mate's *and* from the sum of
/// the two, or a read that summed the whole shard file would pass.
/// Blocks mined before the lock phase, so a member's sync has work to do and
/// holds the shared datadir lock for longer than one poll interval.
const LOCK_PHASE_BLOCKS: u32 = 40;

const FUND_A_ZATOSHIS: u64 = 10_000_000;
const FUND_B_ZATOSHIS: u64 = 4_000_000;
const CONFIRM_BLOCKS: u32 = 5;

const ADMIN_A: &str = "writer-a";
const ADMIN_B: &str = "writer-b";
const MEMBER_A: &str = "reader-a";
const MEMBER_B: &str = "reader-b";
const SOLO: &str = "reader-solo";

/// The keys each admin database writes, and what its reader must see.
///
/// Deliberately disjoint. Two members of one shard reading the *same* memos
/// would pass whether or not the reads are account-scoped, which is the one
/// thing putting several accounts in one file puts at risk; distinct keys make
/// a crossed wire a failure.
const WRITES_A: &[(&str, &str)] = &[("alpha", "one"), ("beta", "two")];
const WRITES_B: &[(&str, &str)] = &[("gamma", "three")];

#[tokio::test]
async fn regtest_fleet_shared_scan() {
    let (Some(zebrad_bin), Some(lwd_bin), Some(devtool_bin)) = (
        resolve_bin("ZEBRAD_BIN"),
        resolve_bin("LIGHTWALLETD_BIN"),
        resolve_bin("DEVTOOL_BIN"),
    ) else {
        eprintln!(
            "SKIP regtest_fleet_shared_scan: set ZEBRAD_BIN, LIGHTWALLETD_BIN and DEVTOOL_BIN \
             to run the funded e2e (see README.md). The harness still compiled and linked."
        );
        return;
    };

    // ---- Phase 1: a funded, initialized database with writes to read --------
    let stack = FundedStack::up(&zebrad_bin, &lwd_bin, &devtool_bin)
        .await
        .expect("bring up a funded regtest stack");
    let FundedStack { zebrad, lwd, .. } = &stack;

    // Two admin databases, so the shard ends up holding two viewing keys with
    // different memos under them.
    let zkv = Zkv::new(lwd.grpc_port).expect("set up the writer home");
    let mut addrs = Vec::new();
    for (admin, writes, funding) in [
        (ADMIN_A, WRITES_A, FUND_A_ZATOSHIS),
        (ADMIN_B, WRITES_B, FUND_B_ZATOSHIS),
    ] {
        let (addr, funding_ua) = zkv.create_db(admin).expect("create an admin database");
        stack
            .fund(&funding_ua, funding)
            .await
            .expect("fund an admin database");
        zkv.init_until_confirmed(admin, zebrad, Duration::from_secs(300))
            .await
            .expect("broadcast and confirm INIT");
        zebrad
            .generate_blocks(CONFIRM_BLOCKS)
            .await
            .expect("confirm the INIT change");

        for (key, value) in writes {
            zkv.set(admin, key, value).expect("write a key");
            zebrad
                .generate_blocks(CONFIRM_BLOCKS)
                .await
                .expect("confirm a write");
        }
        addrs.push(addr);
    }
    let (addr_a, addr_b) = (addrs[0].clone(), addrs[1].clone());

    // ---- Phase 2: two members of the shared scan ----------------------------
    // In their own home: zkv refuses to import a database it already holds, and
    // the writer home holds both of these as admin databases. A reader that
    // knows nothing but the address is also the honest shape for this test.
    let readers = Zkv::new(lwd.grpc_port).expect("set up the reader home");
    readers
        .watch(&addr_a, MEMBER_A)
        .expect("watch into the fleet");
    readers
        .watch(&addr_b, MEMBER_B)
        .expect("watch a second member");

    let fleet_dir = readers.data_dir().join(".fleet").join("regtest");
    for name in [MEMBER_A, MEMBER_B] {
        assert!(
            fleet_dir
                .join("wallets.d")
                .join(format!("{name}.toml"))
                .is_file(),
            "{name} should have a manifest in the shared scan",
        );
        assert!(
            !readers.db_dir(name).join("data.sqlite").exists()
                && !readers.db_dir(name).join("zec").exists(),
            "{name} is served by the shared scan, so it must have no wallet files of its own",
        );
        assert_eq!(
            fleet_state(&readers, name),
            "shared",
            "{name} should be reading from a shard by now",
        );
    }
    assert!(
        fleet_dir
            .join("shards")
            .join("shard-0000")
            .join("lrz")
            .join("data.sqlite")
            .is_file(),
        "the shared scan should have laid down its first shard",
    );

    // Each member reads its own admin database's state, and none of its
    // shard-mate's: one shard file, several accounts, no crossed wires. The
    // negative half is the one that matters, since a read that ignored the
    // account would still pass the positive half.
    for (name, admin, mine, theirs) in [
        (MEMBER_A, ADMIN_A, WRITES_A, WRITES_B),
        (MEMBER_B, ADMIN_B, WRITES_B, WRITES_A),
    ] {
        for (key, value) in mine {
            assert_eq!(
                readers
                    .get(name, key, 1)
                    .expect("read through the shared scan"),
                Some((*value).to_owned()),
                "{name} should read {key} exactly as the admin database wrote it",
            );
        }
        for (key, _) in theirs {
            assert_eq!(
                readers
                    .get(name, key, 1)
                    .expect("read through the shared scan"),
                None,
                "{name} must not see {key}: that is its shard-mate's memo",
            );
        }
        assert_eq!(
            readers.keys(name, "*", 1).expect("list keys"),
            zkv.keys(admin, "*", 1).expect("list the admin's keys"),
        );
    }

    // ---- A member's balance is its own, not its shard's ---------------------
    // `zkv balance` deliberately serves watch-only databases, and every member
    // is watch-only, so this is the one read where a whole-file sum would show
    // shard-mates' money under this database's name. A member watches a funded
    // admin database, so the right answer is that admin's balance and nothing
    // else: not its shard-mate's, and not the two added together, which is what
    // summing the file would give. (This is zkv's instance of what upstream
    // fixed in #270 on the node's own read path.)
    let admin_a = zkv.balance(ADMIN_A).expect("the first admin balance");
    let admin_b = zkv.balance(ADMIN_B).expect("the second admin balance");
    assert!(
        admin_a > 0.0 && admin_b > 0.0 && admin_a != admin_b,
        "the fixture needs two funded databases with different balances to tell \
         a scoped read from a summed one: {admin_a} and {admin_b}",
    );
    for (name, own) in [(MEMBER_A, admin_a), (MEMBER_B, admin_b)] {
        let balance = readers.balance(name).expect("a member's balance");
        assert_eq!(
            balance, own,
            "{name} should report the balance of the database it watches",
        );
        assert_ne!(
            balance,
            admin_a + admin_b,
            "{name} reported its whole shard's money, not its own account's",
        );
    }

    // ---- The lock moved -----------------------------------------------------
    // A member's sync locks the shared scan's datadir, not its own directory.
    // That is the arrangement the whole thing rests on: one lock for the
    // shared node, and nothing for the databases it serves.
    // Give the sync something to do first. Everything above left the shard
    // caught up, and a sync with no work is a node start and an immediate exit:
    // the lock is genuinely taken, for a window a poll can miss. Blocks to scan
    // make the window comfortably longer than the sampling interval, so this
    // observes a real property rather than winning a race.
    zebrad
        .generate_blocks(LOCK_PHASE_BLOCKS)
        .await
        .expect("give the shard some blocks to scan");

    let mut sync = readers
        .spawn_online_quiet(Some(MEMBER_A), &["sync"])
        .expect("spawn a sync");
    let mut saw_fleet_lock = false;
    let mut sync_ran = false;
    // Poll tightly, and keep going while the child is alive rather than for a
    // fixed count: the question is whether the lock is ever held during a
    // member's sync, so the sync's own lifetime is the window to watch.
    for _ in 0..2_000 {
        if datadir_lock_held(&fleet_dir).unwrap_or(false) {
            saw_fleet_lock = true;
            assert!(
                !datadir_lock_held(&readers.db_dir(MEMBER_A)).unwrap_or(false),
                "a member must not lock its own directory",
            );
            break;
        }
        let running = sync.is_running().unwrap_or(false);
        sync_ran |= running;
        if !running && sync_ran {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // Let it finish rather than killing it: a half-done sync would leave the
    // next phase racing a shard that is still catching up.
    for _ in 0..600 {
        if !sync.is_running().unwrap_or(false) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    sync.kill();
    assert!(
        saw_fleet_lock,
        "the shared scan's datadir should be locked while a member syncs \
         (the sync process was {} observed running)",
        if sync_ran { "" } else { "never" },
    );

    // ---- Phase 3: --standalone opts out -------------------------------------
    // A third home, for the same reason the readers got their own: this watches
    // an address the reader home already holds. It also keeps the conversion
    // phases from disturbing the two members above.
    let solo = Zkv::new(lwd.grpc_port).expect("set up the standalone home");
    let solo_fleet_dir = solo.data_dir().join(".fleet").join("regtest");
    solo.ok_online(None, &["watch", &addr_a, SOLO, "--standalone"])
        .expect("watch with a wallet engine of its own");
    assert!(
        solo.db_dir(SOLO).join("data.sqlite").exists() || solo.db_dir(SOLO).join("zec").exists(),
        "a standalone database keeps its own wallet files",
    );
    assert!(
        !solo_fleet_dir
            .join("wallets.d")
            .join(format!("{SOLO}.toml"))
            .exists(),
        "a standalone database must not be enrolled",
    );
    let solo_before = solo
        .keys(SOLO, "*", 1)
        .expect("read the standalone database");
    assert_eq!(solo_before, zkv.keys(ADMIN_A, "*", 1).unwrap());

    // ---- Phase 4: convert it, and never lose a read -------------------------
    solo.ok_local(None, &["fleet", "join", SOLO])
        .expect("join the shared scan");
    assert_eq!(
        fleet_state(&solo, SOLO),
        "joining",
        "a converting database is not a member yet",
    );
    // While converting it still reads from its own files, which is the point.
    assert_eq!(solo.keys(SOLO, "*", 1).unwrap(), solo_before);

    // Drive it to completion. Each sync contributes a bounded slice to the
    // shard, so a few passes are what a CLI-only user would do.
    let mut converted = false;
    for _ in 0..10 {
        let _ = solo.run_online(Some(SOLO), &["sync"]);
        // Reads must be correct at every step, not only at the ends.
        assert_eq!(
            solo.keys(SOLO, "*", 1).unwrap(),
            solo_before,
            "a converting database must keep reading correctly",
        );
        if fleet_state(&solo, SOLO) == "shared" {
            converted = true;
            break;
        }
    }
    assert!(converted, "the conversion should finish within a few syncs");
    assert!(
        !solo.db_dir(SOLO).join("data.sqlite").exists()
            && !zkv
                .db_dir(SOLO)
                .join("zec")
                .join("lrz")
                .join("data.sqlite")
                .exists(),
        "a converted member's own wallet files are deleted",
    );
    assert!(
        solo.db_dir(SOLO).join("zkv_state.sqlite").exists(),
        "the snapshot survives the conversion: it is chain-keyed, and the shard \
         has scanned past all of it",
    );
    assert_eq!(solo.keys(SOLO, "*", 1).unwrap(), solo_before);

    // ---- Phase 5: leaving gives the files back ------------------------------
    solo.ok_local(None, &["fleet", "leave", SOLO])
        .expect("leave the shared scan");
    assert_eq!(fleet_state(&solo, SOLO), "own");
    assert!(
        !solo_fleet_dir
            .join("wallets.d")
            .join(format!("{SOLO}.toml"))
            .exists(),
        "leaving removes the manifest",
    );
    // Its own files rebuild from the birthday; the reads come back.
    let mut recovered = false;
    for _ in 0..10 {
        let _ = solo.run_online(Some(SOLO), &["sync"]);
        if solo.keys(SOLO, "*", 1).unwrap_or_default() == solo_before {
            recovered = true;
            break;
        }
    }
    assert!(
        recovered,
        "a database that left should rebuild and read the same"
    );

    // ---- Phase 6: removing a member takes its manifest ----------------------
    readers
        .ok_local(None, &["remove", MEMBER_B, "-y"])
        .or_else(|_| readers.ok_local(None, &["remove", MEMBER_B]))
        .expect("remove a member");
    assert!(
        !fleet_dir
            .join("wallets.d")
            .join(format!("{MEMBER_B}.toml"))
            .exists(),
        "removing a database must not leave its viewing key enrolled",
    );
    assert!(
        fleet_dir
            .join("shards")
            .join("shard-0000")
            .join("lrz")
            .join("data.sqlite")
            .is_file(),
        "the shard itself is untouched: the wallet engine cannot remove one account",
    );
    // The other member is unaffected.
    assert_eq!(
        readers.keys(MEMBER_A, "*", 1).unwrap(),
        zkv.keys(ADMIN_A, "*", 1).unwrap()
    );

    // ---- Phase 7: a torn manifest is visible, and repairable ----------------
    let manifest = fleet_dir.join("wallets.d").join(format!("{MEMBER_A}.toml"));
    std::fs::write(&manifest, b"").expect("truncate a manifest the way a crash would");
    let status = readers
        .ok_local(None, &["fleet", "status", "--output", "json"])
        .expect("fleet status");
    let rows: Value = serde_json::from_str(status.trim()).expect("status JSON");
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["database"] == MEMBER_A)
        .expect("the member should still be listed");
    assert_eq!(
        row["manifest"], false,
        "a torn manifest must read as missing rather than as fine",
    );

    readers
        .ok_local(None, &["fleet", "rebuild", "-y"])
        .expect("rebuild the shared scan");
    assert!(
        std::fs::read_to_string(&manifest).unwrap().contains("ufvk"),
        "rebuild rewrites the manifest from keys.toml",
    );
    let mut reread = false;
    for _ in 0..15 {
        let _ = readers.run_online(Some(MEMBER_A), &["sync"]);
        if readers.keys(MEMBER_A, "*", 1).unwrap_or_default() == zkv.keys(ADMIN_A, "*", 1).unwrap()
        {
            reread = true;
            break;
        }
    }
    assert!(
        reread,
        "after a rebuild the member should read the same state again"
    );
}

/// The `state` column `zkv fleet status --output json` reports for a database,
/// or `"own"` when it is not listed at all (the command lists only databases
/// that have something to do with the shared scan).
fn fleet_state(zkv: &Zkv, name: &str) -> String {
    let Ok(out) = zkv.ok_local(None, &["fleet", "status", "--output", "json"]) else {
        return "own".to_owned();
    };
    let Ok(rows) = serde_json::from_str::<Value>(out.trim()) else {
        return "own".to_owned();
    };
    rows.as_array()
        .and_then(|rows| rows.iter().find(|r| r["database"] == name))
        .and_then(|r| r["state"].as_str())
        .unwrap_or("own")
        .to_owned()
}
