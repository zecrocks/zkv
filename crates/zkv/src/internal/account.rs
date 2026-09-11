//! Shared account-key derivation for a zkv database.
//!
//! Both [`crate::internal::state::load_state`] and
//! [`crate::internal::write::prepare`] need to: open the wallet DB, pick the
//! single account, extract the UFVK, and derive the canonical zkv address
//! plus recipient UA. Without this module that block was open-coded three
//! times (state, write, faucet test fixture).

use anyhow::anyhow;
use secrecy::ExposeSecret as _;
use transparent::keys::NonHardenedChildIndex;
use zcash_client_backend::data_api::{Account, WalletRead};
use zcash_keys::keys::UnifiedSpendingKey;
use zcash_protocol::consensus::Parameters as _;
use zip32::AccountId;

use crate::{
    config::WalletConfig,
    data::{get_db_paths, open_wallet_db},
    error,
    internal::protocol::{
        encode_zkv_addr, receiver_domain, ua_request_for_pool, zkv_verifying_pubkey,
        ZKV_TRANSPARENT_INDEX, ZKV_TRANSPARENT_SCOPE,
    },
};

/// The error surfaced when a database's wallet DB exists but holds no imported
/// account (no seed-derived key, no watch UFVK). This is distinct from "not
/// initialized" (a zkv-protocol / on-chain INIT concept): here the underlying
/// Zcash wallet itself is empty, so we can't even derive the address. It arises
/// when the local wallet data was reset/wiped without the key being re-imported
/// (or a watch import that never completed). The wallet self-heals on the next
/// sync when `keys.toml` still holds the seed or the `zkv_address` (see
/// [`crate::internal::recover::rebootstrap`]).
pub fn no_account_error(db_name: &str) -> anyhow::Error {
    anyhow!(
        "the {db_name:?} database has no wallet key imported yet — its local wallet data was \
         reset before the key was restored (this is different from an uninitialized database). \
         It rebuilds automatically on the next sync; if it persists, re-import the database."
    )
}

/// Read-side material derived from a zkv database's single account.
///
/// All fields are owned: callers don't need to keep the wallet DB open.
pub(crate) struct AccountKeys {
    /// Canonical `zkv1…` address string for this database (the
    /// human-shareable address / advisory INIT echo).
    pub zkv_addr: String,
    /// The database's [`receiver_domain`]: lowercase hex of the pool receiver's
    /// raw bytes. This (not [`zkv_addr`]) is what `ZKV0` signatures bind to;
    /// so the birthday/UFVK-encoding are not load-bearing.
    ///
    /// [`zkv_addr`]: AccountKeys::zkv_addr
    pub receiver_hex: String,
    /// Single-pool UA (this database's pool) that receives the zero-value
    /// memo-bearing outputs.
    pub recipient_ua: String,
    /// Account UUID as a byte vec (the form the SQL views compare against).
    pub account_uuid_bytes: Vec<u8>,
    /// secp256k1 pubkey used to verify SET/DEL signatures.
    pub verifying_pubkey: secp256k1::PublicKey,
    /// BIP-32 account index for key derivation. `None` for watch-only
    /// databases (no seed, can't sign).
    pub account_index: Option<AccountId>,
}

/// Pick the account this database means, out of whatever the wallet file holds.
///
/// A database with a wallet engine of its own has exactly one account, so
/// "the first" was always right and stays the fallback. A member of the shared
/// scan does not: its file is a shard holding many members' accounts, and the
/// first is whichever was imported first. So when `keys.toml` pins a viewing
/// key, the account is the one that key opens.
///
/// Matching on the **encoded viewing key** is what the wallet engine itself
/// does to reconcile a manifest with an account, so the two cannot disagree
/// about which account belongs to which database. The pin is written at
/// creation for a member, and by adoption for everything else, so the fallback
/// only covers a database that predates adoption and therefore has one account.
pub fn select_account<DB>(
    db_data: &DB,
    cfg: &WalletConfig,
    db_name: &str,
) -> anyhow::Result<DB::AccountId>
where
    DB: WalletRead,
    DB::Error: std::fmt::Debug,
    DB::AccountId: Copy,
{
    let ids = db_data
        .get_account_ids()
        .map_err(|e| anyhow!("reading accounts: {e:?}"))?;
    if let Some(want) = cfg.ufvk.as_deref() {
        for id in &ids {
            let Some(account) = db_data
                .get_account(*id)
                .map_err(|e| anyhow!("reading account: {e:?}"))?
            else {
                continue;
            };
            if account
                .ufvk()
                .is_some_and(|k| k.encode(&cfg.network) == want)
            {
                return Ok(*id);
            }
        }
        // A member whose account is genuinely absent is "not imported yet",
        // which the caller distinguishes; falling through to the first account
        // here would hand it somebody else's memos.
        if cfg.engine.is_fleet_member() {
            return Err(anyhow!(
                "the {db_name:?} database's account is not in its shard yet"
            ));
        }
    }
    ids.first()
        .copied()
        .ok_or_else(|| no_account_error(db_name))
}

/// This database's own balances out of a wallet summary, and nothing else's.
///
/// A summary covers every account in the file it came from. For a database with
/// a wallet engine of its own that is one account, so summing the map and
/// picking this account are the same number. For a member of the shared scan
/// the file is its shard, so summing the map reports **its shard-mates' money
/// under this database's name**, which is the same mistake the wallet engine
/// made on its own read path until it was fixed (upstream #270).
///
/// `None` when this database has no account in the file yet, which for a member
/// means the import has not run. That is a truthful "nothing", distinct from a
/// zero balance, and callers that cannot express the difference should say
/// nothing rather than print a zero.
pub fn account_balance<'a, P: zcash_protocol::consensus::Parameters + 'static>(
    summary: &'a zcash_client_backend::data_api::WalletSummary<zcash_client_sqlite::AccountUuid>,
    db_data: &zcash_client_sqlite::WalletDb<
        rusqlite::Connection,
        P,
        zcash_client_sqlite::util::SystemClock,
        rand::rand_core::UnwrapErr<rand::rngs::SysRng>,
    >,
    cfg: &WalletConfig,
    db_name: &str,
) -> Option<&'a zcash_client_backend::data_api::AccountBalance> {
    let id = select_account(db_data, cfg, db_name).ok()?;
    summary.account_balances().get(&id)
}

/// Open the wallet DB for `db_name`, derive everything the read/write paths
/// need, and drop the DB before returning. The `cfg` parameter is taken
/// rather than re-read so callers that already loaded it can reuse it.
pub(crate) fn account_keys(cfg: &WalletConfig, db_name: &str) -> anyhow::Result<AccountKeys> {
    let (_, db_data_path) = get_db_paths(db_name)?;
    let db_data = open_wallet_db(&db_data_path, cfg.network)?;

    let account_id = select_account(&db_data, cfg, db_name)?;
    let account = db_data
        .get_account(account_id)?
        .ok_or_else(|| anyhow!("account vanished"))?;
    let ufvk = account.ufvk().ok_or_else(|| anyhow!("no UFVK"))?;

    let verifying_pubkey = zkv_verifying_pubkey(ufvk)?;
    let zkv_addr = encode_zkv_addr(ufvk, &cfg.network, cfg.pool, u32::from(cfg.birthday))?;
    let receiver_hex = receiver_domain(ufvk, cfg.pool, cfg.network.network_type())?;
    let (ua, _) = ufvk
        .default_address(ua_request_for_pool(cfg.pool))
        .map_err(|e| anyhow!("derive {:?} UA: {e}", cfg.pool))?;
    let recipient_ua = ua.encode(&cfg.network);

    let account_index = account.source().key_derivation().map(|d| d.account_index());
    let account_uuid_bytes = account_id.expose_uuid().as_bytes().to_vec();

    drop(db_data);

    Ok(AccountKeys {
        zkv_addr,
        receiver_hex,
        recipient_ua,
        account_uuid_bytes,
        verifying_pubkey,
        account_index,
    })
}

/// Derive the secp256k1 signing key for a zkv admin database. Bails on
/// watch-only databases (no seed to decrypt).
pub(crate) fn signing_key(
    cfg: &WalletConfig,
    account_index: AccountId,
) -> anyhow::Result<secp256k1::SecretKey> {
    let seed = cfg.decrypt_seed()?;
    let usk = UnifiedSpendingKey::from_seed(&cfg.network, seed.expose_secret(), account_index)
        .map_err(error::Error::from)?;
    let idx = NonHardenedChildIndex::from_index(ZKV_TRANSPARENT_INDEX)
        .ok_or_else(|| anyhow!("bad zkv index"))?;
    usk.transparent()
        .derive_secret_key(ZKV_TRANSPARENT_SCOPE, idx)
        .map_err(|e| anyhow!("derive signing key: {e}"))
}
