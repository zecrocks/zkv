use clap::Args;

use crate::{commands::connection_args::ConnectionCliArgs, data::resolve_db, db::Database, ui};

/// Send a plain ZEC/TAZ value transfer to any Zcash address (transparent,
/// Sapling, unified, or TEX), distinct from a zkv write (a zero-value memo to
/// this database's own address). Bitcoin-style positional form
/// (`zkv send <address> <amount>`) or named flags (`--address`/`--amount`);
/// `--memo` is optional and only shielded recipients can carry one.
#[derive(Debug, Args)]
pub(crate) struct Command {
    /// Recipient Zcash address (positional). Alternatively use `--address`.
    address: Option<String>,

    /// Amount in ZEC/TAZ as a decimal (positional), e.g. `0.055`. Not zats.
    /// Alternatively use `--amount`.
    amount: Option<String>,

    /// Recipient Zcash address (named form; conflicts with the positional).
    #[arg(long = "address", conflicts_with = "address")]
    address_flag: Option<String>,

    /// Amount in ZEC/TAZ as a decimal (named form; conflicts with the
    /// positional), e.g. `0.055`. Not zats.
    #[arg(long = "amount", conflicts_with = "amount")]
    amount_flag: Option<String>,

    /// Optional text memo (ZIP-302, <=512 bytes). Only shielded recipients
    /// (Sapling/unified) can carry one.
    #[arg(long)]
    memo: Option<String>,

    #[command(flatten)]
    connection: ConnectionCliArgs,

    /// Skip the pre-broadcast sync (still broadcasts immediately, just
    /// doesn't refresh the wallet first). Use when you control sync timing.
    #[arg(long = "no-sync", alias = "offline")]
    no_sync: bool,
}

impl Command {
    pub(crate) async fn run(self, db: Option<String>) -> anyhow::Result<()> {
        let address = self.address.or(self.address_flag).ok_or_else(|| {
            anyhow::anyhow!("missing recipient address (positional or --address)")
        })?;
        let amount = self
            .amount
            .or(self.amount_flag)
            .ok_or_else(|| anyhow::anyhow!("missing amount (positional or --amount)"))?;

        let name = resolve_db(db.as_deref())?;
        let connection = self.connection.into_inner();
        let database = Database::open(&name, connection)?;

        ui::arrow(format!(
            "{} {} {}  {}",
            ui::bold("SEND"),
            amount,
            ui::dim(&format!("→ {address}")),
            ui::dim("broadcasting…"),
        ));

        let txid = if self.no_sync {
            database
                .send_no_sync(&address, &amount, self.memo.as_deref())
                .await?
        } else {
            database
                .send(&address, &amount, self.memo.as_deref())
                .await?
        };

        ui::success(format!("broadcast tx {}", ui::short_hash(&txid)));
        println!("{txid}");
        Ok(())
    }
}
