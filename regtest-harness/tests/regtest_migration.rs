//! Upgrading a database in place: a zkv release that predates the wallet
//! engine lays one down against a live chain, and the build under test adopts
//! it and must read back exactly what was written.
//!
//! This is the test that says an upgrade is safe. Adoption is meant to be a
//! one-field change to `keys.toml`, moving no wallet data and deleting
//! nothing, so the bar is strict: after adopting, every read returns
//! byte-identical output to what the old binary produced, including the
//! signed history and the role registry, and the snapshot the old binary
//! promoted is still there rather than silently rebuilt.
//!
//! It also pins where the upgrade stops being reversible, which is **not** at
//! adoption. Adoption is offline and additive, and an old binary still reads
//! the database afterwards. The one-way step is the first time a node runs:
//! zecd relocates librustzcash's files into `<db>/zec/lrz/` at startup,
//! unconditionally, and a pre-engine binary cannot find them there. The test
//! asserts both halves in order, so the boundary is a decision rather than a
//! surprise.
//!
//! Needs both binaries. `ZKV_OLD_BIN` is the pre-engine release (CI builds it
//! from a pinned ref); `ZKV_BIN` is the build under test. Skips cleanly
//! unless the chain binaries and `ZKV_OLD_BIN` are all present, so it
//! compiles and links everywhere.

use zkv_regtest_harness::{resolve_bin, zkv_bin, Lightwalletd, Zebrad, Zkv};

/// Enough chain for `zkv init` to pin a birthday near the tip.
const INITIAL_BLOCKS: u32 = 25;

/// The database the old binary creates and the new one adopts.
const DB: &str = "upgraded";

#[tokio::test]
async fn a_pre_engine_database_is_adopted_in_place() {
    let (Some(zebrad_bin), Some(lwd_bin), Some(old_bin)) = (
        resolve_bin("ZEBRAD_BIN"),
        resolve_bin("LIGHTWALLETD_BIN"),
        resolve_bin("ZKV_OLD_BIN"),
    ) else {
        eprintln!(
            "SKIP a_pre_engine_database_is_adopted_in_place: set ZEBRAD_BIN, LIGHTWALLETD_BIN \
             and ZKV_OLD_BIN (a pre-engine zkv release) to run the live assertions (see \
             README.md). The harness still compiled and linked."
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

    // The old release creates the database, against the live chain.
    let old = Zkv::with_bin(old_bin, lwd.grpc_port).expect("set up zkv home");
    let (addr, _funding) = old.create_db(DB).expect("zkv init --non-interactive");
    assert!(addr.starts_with("zkvregtest1"), "unexpected address {addr}");

    // What the old binary sees, before anything is upgraded. An unfunded
    // database has no INIT, so these are the reads that work without funds:
    // the address, the key listing, and the history. They are the baseline
    // the adopted database has to reproduce.
    let keys_before = old.keys(DB, "*", 0).expect("keys");
    let history_before = old.history_json(DB, 0).expect("history");
    let address_before = old.address(DB).expect("address");
    let keys_toml_before =
        std::fs::read_to_string(old.data_dir().join(DB).join("keys.toml")).expect("read keys.toml");
    assert!(
        !keys_toml_before.contains("ufvk"),
        "a pre-engine keys.toml carries no viewing-key pin:\n{keys_toml_before}",
    );

    // Swap in the build under test, over the very same directory.
    let new = old.with_other_bin(zkv_bin()).expect("new binary");

    // It reports the database as not yet adopted, and says so without
    // changing anything.
    let status = new
        .ok_local(Some(DB), &["migrate", "--status", "--output", "json"])
        .expect("migrate --status");
    let status: serde_json::Value = serde_json::from_str(&status).expect("status json");
    assert_eq!(status["adopted"], false, "not adopted yet: {status}");
    assert_eq!(
        std::fs::read_to_string(new.data_dir().join(DB).join("keys.toml")).unwrap(),
        keys_toml_before,
        "--status must not write anything",
    );

    // Adopt it.
    let adopted = new
        .ok_local(Some(DB), &["migrate", "--output", "json"])
        .expect("migrate");
    let adopted: serde_json::Value = serde_json::from_str(&adopted).expect("migrate json");
    assert_eq!(adopted["adopted"], true, "adoption reported: {adopted}");
    assert_eq!(adopted["changed"], true, "the first run changes the file");

    // Idempotent: running it again is a no-op, not a second migration.
    let again = new
        .ok_local(Some(DB), &["migrate", "--output", "json"])
        .expect("migrate again");
    let again: serde_json::Value = serde_json::from_str(&again).expect("json");
    assert_eq!(again["changed"], false, "a repeat run changes nothing");

    // `keys.toml` gained the pin and kept everything else, line for line.
    let keys_toml_after =
        std::fs::read_to_string(new.data_dir().join(DB).join("keys.toml")).expect("read keys.toml");
    assert!(
        keys_toml_after.contains("ufvk"),
        "the pin was written:\n{keys_toml_after}",
    );
    for line in keys_toml_before.lines().filter(|l| !l.trim().is_empty()) {
        assert!(
            keys_toml_after.contains(line),
            "adoption dropped a field: {line:?}\nbefore:\n{keys_toml_before}\nafter:\n\
             {keys_toml_after}",
        );
    }

    // Adoption touches only `keys.toml`: the wallet database, the block cache
    // and the snapshot are the same files, not rebuilt ones, and they are
    // still where the old binary left them.
    for file in ["data.sqlite", "blockmeta.sqlite"] {
        assert!(
            new.data_dir().join(DB).join(file).exists(),
            "{file} should still be there after adoption",
        );
    }

    // So adoption on its own is reversible: the old binary opens what the new
    // one wrote, ignoring the extra field rather than rejecting it. This is
    // checked *here*, before anything starts a node, because that is exactly
    // the window in which it is true (see below).
    assert_eq!(old.address(DB).expect("address"), address_before);
    assert_eq!(old.history_json(DB, 0).expect("history"), history_before);

    // Every read on the new binary agrees with what the old one reported.
    // `address` is offline; `keys` and `history` sync, which is what brings a
    // node up for the first time on this database.
    assert_eq!(new.address(DB).expect("address"), address_before);
    assert_eq!(new.keys(DB, "*", 0).expect("keys"), keys_before);
    assert_eq!(new.history_json(DB, 0).expect("history"), history_before);

    // Starting a node is the one-way step, and it is worth pinning rather
    // than discovering. zecd relocates librustzcash's files to `<db>/zec/lrz/`
    // on its first start, unconditionally and under its own datadir lock, so
    // from here on a pre-engine binary looks at the database root and finds
    // nothing there.
    let engine_dir = new.data_dir().join(DB).join("zec").join("lrz");
    for file in ["data.sqlite", "blockmeta.sqlite"] {
        assert!(
            engine_dir.join(file).exists(),
            "the node's first start moves {file} into zec/lrz/",
        );
        assert!(
            !new.data_dir().join(DB).join(file).exists(),
            "{file} is moved, not copied: leaving one at the root would give \
             the two readers of this directory different databases",
        );
    }
    // `keys.toml` and zkv's own snapshot are not the engine's and do not move.
    for file in ["keys.toml", "zkv_state.sqlite"] {
        assert!(
            new.data_dir().join(DB).join(file).exists(),
            "{file} is zkv's own and stays at the database root",
        );
    }

    // The new binary reads across the move without noticing it.
    assert_eq!(new.address(DB).expect("address"), address_before);
    assert_eq!(new.keys(DB, "*", 0).expect("keys"), keys_before);

    // And the downgrade window has closed. The old binary does not fail
    // confusingly-but-harmlessly here: it finds no wallet database at the
    // root, creates an empty one, and reports the database as having no key
    // imported. Asserted so that anyone changing the adoption story has to
    // decide about this deliberately.
    let err = old
        .address(DB)
        .expect_err("a pre-engine binary cannot find a relocated wallet");
    let err = format!("{err:#}");
    assert!(
        err.contains("no wallet key imported"),
        "expected the no-account error from the old binary, got: {err}",
    );
}
