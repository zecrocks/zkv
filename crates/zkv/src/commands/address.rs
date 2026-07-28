use anyhow::anyhow;
use clap::Args;
use zcash_client_backend::data_api::{Account, WalletRead};

use crate::{
    config::WalletConfig,
    data::{get_db_paths, open_wallet_db, resolve_db},
    internal::protocol::{
        encode_zkv_addr, ua_request_for_pool, zkv_addr_to_uview, zkv_verifying_pubkey,
    },
    ui,
};

#[derive(Debug, Args)]
pub(crate) struct Command {
    /// Print the standard `uview…` viewing key instead of the `zkv…` address.
    /// It is the same key (the zkv address with its HRP relabeled), so you can
    /// paste it into a stock Zcash wallet to view the raw memos.
    #[arg(long, conflicts_with = "funding")]
    view_key: bool,

    /// Print the funding address instead of the `zkv…` address: a shielded-only
    /// unified address (the database's single pool, no transparent receiver) to
    /// send ZEC to so the wallet can pay write fees.
    #[arg(long)]
    funding: bool,
}

impl Command {
    pub(crate) fn run(self, db: Option<String>) -> anyhow::Result<()> {
        let name = resolve_db(db.as_deref())?;
        let cfg = WalletConfig::read(&name)?;
        let (_, db_data_path) = get_db_paths(&name)?;
        let db_data = open_wallet_db(db_data_path, cfg.network)?;
        let ids = db_data.get_account_ids()?;
        let account_id = *ids
            .first()
            .ok_or_else(|| crate::internal::account::no_account_error(&name))?;
        let account = db_data
            .get_account(account_id)?
            .ok_or_else(|| anyhow!("account vanished"))?;
        let ufvk = account
            .ufvk()
            .ok_or_else(|| anyhow!("account has no UFVK"))?;
        zkv_verifying_pubkey(ufvk)?; // validates transparent component (signing key)

        if self.funding {
            let (ua, _) = ufvk
                .default_address(ua_request_for_pool(cfg.pool))
                .map_err(|e| anyhow!("could not derive funding address: {e}"))?;
            let funding = ua.encode(&cfg.network);
            // The address is the machine-readable value, so it goes to stdout;
            // the scannable QR is decoration and goes to stderr (rendered only
            // when stderr is a wide-enough TTY).
            println!("{funding}");
            if let Some(qr) = crate::commands::init::render_qr_for_tty(&funding) {
                eprintln!();
                for line in qr.lines() {
                    eprintln!("    {line}");
                }
                eprintln!();
            }
            return Ok(());
        }

        let addr = encode_zkv_addr(ufvk, &cfg.network, cfg.pool, cfg.birthday.into())?;
        if self.view_key {
            println!("{}", zkv_addr_to_uview(&addr)?);
        } else {
            println!("{addr}");
            // Hint goes to stderr so it never pollutes the piped address value.
            let ticker = cfg.network.ticker();
            ui::hint(format!(
                "Tip: run `zkv address --funding` for the address to send {ticker} to."
            ));
        }
        Ok(())
    }
}
