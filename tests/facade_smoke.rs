//! External-crate smoke test for the `Database` facade.
//!
//! Like `tests/address_validation.rs`, this file compiles as if it were a
//! separate crate that depends on `zkv`. It proves the high-level facade
//! is reachable from outside the workspace without referencing any
//! `internal::*` items.

use zkv::{
    config::Role,
    data::{set_data_dir_override, Network},
    db::{AuthRegistry, Authority, Confirmations, Database, Scope, ZkvError},
    protocol::Capability,
    remote::ConnectionArgs,
};

#[test]
fn opening_missing_db_returns_structured_error() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    set_data_dir_override(tmp.path().to_path_buf());

    match Database::open("does-not-exist", ConnectionArgs::default()) {
        Err(ZkvError::UnknownDatabase(name)) => {
            assert_eq!(name, "does-not-exist");
        }
        Err(other) => panic!("expected UnknownDatabase, got: {other:#}"),
        Ok(_) => panic!("expected error opening missing database"),
    }
}

#[test]
fn confirmations_helpers_work() {
    assert_eq!(Confirmations::Mempool.as_u32(), 0);
    assert_eq!(Confirmations::OneBlock.as_u32(), 1);
    assert_eq!(Confirmations::Default.as_u32(), 3);
    assert_eq!(Confirmations::Custom(7).as_u32(), 7);
    assert_eq!(Confirmations::default(), Confirmations::Default);
    // u32 -> Confirmations conversion preserves the integer.
    let c: Confirmations = 5u32.into();
    assert_eq!(c.as_u32(), 5);
}

#[test]
fn public_types_compile() {
    // Compile-only smoke check: every type the README's quick-start
    // snippet names must be reachable from the public API.
    fn _accept(_db: Database, _role: Role, _network: Network, _err: ZkvError) {}
    let _conn = ConnectionArgs::default();
    let _confs = Confirmations::default();
}

#[test]
fn history_surface_is_reachable() {
    use zkv::db::{HistoryEntry, HistoryResult, HistoryStatus};

    // The history view types and the `Database::history` method must be
    // reachable from outside the crate without touching `internal::*`.
    fn _accept(_r: HistoryResult, _e: HistoryEntry, _s: HistoryStatus) {}
    fn _signature(db: &Database) -> zkv::db::Result<HistoryResult> {
        // filter, min_confs, limit (None = all), offset
        db.history(Some("some-key"), Confirmations::Default, Some(100), 0)
    }
    let _ = (_accept, _signature);

    // The status enum's variants and fields are public.
    let s = HistoryStatus::Confirmed { confirmations: 12 };
    assert!(matches!(s, HistoryStatus::Confirmed { confirmations: 12 }));
}

#[test]
fn auth_types_are_reachable() {
    // The owner/writer authorization surface must be usable from an external
    // crate: build a scope, inspect a registry, match an authority, and name
    // the Unauthorized error variant.
    let scope = Scope::parse("CREATE,DESTROY").expect("valid scope");
    assert!(scope.contains(Capability::Create));
    assert!(scope.contains(Capability::Destroy));
    assert!(!scope.contains(Capability::Update));
    assert_eq!(scope.to_wire(), "CREATE,DESTROY");

    // A default registry is empty (no owners, no writers).
    let reg = AuthRegistry::default();
    assert!(reg.is_empty());
    assert!(reg.authority_of("02abc").is_none());

    // The Authority enum and Unauthorized error variant are public.
    fn _accept_authority(_a: Authority) {}
    fn _name_unauthorized() -> ZkvError {
        ZkvError::Unauthorized("example".into())
    }
    let _ = _name_unauthorized();
}

#[test]
fn read_freshness_surface_is_reachable() {
    use zkv::db::ReadResult;

    // The freshness-aware read returns the replayed state plus the height it
    // reflects; `ReadResult` and its fields are reachable without touching
    // `internal::*`.
    fn _as_of(r: ReadResult) -> Option<u32> {
        let _ = r.replay;
        r.as_of_height
    }
    fn _signature(db: &Database) -> zkv::db::Result<ReadResult> {
        db.read_at(Confirmations::Default)
    }
    let _ = (_as_of, _signature);
}

#[test]
fn no_sync_write_surface_is_reachable() {
    // The `*_no_sync` write variants skip the pre-broadcast sync (they still
    // broadcast immediately) so a consumer driving its own sync cadence can
    // write data and manage roles without the forced refresh.
    async fn _signatures(db: &Database, pk: &str, scope: &Scope) -> zkv::db::Result<String> {
        db.set_no_sync("k", "v").await?;
        db.del_no_sync("k").await?;
        db.grant_owner_no_sync(pk).await?;
        db.revoke_owner_no_sync(pk).await?;
        db.grant_writer_no_sync(pk, scope).await?;
        db.revoke_writer_no_sync(pk).await
    }
    let _ = _signatures;
}

#[test]
#[allow(deprecated)]
fn deprecated_offline_aliases_still_resolve() {
    // `set_offline`/`del_offline` remain as deprecated aliases so existing
    // callers keep compiling after the rename to `*_no_sync`.
    async fn _aliases(db: &Database) -> zkv::db::Result<String> {
        db.set_offline("k", "v").await?;
        db.del_offline("k").await
    }
    let _ = _aliases;
}
