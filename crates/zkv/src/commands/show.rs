use anyhow::anyhow;
use clap::Args;
use zcash_client_backend::data_api::{Account, WalletRead};

use crate::{
    commands::connection_args::ConnectionCliArgs,
    config::{Role, WalletConfig},
    data::{db_dir, get_db_paths, open_wallet_db, resolve_db},
    internal::{
        protocol::{
            encode_zkv_addr, pubkey_bech32, ua_request_for_pool, zkv_verifying_pubkey, InitState,
            MAX_DB_VERSION,
        },
        state::{load_state, INIT_CONFIRMATIONS},
        sync::run_sync_read,
    },
    ui,
};

#[derive(Debug, Args)]
pub(crate) struct Command {
    #[command(flatten)]
    connection: ConnectionCliArgs,

    /// Don't sync first; show last-known state.
    #[arg(long)]
    offline: bool,
}

impl Command {
    pub(crate) async fn run(self, db: Option<String>) -> anyhow::Result<()> {
        let name = resolve_db(db.as_deref())?;
        let cfg = WalletConfig::read(&name)?;

        let connection = self.connection.into_inner();
        // Once we've synced far enough to confirm the database is initialized,
        // that fact never changes, so there's nothing new to learn for this
        // diagnostic view: skip the network round-trip and show what the local
        // cache already knows. We still sync while the INIT is unconfirmed (or
        // unknown), and `--offline` always skips.
        let cached_initialized = load_state(&name, INIT_CONFIRMATIONS, false)
            .map(|r| matches!(r.init, InitState::Initialized))
            .unwrap_or(false);
        if !self.offline && !cached_initialized && !crate::commands::blocksync_skip(&name)? {
            run_sync_read(&name, &connection, false).await?;
        }

        let (_, db_data_path) = get_db_paths(&name)?;
        let db_data = open_wallet_db(db_data_path, cfg.network)?;

        let ids = db_data.get_account_ids()?;
        let account_id = *ids
            .first()
            .ok_or_else(|| crate::internal::account::no_account_error(&name))?;
        let account = db_data
            .get_account(account_id)?
            .ok_or_else(|| anyhow!("account vanished"))?;
        let ufvk = account.ufvk().ok_or_else(|| anyhow!("no UFVK"))?;
        let signing_pubkey = zkv_verifying_pubkey(ufvk)?;

        let zkv_addr = encode_zkv_addr(ufvk, &cfg.network, cfg.pool, u32::from(cfg.birthday))?;

        let role_str = match cfg.role {
            Role::Admin => "admin",
            Role::Watch => "watch",
        };
        let network_label = cfg.network;
        let net_str = network_label.name();
        let pool_str = match cfg.pool {
            zcash_protocol::ShieldedPool::Sapling => "sapling",
            zcash_protocol::ShieldedPool::Orchard => "orchard",
            zcash_protocol::ShieldedPool::Ironwood => "ironwood",
        };

        // The funding address: a shielded-only unified address (the database's
        // single pool, no transparent receiver) to send ZEC to so the wallet can
        // pay write fees. Derived straight from the UFVK, so it renders for
        // watch-only databases too.
        let funding_addr = ufvk
            .default_address(ua_request_for_pool(cfg.pool))
            .ok()
            .map(|(ua, _)| ua.encode(&cfg.network));

        println!("Database:    {name}    ({role_str}, {net_str}, {pool_str})");
        println!("Data dir:    {}", db_dir(&name)?.display());
        println!("zkv address: {zkv_addr}");
        if let Some(addr) = &funding_addr {
            println!("Funding:     {addr}");
        }
        // The root signing pubkey (UFVK-derived). This is the identity an admin
        // signs writes with and that the registry keys owners/writers by; copy
        // it into `zkv roles owner add` / `writer add` to delegate to this key.
        println!("Signing key: {}", pubkey_bech32(&signing_pubkey));

        // Init + version status: surface whether the on-chain INIT has been
        // observed and whether the database has moved to a newer protocol epoch.
        // `show` is diagnostic, so a version block is *reported*, never fatal.
        let (init_label, version_line, version_warning) =
            match load_state(&name, INIT_CONFIRMATIONS, false) {
                Ok(result) => {
                    let init_label = match result.init {
                        InitState::Initialized => "initialized".to_owned(),
                        InitState::Initializing { done, required } => {
                            format!("initializing ({done}/{required})")
                        }
                        InitState::Uninitialized => "uninitialized, no INIT memo yet".to_owned(),
                    };
                    let v = &result.version;
                    let version_line = if v.is_outdated() {
                        format!(
                            "{}, NEWER THAN THIS CLIENT (supports up to {MAX_DB_VERSION}); \
                             blocks: {}",
                            v.version,
                            v.blocks.to_wire(),
                        )
                    } else {
                        format!("{} (client supports up to {MAX_DB_VERSION})", v.version)
                    };
                    (init_label, version_line, v.upgrade_warning())
                }
                // Pre-sync or DB-state errors shouldn't break `show`; fall back
                // to "unknown" so the rest of the output still renders.
                Err(_) => ("unknown".to_owned(), "unknown".to_owned(), None),
            };
        println!("Status:      {init_label}");
        println!("Version:     {version_line}");
        if let Some(warning) = version_warning {
            ui::warn(warning);
        }
        // Balance is intentionally not shown here: `show` is a sync-free
        // diagnostic once the database is known-initialized. Use `zkv balance`
        // for the wallet balance.

        Ok(())
    }
}
