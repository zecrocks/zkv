//! Unfunded regtest surface: everything zkv can (and must refuse to) do
//! before a database holds funds or an INIT. Needs only zebrad + lightwalletd
//! (no funding wallet), so it runs even where `DEVTOOL_BIN` is unavailable.
//!
//! Covers: creating a regtest database against a live chain (birthday pinned
//! near the tip), the self-describing `zkvregtest1...` address (offline
//! `inspect` recovers network/pool/keys), the not-initialized read refusal,
//! the zero balance, and `list` visibility.
//!
//! Skips cleanly unless `ZEBRAD_BIN` and `LIGHTWALLETD_BIN` are set.

use zkv_regtest_harness::{resolve_bin, Lightwalletd, Zebrad, Zkv};

/// Enough chain for `zkv init` to pin a birthday: the near-tip default backs
/// off `BIRTHDAY_SAFETY_BUFFER` (10) blocks and fetches the tree state one
/// block below that, so the tip must comfortably exceed 12.
const INITIAL_BLOCKS: u32 = 25;

#[tokio::test]
async fn regtest_unfunded_surface() {
    let (Some(zebrad_bin), Some(lwd_bin)) =
        (resolve_bin("ZEBRAD_BIN"), resolve_bin("LIGHTWALLETD_BIN"))
    else {
        eprintln!(
            "SKIP regtest_unfunded_surface: set ZEBRAD_BIN and LIGHTWALLETD_BIN to run the live \
             assertions (see README.md). The harness still compiled and linked."
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
    let (addr, funding) = zkv.create_db("db").expect("zkv init --non-interactive");
    assert!(
        addr.starts_with("zkvregtest1"),
        "expected a zkvregtest1 address, got {addr}"
    );
    assert!(
        funding.starts_with("uregtest1"),
        "expected a uregtest1 funding UA, got {funding}"
    );

    // The address is fully self-describing: offline `inspect` recovers the
    // network, pool, birthday, and the creator's signing key from the string.
    let info = zkv.inspect_json(&addr).expect("inspect");
    assert_eq!(info["network"], "regtest");
    assert_eq!(info["pool"], "orchard");
    let birthday = info["birthday"].as_u64().expect("birthday");
    assert!(
        birthday >= 1 && birthday <= u64::from(INITIAL_BLOCKS),
        "near-tip birthday should land on the young chain, got {birthday}"
    );
    assert!(info["signing_key"]
        .as_str()
        .is_some_and(|k| k.starts_with("zkvid1")));
    assert!(info["funding_address"]
        .as_str()
        .is_some_and(|f| f.starts_with("uregtest1")));
    assert!(info["receiver"]
        .as_str()
        .is_some_and(|r| r.starts_with("regtest:")));

    // Reads must refuse a never-INITed database rather than serve empty state.
    let get = zkv
        .run_online(Some("db"), &["get", "somekey", "--confirmations", "1"])
        .expect("spawn get");
    assert!(
        !get.status_ok && get.stderr.contains("not initialized"),
        "get on an uninitialized database must fail with the init hint, got ok={} stderr:\n{}",
        get.status_ok,
        get.stderr
    );

    // A brand-new wallet is empty. (Tolerate the pre-first-scan "no wallet
    // summary" state; a summary that exists must be zero.)
    match zkv.balance("db") {
        Ok(balance) => assert_eq!(balance, 0.0, "fresh wallet must hold nothing"),
        Err(e) => assert!(
            format!("{e:#}").contains("no wallet summary"),
            "unexpected balance failure: {e:#}"
        ),
    }

    // And visible in `list` (an offline command).
    let list = zkv.ok_local(None, &["list"]).expect("list");
    assert!(list.contains("db"), "list must show the database:\n{list}");

    // Re-running init non-interactively on the existing, unfunded database is
    // the documented resume path: it must succeed (exit 0) and ask for funds
    // rather than recreate or corrupt anything.
    let resume = zkv
        .run_online(
            None,
            &["init", "db", "--network", "regtest", "--non-interactive"],
        )
        .expect("spawn init resume");
    assert!(
        resume.status_ok && resume.stderr.contains("Fund the wallet"),
        "non-interactive resume must ask for funding, got ok={} stderr:\n{}",
        resume.status_ok,
        resume.stderr
    );
}
