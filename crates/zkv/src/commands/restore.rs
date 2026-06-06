use anyhow::anyhow;
use bip0039::{English, Mnemonic};
use clap::Args;
use secrecy::{SecretVec, Zeroize};
use zcash_client_backend::data_api::WalletWrite;
use zcash_protocol::consensus;
use zcash_protocol::ShieldedProtocol;

use crate::{
    commands::connection_args::ConnectionCliArgs,
    config::{parse_pool, WalletConfig},
    data::{db_dir, init_dbs, Network},
    internal::protocol::{encode_zkv_addr, zkv_verifying_pubkey},
    ui,
};

#[derive(Debug, Args)]
pub(crate) struct Command {
    /// Database name. Defaults to "default".
    pub(crate) name: Option<String>,

    /// Network: "mainnet" (default) or "testnet".
    #[arg(long, default_value = "mainnet", value_parser = Network::parse)]
    pub(crate) network: Network,

    /// Birthday height; defaults to ~10 blocks below the chain tip (you'll miss
    /// older memos with the default, pass your original birthday for full history).
    #[arg(long)]
    pub(crate) birthday: Option<u32>,

    /// Shielded pool of the database being restored: "orchard" (default) or
    /// "sapling". Must match the pool chosen at the original `zkv init`, or the
    /// reconstructed zkv address won't match and the database will look empty.
    #[arg(long, default_value = "orchard", value_parser = parse_pool)]
    pub(crate) pool: ShieldedProtocol,

    #[command(flatten)]
    pub(crate) connection: ConnectionCliArgs,
}

impl Command {
    pub(crate) async fn run(self, _db: Option<String>) -> anyhow::Result<()> {
        let name = self.name.clone().unwrap_or_else(|| "default".to_owned());
        let params: consensus::Network = self.network.into();

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

        // Refuse to import the same database (same seed, pool, network) twice
        // under a different name.
        if let Some(existing) = zkv::db::find_duplicate_database(&phrase, self.pool, self.network)?
        {
            anyhow::bail!(
                "this database is already imported as {existing:?}; \
                 switch to it with `zkv use {existing}` instead of importing it again"
            );
        }

        let connection = self.connection.into_inner();
        // Restore re-derives an existing database from its seed; the birthday is
        // not part of that identity, so honor an explicit `--birthday` verbatim
        // and otherwise default to tip − safety buffer (you'll miss older memos
        // with the default). Refuses a stale/unreachable tip either way.
        let mut client = connection.connect(params).await?;
        let birthday = match self.birthday {
            Some(height) => crate::internal::sync::pinned_birthday(&mut client, height).await?,
            None => crate::internal::sync::near_tip_birthday(&mut client).await?,
        };

        WalletConfig::init_admin(&name, &mnemonic, birthday.height(), params, self.pool)?;

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
        let zkv_addr = encode_zkv_addr(ufvk, &params, self.pool, u32::from(birthday.height()))?;

        ui::success(format!(
            "Imported database {:?} ({}, birthday {})",
            name,
            self.network.name(),
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
