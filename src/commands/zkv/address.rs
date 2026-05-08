use anyhow::anyhow;
use clap::Args;
use rand::rngs::OsRng;
use uuid::Uuid;
use zcash_client_backend::data_api::Account;
use zcash_client_sqlite::{util::SystemClock, WalletDb};

use crate::{
    commands::{
        select_account,
        zkv::{encode_zkv_addr, zkv_verifying_pubkey},
    },
    config::WalletConfig,
    data::get_db_paths,
};

#[derive(Debug, Args)]
pub(crate) struct Command {
    /// The UUID of the account whose UFVK should back the zkv database.
    account_id: Option<Uuid>,
}

impl Command {
    pub(crate) fn run(self, wallet_dir: Option<String>) -> anyhow::Result<()> {
        let config = WalletConfig::read(wallet_dir.as_ref())?;
        let params = config.network();
        let birthday: u32 = config.birthday().into();

        let (_, db_data) = get_db_paths(wallet_dir.as_ref());
        let db_data = WalletDb::for_path(db_data, params, SystemClock, OsRng)?;
        let account = select_account(&db_data, self.account_id)?;

        let ufvk = account
            .ufvk()
            .ok_or_else(|| anyhow!("selected account has no UFVK"))?;

        // Verify the UFVK has a transparent component (zkv signing requires it).
        zkv_verifying_pubkey(ufvk)?;

        // Verify the UFVK has an Orchard component (memos are delivered to Orchard).
        if ufvk.orchard().is_none() {
            return Err(anyhow!(
                "selected account's UFVK has no Orchard component; zkv requires Orchard for memo delivery"
            ));
        }

        let ufvk_str = ufvk.encode(&params);
        println!("{}", encode_zkv_addr(&ufvk_str, birthday));
        Ok(())
    }
}
