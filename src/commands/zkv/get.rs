use anyhow::{anyhow, bail};
use clap::Args;
use rand::rngs::OsRng;
use rusqlite::{named_params, Connection};
use uuid::Uuid;
use zcash_client_backend::data_api::{Account, WalletRead};
use zcash_client_sqlite::{util::SystemClock, AccountUuid, WalletDb};
use zcash_protocol::{
    consensus::Parameters,
    memo::{Memo, MemoBytes},
};

use crate::{
    commands::{
        select_account,
        zkv::{
            encode_zkv_addr, network_from_type, parse_zkv_addr, replay, zkv_verifying_pubkey,
            ParsedZkvAddr,
        },
    },
    config::WalletConfig,
    data::get_db_paths,
};

#[derive(Debug, Args)]
pub(crate) struct Command {
    /// A specific key to look up. If omitted, all keys are printed.
    key: Option<String>,

    /// The zkv address to read from. If omitted, derived from the selected local account.
    #[arg(long)]
    zkv_addr: Option<String>,

    /// The UUID of the local account whose UFVK matches the zkv address.
    #[arg(long)]
    account_id: Option<Uuid>,

    /// Error on malformed memos or invalid signatures instead of skipping them.
    #[arg(long)]
    strict: bool,
}

impl Command {
    pub(crate) fn run(self, wallet_dir: Option<String>) -> anyhow::Result<()> {
        let config = WalletConfig::read(wallet_dir.as_ref())?;
        let params = config.network();

        let (_, db_data_path) = get_db_paths(wallet_dir.as_ref());
        let db_data = WalletDb::for_path(&db_data_path, params, SystemClock, OsRng)?;

        // Resolve the zkv address: either from --zkv-addr, or derive from the selected local account.
        let parsed = match self.zkv_addr {
            Some(s) => {
                let parsed = parse_zkv_addr(&s)?;
                if network_from_type(parsed.network)? != params {
                    bail!("zkv address network does not match local wallet network");
                }
                parsed
            }
            None => {
                let account = select_account(&db_data, self.account_id)?;
                let ufvk = account
                    .ufvk()
                    .ok_or_else(|| anyhow!("selected account has no UFVK"))?;
                let birthday: u32 = config.birthday().into();
                let raw = encode_zkv_addr(&ufvk.encode(&params), birthday);
                ParsedZkvAddr {
                    raw,
                    network: params.network_type(),
                    ufvk: ufvk.clone(),
                    birthday,
                }
            }
        };

        // Locate the matching local account so we can scan its received outputs.
        let target_encoded = parsed.ufvk.encode(&params);
        let mut matched: Option<AccountUuid> = None;
        for id in db_data.get_account_ids()? {
            if let Some(account) = db_data.get_account(id)? {
                if let Some(u) = account.ufvk() {
                    if u.encode(&params) == target_encoded {
                        matched = Some(id);
                        break;
                    }
                }
            }
        }
        let account_uuid = matched.ok_or_else(|| {
            anyhow!(
                "this wallet has not imported the zkv address's UFVK; \
                 run `wallet init-fvk --fvk <ufvk> --birthday <h>` then `wallet sync` first"
            )
        })?;
        // Use the inner UUID for the SQL parameter.
        let account_uuid_inner: Uuid = account_uuid.expose_uuid();

        let pk = zkv_verifying_pubkey(&parsed.ufvk)?;

        // Drop the WalletDb so we can open a plain rusqlite connection (read-only views).
        drop(db_data);

        let conn = Connection::open(&db_data_path)?;
        let mut stmt = conn.prepare(
            "SELECT v.memo
             FROM v_tx_outputs v
             JOIN v_transactions t ON t.txid = v.txid AND t.account_uuid = v.to_account_uuid
             WHERE v.to_account_uuid = :account_uuid
               AND v.output_pool = 3
               AND v.memo IS NOT NULL
             ORDER BY t.mined_height ASC NULLS LAST, v.txid ASC, v.output_index ASC",
        )?;

        let entries: Vec<String> = stmt
            .query_and_then(
                named_params! { ":account_uuid": account_uuid_inner },
                |row| -> anyhow::Result<Option<String>> {
                    let bytes: Option<Vec<u8>> = row.get("memo")?;
                    Ok(bytes.and_then(|b| {
                        MemoBytes::from_bytes(&b)
                            .ok()
                            .and_then(|mb| Memo::try_from(mb).ok())
                            .and_then(|m| match m {
                                Memo::Text(t) => Some(t.to_string()),
                                _ => None,
                            })
                    }))
                },
            )?
            .filter_map(|r| r.transpose())
            .collect::<anyhow::Result<Vec<_>>>()?;

        let state = replay(entries, &parsed.raw, &pk, self.strict)?;

        match self.key {
            Some(k) => match state.get(&k) {
                Some(v) => println!("{v}"),
                None => {
                    eprintln!("(key not set)");
                    std::process::exit(1);
                }
            },
            None => {
                if state.is_empty() {
                    println!("(empty)");
                } else {
                    for (k, v) in &state {
                        println!("{k} = {v}");
                    }
                }
            }
        }

        Ok(())
    }
}
