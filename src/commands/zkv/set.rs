use age::Identity;
use anyhow::Result;
use clap::Args;
use uuid::Uuid;

use crate::{
    commands::{
        wallet::send::PaymentContext,
        zkv::{do_write, Op},
    },
    remote::ConnectionArgs,
};

#[derive(Debug, Args)]
pub(crate) struct Command {
    /// The key to set.
    key: String,

    /// The value to set.
    value: String,

    /// The zkv address. If omitted, derived from the selected local account's UFVK.
    #[arg(long)]
    zkv_addr: Option<String>,

    /// The UUID of the account to send funds (and sign with).
    #[arg(long)]
    account_id: Option<Uuid>,

    /// age identity file to decrypt the mnemonic phrase with.
    #[arg(short, long)]
    identity: String,

    #[command(flatten)]
    connection: ConnectionArgs,

    /// Note management: the number of notes to maintain in the wallet.
    #[arg(long, default_value_t = 4)]
    target_note_count: usize,

    /// Note management: the minimum allowed value for split change amounts.
    #[arg(long, default_value_t = 10_000_000)]
    min_split_output_value: u64,

    /// Sign and print the memo without broadcasting; you can paste it into another wallet.
    #[arg(long)]
    print_memo: bool,
}

impl PaymentContext for Command {
    fn spending_account(&self) -> Option<Uuid> {
        self.account_id
    }

    fn age_identities(&self) -> Result<Vec<Box<dyn Identity>>> {
        let identities = age::IdentityFile::from_file(self.identity.clone())?.into_identities()?;
        Ok(identities)
    }

    fn connection_args(&self) -> &ConnectionArgs {
        &self.connection
    }

    fn target_note_count(&self) -> usize {
        self.target_note_count
    }

    fn min_split_output_value(&self) -> u64 {
        self.min_split_output_value
    }

    fn require_confirmation(&self) -> bool {
        false
    }
}

impl Command {
    pub(crate) async fn run(self, wallet_dir: Option<String>) -> Result<()> {
        let print_only = self.print_memo;
        do_write(
            wallet_dir,
            self.zkv_addr.clone(),
            Op::Set,
            self.key.clone(),
            Some(self.value.clone()),
            self.identity.clone(),
            print_only,
            self,
        )
        .await
    }
}
