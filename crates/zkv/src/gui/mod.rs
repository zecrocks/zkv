//! Localhost web UI server (`zkv gui-browser`): serves the embedded
//! single-page database browser plus a JSON API backed by
//! [`crate::db::Database`]. Everything here is gated behind the `gui`
//! cargo feature.
//!
//! The request handling is a thin transport shell: every `/api/*` handler
//! deserializes its inputs and forwards to a method on `engine::Engine`,
//! the transport-agnostic core shared with the desktop (`zkv gui`) Tauri
//! IPC command layer. The HTTP-specific concerns live here: routing, the
//! session-token + `Host` security guard, static-asset serving, and the
//! `ZkvError`→HTTP mapping (`ApiError`).
//!
//! # Security
//!
//! The server can broadcast transactions that spend real ZEC, so it is
//! locked down for the localhost-only threat model: it binds `127.0.0.1`
//! only, every `/api/*` request must carry a per-launch secret token
//! (injected into the served page, unreadable cross-origin), and the
//! `Host` header is validated to defeat DNS-rebinding. See `guard`. (The
//! desktop window has no network boundary and uses Tauri IPC instead, so
//! it needs none of this.)

mod assets;
#[cfg(feature = "desktop")]
pub mod desktop;
pub mod engine;

/// The bundled third-party license text (generated at build time, embedded
/// gzip-compressed, inflated lazily on first call). Exposed so the `zkv`
/// CLI's `--licenses` flag can dump it without standing up an [`Engine`].
pub fn licenses_text() -> &'static str {
    assets::licenses_text()
}

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use zcash_protocol::ShieldedProtocol;

use crate::data::Network;
use crate::db::ZkvError;
use crate::remote::ConnectionArgs;

pub use engine::Engine;
use engine::{
    AddDbResp, AddrCheckResp, CreateResp, DbDetail, DbSummary, FaucetResp, FundingResp,
    HistoryResp, LicensesResp, OkResp, PauseResp, PhraseResp, QrResp, RejectionsResp,
    RevealPhraseResp, RolesResp, ServersResp, SettingsResp, SignMemoResp, SignPreviewResp,
    StatusResp, SyncResp, TxResp, ZkvAddrInfoResp,
};

/// Configuration for [`serve`].
pub struct GuiConfig {
    /// Address to bind. Should be on the loopback interface.
    pub bind: SocketAddr,
    /// lightwalletd connection used for all chain operations.
    pub conn: ConnectionArgs,
    /// Attempt to open the system browser at the served URL.
    pub open_browser: bool,
}

/// Shared server state. Cheap to clone (an `Arc` wrapper is used). The
/// database operations all live on [`Engine`]; this struct adds only the
/// HTTP-transport concerns (the session token + the bound address used to
/// validate the `Host` header).
struct AppState {
    engine: Arc<Engine>,
    /// Per-launch secret required on every `/api` request.
    token: String,
    /// The actually-bound address (used to validate `Host`).
    bound: SocketAddr,
}

/// Bind the loopback listener and assemble the shared server state +
/// router, spawning the background sync loop. Shared by [`serve`].
async fn build_server(config: GuiConfig) -> anyhow::Result<(TcpListener, Router, SocketAddr)> {
    let token = random_token();

    let listener = TcpListener::bind(config.bind)
        .await
        .map_err(|e| anyhow::anyhow!("bind {}: {e}", config.bind))?;
    let bound = listener.local_addr()?;

    let engine = Engine::new(config.conn);
    let state = Arc::new(AppState {
        engine: engine.clone(),
        token,
        bound,
    });
    let app = router(state);

    // Continuously sync every database in the background.
    tokio::spawn(engine.run_auto_sync());

    Ok((listener, app, bound))
}

/// Start the GUI server and block until it shuts down.
///
/// Binds [`GuiConfig::bind`], prints the URL to stderr, optionally opens
/// a browser, and serves until the process is interrupted.
pub async fn serve(config: GuiConfig) -> anyhow::Result<()> {
    let open = config.open_browser;
    let (listener, app, bound) = build_server(config).await?;
    let url = format!("http://{bound}/");

    eprintln!("zkv gui: serving the database browser at {url}");
    eprintln!("zkv gui: press Ctrl-C to stop.");
    if open {
        open_browser(&url);
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| anyhow::anyhow!("server error: {e}"))?;
    Ok(())
}

fn router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/status", get(handle_status))
        .route("/servers", get(handle_servers))
        .route("/licenses", get(handle_licenses))
        .route("/databases", get(handle_list).post(handle_create))
        .route("/phrase", post(handle_generate_phrase))
        .route("/open-data-dir", post(handle_open_data_dir))
        .route("/databases/:name", get(handle_detail).delete(handle_forget))
        .route("/databases/:name/reveal-phrase", post(handle_reveal_phrase))
        .route("/databases/:name/history", get(handle_history))
        .route("/databases/:name/rejections", get(handle_rejections))
        .route("/databases/:name/roles", get(handle_roles))
        .route("/databases/:name/funding", get(handle_funding))
        .route("/databases/:name/sync", post(handle_sync))
        .route("/databases/:name/pause", post(handle_pause))
        .route("/databases/:name/init", post(handle_init))
        .route("/databases/:name/faucet", post(handle_faucet_funds))
        .route("/databases/:name/faucet-init", post(handle_faucet_init))
        .route("/databases/:name/sign-preview", post(handle_sign_preview))
        .route("/databases/:name/keys", post(handle_set))
        .route("/databases/:name/keys/:key", delete(handle_del))
        .route("/databases/:name/sign", post(handle_sign_memo))
        .route("/databases/:name/send", post(handle_send))
        .route("/databases/:name/check-address", post(handle_check_address))
        .route("/watch", post(handle_watch))
        .route("/reimport-demo", post(handle_reimport_demo))
        .route("/onboarded", post(handle_mark_onboarded))
        .route("/inspect-address", post(handle_inspect_address))
        .route("/restore", post(handle_restore))
        .route("/current", post(handle_current))
        .route("/settings", post(handle_settings))
        .route("/pause-all", post(handle_pause_all))
        .route("/qr", get(handle_qr))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state.clone());

    Router::new()
        .nest("/api", api)
        .fallback(static_handler)
        .with_state(state)
}

// ===================================================================
// Request DTOs (axum extractor types)
// ===================================================================

#[derive(Deserialize)]
struct HistoryQuery {
    /// Case-insensitive substring filter on the key.
    #[serde(default)]
    filter: Option<String>,
    /// Page size; defaults to the engine's `DEFAULT_HISTORY_PAGE`.
    #[serde(default)]
    limit: Option<u32>,
    /// Rows to skip from the newest end.
    #[serde(default)]
    offset: u32,
    /// Jump to the page containing this txid (full context, ignores `offset`).
    #[serde(default)]
    locate: Option<String>,
}

#[derive(Deserialize)]
struct FundingQuery {
    /// Page size; defaults to the engine's `DEFAULT_HISTORY_PAGE`.
    #[serde(default)]
    limit: Option<u32>,
    /// Rows to skip from the newest end.
    #[serde(default)]
    offset: u32,
}

#[derive(Deserialize)]
struct QrQuery {
    /// The text to encode (e.g. a funding address).
    data: String,
}

#[derive(Deserialize)]
struct CreateReq {
    name: String,
    #[serde(default = "default_network")]
    network: String,
    /// `"sapling"` or `"orchard"`; absent means Orchard (the default).
    #[serde(default)]
    pool: Option<String>,
    /// A confirmed recovery phrase from the deferred create flow. Absent means
    /// mint a fresh seed server-side (CLI parity).
    #[serde(default)]
    phrase: Option<String>,
}

#[derive(Deserialize, Default)]
struct SyncReq {
    #[serde(default)]
    mempool: bool,
}

#[derive(Deserialize)]
struct PauseReq {
    paused: bool,
}

#[derive(Deserialize)]
struct SettingsReq {
    sync_workers: usize,
}

#[derive(Deserialize)]
struct SetReq {
    key: String,
    #[serde(default)]
    value: String,
    #[serde(default)]
    offline: bool,
}

/// Body for `POST /databases/:name/send`: a plain ZEC value transfer to an
/// arbitrary Zcash address. `amount` is a decimal ZEC string.
#[derive(Deserialize)]
struct SendReq {
    recipient: String,
    amount: String,
    /// Optional ZIP-302 text memo (<=512 bytes); only shielded recipients can
    /// carry one.
    #[serde(default)]
    memo: Option<String>,
}

/// Body for `POST /databases/:name/check-address`: the Send flow's live
/// recipient-validation probe.
#[derive(Deserialize)]
struct CheckAddrReq {
    address: String,
}

/// Body for `POST /databases/:name/sign`: the Reference builder's "sign this
/// memo without broadcasting" request. `op` is the opcode (`SET`, `SETL`,
/// `DEL`, `INIT`, `OWNERSET`/`OWNERDEL`/`WRITERSET`/`WRITERDEL`/`FINALIZE`);
/// `key` is the data key or, for management ops, the target pubkey; `scope` is
/// the `WRITERSET` capability string.
#[derive(Deserialize)]
struct SignMemoReq {
    op: String,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

/// Body for `POST /databases/:name/sign-preview`. `op` is
/// `set`/`setl`/`del`/`init`; `key`/`value` are ignored for `init`.
#[derive(Deserialize)]
struct SignPreviewReq {
    op: String,
    #[serde(default)]
    key: String,
    #[serde(default)]
    value: Option<String>,
}

/// Body for `POST /databases/:name/init`. `require_sync` opts into the
/// full-sync-to-tip gate: the write-flow's "re-broadcast INIT on an existing
/// database" path sets it so we don't double-INIT a db whose valid INIT is
/// still in not-yet-scanned blocks. The create flow leaves it `false`; a
/// brand-new db has nothing to scan, so the gate must not block creation.
#[derive(Deserialize, Default)]
#[serde(default)]
struct InitReq {
    require_sync: bool,
}

#[derive(Deserialize)]
struct WatchReq {
    zkv_address: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct InspectAddressReq {
    address: String,
}

#[derive(Deserialize)]
struct RestoreReq {
    name: String,
    phrase: String,
    #[serde(default = "default_network")]
    network: String,
    #[serde(default)]
    birthday: Option<u32>,
}

#[derive(Deserialize)]
struct CurrentReq {
    name: String,
}

fn default_network() -> String {
    "mainnet".to_owned()
}

// ===================================================================
// Handlers: thin shells over `Engine`
// ===================================================================

async fn handle_status(State(state): State<Arc<AppState>>) -> Result<Json<StatusResp>, ApiError> {
    Ok(Json(state.engine.status().await?))
}

async fn handle_servers(State(state): State<Arc<AppState>>) -> Json<ServersResp> {
    Json(state.engine.servers().await)
}

async fn handle_licenses(State(state): State<Arc<AppState>>) -> Json<LicensesResp> {
    Json(state.engine.licenses())
}

async fn handle_list(State(state): State<Arc<AppState>>) -> Result<Json<Vec<DbSummary>>, ApiError> {
    Ok(Json(state.engine.list_databases().await?))
}

async fn handle_detail(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<DbDetail>, ApiError> {
    Ok(Json(state.engine.detail(name).await?))
}

/// Permanently delete a database's local state (the `<data-dir>/<name>/`
/// directory). A local cache wipe only; the on-chain writes remain readable by
/// anyone holding the zkv address.
async fn handle_forget(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<OkResp>, ApiError> {
    Ok(Json(state.engine.forget(name).await?))
}

/// Decrypt and return an admin database's recovery phrase for the Danger Zone
/// "show seed phrase" action. POST (not GET) so the secret never lands in a
/// URL, browser history, or proxy log.
async fn handle_reveal_phrase(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<RevealPhraseResp>, ApiError> {
    Ok(Json(state.engine.reveal_phrase(name).await?))
}

async fn handle_history(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<HistoryResp>, ApiError> {
    Ok(Json(
        state
            .engine
            .history(name, q.filter, q.limit, q.offset, q.locate)
            .await?,
    ))
}

async fn handle_rejections(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<RejectionsResp>, ApiError> {
    Ok(Json(state.engine.rejections(name).await?))
}

async fn handle_roles(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<RolesResp>, ApiError> {
    Ok(Json(state.engine.roles(name).await?))
}

async fn handle_funding(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(q): Query<FundingQuery>,
) -> Result<Json<FundingResp>, ApiError> {
    Ok(Json(state.engine.funding(name, q.limit, q.offset).await?))
}

async fn handle_sync(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<SyncReq>,
) -> Result<Json<SyncResp>, ApiError> {
    Ok(Json(state.engine.sync(name, body.mempool).await?))
}

async fn handle_pause(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<PauseReq>,
) -> Json<PauseResp> {
    Json(state.engine.set_pause(name, body.paused))
}

async fn handle_pause_all(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PauseReq>,
) -> Json<PauseResp> {
    Json(state.engine.set_pause_all(body.paused))
}

async fn handle_settings(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SettingsReq>,
) -> Json<SettingsResp> {
    Json(state.engine.set_settings(body.sync_workers))
}

async fn handle_init(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<InitReq>,
) -> Result<Json<TxResp>, ApiError> {
    Ok(Json(state.engine.init(name, body.require_sync).await?))
}

async fn handle_faucet_funds(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<FaucetResp>, ApiError> {
    Ok(Json(state.engine.faucet_funds(name).await?))
}

async fn handle_faucet_init(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<FaucetResp>, ApiError> {
    Ok(Json(state.engine.faucet_init(name).await?))
}

async fn handle_set(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<SetReq>,
) -> Result<Json<TxResp>, ApiError> {
    Ok(Json(
        state
            .engine
            .set_key(name, body.key, body.value, body.offline)
            .await?,
    ))
}

async fn handle_sign_preview(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<SignPreviewReq>,
) -> Result<Json<SignPreviewResp>, ApiError> {
    Ok(Json(
        state
            .engine
            .sign_preview(name, body.op, body.key, body.value)
            .await?,
    ))
}

async fn handle_del(
    State(state): State<Arc<AppState>>,
    Path((name, key)): Path<(String, String)>,
) -> Result<Json<TxResp>, ApiError> {
    Ok(Json(state.engine.del_key(name, key).await?))
}

async fn handle_send(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<SendReq>,
) -> Result<Json<TxResp>, ApiError> {
    Ok(Json(
        state
            .engine
            .send(name, body.recipient, body.amount, body.memo)
            .await?,
    ))
}

async fn handle_check_address(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<CheckAddrReq>,
) -> Result<Json<AddrCheckResp>, ApiError> {
    Ok(Json(state.engine.check_address(name, body.address).await?))
}

async fn handle_sign_memo(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<SignMemoReq>,
) -> Result<Json<SignMemoResp>, ApiError> {
    Ok(Json(
        state
            .engine
            .sign_memo(name, body.op, body.key, body.value, body.scope)
            .await?,
    ))
}

async fn handle_create(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateReq>,
) -> Result<Json<CreateResp>, ApiError> {
    let network = parse_network(&body.network)?;
    let pool = parse_pool(body.pool.as_deref())?;
    Ok(Json(
        state
            .engine
            .create(body.name, network, pool, body.phrase)
            .await?,
    ))
}

async fn handle_generate_phrase(
    State(state): State<Arc<AppState>>,
) -> Result<Json<PhraseResp>, ApiError> {
    Ok(Json(state.engine.generate_phrase()))
}

async fn handle_open_data_dir(
    State(state): State<Arc<AppState>>,
) -> Result<Json<OkResp>, ApiError> {
    Ok(Json(state.engine.open_data_dir()?))
}

async fn handle_watch(
    State(state): State<Arc<AppState>>,
    Json(body): Json<WatchReq>,
) -> Result<Json<AddDbResp>, ApiError> {
    Ok(Json(state.engine.watch(body.zkv_address, body.name).await?))
}

async fn handle_reimport_demo(
    State(state): State<Arc<AppState>>,
) -> Result<Json<AddDbResp>, ApiError> {
    Ok(Json(state.engine.reimport_demo().await?))
}

async fn handle_mark_onboarded(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.engine.mark_onboarded()?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn handle_inspect_address(
    State(state): State<Arc<AppState>>,
    Json(body): Json<InspectAddressReq>,
) -> Result<Json<ZkvAddrInfoResp>, ApiError> {
    Ok(Json(state.engine.inspect_address(body.address).await?))
}

async fn handle_restore(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RestoreReq>,
) -> Result<Json<AddDbResp>, ApiError> {
    let network = parse_network(&body.network)?;
    Ok(Json(
        state
            .engine
            .restore(body.name, body.phrase, network, body.birthday)
            .await?,
    ))
}

async fn handle_current(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CurrentReq>,
) -> Result<Json<OkResp>, ApiError> {
    Ok(Json(state.engine.set_current(body.name)?))
}

/// Render `?data=` as a scannable QR code SVG. Token-guarded like every
/// `/api` route, which is why the frontend fetches it (sending the token
/// header) rather than pointing an `<img>` at it.
async fn handle_qr(
    State(state): State<Arc<AppState>>,
    Query(q): Query<QrQuery>,
) -> Result<Json<QrResp>, ApiError> {
    Ok(Json(state.engine.qr(q.data)?))
}

// ===================================================================
// Helpers
// ===================================================================

fn parse_network(s: &str) -> Result<Network, ApiError> {
    Network::parse(s).map_err(|msg| {
        ApiError(
            StatusCode::BAD_REQUEST,
            ErrorBody {
                error: msg,
                code: "bad_network",
                available: None,
                required: None,
                pending: None,
            },
        )
    })
}

/// Parse the optional `pool` field from a create request. Absent means
/// Orchard (the default pool).
fn parse_pool(s: Option<&str>) -> Result<ShieldedProtocol, ApiError> {
    match s {
        None => Ok(ShieldedProtocol::Orchard),
        Some(v) => crate::config::parse_pool(v).map_err(|msg| {
            ApiError(
                StatusCode::BAD_REQUEST,
                ErrorBody {
                    error: msg,
                    code: "bad_pool",
                    available: None,
                    required: None,
                    pending: None,
                },
            )
        }),
    }
}

fn random_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// A fresh 128-bit CSP nonce (hex), minted per index response so the inline
/// session-token script is the only inline script the browser will run.
fn random_nonce() -> String {
    random_token()
}

fn open_browser(url: &str) {
    // Best-effort: ignore failures (headless box, no browser, etc.).
    let candidates: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("open", &[])]
    } else if cfg!(target_os = "windows") {
        &[("cmd", &["/C", "start", ""])]
    } else {
        &[("xdg-open", &[]), ("sensible-browser", &[])]
    };
    for (cmd, prefix) in candidates {
        let mut c = std::process::Command::new(cmd);
        c.args(prefix.iter()).arg(url);
        if c.spawn().is_ok() {
            return;
        }
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    eprintln!("\nzkv gui: shutting down.");
}

// ===================================================================
// Security middleware
// ===================================================================

/// Reject any `/api` request that doesn't carry the session token or
/// that targets a non-loopback `Host`. The custom token header also
/// forces a CORS preflight on cross-origin requests, which we never
/// answer, so a hostile web page can't drive the API.
async fn guard(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let headers = req.headers();

    // Host must be one of our loopback aliases on the bound port.
    if !host_is_local(headers, state.bound.port()) {
        return error_response(
            StatusCode::FORBIDDEN,
            "bad_host",
            "request Host is not a recognized loopback address",
        );
    }

    let presented = headers
        .get("x-zkv-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !constant_time_eq(presented.as_bytes(), state.token.as_bytes()) {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "bad_token",
            "missing or invalid session token",
        );
    }

    next.run(req).await
}

fn host_is_local(headers: &HeaderMap, port: u16) -> bool {
    let host = match headers.get(header::HOST).and_then(|v| v.to_str().ok()) {
        Some(h) => h,
        None => return false,
    };
    let allowed = [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ];
    allowed.iter().any(|a| a == host)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ===================================================================
// Static asset serving
// ===================================================================

async fn static_handler(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let path = uri.path();
    let path = if path == "/" { "/index.html" } else { path };

    // index.html carries the session token, injected per-launch, plus a strict
    // Content-Security-Policy whose nonce authorizes the single inline token
    // script. Everything else loads from 'self'. `connect-src 'self'` is the
    // load-bearing part: it stops an injected script from exfiltrating the
    // session token off-box, shrinking any XSS from "steal token + spend" to a
    // much narrower footprint. (Defense in depth; no XSS sink is known.)
    if path == "/index.html" {
        let nonce = random_nonce();
        let html = assets::index_html(&state.token, &nonce);
        let csp = format!(
            "default-src 'self'; script-src 'self' 'nonce-{nonce}'; \
             object-src 'none'; base-uri 'none'; frame-ancestors 'none'; \
             form-action 'self'"
        );
        return (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8".to_owned()),
                (header::CONTENT_SECURITY_POLICY, csp),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
                (header::REFERRER_POLICY, "no-referrer".to_owned()),
            ],
            html,
        )
            .into_response();
    }

    match assets::lookup(path.trim_start_matches('/')) {
        Some((bytes, mime)) => (
            [
                (header::CONTENT_TYPE, mime),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            bytes,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

// ===================================================================
// Error responses
// ===================================================================

#[derive(Serialize)]
struct ErrorBody {
    error: String,
    code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    available: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    required: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending: Option<u64>,
}

struct ApiError(StatusCode, ErrorBody);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

impl From<ZkvError> for ApiError {
    fn from(e: ZkvError) -> Self {
        let (status, code) = match &e {
            ZkvError::UnknownDatabase(_) => (StatusCode::NOT_FOUND, "unknown_database"),
            ZkvError::NotInitialized => (StatusCode::CONFLICT, "not_initialized"),
            ZkvError::Initializing { .. } => (StatusCode::CONFLICT, "initializing"),
            ZkvError::NotSynced => (StatusCode::CONFLICT, "not_synced"),
            ZkvError::StaleChainTip => (StatusCode::SERVICE_UNAVAILABLE, "stale_tip"),
            ZkvError::WatchOnly => (StatusCode::FORBIDDEN, "watch_only"),
            ZkvError::Unauthorized(_) => (StatusCode::FORBIDDEN, "unauthorized"),
            ZkvError::InsufficientFunds { .. } => {
                (StatusCode::PAYMENT_REQUIRED, "insufficient_funds")
            }
            ZkvError::ClientUpgradeRequired { .. } => {
                (StatusCode::UPGRADE_REQUIRED, "client_upgrade_required")
            }
            ZkvError::Other(_) => (StatusCode::INTERNAL_SERVER_ERROR, "error"),
        };
        let (available, required, pending) = match &e {
            ZkvError::InsufficientFunds {
                available,
                required,
                pending,
            } => (Some(*available), Some(*required), Some(*pending)),
            _ => (None, None, None),
        };
        ApiError(
            status,
            ErrorBody {
                error: e.to_string(),
                code,
                available,
                required,
                pending,
            },
        )
    }
}

/// Build a bare error response for the middleware (which can't use the
/// `ApiError` extractor return path).
fn error_response(status: StatusCode, code: &'static str, message: &str) -> Response {
    (
        status,
        Json(ErrorBody {
            error: message.to_owned(),
            code,
            available: None,
            required: None,
            pending: None,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt; // for `oneshot`

    fn test_state(token: &str) -> Arc<AppState> {
        Arc::new(AppState {
            engine: Engine::new(ConnectionArgs::default()),
            token: token.to_owned(),
            bound: "127.0.0.1:8088".parse().unwrap(),
        })
    }

    fn api_req(uri: &str, host: &str, token: Option<&str>) -> Request<Body> {
        let mut b = Request::builder().uri(uri).header("host", host);
        if let Some(t) = token {
            b = b.header("x-zkv-token", t);
        }
        b.body(Body::empty()).unwrap()
    }

    /// Build an authorized POST `/api` request with a JSON body.
    fn api_post(uri: &str, json: &str, token: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("host", "127.0.0.1:8088")
            .header("x-zkv-token", token)
            .header("content-type", "application/json")
            .body(Body::from(json.to_owned()))
            .unwrap()
    }

    async fn body_string(res: Response) -> String {
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn serves_index_with_token_injected() {
        let app = router(test_state("secrettoken"));
        let res = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // A strict CSP is served, and its nonce matches the inline token script.
        let csp = res
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .expect("CSP header present")
            .to_str()
            .unwrap()
            .to_owned();
        assert!(csp.contains("default-src 'self'"), "CSP locks to self");
        assert!(
            csp.contains("script-src 'self' 'nonce-"),
            "CSP carries a script nonce"
        );
        let nonce = csp
            .split("'nonce-")
            .nth(1)
            .and_then(|s| s.split('\'').next())
            .expect("nonce in CSP");
        let html = body_string(res).await;
        assert!(html.contains("secrettoken"), "token should be injected");
        assert!(
            html.contains(&format!("nonce=\"{nonce}\"")),
            "inline script nonce should match the CSP nonce"
        );
        assert!(
            html.contains("/js/app.js"),
            "should reference the app bundle"
        );
        assert!(
            !html.contains("__ZKV_TOKEN__"),
            "token placeholder should be replaced"
        );
        assert!(
            !html.contains("__ZKV_NONCE__"),
            "nonce placeholder should be replaced"
        );
    }

    #[tokio::test]
    async fn serves_a_static_asset() {
        let app = router(test_state("t"));
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/js/app.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn api_requires_token() {
        let app = router(test_state("secrettoken"));
        let res = app
            .oneshot(api_req("/api/databases", "127.0.0.1:8088", None))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn api_rejects_foreign_host() {
        let app = router(test_state("secrettoken"));
        let res = app
            .oneshot(api_req(
                "/api/databases",
                "evil.example.com",
                Some("secrettoken"),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn api_lists_databases_as_json() {
        // Point the data dir at an empty temp dir so the list is [].
        let dir = std::env::temp_dir().join(format!("zkv-gui-it-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("ZKV_DATA", &dir);

        let app = router(test_state("secrettoken"));
        let res = app
            .oneshot(api_req(
                "/api/databases",
                "localhost:8088",
                Some("secrettoken"),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_string(res).await, "[]");
    }

    #[tokio::test]
    async fn api_history_unknown_db_is_404() {
        // Routing + the `?key=` Query extractor + error mapping: an unknown
        // database resolves to a structured 404 just like the detail route.
        let dir = std::env::temp_dir().join(format!("zkv-gui-hist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("ZKV_DATA", &dir);

        let app = router(test_state("secrettoken"));
        let res = app
            .oneshot(api_req(
                "/api/databases/nope/history?key=anything",
                "localhost:8088",
                Some("secrettoken"),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn api_forget_unknown_db_is_404() {
        // Routing + token guard + the engine's existence check + error mapping:
        // a DELETE against an unknown database resolves to a structured 404,
        // just like the GET detail/history routes. (The success path deletes a
        // real db directory, exercised by the facade/data layer.)
        let dir = std::env::temp_dir().join(format!("zkv-gui-forget-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("ZKV_DATA", &dir);

        let req = Request::builder()
            .method("DELETE")
            .uri("/api/databases/nope")
            .header("host", "127.0.0.1:8088")
            .header("x-zkv-token", "secrettoken")
            .body(Body::empty())
            .unwrap();
        let app = router(test_state("secrettoken"));
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn api_roles_unknown_db_is_404() {
        // Routing + error mapping: an unknown database resolves to a
        // structured 404 just like the detail and history routes.
        let dir = std::env::temp_dir().join(format!("zkv-gui-roles-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("ZKV_DATA", &dir);

        let app = router(test_state("secrettoken"));
        let res = app
            .oneshot(api_req(
                "/api/databases/nope/roles",
                "localhost:8088",
                Some("secrettoken"),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn api_funding_unknown_db_is_404() {
        // Routing + the `FundingQuery` extractor + error mapping: an unknown
        // database resolves to a structured 404, like the history route.
        let dir = std::env::temp_dir().join(format!("zkv-gui-fund-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("ZKV_DATA", &dir);

        let app = router(test_state("secrettoken"));
        let res = app
            .oneshot(api_req(
                "/api/databases/nope/funding?limit=10",
                "localhost:8088",
                Some("secrettoken"),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn api_sign_unknown_db_is_404() {
        // Routing + token guard + SignMemoReq deserialization + error mapping:
        // signing against an unknown database resolves to a structured 404,
        // just like the detail/history routes (the op never gets as far as a
        // wallet).
        let dir = std::env::temp_dir().join(format!("zkv-gui-sign-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("ZKV_DATA", &dir);

        let app = router(test_state("secrettoken"));
        let res = app
            .oneshot(api_post(
                "/api/databases/nope/sign",
                r#"{"op":"SET","key":"k","value":"v"}"#,
                "secrettoken",
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn api_sign_preview_unknown_db_is_404() {
        // Routing + the SignPreviewReq body + error mapping: signing a memo for
        // an unknown database resolves to a structured 404, like the other
        // db-scoped routes. (A real preview needs an initialized, funded wallet,
        // so the success path is exercised by the facade/protocol tests.)
        let dir = std::env::temp_dir().join(format!("zkv-gui-signprev-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("ZKV_DATA", &dir);

        let app = router(test_state("secrettoken"));
        let res = app
            .oneshot(api_post(
                "/api/databases/nope/sign-preview",
                r#"{"op":"set","key":"k","value":"v"}"#,
                "secrettoken",
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn api_pause_toggles_without_wallet() {
        // The per-db pause endpoint is pure in-memory state: it routes, passes
        // the token guard, deserializes, and echoes the new flag; no wallet.
        let app = router(test_state("secrettoken"));
        let res = app
            .oneshot(api_post(
                "/api/databases/anyname/pause",
                r#"{"paused":true}"#,
                "secrettoken",
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(body_string(res).await.contains("\"paused\":true"));
    }

    #[tokio::test]
    async fn api_settings_clamps_workers() {
        let max = engine::MAX_SYNC_WORKERS;
        // Over the max clamps down to MAX_SYNC_WORKERS.
        let res = router(test_state("secrettoken"))
            .oneshot(api_post(
                "/api/settings",
                r#"{"sync_workers":999}"#,
                "secrettoken",
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(body_string(res)
            .await
            .contains(&format!("\"sync_workers\":{max}")));

        // Zero clamps up to 1.
        let res = router(test_state("secrettoken"))
            .oneshot(api_post(
                "/api/settings",
                r#"{"sync_workers":0}"#,
                "secrettoken",
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(body_string(res).await.contains("\"sync_workers\":1"));
    }

    #[tokio::test]
    async fn api_pause_all_toggles_state() {
        let res = router(test_state("secrettoken"))
            .oneshot(api_post(
                "/api/pause-all",
                r#"{"paused":true}"#,
                "secrettoken",
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(body_string(res).await.contains("\"paused\":true"));
    }

    #[test]
    fn init_gate_errors_map_to_expected_codes() {
        let ns = ApiError::from(ZkvError::NotSynced);
        assert_eq!(ns.0, StatusCode::CONFLICT);
        assert_eq!(ns.1.code, "not_synced");

        let stale = ApiError::from(ZkvError::StaleChainTip);
        assert_eq!(stale.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(stale.1.code, "stale_tip");
    }
}
