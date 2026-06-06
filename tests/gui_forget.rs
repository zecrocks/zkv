//! External-crate integration test for the GUI engine's "forget database"
//! action (the Settings Danger Zone).
//!
//! Like `tests/facade_smoke.rs`, this compiles as a separate crate consuming
//! `zkv`'s public surface. It proves the success path actually deletes the
//! database directory on disk and clears the "current" marker, against a real
//! (hand-crafted) data directory. It uses `set_data_dir_override` (a process
//! `OnceLock`, set once) rather than the `ZKV_DATA` env var, so it can't race
//! other tests on a process-global environment.

use zkv::data::set_data_dir_override;
use zkv::db::ZkvError;
use zkv::gui::Engine;
use zkv::remote::ConnectionArgs;

#[tokio::test]
async fn forget_deletes_the_database_directory_and_clears_current() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let base = tmp.path().to_path_buf();
    set_data_dir_override(base.clone());

    // A directory with a parseable keys.toml is all the forget path's existence
    // check needs; craft a minimal watch-only database by hand (no network).
    let dbdir = base.join("mydb");
    std::fs::create_dir_all(&dbdir).expect("create db dir");
    std::fs::write(
        dbdir.join("keys.toml"),
        "birthday = 100\nrole = \"watch\"\n",
    )
    .expect("write keys.toml");
    // Point the "current" marker at it, so we can assert forget clears it.
    std::fs::write(base.join("current"), "mydb").expect("write current marker");

    let engine = Engine::new(ConnectionArgs::default());
    let resp = engine
        .forget("mydb".to_string())
        .await
        .expect("forget should succeed");
    assert!(resp.ok);

    assert!(!dbdir.exists(), "the database directory should be deleted");
    assert!(
        !base.join("current").exists(),
        "the current marker should be cleared when it pointed at the forgotten db"
    );

    // Forgetting an unknown database is a clean structured error, not a panic
    // or a silent success.
    match engine.forget("ghost".to_string()).await {
        Err(ZkvError::UnknownDatabase(name)) => assert_eq!(name, "ghost"),
        Err(other) => panic!("expected UnknownDatabase for a missing db, got error: {other:#}"),
        Ok(_) => panic!("expected an error forgetting a missing db, got Ok"),
    }
}
