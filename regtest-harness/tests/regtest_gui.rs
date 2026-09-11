//! The GUI browser transport, driven over its real HTTP API against a live
//! chain.
//!
//! This is the surface with the least coverage and the worst track record: a
//! GUI-only regression (the auto-sync loop deadlocking against its own node's
//! datadir lock) reached CI green during the zecd port and had to be caught by
//! re-reading the code, because nothing in CI drives the GUI at all. The
//! in-process route tests in `gui/mod.rs` cover the token guard and the error
//! mapping, but they never touch a chain, so nothing exercised the loop, a
//! write, or the lock.
//!
//! What is asserted here, in value order: the security guard, that the demo
//! database is not auto-provisioned (which would mean CI dialed a public
//! server), that the **auto-sync loop actually advances** with no CLI
//! involvement, that a **write through the GUI** lands on chain and reads back
//! through the CLI, the `ZkvError` -> HTTP contract the frontend branches on,
//! and **CLI-vs-GUI lock contention** in the direction that can be made
//! deterministic.
//!
//! Skips cleanly unless `ZEBRAD_BIN`, `LIGHTWALLETD_BIN` and `DEVTOOL_BIN` are
//! set, or if the `zkv` under test was built without the `gui` feature (the
//! lean CLI build has no `gui-browser` subcommand).

use std::time::Duration;

use serde_json::{json, Value};
use zkv_regtest_harness::{
    resolve_bin, wait_for_datadir_lock, wait_until, FundedStack, GuiServer, Zkv,
};

/// 0.1 TAZ, as in the main lifecycle test: far more than the one write here
/// needs.
const FUND_ZATOSHIS: u64 = 10_000_000;

/// Blocks mined after a broadcast: 1 confirms the write, 3 make the change
/// spendable again, 1 spare for tip skew.
const CONFIRM_BLOCKS: u32 = 5;

const DB: &str = "guidb";
/// A second database, created through the GUI and deliberately never INITed,
/// so it can play both the error case and the lock holder.
const UNINIT: &str = "uninit";

const LOCK_PROBES: u32 = 3;

#[tokio::test]
async fn the_gui_drives_a_real_database() {
    let (Some(zebrad_bin), Some(lwd_bin), Some(devtool_bin)) = (
        resolve_bin("ZEBRAD_BIN"),
        resolve_bin("LIGHTWALLETD_BIN"),
        resolve_bin("DEVTOOL_BIN"),
    ) else {
        eprintln!(
            "SKIP the_gui_drives_a_real_database: set ZEBRAD_BIN, LIGHTWALLETD_BIN and \
             DEVTOOL_BIN (see README.md). The harness still compiled and linked."
        );
        return;
    };

    let stack = FundedStack::up(&zebrad_bin, &lwd_bin, &devtool_bin)
        .await
        .expect("bring up a funded regtest stack");
    let zkv = Zkv::new(stack.lwd.grpc_port).expect("set up zkv home");

    // A lean build has no `gui-browser`; skip rather than fail, so a local run
    // against a CLI-only binary still reports the rest of the suite honestly.
    if !zkv
        .run_local(None, &["gui-browser", "--help"])
        .expect("probe for gui-browser")
        .status_ok
    {
        eprintln!(
            "SKIP the_gui_drives_a_real_database: this zkv has no `gui-browser` subcommand; \
             build it with --features gui."
        );
        return;
    }

    let (_addr, funding_ua) = zkv.create_db(DB).expect("create the database");
    stack
        .fund(&funding_ua, FUND_ZATOSHIS)
        .await
        .expect("fund the database");
    zkv.init_until_confirmed(DB, &stack.zebrad, Duration::from_secs(300))
        .await
        .expect("broadcast + confirm INIT");
    stack
        .zebrad
        .generate_blocks(CONFIRM_BLOCKS)
        .await
        .expect("confirm INIT change");

    let gui = GuiServer::start(&zkv, Duration::from_secs(120))
        .await
        .expect("start the GUI server");

    // ---- the security guard ----
    //
    // Cheap, and it is the whole thing standing between a hostile page in the
    // user's browser and a wallet that can spend.
    let (status, body) = gui
        .get_raw("/api/status", None, None)
        .await
        .expect("request without a token");
    assert_eq!(status, 401, "an unauthenticated API call must be refused");
    assert_eq!(body["code"], "bad_token");

    let (status, body) = gui
        .get_raw("/api/status", Some(gui.token()), Some("evil.example"))
        .await
        .expect("request with a foreign Host");
    assert_eq!(
        status, 403,
        "a foreign Host must be refused (DNS rebinding)"
    );
    assert_eq!(body["code"], "bad_host");

    // ---- status and the database list ----
    let (status, body) = gui.get("/api/status").await.expect("status");
    assert_eq!(status, 200);
    assert_eq!(
        body["server"],
        format!("127.0.0.1:{}", stack.lwd.grpc_port),
        "the GUI should be talking to our regtest lightwalletd, not a public server",
    );

    let (status, body) = gui.get("/api/databases").await.expect("list databases");
    assert_eq!(status, 200);
    let rows = body.as_array().expect("a list of databases");
    let ours = rows
        .iter()
        .find(|d| d["name"] == DB)
        .unwrap_or_else(|| panic!("{DB} missing from {rows:#?}"));
    assert_eq!(ours["role"], "admin");
    assert_eq!(ours["network"], "regtest");
    // The auto-sync loop's first act is to provision the bundled demo
    // database, which dials a public testnet server. The harness's data dir
    // carries the marker that suppresses it; if this ever fires, CI has
    // started reaching the internet.
    assert!(
        !rows.iter().any(|d| d["name"] == "demo-oracles"),
        "the demo database must not be auto-provisioned in the sandbox, got {rows:#?}",
    );

    // ---- the auto-sync loop actually advances ----
    //
    // Nothing below touches the CLI, and the list endpoint only reads local
    // state, so the only thing that can move `synced` is `run_auto_sync`.
    let before = ours["synced"].as_u64().unwrap_or(0);
    stack
        .zebrad
        .generate_blocks(8)
        .await
        .expect("mine past the wallet");
    let tip = stack.zebrad.tip_height().await.expect("tip");

    // Polled inline rather than through `wait_until`, which takes a synchronous
    // closure for the CLI helpers; this one has to await.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    let synced = loop {
        let (_, rows) = gui.get("/api/databases").await.expect("list databases");
        let synced = rows
            .as_array()
            .and_then(|rows| rows.iter().find(|d| d["name"] == DB))
            .and_then(|d| d["synced"].as_u64())
            .unwrap_or(0);
        if synced >= tip {
            break synced;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the auto-sync loop should have reached tip {tip} on its own, but the GUI still \
             reports {synced} (it was {before} before the blocks were mined)",
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    assert!(synced >= tip);

    // ---- a write through the GUI, read back through the CLI ----
    let (status, body) = gui
        .post(
            &format!("/api/databases/{DB}/keys"),
            json!({ "key": "from-gui", "value": "wrote-it" }),
        )
        .await
        .expect("set a key through the GUI");
    assert_eq!(status, 200, "the GUI write should succeed, got {body:#?}");
    let txid = body["txid"].as_str().expect("a txid").to_owned();
    assert_eq!(txid.len(), 64, "expected a txid, got {txid:?}");

    stack
        .zebrad
        .generate_blocks(CONFIRM_BLOCKS)
        .await
        .expect("confirm the GUI write");
    wait_until(
        || -> anyhow::Result<bool> {
            Ok(zkv.get(DB, "from-gui", 1)? == Some("wrote-it".to_owned()))
        },
        "the CLI to read back what the GUI wrote",
        Duration::from_secs(180),
    )
    .await
    .expect("a GUI write is an ordinary on-chain write");

    let (status, body) = gui
        .get(&format!("/api/databases/{DB}/history"))
        .await
        .expect("history");
    assert_eq!(status, 200);
    let entry = body["entries"]
        .as_array()
        .expect("history entries")
        .iter()
        .find(|e| e["key"] == "from-gui")
        .expect("the GUI write should appear in history");
    assert_eq!(
        entry["verified"], true,
        "the GUI's own write must verify against the registry",
    );

    // ---- the ZkvError -> HTTP contract the frontend branches on ----
    let (status, _) = gui
        .post(
            "/api/databases",
            json!({ "name": UNINIT, "network": "regtest" }),
        )
        .await
        .expect("create a second database through the GUI");
    assert_eq!(status, 200, "the GUI should be able to create a database");

    let (status, body) = gui
        .post(
            &format!("/api/databases/{UNINIT}/keys"),
            json!({ "key": "nope", "value": "x" }),
        )
        .await
        .expect("write to an uninitialized database");
    assert_eq!(
        status, 409,
        "writing to an uninitialized database must be a 409, got {body:#?}",
    );
    assert_eq!(body["code"], "not_initialized");

    // ---- pausing is not a failure ----
    //
    // A pause cancels whatever sync is in flight. That cancellation used to
    // reach the GUI's error accounting as an ordinary failure, and two of them
    // are enough to raise a per-database error banner, so a user who paused
    // twice was shown a sync failure for a button they pressed on purpose.
    // Mining first makes it likely a sync is actually running to be cancelled.
    stack
        .zebrad
        .generate_blocks(8)
        .await
        .expect("give the auto-sync something to do");
    for _ in 0..2 {
        let (status, _) = gui
            .post(
                &format!("/api/databases/{DB}/pause"),
                json!({ "paused": true }),
            )
            .await
            .expect("pause");
        assert_eq!(status, 200);
        tokio::time::sleep(Duration::from_secs(2)).await;
        let (status, _) = gui
            .post(
                &format!("/api/databases/{DB}/pause"),
                json!({ "paused": false }),
            )
            .await
            .expect("resume");
        assert_eq!(status, 200);
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    let (status, detail) = gui
        .get(&format!("/api/databases/{DB}"))
        .await
        .expect("detail after pausing");
    assert_eq!(status, 200);
    assert_eq!(
        detail["sync_error"],
        Value::Null,
        "pausing and resuming must not raise a sync-error banner, got {:#?}",
        detail["sync_error"],
    );

    // ---- CLI vs GUI on one database ----
    //
    // The regression this test exists for. Only the deterministic direction is
    // hard-asserted: a CLI process holds the lock, the GUI asks to sync, and
    // must wait rather than fail. The reverse (a CLI command arriving during a
    // GUI write) depends on landing inside a window measured in seconds, and
    // the deterministic wait assertion already lives in `regtest_lock.rs`.
    let mut holder = zkv
        .spawn_online_quiet(None, &["init", UNINIT, "--init-timeout", "900"])
        .expect("spawn the CLI lock holder");
    wait_for_datadir_lock(&zkv.db_dir(UNINIT), LOCK_PROBES, Duration::from_secs(180))
        .await
        .expect("the CLI should take the datadir lock");

    let sync = tokio::spawn({
        let path = format!("/api/databases/{UNINIT}/sync");
        async move { gui.post(&path, json!({})).await }
    });
    tokio::time::sleep(Duration::from_secs(8)).await;
    holder.kill();

    let (status, body) = sync
        .await
        .expect("the GUI sync task should not panic")
        .expect("the GUI sync should return");
    assert_eq!(
        status, 200,
        "the GUI must wait out a CLI holding the lock and then succeed, got {body:#?}",
    );

    // The task owned the server, so awaiting it dropped and killed it. That is
    // worth one more assertion: `serve` only stops gracefully on Ctrl-C, so
    // this is a SIGKILL, and the CLI should find nothing to clean up after it
    // (both locks are OS advisory locks the kernel releases on process death).
    zkv.ok_online(Some(DB), &["sync"])
        .expect("a CLI sync after the GUI is killed should find no stale lock");
}
