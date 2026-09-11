//! Forgetting a database the wallet engine has already migrated.
//!
//! Its own file, not another test in `gui_forget.rs`, and deliberately so:
//! `set_data_dir_override` is a process-wide `OnceLock`, so a second test in
//! the same binary would silently inherit the first one's data directory and
//! whichever ran second would fail. Each workspace-root integration test
//! compiles as its own crate and runs as its own process, which is what keeps
//! one override per test honest.

use zkv::data::set_data_dir_override;
use zkv::gui::Engine;
use zkv::remote::ConnectionArgs;

/// A forget must delete the whole database directory, seed included.
///
/// `gui_forget.rs` builds a database in the pre-engine layout, where the
/// engine directory *is* the database root, so it cannot tell the two apart.
/// Once a node has started, librustzcash's files live under `<db>/zec/lrz/`
/// and they diverge: a forget that targeted the engine directory would remove
/// the wallet files and leave `keys.toml`, the age identity, the snapshot and
/// `pending.toml` behind, having told the user the seed was destroyed.
#[tokio::test]
async fn forget_deletes_a_migrated_database_including_the_seed() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let base = tmp.path().to_path_buf();
    set_data_dir_override(base.clone());

    // A database in the post-first-sync layout: wallet files nested under
    // `zec/lrz`, everything zkv owns at the root.
    let dbdir = base.join("migrated");
    let engine_dir = dbdir.join("zec").join("lrz");
    std::fs::create_dir_all(engine_dir.join("blocks")).expect("create engine dir");
    std::fs::write(engine_dir.join("data.sqlite"), b"wallet").expect("write wallet db");
    std::fs::write(
        dbdir.join("keys.toml"),
        "birthday = 100\nrole = \"watch\"\n",
    )
    .expect("write keys.toml");
    std::fs::write(dbdir.join("security-theater-key"), b"identity").expect("write identity");
    std::fs::write(dbdir.join("zkv_state.sqlite"), b"snapshot").expect("write snapshot");

    let engine = Engine::new(ConnectionArgs::default());
    let resp = engine
        .forget("migrated".to_string())
        .await
        .expect("forget should succeed");
    assert!(resp.ok);

    assert!(
        !dbdir.exists(),
        "the whole database directory should be gone, but {} still holds {:?}",
        dbdir.display(),
        std::fs::read_dir(&dbdir)
            .map(|d| d
                .filter_map(|e| e.ok().map(|e| e.file_name()))
                .collect::<Vec<_>>())
            .unwrap_or_default(),
    );
}
