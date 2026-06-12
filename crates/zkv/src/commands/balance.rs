use anyhow::anyhow;
use clap::Args;
use zcash_client_backend::data_api::{wallet::ConfirmationsPolicy, WalletRead};

use crate::{
    commands::connection_args::ConnectionCliArgs,
    config::{Role, WalletConfig},
    data::{get_db_paths, open_wallet_db, resolve_db},
    internal::sync::run_sync_read,
    ui::{self, format_zec},
};

#[derive(Debug, Args)]
pub(crate) struct Command {
    #[command(flatten)]
    connection: ConnectionCliArgs,

    /// Don't sync first; report last-known balance.
    #[arg(long)]
    offline: bool,
}

impl Command {
    pub(crate) async fn run(self, db: Option<String>) -> anyhow::Result<()> {
        let name = resolve_db(db.as_deref())?;
        let cfg = WalletConfig::read(&name)?;
        // A watch-only database holds the viewing key, so its balance is still
        // visible (the wallet scans received notes); it just can't spend.
        let watch_only = cfg.role != Role::Admin;

        let connection = self.connection.into_inner();
        if !self.offline && !crate::commands::blocksync_skip(&name)? {
            run_sync_read(&name, &connection, false).await?;
        }

        let (_, db_data_path) = get_db_paths(&name)?;
        let db_data = open_wallet_db(db_data_path, cfg.network)?;
        let summary = db_data
            .get_wallet_summary(ConfirmationsPolicy::default())?
            .ok_or_else(|| anyhow!("no wallet summary yet, try running `zkv sync`"))?;

        // Spendable and confirming reported separately (never summed), the
        // same split the GUI shows: usable now on one side, about to become
        // usable on the other.
        let (spendable, total) = summary.account_balances().values().fold(
            (0u64, 0u64),
            |(s, t), b| {
                (
                    s + u64::from(b.spendable_value()),
                    t + u64::from(b.total()),
                )
            },
        );
        let confirming = total.saturating_sub(spendable);

        let network = crate::data::Network::from(cfg.network);
        println!("{}", format_zec(spendable as i64, network).trim_start());
        if confirming > 0 {
            ui::hint(format!(
                "(+ {} confirming)",
                format_zec(confirming as i64, network).trim_start()
            ));
        }
        if watch_only {
            ui::hint("(watch-only, cannot send)");
        }
        Ok(())
    }
}
