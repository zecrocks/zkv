//! Two zkv processes on one database.
//!
//! zkv used to take an advisory lock of its own and block on it indefinitely.
//! It now holds no lock at all: the embedded node's datadir lock, on the same
//! `.lock` file, is what serializes two zkv processes, and a node start
//! *retries* for `DEFAULT_BUSY_WAIT` (60s) rather than waiting forever. That
//! swap is invisible until two processes actually contend, and nothing else in
//! CI ever runs two at once.
//!
//! So this asserts both halves of the new behaviour: a command whose turn
//! comes waits and then succeeds, and one whose turn never comes gives up
//! inside a bounded window with a message that says why.
//!
//! Needs only zebrad + lightwalletd. The lock holder is `zkv init` on an
//! *unfunded* database, whose poll loop holds one engine (hence the lock) for
//! its whole timeout while waiting for funds that never arrive, so the hold
//! lasts exactly as long as the test wants it to.
//!
//! Skips cleanly unless `ZEBRAD_BIN` and `LIGHTWALLETD_BIN` are set.

use std::time::{Duration, Instant};

use zkv_regtest_harness::{resolve_bin, wait_for_datadir_lock, Lightwalletd, Zebrad, Zkv};

/// Enough chain for `zkv init` to pin a birthday near the tip.
const INITIAL_BLOCKS: u32 = 25;

const DB: &str = "lockdb";

/// Consecutive positive probes before the lock counts as held. The resume path
/// opens and drops one engine for its pre-check sync before the poll loop
/// takes the lock again, so a single sighting is not the steady state.
const LOCK_PROBES: u32 = 3;

/// `DEFAULT_BUSY_WAIT` in `crates/zkv/src/engine/mod.rs`. Not imported: the
/// harness has no zkv dependency on purpose, and a copy that drifts is caught
/// by the bounds below rather than silently tracking the change.
const BUSY_WAIT: Duration = Duration::from_secs(60);

#[tokio::test]
async fn two_processes_on_one_database_serialize() {
    let (Some(zebrad_bin), Some(lwd_bin)) =
        (resolve_bin("ZEBRAD_BIN"), resolve_bin("LIGHTWALLETD_BIN"))
    else {
        eprintln!(
            "SKIP two_processes_on_one_database_serialize: set ZEBRAD_BIN and LIGHTWALLETD_BIN \
             to run the live assertions (see README.md). The harness still compiled and linked."
        );
        return;
    };

    let zebrad = Zebrad::start(&zebrad_bin).await.expect("start zebrad");
    zebrad
        .generate_blocks(INITIAL_BLOCKS)
        .await
        .expect("mine initial blocks");
    let lwd = Lightwalletd::start(&lwd_bin, zebrad.rpc_port)
        .await
        .expect("start lightwalletd");

    let zkv = Zkv::new(lwd.grpc_port).expect("set up zkv home");
    let (addr_before, _funding) = zkv.create_db(DB).expect("create the database");
    let db_dir = zkv.db_dir(DB);

    // ---- it waits its turn ----
    //
    // Hold the database, start a second command, then release. The waiter must
    // come back successful, and must have *waited*: without the stderr line
    // this assertion would also pass for a process that raced in during a gap.
    let mut holder = zkv
        .spawn_online_quiet(None, &["init", DB, "--init-timeout", "900"])
        .expect("spawn the lock holder");
    wait_for_datadir_lock(&db_dir, LOCK_PROBES, Duration::from_secs(180))
        .await
        .expect("the holder should take the datadir lock");

    let waiter = zkv
        .spawn_online(Some(DB), &["sync"])
        .expect("spawn the waiting sync");
    let started = Instant::now();
    tokio::time::sleep(Duration::from_secs(8)).await;
    holder.kill();

    let out = waiter
        .wait_with_output(Duration::from_secs(180))
        .await
        .expect("the waiter should exit once the holder is gone");
    assert!(
        out.status_ok,
        "a sync that waited its turn should succeed, got stderr:\n{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("waiting for it to finish"),
        "the second process should report that it is waiting, got stderr:\n{}",
        out.stderr
    );
    assert!(
        started.elapsed() >= Duration::from_secs(5),
        "it cannot have finished before the holder was killed, took {:?}",
        started.elapsed()
    );

    // ---- it gives up, in bounded time, with a clear reason ----
    let mut holder = zkv
        .spawn_online_quiet(None, &["init", DB, "--init-timeout", "900"])
        .expect("spawn the second lock holder");
    wait_for_datadir_lock(&db_dir, LOCK_PROBES, Duration::from_secs(180))
        .await
        .expect("the second holder should take the datadir lock");

    let started = Instant::now();
    let out = zkv
        .spawn_online(Some(DB), &["sync"])
        .expect("spawn the sync that gives up")
        .wait_with_output(BUSY_WAIT + Duration::from_secs(90))
        .await
        .expect("it must give up rather than hang");
    let waited = started.elapsed();

    assert!(
        !out.status_ok,
        "a sync that never gets the lock must fail, got stderr:\n{}",
        out.stderr
    );
    // Quote-agnostic: the CLI path renders the name in single quotes and the
    // facade path in double, and which one answers is not this test's business.
    assert!(
        out.stderr.contains("in use by another process"),
        "the failure should say the database is in use, got stderr:\n{}",
        out.stderr
    );
    assert!(
        waited >= BUSY_WAIT - Duration::from_secs(5),
        "it should have honoured the {BUSY_WAIT:?} busy wait rather than failing at once, \
         but gave up after {waited:?}",
    );
    assert!(
        waited <= BUSY_WAIT + Duration::from_secs(50),
        "the wait must be bounded, but took {waited:?}",
    );

    // ---- and the database survives it ----
    //
    // The holder is SIGKILLed, which is the case zkv's own lock never had to
    // handle: both locks are OS advisory locks released by the kernel on
    // process death, so the next command should just work.
    holder.kill();
    zkv.ok_online(Some(DB), &["sync"])
        .expect("a sync after the holder dies should succeed with no stale lock to clear");
    assert_eq!(
        zkv.address(DB).expect("read the address back"),
        addr_before,
        "the database must be unchanged by the contention",
    );

    // ---- a rebuild refuses to wipe a database another process is using ----
    //
    // `sync --rebuild` deletes the wallet files outright. It runs outside a
    // node, so nothing about the engine's own lifecycle protects it: without
    // taking the datadir lock first it would unlink the files a running node
    // holds open, and on Unix that node keeps writing the unlinked inodes and
    // flushes a second wallet into the directory afterwards. This asserts it
    // gives up instead, which is the only safe answer while someone else is
    // in there.
    let mut holder = zkv
        .spawn_online_quiet(None, &["init", DB, "--init-timeout", "900"])
        .expect("spawn the lock holder for the rebuild check");
    wait_for_datadir_lock(&db_dir, LOCK_PROBES, Duration::from_secs(180))
        .await
        .expect("the holder should take the datadir lock");

    let out = zkv
        .spawn_online(Some(DB), &["sync", "--rebuild"])
        .expect("spawn the rebuild")
        .wait_with_output(Duration::from_secs(120))
        .await
        .expect("the rebuild must return rather than hang");
    assert!(
        !out.status_ok,
        "a rebuild must refuse while another process holds the database, got stderr:\n{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("in use by another process"),
        "the refusal should say the database is in use, got stderr:\n{}",
        out.stderr
    );
    // The wipe must not have happened: the wallet files are still there and
    // the database still reads.
    holder.kill();
    zkv.ok_online(Some(DB), &["sync"])
        .expect("the database should still sync after the refused rebuild");
    assert_eq!(
        zkv.address(DB).expect("read the address back"),
        addr_before,
        "a refused rebuild must leave the database untouched",
    );
}
