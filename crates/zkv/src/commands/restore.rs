use anyhow::anyhow;
use bip0039::{English, Mnemonic};
use clap::Args;
use secrecy::{SecretVec, Zeroize};
use zcash_client_backend::data_api::WalletWrite;
use zcash_protocol::ShieldedPool;

use crate::{
    commands::connection_args::ConnectionCliArgs,
    config::{parse_pool, WalletConfig},
    data::{db_dir, init_dbs, Network},
    internal::protocol::{
        encode_zkv_addr, network_from_type, parse_zkv_addr, receiver_domain, zkv_verifying_pubkey,
    },
    ui,
};

#[derive(Debug, Args)]
pub(crate) struct Command {
    /// Database name. Defaults to "default".
    pub(crate) name: Option<String>,

    /// The database's zkv address (`zkv1…`). When given, the network, pool, and
    /// birthday are read from it, and the recovery phrase is checked against it:
    /// a phrase that does not control this database is rejected before anything
    /// is written. Supplying this OR `--birthday` is required.
    #[arg(long)]
    pub(crate) address: Option<String>,

    /// Network: "mainnet" (default) or "testnet". Ignored when `--address` is
    /// given (the address carries the network).
    #[arg(long, default_value = "mainnet", value_parser = Network::parse)]
    pub(crate) network: Network,

    /// Birthday height to start scanning from. Required unless `--address` is
    /// given (which carries the birthday); pass the lowest height that still
    /// covers your INIT. With `--address`, an explicit `--birthday` overrides
    /// the address's.
    #[arg(long)]
    pub(crate) birthday: Option<u32>,

    /// Shielded pool of the database being restored: "ironwood", "orchard", or
    /// "sapling". Must match the pool chosen at the original `zkv init`, or the
    /// reconstructed zkv address won't match and the database will look empty.
    /// Defaults to the network's pool when omitted (Ironwood on testnet, Orchard
    /// on mainnet). Ironwood and Orchard share the Orchard receiver, so an old
    /// Orchard wallet restored as Ironwood on testnet derives the identical
    /// address and reads the identical memos. Ignored when `--address` is given
    /// (the address carries the pool).
    #[arg(long, value_parser = parse_pool)]
    pub(crate) pool: Option<ShieldedPool>,

    #[command(flatten)]
    pub(crate) connection: ConnectionCliArgs,
}

impl Command {
    pub(crate) async fn run(self, _db: Option<String>) -> anyhow::Result<()> {
        // Restoring needs a pinned starting point: either a zkv address (which
        // carries network, pool, and birthday) or an explicit birthday height.
        // Without one we'd have to scan for the INIT to recover the right
        // database; that is future work, so require the caller to pin it.
        if self.address.is_none() && self.birthday.is_none() {
            anyhow::bail!(
                "restoring needs a starting point: pass --address <zkv1…> \
                 (carries network, pool, and birthday) or --birthday <height>"
            );
        }

        let name = self.name.clone().unwrap_or_else(|| "default".to_owned());

        // A zkv address is authoritative for network and pool (and lets us
        // verify the phrase below); otherwise fall back to the flags.
        let parsed_addr = match &self.address {
            Some(addr) => {
                Some(parse_zkv_addr(addr).map_err(|e| anyhow!("invalid zkv address: {e}"))?)
            }
            None => None,
        };
        let network: Network = match &parsed_addr {
            Some(p) => network_from_type(p.network)?,
            None => self.network,
        };
        let params = network;
        // With an address, the pool is authoritative (and already network-aware
        // from parsing). Without one, resolve the flag against the network:
        // default per network, and reject Ironwood on mainnet.
        let pool = match &parsed_addr {
            Some(p) => p.pool,
            None => crate::config::resolve_pool_for_network(self.pool, network)?,
        };

        let dir = db_dir(&name)?;
        if dir.join("keys.toml").exists() {
            anyhow::bail!("database {name:?} already exists at {}", dir.display());
        }

        eprintln!(
            "{}",
            ui::bold("Enter your 24-word recovery phrase (separated by spaces):")
        );
        use std::io::BufRead;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        let mut phrase = line
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let mnemonic: Mnemonic<English> =
            Mnemonic::from_phrase(&phrase).map_err(|e| anyhow!("invalid phrase: {e}"))?;

        // With a zkv address, verify the phrase actually controls that database
        // (same seed => same single-pool receiver) before writing anything, so a
        // wrong phrase or a phrase pasted against the wrong address fails loudly
        // instead of silently building an empty database. This is offline, so it
        // runs before the network round-trip below.
        if let Some(p) = &parsed_addr {
            use zcash_keys::keys::UnifiedSpendingKey;
            use zip32::AccountId;
            let ufvk_seed = {
                let mut seed = mnemonic.to_seed("");
                let usk = UnifiedSpendingKey::from_seed(&params, &seed, AccountId::ZERO);
                seed.zeroize();
                usk.map_err(|e| anyhow!("derive key from phrase: {e}"))?
                    .to_unified_full_viewing_key()
            };
            let want = receiver_domain(&p.ufvk, p.pool, p.network)?;
            let got = receiver_domain(&ufvk_seed, p.pool, p.network)?;
            if want != got {
                phrase.zeroize();
                line.zeroize();
                anyhow::bail!(
                    "the recovery phrase does not control the database at this zkv address \
                     (different seed, pool, or network)"
                );
            }
        }

        // Refuse to import the same database (same seed, pool, network) twice
        // under a different name.
        if let Some(existing) = zkv::db::find_duplicate_database(&phrase, pool, network)? {
            anyhow::bail!(
                "this database is already imported as {existing:?}; \
                 switch to it with `zkv use {existing}` instead of importing it again"
            );
        }

        let connection = self.connection.into_inner();
        // Birthday: an explicit `--birthday` wins (honored verbatim); otherwise
        // the address carries it. One of the two is always present (checked at
        // the top). Refuses a stale/unreachable tip either way.
        let birthday_height = self
            .birthday
            .or_else(|| parsed_addr.as_ref().map(|p| p.birthday))
            .expect("address or birthday is required (checked above)");
        let mut client = connection.connect(params).await?;
        let birthday =
            crate::internal::sync::pinned_birthday(&mut client, params, birthday_height).await?;

        WalletConfig::init_admin(&name, &mnemonic, birthday.height(), params, pool)?;

        let seed = {
            let mut s = mnemonic.to_seed("");
            let secret = s.to_vec();
            s.zeroize();
            SecretVec::new(secret)
        };
        let mut db_data = init_dbs(params, &name)?;
        db_data.create_account(&name, &seed, &birthday, None)?;

        crate::demo::promote_current(&name)?;

        let ids = zcash_client_backend::data_api::WalletRead::get_account_ids(&db_data)?;
        let account = zcash_client_backend::data_api::WalletRead::get_account(&db_data, ids[0])?
            .ok_or_else(|| anyhow!("account vanished"))?;
        let ufvk = zcash_client_backend::data_api::Account::ufvk(&account)
            .ok_or_else(|| anyhow!("no UFVK"))?;
        zkv_verifying_pubkey(ufvk)?;
        let zkv_addr = encode_zkv_addr(ufvk, &params, pool, u32::from(birthday.height()))?;

        ui::success(format!(
            "Imported database {:?} ({}, birthday {})",
            name,
            network.name(),
            u32::from(birthday.height()),
        ));
        eprintln!();
        eprintln!("{}", ui::bold("Your zkv address:"));
        println!("  {zkv_addr}");

        // The raw recovery-phrase strings are seed equivalents; wipe the owned
        // copies now that the seed is derived and persisted.
        phrase.zeroize();
        line.zeroize();
        Ok(())
    }
}
