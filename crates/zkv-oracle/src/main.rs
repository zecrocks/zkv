//! Production CoinGecko price oracle for zkv (testnet).
//!
//! Every `ZKV_INTERVAL_SECS` (default 15 minutes) it fetches the latest
//! ZEC/USD and BTC/USD spot prices from the CoinGecko **Pro** API and
//! publishes them to a testnet zkv database under
//!
//! ```text
//! rates/zec_usd/coingecko
//! rates/btc_usd/coingecko
//! ```
//!
//! The provider lives in the final key segment so other providers
//! (`rates/zec_usd/<other>`) can be added later without disturbing this one.
//!
//! It exposes a health gate on `ZKV_HEALTH_ADDR` (default `0.0.0.0:8099`):
//! `GET /health` returns 200 while the wallet is in sync and writes are
//! landing, 503 once it falls too far behind; a container orchestrator
//! can restart it. See the crate README for the docker-compose wiring.
//!
//! Configuration is entirely environment-driven (see [`Config::from_env`]);
//! the seed and the CoinGecko key are never passed on the command line.

use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context};
use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use serde_json::{json, Value};

use zkv::{
    config::Role,
    data::{set_data_dir_override, Network},
    db::{Database, ZkvError},
    protocol::{network_from_type, parse_zkv_addr},
    remote::{ConnectionArgs, ConnectionMode, Servers},
};

const ZEC_KEY: &str = "rates/zec_usd/coingecko";
const BTC_KEY: &str = "rates/btc_usd/coingecko";
const COINGECKO_URL: &str = "https://pro-api.coingecko.com/api/v3/simple/price";

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

struct Config {
    seed: String,
    api_key: String,
    db_name: String,
    data_dir: PathBuf,
    /// The database's zkv address (`zkvtest1…`). On a fresh data dir the
    /// network, birthday, and shielded pool for the restore are all read
    /// from it, so they never have to be configured by hand.
    zkv_address: Option<String>,
    /// Optional explicit birthday override. Normally unset: the birthday
    /// rides inside the zkv address. Only honoured when restoring.
    birthday: Option<u32>,
    server: String,
    interval: Duration,
    health_addr: SocketAddr,
    max_lag_blocks: u32,
    max_write_age: Duration,
    /// Max attempts (fetch + sync + write) within a single tick before giving
    /// up and waiting for the next scheduled tick. 1 disables retrying.
    tick_attempts: u32,
    /// Pause between attempts within a tick. Retries never bleed past the
    /// next scheduled tick (capped against `interval`).
    retry_backoff: Duration,
}

impl Config {
    fn from_env() -> anyhow::Result<Self> {
        let seed = env_required("ZKV_ORACLE_SEED")?;
        let api_key = env_required("COINGECKO_API_KEY")?;
        let db_name = env_or("ZKV_DB", "oracle-testnet");
        let data_dir = PathBuf::from(env_or("ZKV_DATA", "/data"));
        let zkv_address = std::env::var("ZKV_ADDRESS")
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        let birthday = env_opt_parse::<u32>("ZKV_BIRTHDAY")?;
        let server = env_or("ZKV_SERVER", "zecrocks");
        let interval =
            Duration::from_secs(env_opt_parse::<u64>("ZKV_INTERVAL_SECS")?.unwrap_or(15 * 60));
        let health_addr: SocketAddr = env_or("ZKV_HEALTH_ADDR", "0.0.0.0:8099")
            .parse()
            .context("ZKV_HEALTH_ADDR must be a host:port socket address")?;
        let max_lag_blocks = env_opt_parse::<u32>("ZKV_MAX_LAG_BLOCKS")?.unwrap_or(50);
        // Default: twice the write cadence; a single missed tick is tolerated.
        let max_write_age = env_opt_parse::<u64>("ZKV_MAX_WRITE_AGE_SECS")?
            .map(Duration::from_secs)
            .unwrap_or(interval * 2);
        // Retry within a tick so a transient fetch/sync/write failure is
        // re-attempted in seconds rather than going dark until the next tick.
        let tick_attempts = env_opt_parse::<u32>("ZKV_TICK_ATTEMPTS")?
            .unwrap_or(4)
            .max(1);
        let retry_backoff =
            Duration::from_secs(env_opt_parse::<u64>("ZKV_RETRY_BACKOFF_SECS")?.unwrap_or(30));

        Ok(Self {
            seed,
            api_key,
            db_name,
            data_dir,
            zkv_address,
            birthday,
            server,
            interval,
            health_addr,
            max_lag_blocks,
            max_write_age,
            tick_attempts,
            retry_backoff,
        })
    }

    fn connection(&self) -> anyhow::Result<ConnectionArgs> {
        Ok(ConnectionArgs {
            server: Servers::parse(&self.server)
                .with_context(|| format!("invalid ZKV_SERVER {:?}", self.server))?,
            mainnet_server: None,
            testnet_server: None,
            connection: ConnectionMode::Direct,
        })
    }
}

fn env_required(key: &str) -> anyhow::Result<String> {
    let v = std::env::var(key).map_err(|_| anyhow!("missing required env var {key}"))?;
    if v.trim().is_empty() {
        return Err(anyhow!("env var {key} is set but empty"));
    }
    Ok(v)
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_owned())
}

fn env_opt_parse<T: std::str::FromStr>(key: &str) -> anyhow::Result<Option<T>>
where
    T::Err: std::fmt::Display,
{
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => v
            .trim()
            .parse::<T>()
            .map(Some)
            .map_err(|e| anyhow!("env var {key} is not a valid value: {e}")),
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Health state (shared between the oracle loop and the health server)
// ---------------------------------------------------------------------------

struct Health {
    max_lag_blocks: u32,
    max_write_age: Duration,
    /// Set once the first tick has completed (success or failure), ending the
    /// startup grace period during which we report healthy.
    first_tick_done: AtomicBool,
    /// Wallet scan height behind the chain tip at the last successful sync.
    blocks_behind: AtomicU32,
    /// Unix seconds of the last tick where both keys were written. 0 = never.
    last_write_unix: AtomicU64,
    last_error: Mutex<Option<String>>,
}

impl Health {
    fn new(cfg: &Config) -> Self {
        Self {
            max_lag_blocks: cfg.max_lag_blocks,
            max_write_age: cfg.max_write_age,
            first_tick_done: AtomicBool::new(false),
            blocks_behind: AtomicU32::new(0),
            last_write_unix: AtomicU64::new(0),
            last_error: Mutex::new(None),
        }
    }

    fn set_blocks_behind(&self, n: u32) {
        self.blocks_behind.store(n, Ordering::Relaxed);
    }

    fn mark_write_ok(&self) {
        self.last_write_unix.store(now_unix(), Ordering::Relaxed);
        *self.last_error.lock().unwrap() = None;
    }

    fn set_error(&self, msg: impl Into<String>) {
        *self.last_error.lock().unwrap() = Some(msg.into());
    }

    fn finish_tick(&self) {
        self.first_tick_done.store(true, Ordering::Relaxed);
    }

    /// (healthy, status string, JSON body).
    fn snapshot(&self) -> (bool, Value) {
        let behind = self.blocks_behind.load(Ordering::Relaxed);
        let last_write = self.last_write_unix.load(Ordering::Relaxed);
        let last_error = self.last_error.lock().unwrap().clone();
        let write_secs_ago = if last_write == 0 {
            None
        } else {
            Some(now_unix().saturating_sub(last_write))
        };

        let (healthy, status) = if !self.first_tick_done.load(Ordering::Relaxed) {
            // Cold start: still catching up. Docker's start_period covers this.
            (true, "starting")
        } else {
            let lag_ok = behind <= self.max_lag_blocks;
            // Write freshness only gates health once a full write has actually
            // landed. Before the first successful write (cold start, or a
            // funding/confirmation lull right after a restart, when
            // `last_write` is still 0) we don't report unhealthy: a restart
            // can't make the chain confirm or conjure funds, so flipping to 503
            // here just invites a pointless autoheal restart loop. Sync lag is
            // still enforced, so a genuinely stuck wallet is still caught.
            let write_fresh = write_secs_ago
                .map(|s| s <= self.max_write_age.as_secs())
                .unwrap_or(true);
            if lag_ok && write_fresh {
                (true, "ok")
            } else {
                (false, "unhealthy")
            }
        };

        let body = json!({
            "status": status,
            "healthy": healthy,
            "blocks_behind": behind,
            "max_lag_blocks": self.max_lag_blocks,
            "last_write_secs_ago": write_secs_ago,
            "max_write_age_secs": self.max_write_age.as_secs(),
            "last_error": last_error,
        });
        (healthy, body)
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn health_handler(State(health): State<Arc<Health>>) -> impl IntoResponse {
    let (healthy, body) = health.snapshot();
    let code = if healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(body))
}

// ---------------------------------------------------------------------------
// CoinGecko fetch
// ---------------------------------------------------------------------------

async fn fetch_prices(client: &reqwest::Client, api_key: &str) -> anyhow::Result<(String, String)> {
    let resp = client
        .get(COINGECKO_URL)
        .query(&[("ids", "zcash,bitcoin"), ("vs_currencies", "usd")])
        .header("x-cg-pro-api-key", api_key)
        .send()
        .await
        .context("coingecko request failed")?;

    let status = resp.status();
    let text = resp.text().await.context("coingecko: reading body")?;
    if !status.is_success() {
        return Err(anyhow!("coingecko HTTP {status}: {}", text.trim()));
    }

    let v: Value = serde_json::from_str(&text).context("coingecko: invalid JSON")?;
    let zec = extract_price(&v, "zcash")?;
    let btc = extract_price(&v, "bitcoin")?;
    Ok((zec, btc))
}

/// Pull `<id>.usd` out of the CoinGecko `simple/price` response as a bare
/// decimal string, preserving its on-the-wire numeric form (no float
/// reformatting noise). Rejects missing, non-finite, or non-positive prices.
fn extract_price(v: &Value, id: &str) -> anyhow::Result<String> {
    let usd = v
        .get(id)
        .and_then(|q| q.get("usd"))
        .ok_or_else(|| anyhow!("coingecko: missing {id}.usd in response"))?;
    let n = usd
        .as_f64()
        .ok_or_else(|| anyhow!("coingecko: {id}.usd is not a number: {usd}"))?;
    if !n.is_finite() || n <= 0.0 {
        return Err(anyhow!("coingecko: implausible {id}.usd price: {n}"));
    }
    // `Value::to_string` on a JSON number yields its canonical decimal text
    // (e.g. "23.4", "65000"), a clean bare decimal for the memo value.
    Ok(usd.to_string())
}

// ---------------------------------------------------------------------------
// Oracle loop
// ---------------------------------------------------------------------------

fn short(txid: &str) -> String {
    if txid.len() <= 12 {
        return txid.to_owned();
    }
    format!("{}…{}", &txid[..6], &txid[txid.len() - 6..])
}

/// One full publish cycle: fetch, sync, measure lag, publish both keys in a
/// single transaction, with bounded retries inside the tick.
///
/// Both keys are written in **one** "sendmany" transaction (one ZIP-317 fee,
/// one txid) via `Database::set_many_no_sync`, so the publish is atomic: either
/// the whole batch broadcasts or it doesn't. A failed batch is retried whole,
/// and each attempt **re-fetches** the prices so a retry carries a current
/// quote, never one held over from the failed attempt: freshness over fee
/// economy. Retries are bounded by `tick_attempts` and never bleed past the
/// next scheduled tick.
///
/// Never returns an error and never panics; every failure is logged and
/// recorded on `health`, and the loop simply carries on to the next tick.
async fn run_tick(db: &Database, client: &reqwest::Client, cfg: &Config, health: &Health) {
    let ticker = db.network().ticker();
    let start = Instant::now();
    let mut published = false;

    for attempt in 1..=cfg.tick_attempts {
        // Re-fetch every attempt: a retried write must carry a fresh quote.
        let prices = match fetch_prices(client, &cfg.api_key).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    "price fetch failed (attempt {attempt}/{}): {e:#}",
                    cfg.tick_attempts
                );
                health.set_error(format!("price fetch: {e}"));
                if backoff_before_retry(attempt, cfg, start).await {
                    continue;
                }
                break;
            }
        };

        // A single sync covers the whole batch; cheaper than per-key syncs.
        if let Err(e) = db.sync().await {
            tracing::warn!(
                "sync failed (attempt {attempt}/{}): {e:#}",
                cfg.tick_attempts
            );
            health.set_error(format!("sync: {e}"));
            if backoff_before_retry(attempt, cfg, start).await {
                continue;
            }
            break;
        }

        measure_lag(db, health).await;

        // Publish both keys in ONE transaction (one fee, one txid). The
        // pre-broadcast sync is skipped: we already synced just above.
        let pairs = [(ZEC_KEY, prices.0.as_str()), (BTC_KEY, prices.1.as_str())];
        match db.set_many_no_sync(&pairs).await {
            Ok(txid) => {
                tracing::info!(
                    "✓ {ZEC_KEY}={} {BTC_KEY}={} (txid {})",
                    prices.0,
                    prices.1,
                    short(&txid)
                );
                health.mark_write_ok();
                published = true;
                break;
            }
            Err(e) => {
                let detail = describe_write_error(&e, ticker);
                tracing::warn!(
                    "batch write failed (attempt {attempt}/{}): {detail}",
                    cfg.tick_attempts
                );
                health.set_error(format!("write: {detail}"));
                if !backoff_before_retry(attempt, cfg, start).await {
                    break;
                }
            }
        }
    }

    if !published {
        tracing::warn!(
            "tick gave up after {} attempt(s); prices not published, retrying next tick",
            cfg.tick_attempts,
        );
    }

    health.finish_tick();
}

/// Sleep `retry_backoff` before the next in-tick attempt, returning whether a
/// retry should happen at all. Stops (returns false) once attempts are
/// exhausted or another backoff would run into the next scheduled tick.
async fn backoff_before_retry(attempt: u32, cfg: &Config, start: Instant) -> bool {
    if attempt >= cfg.tick_attempts {
        return false;
    }
    if start.elapsed() + cfg.retry_backoff >= cfg.interval {
        return false;
    }
    tokio::time::sleep(cfg.retry_backoff).await;
    true
}

/// Best-effort scan-lag measurement for the health gate. Never fails the tick.
async fn measure_lag(db: &Database, health: &Health) {
    match (db.chain_tip().await, db.synced_height()) {
        (Ok(tip), Ok(synced)) => {
            let behind = tip.saturating_sub(synced.unwrap_or(0));
            health.set_blocks_behind(behind);
            tracing::info!(
                "synced to {}, chain tip {tip} (behind {behind})",
                synced.unwrap_or(0)
            );
        }
        (tip, synced) => {
            tracing::warn!("could not measure lag: tip={tip:?} synced={synced:?}");
        }
    }
}

fn describe_write_error(e: &ZkvError, ticker: &str) -> String {
    match e {
        ZkvError::InsufficientFunds {
            available,
            required,
            pending,
        } => format!(
            "insufficient funds: have {:.8} {ticker}, need {:.8} {ticker} (pending {:.8} {ticker})",
            *available as f64 / 1e8,
            *required as f64 / 1e8,
            *pending as f64 / 1e8,
        ),
        ZkvError::Initializing { done, required } => {
            format!("database still initializing ({done}/{required} confirmations)")
        }
        ZkvError::NotInitialized => {
            "database is not initialized on-chain (broadcast INIT for this seed first)".to_owned()
        }
        other => format!("{other}"),
    }
}

/// Open the database, restoring the wallet from the seed if it does not yet
/// exist locally. Assumes the database is already INITed on-chain for this
/// seed; restore-only bootstrap does not broadcast INIT.
///
/// On a fresh data dir the restore needs the database's network, birthday,
/// and shielded pool. All three ride inside the zkv address, so we decode
/// them from `ZKV_ADDRESS` rather than asking the operator to repeat them
/// (the pool especially: a Sapling database restored as Orchard would see
/// nothing). The seed still supplies the spending key; after the restore we
/// re-derive the address and confirm it matches, catching a seed that
/// doesn't belong to the configured database.
async fn open_or_restore(cfg: &Config) -> anyhow::Result<Database> {
    let conn = cfg.connection()?;
    let db = match Database::open(&cfg.db_name, conn.clone()) {
        Ok(db) => {
            tracing::info!("opened existing database {:?}", cfg.db_name);
            db
        }
        Err(ZkvError::UnknownDatabase(_)) => restore_from_address(cfg, conn).await?,
        Err(e) => return Err(anyhow!("opening database {:?}: {e}", cfg.db_name)),
    };

    if db.role() != Role::Admin {
        return Err(anyhow!(
            "database {:?} is watch-only; the oracle needs an admin (spending) key",
            cfg.db_name
        ));
    }
    Ok(db)
}

/// Restore the admin wallet on a fresh data dir, taking the network, birthday,
/// and pool from `ZKV_ADDRESS` (with `ZKV_BIRTHDAY` as an optional override).
async fn restore_from_address(cfg: &Config, conn: ConnectionArgs) -> anyhow::Result<Database> {
    let addr = cfg.zkv_address.as_deref().ok_or_else(|| {
        anyhow!(
            "database {:?} not found locally and ZKV_ADDRESS is unset; set ZKV_ADDRESS to the \
             database's zkv address (zkvtest1…) so the oracle can derive its network, birthday, \
             and shielded pool for the restore",
            cfg.db_name
        )
    })?;

    let parsed = parse_zkv_addr(addr).context("parsing ZKV_ADDRESS")?;
    let network = Network::from(network_from_type(parsed.network).context("ZKV_ADDRESS network")?);
    // The birthday lives inside the address; a ZKV_BIRTHDAY override is honoured
    // for unusual cases (e.g. forcing an earlier rescan).
    let birthday = cfg.birthday.or(Some(parsed.birthday));

    tracing::info!(
        "database {:?} not found locally; restoring admin wallet from seed \
         (network {:?}, pool {:?}, birthday {})",
        cfg.db_name,
        network,
        parsed.pool,
        birthday.unwrap_or(parsed.birthday),
    );

    let db = Database::restore_admin_with_pool(
        &cfg.db_name,
        network,
        &cfg.seed,
        birthday,
        parsed.pool,
        conn,
    )
    .await
    .context("restoring wallet from ZKV_ORACLE_SEED")?;

    // The reconstructed address must match the one we were given. With the
    // birthday taken from the address this is byte-for-byte; an override
    // changes the encoded birthday, so only enforce it in the normal case.
    if cfg.birthday.is_none() {
        let derived = db
            .zkv_address()
            .context("deriving zkv address after restore")?;
        if derived != addr {
            return Err(anyhow!(
                "ZKV_ORACLE_SEED does not match ZKV_ADDRESS: the restored wallet's address is \
                 {derived}, not the configured {addr}. Check that the seed belongs to this database."
            ));
        }
    }

    Ok(db)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging();

    let cfg = Config::from_env()?;
    set_data_dir_override(cfg.data_dir.clone());

    let db = open_or_restore(&cfg).await?;

    tracing::info!("zkv oracle");
    tracing::info!("  database   : {} ({:?})", db.name(), db.network());
    tracing::info!("  zkv address: {}", db.zkv_address()?);
    tracing::info!("  keys       : {ZEC_KEY}, {BTC_KEY}");
    tracing::info!("  cadence    : every {}s", cfg.interval.as_secs());
    tracing::info!("  health     : http://{}/health", cfg.health_addr);

    // Health server: only touches the shared Arc, never the Database.
    let health = Arc::new(Health::new(&cfg));
    spawn_health_server(cfg.health_addr, health.clone()).await?;

    let client = reqwest::Client::builder()
        .user_agent(concat!("zkv-oracle/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .context("building HTTP client")?;

    // First tick fires immediately, then every `interval`.
    let mut ticker = tokio::time::interval(cfg.interval);
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                run_tick(&db, &client, &cfg, &health).await;
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received Ctrl-C, shutting down");
                break;
            }
        }
    }
    Ok(())
}

async fn spawn_health_server(addr: SocketAddr, health: Arc<Health>) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/", get(health_handler))
        .route("/health", get(health_handler))
        .route("/healthz", get(health_handler))
        .with_state(health);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding health server on {addr}"))?;
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("health server stopped: {e:#}");
        }
    });
    Ok(())
}

fn init_logging() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_price_reads_both_assets() {
        let v: Value =
            serde_json::from_str(r#"{"zcash":{"usd":23.4},"bitcoin":{"usd":65000}}"#).unwrap();
        assert_eq!(extract_price(&v, "zcash").unwrap(), "23.4");
        assert_eq!(extract_price(&v, "bitcoin").unwrap(), "65000");
    }

    #[test]
    fn extract_price_preserves_decimal_form() {
        let v: Value = serde_json::from_str(r#"{"zcash":{"usd":1234.5678}}"#).unwrap();
        assert_eq!(extract_price(&v, "zcash").unwrap(), "1234.5678");
    }

    #[test]
    fn extract_price_rejects_missing_field() {
        let v: Value = serde_json::from_str(r#"{"zcash":{"usd":23.4}}"#).unwrap();
        assert!(extract_price(&v, "bitcoin").is_err());
    }

    #[test]
    fn extract_price_rejects_non_positive() {
        let v: Value = serde_json::from_str(r#"{"zcash":{"usd":0}}"#).unwrap();
        assert!(extract_price(&v, "zcash").is_err());
    }
}
