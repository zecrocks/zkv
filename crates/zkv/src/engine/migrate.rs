//! Adopting an existing zkv database into the wallet engine.
//!
//! A zkv database predating the engine is already almost a zecd wallet. Both
//! projects keep the same three files in the same layout, built by the same
//! `zcash_client_sqlite` version through the same `FsBlockDb`, and both
//! describe the wallet in a `keys.toml` whose `mnemonic`, `network` and
//! `birthday` fields have the same names and, for the seed, byte-identical age
//! armor. So adoption moves no data and rewrites no database. It adds one
//! field.
//!
//! # The field, and why zkv writes it
//!
//! zecd pins the wallet's viewing key in `keys.toml` and checks it against the
//! account in `data.sqlite` at every start, so a swapped database or a swapped
//! key file is caught rather than quietly served. When the field is missing it
//! trusts what it finds and writes it back.
//!
//! zkv cannot let that happen, for a boring reason: zecd's writer serializes
//! through a struct that has no `role`, `pool` or `zkv_address`, so its
//! rewrite would drop the three fields zkv depends on. `pool` in particular is
//! load-bearing three times over (which output pool reads filter to, which
//! address memo writes are sent to, and which receiver every signature binds
//! to), and losing it would leave a database that reads as empty.
//!
//! So zkv writes the pin first, from its own key material, through its own
//! writer. That is strictly stronger than letting the node fill it in: the
//! value comes from the seed (or, for a watch-only database, from the stored
//! address) rather than from the database being vouched for, so a swapped
//! `data.sqlite` still fails the check instead of being adopted as authentic.
//!
//! # What adoption does not do
//!
//! It does not touch `data.sqlite`, `blockmeta.sqlite` or `blocks/`; it does
//! not delete the snapshot or `pending.toml`; and it needs no network. A
//! database that has been adopted still opens on an older zkv build, which
//! ignores the extra field.

use anyhow::{anyhow, Context};
use secrecy::ExposeSecret;
use zcash_client_backend::data_api::{Account, WalletRead};
use zcash_keys::keys::UnifiedSpendingKey;
use zip32::AccountId;

use crate::config::{Role, WalletConfig};
use crate::data::open_wallet_db;
use crate::internal::protocol::parse_zkv_addr;

use super::EngineError;

/// What [`ensure_adopted`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adoption {
    /// The database was already adopted; nothing was written.
    AlreadyAdopted,
    /// The viewing key was derived and pinned just now.
    Pinned,
}

/// Make sure this database carries the viewing-key pin the engine needs,
/// deriving and writing it if it does not.
///
/// Idempotent, offline, and safe to call on every open: the common case reads
/// one field and returns.
pub fn ensure_adopted(db_name: &str, cfg: &mut WalletConfig) -> Result<Adoption, EngineError> {
    if cfg.ufvk.is_some() {
        return Ok(Adoption::AlreadyAdopted);
    }
    let ufvk = derive_ufvk(db_name, cfg).map_err(EngineError::Other)?;
    cfg.pin_ufvk(&ufvk).map_err(EngineError::Other)?;
    tracing::info!(
        db = db_name,
        "adopted the database into the wallet engine (pinned its viewing key)",
    );
    Ok(Adoption::Pinned)
}

/// This database's viewing key, in the encoded form the pin stores.
///
/// Derived from what `keys.toml` itself holds wherever possible, so the pin is
/// an independent statement about which wallet this is rather than an echo of
/// whatever `data.sqlite` happens to contain:
///
/// * an admin database derives it from its seed, and
/// * a watch-only database reads it out of the `zkv1…` address it was created
///   from, which is that same key under a different label.
///
/// Only a watch-only database old enough to predate the stored address has
/// neither, and there the wallet database is the sole surviving source.
///
/// The two roles produce keys of different *shape*, and that is correct rather
/// than an inconsistency: the pin has to equal whatever the account in
/// `data.sqlite` holds, and the two roles put different things there. An admin
/// database's account was created from the seed, so it carries the full
/// viewing key; a watch-only database imported the pool-restricted key its
/// `zkv1…` address encodes. Deriving each from the same source its account
/// came from is what keeps the pin matching.
fn derive_ufvk(db_name: &str, cfg: &WalletConfig) -> anyhow::Result<String> {
    match cfg.role {
        Role::Admin => {
            let seed = cfg
                .decrypt_seed()
                .context("reading the seed to derive this database's viewing key")?;
            // Match the account the wallet database actually holds, so the pin
            // describes that account rather than an assumed one. zkv creates
            // account zero, which is also the fallback when the database has
            // not been created yet.
            let account_index = account_index_in_db(cfg).unwrap_or(AccountId::ZERO);
            let usk =
                UnifiedSpendingKey::from_seed(&cfg.network, seed.expose_secret(), account_index)
                    .map_err(crate::error::Error::from)?;
            Ok(usk.to_unified_full_viewing_key().encode(&cfg.network))
        }
        Role::Watch => {
            if let Some(addr) = cfg.zkv_address.as_deref() {
                let parsed = parse_zkv_addr(addr)?;
                return Ok(parsed.ufvk.encode(&cfg.network));
            }
            // A watch database created before the address was recorded. The
            // wallet database is all that is left to read it from; if that is
            // gone too, the database cannot be identified at all and has to be
            // re-imported from its address.
            ufvk_in_db(cfg).ok_or_else(|| {
                anyhow!(
                    "the {db_name:?} database is watch-only and records neither its zkv address \
                     nor a wallet key, so its viewing key cannot be recovered. Re-import it with \
                     `zkv watch <zkv1…>`"
                )
            })
        }
    }
}

/// This database's wallet database.
///
/// Resolved rather than joined: the engine nests librustzcash's files under the
/// database directory once a node has started on it, and adoption runs before
/// that has ever happened, so both layouts have to read.
fn wallet_db_path(cfg: &WalletConfig) -> Option<std::path::PathBuf> {
    crate::data::engine_dir_in(cfg.db_dir())
        .ok()
        .map(|dir| dir.join(crate::data::DATA_DB))
}

/// The ZIP-32 account index of the account in this database's wallet DB, if
/// there is one to read. `None` covers every "not available" case (no
/// database yet, no account yet, an imported account with no derivation path),
/// since all of them mean the same thing here: fall back to account zero.
fn account_index_in_db(cfg: &WalletConfig) -> Option<AccountId> {
    let db = open_wallet_db(wallet_db_path(cfg)?, cfg.network).ok()?;
    let id = crate::internal::account::select_account(&db, cfg, "").ok()?;
    let account = db.get_account(id).ok()??;
    account.source().key_derivation().map(|d| d.account_index())
}

/// The encoded viewing key of the account in this database's wallet DB, if
/// there is one to read.
fn ufvk_in_db(cfg: &WalletConfig) -> Option<String> {
    let db = open_wallet_db(wallet_db_path(cfg)?, cfg.network).ok()?;
    let id = crate::internal::account::select_account(&db, cfg, "").ok()?;
    let account = db.get_account(id).ok()??;
    Some(account.ufvk()?.encode(&cfg.network))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::Network;
    use bip0039::{Count, Mnemonic};
    use std::path::Path;
    use tempfile::TempDir;
    use zcash_protocol::{consensus::BlockHeight, ShieldedPool};

    /// A database directory of its own, so these tests touch no process-wide
    /// state and can run beside everything else. Adoption works on the
    /// directory a config came from, which is what makes that possible.
    fn admin_db(network: Network, pool: ShieldedPool) -> (TempDir, Mnemonic, WalletConfig) {
        let dir = tempfile::tempdir().expect("temp dir");
        let mnemonic = Mnemonic::generate(Count::Words24);
        WalletConfig::init_admin_at(
            dir.path(),
            &mnemonic,
            BlockHeight::from_u32(1),
            network,
            pool,
        )
        .expect("create admin database");
        let cfg = WalletConfig::read_at(dir.path()).expect("read back");
        (dir, mnemonic, cfg)
    }

    /// zecd's view of the same `keys.toml`.
    fn zecd_store(dir: &Path) -> zecd::wallet::store::WalletStore {
        zecd::wallet::store::WalletStore::read(&zecd::wallet::store::keys_path(dir))
            .expect("zecd should parse a zkv keys.toml")
    }

    #[test]
    fn adoption_pins_the_key_and_keeps_every_zkv_field() {
        let (dir, _, mut cfg) = admin_db(Network::Test, ShieldedPool::Sapling);
        assert!(cfg.ufvk.is_none(), "a fresh database has no pin");

        assert_eq!(ensure_adopted("db", &mut cfg).unwrap(), Adoption::Pinned);

        // Re-read from disk: the pin is there, and nothing else was dropped by
        // the rewrite. `pool` matters most, since losing it would silently
        // change which outputs reads look at and which receiver signatures
        // bind to.
        let reread = WalletConfig::read_at(dir.path()).unwrap();
        assert!(reread.ufvk.is_some(), "the pin was written");
        assert_eq!(reread.pool, ShieldedPool::Sapling);
        assert_eq!(reread.network, Network::Test);
        assert_eq!(reread.role, Role::Admin);
        assert_eq!(reread.birthday, BlockHeight::from_u32(1));
        assert!(reread.decrypt_seed().is_ok(), "the seed still decrypts");
    }

    #[test]
    fn adoption_is_idempotent() {
        let (dir, _, mut cfg) = admin_db(Network::Test, ShieldedPool::Orchard);
        assert_eq!(ensure_adopted("db", &mut cfg).unwrap(), Adoption::Pinned);
        let pinned = cfg.ufvk.clone();

        // A second call is a field read, and a third against a freshly-read
        // config sees the same value rather than deriving a different one.
        assert_eq!(
            ensure_adopted("db", &mut cfg).unwrap(),
            Adoption::AlreadyAdopted,
        );
        let mut reread = WalletConfig::read_at(dir.path()).unwrap();
        assert_eq!(
            ensure_adopted("db", &mut reread).unwrap(),
            Adoption::AlreadyAdopted,
        );
        assert_eq!(reread.ufvk, pinned);
    }

    /// The cross-project canary: one file, two readers.
    ///
    /// If a zecd upgrade renames a field, changes the seed's encryption, or
    /// starts rejecting unknown keys, this fails here rather than at a node
    /// start against a real database.
    #[test]
    fn zecd_reads_the_same_keys_file_zkv_wrote() {
        let (dir, mnemonic, mut cfg) = admin_db(Network::Test, ShieldedPool::Orchard);
        ensure_adopted("db", &mut cfg).unwrap();

        let store = zecd_store(dir.path());
        assert_eq!(store.network, zecd::network::ZNetwork::Test);
        assert_eq!(u32::from(store.birthday), 1);
        assert!(store.has_seed());
        assert!(!store.is_encrypted(), "zkv wraps with an age identity");
        assert_eq!(
            store.pinned_ufvk(),
            cfg.ufvk.as_deref(),
            "zecd should read the pin zkv wrote",
        );

        // And the seed zecd unwraps through zkv's age identity is the same
        // seed: the two projects' wrapping is byte-compatible, which is what
        // lets one file serve both.
        let seed = zecd::wallet::keys::decrypt_seed_with_identity(
            &store,
            &crate::config::identity_path(dir.path()),
        )
        .expect("zecd should decrypt the seed zkv wrote")
        .expect("a seed is present");
        assert_eq!(
            seed.expose_secret(),
            &mnemonic.to_seed("")[..],
            "the same seed, through both readers",
        );
    }

    #[test]
    fn the_pin_is_the_key_the_seed_derives() {
        // The pin has to be an independent statement about which wallet this
        // is, or it could not catch a swapped database. Deriving it a second
        // way here proves it is not just an echo of something already stored.
        let (_dir, mnemonic, mut cfg) = admin_db(Network::Main, ShieldedPool::Orchard);
        ensure_adopted("db", &mut cfg).unwrap();

        let usk = UnifiedSpendingKey::from_seed(
            &Network::Main,
            &mnemonic.to_seed("")[..],
            AccountId::ZERO,
        )
        .unwrap();
        assert_eq!(
            cfg.ufvk.as_deref(),
            Some(usk.to_unified_full_viewing_key().encode(&Network::Main)).as_deref(),
        );
    }

    #[test]
    fn a_watch_database_pins_the_key_from_its_address() {
        // A watch-only database has no seed, so its address is the source.
        let (_src, mnemonic, _) = admin_db(Network::Test, ShieldedPool::Orchard);
        let usk = UnifiedSpendingKey::from_seed(
            &Network::Test,
            &mnemonic.to_seed("")[..],
            AccountId::ZERO,
        )
        .unwrap();
        let ufvk = usk.to_unified_full_viewing_key();
        let addr = crate::internal::protocol::encode_zkv_addr(
            &ufvk,
            &Network::Test,
            ShieldedPool::Orchard,
            1,
        )
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        WalletConfig::init_watch_at(
            dir.path(),
            BlockHeight::from_u32(1),
            Network::Test,
            &addr,
            ShieldedPool::Orchard,
            crate::config::WalletEngine::Unset,
        )
        .unwrap();
        let mut cfg = WalletConfig::read_at(dir.path()).unwrap();
        assert_eq!(cfg.role, Role::Watch);

        assert_eq!(ensure_adopted("db", &mut cfg).unwrap(), Adoption::Pinned);

        // The pin is the *pool-restricted* key the address encodes, which is
        // what `zkv watch` imported into the wallet database, not the full
        // key the seed would derive. Matching the account is the whole point.
        let from_address = parse_zkv_addr(&addr).unwrap().ufvk.encode(&Network::Test);
        assert_eq!(cfg.ufvk.as_deref(), Some(from_address.as_str()));
        assert_ne!(
            cfg.ufvk.as_deref(),
            Some(ufvk.encode(&Network::Test)).as_deref(),
            "a watch database pins the restricted key, not the full one",
        );

        // The watch database's own fields survive too, including the address
        // that recovery re-imports from.
        let reread = WalletConfig::read_at(dir.path()).unwrap();
        assert_eq!(reread.zkv_address.as_deref(), Some(addr.as_str()));
        assert_eq!(reread.role, Role::Watch);
        assert!(zecd_store(dir.path()).pinned_ufvk().is_some());
    }

    /// zkv's "an older build could still read this" check, against the
    /// upstream helper that answers the same question for zecd.
    ///
    /// `zkv migrate` decides `downgradable` by resolving the layout itself
    /// (`data::engine_dir_in(root) == root`), because `data` sits below this
    /// seam and may not name a `zecd::` path. That makes the two
    /// implementations independent, and this is what keeps them honest: if a
    /// future zecd moved the files somewhere else, zkv would go on reporting a
    /// database as downgradable while the node had already made it otherwise.
    ///
    /// They answer *slightly* different questions, and the difference is
    /// deliberate: with no wallet database anywhere, upstream says "nothing to
    /// migrate" while zkv says "still readable by an old build". Both are
    /// right for what they are asked. The case that matters, and where they
    /// must agree, is a database that exists.
    #[test]
    fn the_downgradable_check_agrees_with_zecds_own() {
        use zecd::coin::Coin;

        let unmoved = |root: &Path| {
            crate::data::engine_dir_in(root).expect("resolvable layout") == root.to_path_buf()
        };

        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        let nested = root
            .join(crate::data::ENGINE_COIN_DIR)
            .join(crate::data::ENGINE_STORAGE_DIR);

        // Old layout: the wallet database sits at the database root, so an
        // older zkv build still finds it and the node has a move to make.
        std::fs::write(root.join(crate::data::DATA_DB), b"").expect("write root db");
        assert!(unmoved(root));
        assert!(zecd::migrate::awaits_migration(root, Coin::Zcash));

        // Current layout: the node has moved the files, which is the one-way
        // step. Both sides must now say so.
        std::fs::create_dir_all(&nested).expect("create engine dir");
        std::fs::rename(
            root.join(crate::data::DATA_DB),
            nested.join(crate::data::DATA_DB),
        )
        .expect("move db");
        assert!(!unmoved(root));
        assert!(!zecd::migrate::awaits_migration(root, Coin::Zcash));

        // Nothing anywhere: no migration pending, and trivially downgradable,
        // since an older build would just create its own. This is the case the
        // two answer differently, asserted so a change in either is noticed.
        let empty = tempfile::tempdir().expect("temp dir");
        assert!(unmoved(empty.path()));
        assert!(!zecd::migrate::awaits_migration(empty.path(), Coin::Zcash));
    }
}
