//! Native desktop GUI transport (Tauri).
//!
//! This is the desktop counterpart to the localhost HTTP server in
//! [`super`] (`zkv gui-browser`): it renders the same single-page app, but
//! with **nothing listening on a localhost port**. Tauri serves the bundled
//! assets through its own protocol (`frontendDist`) and the frontend talks
//! to Rust over `invoke(...)`. Each call lands on a `#[tauri::command]`
//! (see [`ipc`]) that forwards to a method on the shared, transport-agnostic
//! [`Engine`]: the very same core the browser server uses, so there is no
//! duplicated database logic. The desktop window has no network boundary, so
//! it needs none of the session-token / `Host` guard the HTTP server uses.
//!
//! Gated behind the opt-in `desktop` cargo feature (pulls `tauri` +
//! `tauri-build`, and on Linux the system webview dev libraries). Two
//! binaries drive it: the `zkv gui` subcommand and the standalone
//! `zkv-browser` binary, both of which build a tokio runtime and then hand
//! the main thread to [`run`] (the webview event loop must own the main
//! thread).

use crate::db::ZkvError;
use crate::gui::Engine;
use crate::remote::ConnectionArgs;

/// Error returned to the webview by a failed command. Serialized as the
/// `invoke` rejection value, so the frontend's `api.js` reads `.code` /
/// `.message` exactly as it does the browser server's JSON error body
/// (`available`/`required`/`pending` carry insufficient-funds detail).
#[derive(serde::Serialize)]
pub struct CmdError {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    available: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    required: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending: Option<u64>,
}

impl From<ZkvError> for CmdError {
    fn from(e: ZkvError) -> Self {
        use ZkvError::*;
        let code = match &e {
            UnknownDatabase(_) => "unknown_database",
            NotInitialized => "not_initialized",
            Initializing { .. } => "initializing",
            NotSynced => "not_synced",
            StaleChainTip => "stale_tip",
            WatchOnly => "watch_only",
            Unauthorized(_) => "unauthorized",
            InsufficientFunds { .. } => "insufficient_funds",
            ClientUpgradeRequired { .. } => "client_upgrade_required",
            Other(_) => "error",
        };
        let (available, required, pending) = match &e {
            InsufficientFunds {
                available,
                required,
                pending,
            } => (Some(*available), Some(*required), Some(*pending)),
            _ => (None, None, None),
        };
        CmdError {
            code,
            message: e.to_string(),
            available,
            required,
            pending,
        }
    }
}

/// The Tauri command surface: one `invoke` target per GUI action, each a
/// thin wrapper over an [`Engine`] method. Arguments use `rename_all =
/// "snake_case"` so the JS arg objects (`{zkv_address}`, `{sync_workers}`,
/// `{require_sync}`) map verbatim to the Rust parameters.
mod ipc {
    use std::sync::Arc;

    use tauri::State;

    use zcash_protocol::ShieldedPool;

    use crate::data::Network;
    use crate::gui::engine::{
        AddDbResp, AddrCheckResp, CreateResp, DbDetail, DbSummary, FaucetResp, FundingResp,
        HistoryResp, LicensesResp, OkResp, PauseResp, PhraseResp, QrResp, RejectionsResp,
        RevealPhraseResp, RolesResp, SaveResp, ServersResp, SettingsResp, SignMemoResp,
        SignPreviewResp, StatusResp, SyncResp, TxResp, ZkvAddrInfoResp,
    };
    use crate::gui::Engine;

    use super::CmdError;

    type E<'a> = State<'a, Arc<Engine>>;

    /// Build a generic ("error"-coded) [`CmdError`] from any message, used
    /// for the filesystem/dialog/opener failures that have no richer
    /// [`ZkvError`](crate::db::ZkvError) shape.
    fn other_err(message: String) -> CmdError {
        CmdError {
            code: "error",
            message,
            available: None,
            required: None,
            pending: None,
        }
    }

    fn parse_network(s: Option<String>) -> Result<Network, CmdError> {
        Network::parse(&s.unwrap_or_else(|| "mainnet".to_owned())).map_err(|message| CmdError {
            code: "bad_network",
            message,
            available: None,
            required: None,
            pending: None,
        })
    }

    /// Parse the optional `pool` IPC argument. Absent means the network's
    /// default pool (Ironwood everywhere); the facade also rejects the legacy
    /// Orchard label for brand-new databases (import-only).
    fn parse_pool(s: Option<String>, network: Network) -> Result<ShieldedPool, CmdError> {
        match s {
            None => Ok(crate::config::default_pool_for_network(network)),
            Some(v) => crate::config::parse_pool(&v).map_err(|message| CmdError {
                code: "bad_pool",
                message,
                available: None,
                required: None,
                pending: None,
            }),
        }
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn status(engine: E<'_>) -> Result<StatusResp, CmdError> {
        Ok(engine.status().await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn servers(engine: E<'_>) -> Result<ServersResp, CmdError> {
        Ok(engine.servers().await)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub fn licenses(engine: E<'_>) -> LicensesResp {
        engine.licenses()
    }

    /// Save the third-party license bundle to a user-chosen file via the
    /// native save dialog. Returns `saved: false` (no error) if the user
    /// cancels. Desktop-only; the browser transport downloads the bundle
    /// client-side instead.
    ///
    /// `async` so Tauri runs it off the main thread: `blocking_save_file`
    /// blocks until the dialog closes, which would deadlock the event loop
    /// if it ran on the main thread.
    #[tauri::command(rename_all = "snake_case")]
    pub async fn save_licenses(app: tauri::AppHandle, engine: E<'_>) -> Result<SaveResp, CmdError> {
        use tauri_plugin_dialog::DialogExt;

        let text = engine.licenses().text;
        let chosen = app
            .dialog()
            .file()
            .set_file_name("zkv-licenses.txt")
            .add_filter("Text", &["txt"])
            .blocking_save_file();
        let Some(file_path) = chosen else {
            return Ok(SaveResp {
                saved: false,
                path: None,
            });
        };
        let path = file_path
            .into_path()
            .map_err(|e| other_err(e.to_string()))?;
        std::fs::write(&path, text).map_err(|e| other_err(e.to_string()))?;
        Ok(SaveResp {
            saved: true,
            path: Some(path.display().to_string()),
        })
    }

    /// Open an external URL in the user's default browser (the Settings
    /// "view on GitHub" link). Desktop-only; the browser transport uses a
    /// plain anchor.
    #[tauri::command(rename_all = "snake_case")]
    pub fn open_url(app: tauri::AppHandle, url: String) -> Result<(), CmdError> {
        use tauri_plugin_opener::OpenerExt;

        app.opener()
            .open_url(url, None::<&str>)
            .map_err(|e| other_err(e.to_string()))
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn list_databases(engine: E<'_>) -> Result<Vec<DbSummary>, CmdError> {
        Ok(engine.list_databases().await?)
    }

    /// Fast wallet-free sidebar list (names + config only) for an instant paint
    /// on launch; see [`super::super::engine::Engine::list_databases_basic`].
    #[tauri::command(rename_all = "snake_case")]
    pub async fn list_databases_basic(engine: E<'_>) -> Result<Vec<DbSummary>, CmdError> {
        Ok(engine.list_databases_basic().await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn detail(engine: E<'_>, name: String) -> Result<DbDetail, CmdError> {
        Ok(engine.detail(name).await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn history(
        engine: E<'_>,
        name: String,
        filter: Option<String>,
        limit: Option<u32>,
        offset: Option<u32>,
        locate: Option<String>,
    ) -> Result<HistoryResp, CmdError> {
        Ok(engine
            .history(name, filter, limit, offset.unwrap_or(0), locate)
            .await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn rejections(engine: E<'_>, name: String) -> Result<RejectionsResp, CmdError> {
        Ok(engine.rejections(name).await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn roles(engine: E<'_>, name: String) -> Result<RolesResp, CmdError> {
        Ok(engine.roles(name).await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn funding(
        engine: E<'_>,
        name: String,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<FundingResp, CmdError> {
        Ok(engine.funding(name, limit, offset.unwrap_or(0)).await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn sync(
        engine: E<'_>,
        name: String,
        mempool: Option<bool>,
    ) -> Result<SyncResp, CmdError> {
        Ok(engine.sync(name, mempool.unwrap_or(false)).await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn init(
        engine: E<'_>,
        name: String,
        require_sync: Option<bool>,
    ) -> Result<TxResp, CmdError> {
        Ok(engine.init(name, require_sync.unwrap_or(false)).await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn faucet_funds(engine: E<'_>, name: String) -> Result<FaucetResp, CmdError> {
        Ok(engine.faucet_funds(name).await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn faucet_init(engine: E<'_>, name: String) -> Result<FaucetResp, CmdError> {
        Ok(engine.faucet_init(name).await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn set_key(
        engine: E<'_>,
        name: String,
        key: String,
        value: Option<String>,
        offline: Option<bool>,
    ) -> Result<TxResp, CmdError> {
        Ok(engine
            .set_key(
                name,
                key,
                value.unwrap_or_default(),
                offline.unwrap_or(false),
            )
            .await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn del_key(engine: E<'_>, name: String, key: String) -> Result<TxResp, CmdError> {
        Ok(engine.del_key(name, key).await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn send(
        engine: E<'_>,
        name: String,
        recipient: String,
        amount: String,
        memo: Option<String>,
    ) -> Result<TxResp, CmdError> {
        Ok(engine.send(name, recipient, amount, memo).await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn check_address(
        engine: E<'_>,
        name: String,
        address: String,
    ) -> Result<AddrCheckResp, CmdError> {
        Ok(engine.check_address(name, address).await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn sign_memo(
        engine: E<'_>,
        name: String,
        op: String,
        key: Option<String>,
        value: Option<String>,
        scope: Option<String>,
    ) -> Result<SignMemoResp, CmdError> {
        Ok(engine.sign_memo(name, op, key, value, scope).await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn sign_preview(
        engine: E<'_>,
        name: String,
        op: String,
        key: String,
        value: Option<String>,
    ) -> Result<SignPreviewResp, CmdError> {
        Ok(engine.sign_preview(name, op, key, value).await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn create(
        engine: E<'_>,
        name: String,
        network: Option<String>,
        pool: Option<String>,
        phrase: Option<String>,
    ) -> Result<CreateResp, CmdError> {
        let net = parse_network(network)?;
        Ok(engine
            .create(name, net, parse_pool(pool, net)?, phrase)
            .await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub fn generate_phrase(engine: E<'_>) -> PhraseResp {
        engine.generate_phrase()
    }

    #[tauri::command(rename_all = "snake_case")]
    pub fn open_data_dir(engine: E<'_>) -> Result<OkResp, CmdError> {
        Ok(engine.open_data_dir()?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn watch(
        engine: E<'_>,
        zkv_address: String,
        name: Option<String>,
    ) -> Result<AddDbResp, CmdError> {
        Ok(engine.watch(zkv_address, name).await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn reimport_demo(engine: E<'_>) -> Result<AddDbResp, CmdError> {
        Ok(engine.reimport_demo().await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn mark_onboarded(engine: E<'_>) -> Result<(), CmdError> {
        Ok(engine.mark_onboarded()?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn inspect_address(
        engine: E<'_>,
        address: String,
    ) -> Result<ZkvAddrInfoResp, CmdError> {
        Ok(engine.inspect_address(address).await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn verify_phrase(
        engine: E<'_>,
        phrase: String,
        address: String,
    ) -> Result<bool, CmdError> {
        Ok(engine.verify_phrase(phrase, address).await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn restore(
        engine: E<'_>,
        name: String,
        phrase: String,
        network: Option<String>,
        pool: Option<String>,
        birthday: Option<u32>,
    ) -> Result<AddDbResp, CmdError> {
        let net = parse_network(network)?;
        Ok(engine
            .restore(name, phrase, net, parse_pool(pool, net)?, birthday)
            .await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn set_current(engine: E<'_>, name: String) -> Result<OkResp, CmdError> {
        Ok(engine.set_current(name)?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn forget(engine: E<'_>, name: String) -> Result<OkResp, CmdError> {
        Ok(engine.forget(name).await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn reveal_phrase(engine: E<'_>, name: String) -> Result<RevealPhraseResp, CmdError> {
        Ok(engine.reveal_phrase(name).await?)
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn set_pause(
        engine: E<'_>,
        name: String,
        paused: bool,
    ) -> Result<PauseResp, CmdError> {
        Ok(engine.set_pause(name, paused))
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn pause_all(engine: E<'_>, paused: bool) -> Result<PauseResp, CmdError> {
        Ok(engine.set_pause_all(paused))
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn set_settings(
        engine: E<'_>,
        sync_workers: usize,
    ) -> Result<SettingsResp, CmdError> {
        Ok(engine.set_settings(sync_workers))
    }

    #[tauri::command(rename_all = "snake_case")]
    pub async fn qr(engine: E<'_>, data: String) -> Result<QrResp, CmdError> {
        Ok(engine.qr(data)?)
    }
}

/// Launch the desktop window on the main thread. Builds the shared
/// [`Engine`] (no listener, no server) from `conn`, serves the bundled SPA
/// through Tauri's asset protocol, and routes the frontend's `invoke` calls
/// to the IPC commands. Blocks until the window closes; dropping `runtime`
/// on return stops the background auto-sync task.
pub fn run(runtime: tokio::runtime::Runtime, conn: ConnectionArgs) -> anyhow::Result<()> {
    // Let Tauri's async APIs reuse our runtime instead of spawning a second one.
    tauri::async_runtime::set(runtime.handle().clone());

    let engine = Engine::new(conn);
    let bg = engine.clone();

    tauri::Builder::default()
        // Native save dialog (the Settings "Save licenses" picker) and
        // external-link opening (the "view on GitHub" link).
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(engine)
        .invoke_handler(tauri::generate_handler![
            ipc::status,
            ipc::servers,
            ipc::licenses,
            ipc::save_licenses,
            ipc::open_url,
            ipc::list_databases,
            ipc::list_databases_basic,
            ipc::detail,
            ipc::history,
            ipc::rejections,
            ipc::roles,
            ipc::funding,
            ipc::sync,
            ipc::init,
            ipc::faucet_funds,
            ipc::faucet_init,
            ipc::set_key,
            ipc::del_key,
            ipc::send,
            ipc::check_address,
            ipc::sign_memo,
            ipc::sign_preview,
            ipc::create,
            ipc::generate_phrase,
            ipc::open_data_dir,
            ipc::watch,
            ipc::reimport_demo,
            ipc::mark_onboarded,
            ipc::inspect_address,
            ipc::verify_phrase,
            ipc::restore,
            ipc::set_current,
            ipc::forget,
            ipc::reveal_phrase,
            ipc::set_pause,
            ipc::pause_all,
            ipc::set_settings,
            ipc::qr,
        ])
        .setup(move |app| {
            // Background continuous sync, on the shared runtime.
            tauri::async_runtime::spawn(bg.run_auto_sync());
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("z:kv Browser")
            .inner_size(1200.0, 820.0)
            // Floor for the 3-column shell (260px sidebar + main + 380px
            // detail + topbar); below this the layout starts to collapse.
            .min_inner_size(1210.0, 720.0)
            .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .map_err(|e| anyhow::anyhow!("tauri error: {e}"))?;
    Ok(())
}
