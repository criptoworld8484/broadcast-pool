use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::config::Config;
use crate::db::models::*;
use crate::db::Database;
use crate::pool::manager::PoolManager;

mod archive_key;
pub use archive_key::ArchiveKeyStore;

#[derive(Clone)]
pub struct AppState {
    pub pool_manager: Arc<PoolManager>,
    pub db: Arc<Database>,
    pub config: Arc<std::sync::Mutex<Config>>,
    pub archive_keys: Arc<ArchiveKeyStore>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(dashboard))
        .route("/api/stats", get(get_stats))
        .route("/api/mempool-status", get(get_mempool_status))
        .route("/api/transactions", get(list_transactions))
        .route("/api/transactions/import", post(import_transaction))
        .route("/api/transactions/{id}/schedule", post(schedule_transaction))
        .route("/api/transactions/{id}", get(get_transaction))
        .route("/api/transactions/{id}/remove", post(remove_transaction))
        .route("/api/transactions/{id}/retry", post(retry_transaction))
        .route("/api/status", get(get_status))
        .route("/api/security/acknowledge", post(security_acknowledge))
        .route("/api/config", get(get_config))
        .route("/api/config", post(save_config))
        .route("/api/restart", post(restart_daemon))
        .route("/api/estimate-fee", post(estimate_fee))
        .route("/api/indexer-debug", get(get_indexer_debug))
        .route("/api/btc-price", get(get_btc_price))
        .route("/api/archive/set-password", post(archive_set_password))
        .route("/api/archive/unlock", post(archive_unlock))
        .route("/api/archive/lock", post(archive_lock))
        .route("/api/archive", get(list_archive))
        .route("/api/archive/flush", post(archive_flush))
        .route("/api/archive/{id}", get(get_archive_item))
        .with_state(state)
}

async fn dashboard() -> impl IntoResponse {
    let html = load_dashboard_html();
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
    (headers, Html(html))
}

fn load_dashboard_html() -> String {
    const EMBEDDED: &str = include_str!("dashboard.html");
    /// Path to dashboard.html in the source tree at compile time. When the repo
    /// is still present (typical local dev), serve this file so HTML edits apply
    /// without rebuilding the binary.
    const SOURCE_TREE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/api/dashboard.html");

    if let Ok(path) = std::env::var("BROADCAST_POOL_DASHBOARD") {
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                tracing::debug!("Serving dashboard from {}", path);
                return content;
            }
            Err(e) => {
                tracing::warn!(
                    "BROADCAST_POOL_DASHBOARD={} unreadable ({}), using embedded HTML",
                    path,
                    e
                );
            }
        }
    } else if let Ok(content) = std::fs::read_to_string(SOURCE_TREE) {
        tracing::debug!("Serving dashboard from {} (source tree)", SOURCE_TREE);
        return content;
    }
    EMBEDDED.to_string()
}

fn supported_networks_vec() -> Vec<String> {
    crate::config::NetworkType::supported_networks()
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Best-effort LAN IP for wallet Electrum connections (when server binds 0.0.0.0).
fn wallet_connect_url(config: &Config) -> String {
    crate::discovery::wallet_connect_url(config, config.electrum_server.port)
}

fn config_to_response(config: &Config, network_changed: bool) -> ConfigResponse {
    let umbrel = crate::discovery::is_umbrel_mode();
    let pinned = std::env::var("BROADCAST_POOL_INDEXER_URL").is_ok();
    let indexer = config.indexer.as_ref();
    let active_url = indexer.map(|i| i.url.as_str()).unwrap_or("");
    let is_manual = indexer.map(|i| i.manual_override).unwrap_or(false)
        && crate::discovery::extract_indexer_host(active_url)
            .is_none_or(|h| !crate::discovery::is_mistaken_umbrel_lan_override(&h));
    let node_display = if active_url.is_empty()
        || crate::discovery::extract_indexer_host(active_url)
            .is_some_and(|h| crate::discovery::is_mistaken_umbrel_lan_override(&h))
    {
        String::new()
    } else {
        crate::discovery::display_indexer_url(active_url)
    };

    ConfigResponse {
        indexer_url: if is_manual {
            node_display.clone()
        } else {
            String::new()
        },
        indexer_node_url: node_display,
        indexer_use_ssl: crate::discovery::indexer_url_uses_ssl(active_url),
        indexer_is_manual: is_manual,
        network_editable: !umbrel,
        umbrel_mode: umbrel,
        startos_mode: crate::discovery::is_startos_mode(),
        network: config.network.network_type.data_dir_name().to_string(),
        broadcast_mode: config.schedule.broadcast_mode.to_string(),
        default_delay_hours: config.schedule.default_delay_hours,
        scheduled_datetime: config.schedule.scheduled_datetime.clone(),
        liana_vb_enabled: config.schedule.liana_virtual_block.enabled,
        liana_vb_target_height: config.schedule.liana_virtual_block.target_height,
        liana_vb_armed_at_height: config.schedule.liana_virtual_block.armed_at_height,
        min_delay_hours: config.schedule.min_delay_hours,
        max_delay_hours: config.schedule.max_delay_hours,
        min_fee_rate: config.schedule.min_fee_rate,
        max_fee_rate: config.schedule.max_fee_rate,
        web_port: config.web.port,
        electrum_port: config.electrum_server.port,
        electrum_host: config.electrum_server.host.clone(),
        wallet_connect_url: wallet_connect_url(config),
        indexer_auto_detected: !pinned && !is_manual,
        network_changed,
        supported_networks: supported_networks_vec(),
    }
}

/// Maximum allowed gap between the real chain tip and an armed virtual target height
/// (~100_000 blocks, roughly 2 years). Bounds the header-fabrication allocation/loop
/// triggered on every armed Liana subscribe.
const MAX_VIRTUAL_BLOCK_GAP: u64 = 100_000;

/// A virtual block height must be a real future block. Reject 0 (unset) and anything at/below
/// the current tip (it would be non-final immediately, defeating the point), and anything
/// unreasonably far in the future (would force fabricating an enormous header range).
fn validate_virtual_height(target: u64, real_height: Option<u64>) -> Result<(), String> {
    if target == 0 {
        return Err("Introduce una altura de bloque virtual futura.".into());
    }
    if let Some(h) = real_height {
        if target <= h {
            return Err(format!(
                "La altura virtual {} debe ser mayor que la altura actual {}.",
                target, h
            ));
        }
        if target > h + MAX_VIRTUAL_BLOCK_GAP {
            return Err(format!(
                "La altura virtual {} está demasiado lejos de la actual {} (máximo +{}).",
                target, h, MAX_VIRTUAL_BLOCK_GAP
            ));
        }
    }
    Ok(())
}

async fn get_stats(State(state): State<AppState>) -> Result<Json<PoolStats>, (StatusCode, String)> {
    state
        .pool_manager
        .get_stats()
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn get_mempool_status(
    State(state): State<AppState>,
) -> Json<MempoolStatus> {
    let pool_manager = state.pool_manager.clone();
    let status = tokio::task::spawn_blocking(move || pool_manager.get_mempool_status())
        .await
        .unwrap_or(MempoolStatus {
            available: false,
            mempool_tx_count: None,
            fee_rate_sat_vb: None,
            congestion: None,
        });
    Json(status)
}

#[derive(Serialize)]
struct BtcPriceResponse {
    prices: std::collections::HashMap<String, f64>,
    provider: String,
    source: String,
    stale: bool,
    fetched_at: String,
}

async fn get_btc_price(
    State(state): State<AppState>,
) -> Result<Json<BtcPriceResponse>, (StatusCode, String)> {
    let feed = state.pool_manager.price_feed().clone();
    let snapshot = feed
        .fetch_snapshot()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(Json(BtcPriceResponse {
        prices: snapshot.prices,
        provider: feed.provider_name().to_string(),
        source: snapshot.source,
        stale: snapshot.stale,
        fetched_at: snapshot.fetched_at.to_rfc3339(),
    }))
}

async fn list_transactions(
    State(state): State<AppState>,
) -> Result<Json<Vec<BroadcastTx>>, (StatusCode, String)> {
    // Warm price cache so table rows with price triggers show current BTC/fiat.
    let feed = state.pool_manager.price_feed().clone();
    if let Err(e) = feed.fetch_snapshot().await {
        tracing::debug!("Could not prefetch BTC prices for list: {}", e);
    }

    let pool_manager = state.pool_manager.clone();
    let started = std::time::Instant::now();
    // #region agent log
    crate::utils::debug_log::agent_log(
        "H4",
        "api/mod.rs:list_transactions",
        "handler start",
        serde_json::json!({ "blocking_on_async": true }),
    );
    // #endregion
    let result = tokio::task::spawn_blocking(move || pool_manager.list_transactions(None, 100))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Task failed: {}", e)))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    // #region agent log
    crate::utils::debug_log::agent_log(
        "H4",
        "api/mod.rs:list_transactions",
        "handler end",
        serde_json::json!({
            "elapsed_ms": started.elapsed().as_millis(),
            "ok": result.is_ok(),
        }),
    );
    // #endregion
    result.map(Json)
}

#[derive(Deserialize)]
struct ImportRequest {
    tx_hex: String,
    label: Option<String>,
    target_fee_rate: Option<f64>,
    network: Option<String>,
}

#[derive(Deserialize)]
struct ScheduleRequest {
    scheduled_time: Option<String>,
    min_delay_hours: Option<u64>,
    max_delay_hours: Option<u64>,
    min_fee_rate: Option<f64>,
    max_fee_rate: Option<f64>,
    fixed_fee_rate: Option<f64>,
    target_price: Option<f64>,
    price_currency: Option<String>,
    price_condition: Option<String>,
}

async fn import_transaction(
    State(state): State<AppState>,
    Json(req): Json<ImportRequest>,
) -> Result<(StatusCode, Json<BroadcastTx>), (StatusCode, String)> {
    let network = state.config.lock()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .network.network_type.data_dir_name()
        .to_string();

    let new_tx = NewBroadcastTx {
        tx_hex: req.tx_hex,
        network: req.network.unwrap_or(network),
        nlocktime: None,
        broadcast_mode: Some("imported".to_string()),
        scheduled_time: None,
        target_fee_rate: req.target_fee_rate,
        source_label: req.label,
        destination_address: None,
        utxo_count: Some(1),
        total_value_btc: None,
        replacement_of: None,
    };

    state
        .pool_manager
        .import_transaction(&new_tx)
        .map(|tx| (StatusCode::CREATED, Json(tx)))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn schedule_transaction(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<ScheduleRequest>,
) -> Result<Json<BroadcastTx>, (StatusCode, String)> {
    if let Some(target_price) = req.target_price {
        let currency = req.price_currency.as_deref().unwrap_or("usd");
        let condition = req.price_condition.as_deref().unwrap_or("above");
        let fee_rate = req.fixed_fee_rate.unwrap_or(5.0);
        return state
            .pool_manager
            .schedule_by_price(&id, target_price, currency, condition, fee_rate)
            .map(Json)
            .map_err(|e| {
                let msg = e.to_string();
                if msg.contains("must be") || msg.contains("only available") || msg.contains("Cannot set") {
                    (StatusCode::BAD_REQUEST, msg)
                } else {
                    (StatusCode::INTERNAL_SERVER_ERROR, msg)
                }
            });
    }

    // If exact datetime provided, use it directly
    if let Some(ref time_str) = req.scheduled_time {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(time_str) {
            let scheduled = dt.with_timezone(&chrono::Utc);
            let fee_rate = req.fixed_fee_rate.unwrap_or(5.0);
            return state
                .pool_manager
                .schedule_at(&id, scheduled, fee_rate)
                .map(Json)
                .map_err(|e| {
                    let msg = e.to_string();
                    if msg.contains("must be in the future")
                        || msg.contains("cannot be before nLockTime")
                    {
                        (StatusCode::BAD_REQUEST, msg)
                    } else {
                        (StatusCode::INTERNAL_SERVER_ERROR, msg)
                    }
                });
        }
    }

    state
        .pool_manager
        .schedule_transaction(
            &id,
            req.min_delay_hours,
            req.max_delay_hours,
            req.min_fee_rate,
            req.max_fee_rate,
            req.fixed_fee_rate,
        )
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn get_transaction(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<BroadcastTx>, (StatusCode, String)> {
    let pool_manager = state.pool_manager.clone();
    tokio::task::spawn_blocking(move || pool_manager.get_transaction(&id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Task failed: {}", e)))?
        .map(Json)
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))
}

async fn remove_transaction(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let pool_manager = state.pool_manager.clone();
    tokio::task::spawn_blocking(move || pool_manager.remove_transaction(&id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Task failed: {}", e)))?
        .map(|_| StatusCode::OK)
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("not found") {
                (StatusCode::NOT_FOUND, msg)
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, msg)
            }
        })
}

async fn retry_transaction(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<BroadcastTx>, (StatusCode, String)> {
    state
        .pool_manager
        .retry_failed_transaction(&id)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

#[derive(serde::Serialize)]
struct StatusResponse {
    network: String,
    network_display: String,
    supported_networks: Vec<String>,
    rpc_connected: bool,
    electrum_connected: bool,
    indexer_height: Option<u64>,
    chain_mtp: Option<u64>,
    /// Which backend is serving the chain clock right now: `indexer`, `bitcoin_core` or `none`.
    chain_source: crate::pool::ChainSource,
    /// Tip height from whichever source is alive, so the UI keeps ticking on Core alone.
    chain_height: Option<u64>,
    /// `host:port` of the configured indexer, for the degraded-mode message.
    indexer_url: String,
    /// Indexer software name (`electrs`, `Fulcrum`); `None` until known.
    indexer_software: Option<String>,
    core_ibd: bool,
    core_sync_pct: Option<f64>,
    pool_stats: PoolStats,
    retain_by_default: bool,
    #[serde(alias = "sparrow_connect_url")]
    wallet_connect_url: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    indexer_status_hint: String,
    /// True when electrs is reachable and wallet URL is configured (Umbrel readiness).
    sparrow_ready: bool,
    sparrow_tor_warning: String,
    liana_vb_enabled: bool,
    liana_vb_target_height: u64,
    liana_vb_disarm_height: u64,
    /// True when a tampered row (row_mac mismatch) halted broadcasting globally; cleared only by
    /// an admin acknowledging via `/api/security/acknowledge`.
    safe_mode: bool,
    /// Ids of the tampered rows that triggered safe mode, for the dashboard banner.
    tampered_ids: Vec<String>,
    /// True when the archive encryption key is not currently cached in memory.
    archive_locked: bool,
    /// True once an archive password has been configured (salt+verifier persisted).
    archive_password_set: bool,
}

async fn security_acknowledge(State(state): State<AppState>) -> Json<serde_json::Value> {
    state.pool_manager.clear_safe_mode();
    Json(serde_json::json!({ "ok": true }))
}

#[derive(serde::Serialize, Deserialize)]
struct ConfigResponse {
    indexer_url: String,
    indexer_node_url: String,
    indexer_use_ssl: bool,
    indexer_is_manual: bool,
    network_editable: bool,
    umbrel_mode: bool,
    startos_mode: bool,
    network: String,
    broadcast_mode: String,
    default_delay_hours: u64,
    scheduled_datetime: Option<String>,
    liana_vb_enabled: bool,
    liana_vb_target_height: u64,
    liana_vb_armed_at_height: u64,
    min_delay_hours: u64,
    max_delay_hours: u64,
    min_fee_rate: f64,
    max_fee_rate: f64,
    web_port: u16,
    electrum_port: u16,
    electrum_host: String,
    #[serde(alias = "sparrow_connect_url")]
    wallet_connect_url: String,
    indexer_auto_detected: bool,
    network_changed: bool,
    supported_networks: Vec<String>,
}

async fn get_config(State(state): State<AppState>) -> Result<Json<ConfigResponse>, (StatusCode, String)> {
    let mut config = state
        .config
        .lock()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut reconnect = false;
    if crate::discovery::is_umbrel_mode() {
        if crate::discovery::sanitize_umbrel_indexer_config(&mut config) {
            reconnect = true;
            let _ = crate::discovery::save_config_to_disk(&config, state.db.key());
        }
    }
    let response = config_to_response(&config, false);
    drop(config);
    if reconnect {
        let pool_manager = state.pool_manager.clone();
        if let Err(e) = pool_manager.reconnect_indexer_from_config() {
            tracing::warn!("Could not reconnect indexer after auto-heal: {}", e);
        }
    }
    Ok(Json(response))
}

#[derive(Deserialize)]
struct SaveConfigRequest {
    network: Option<String>,
    broadcast_mode: Option<String>,
    default_delay_hours: Option<u64>,
    scheduled_datetime: Option<String>,
    min_delay_hours: Option<u64>,
    max_delay_hours: Option<u64>,
    min_fee_rate: Option<f64>,
    max_fee_rate: Option<f64>,
    liana_vb_enabled: Option<bool>,
    liana_vb_target_height: Option<u64>,
}

async fn save_config(
    State(state): State<AppState>,
    Json(req): Json<SaveConfigRequest>,
) -> Result<Json<ConfigResponse>, (StatusCode, String)> {
    tracing::info!("save_config called");
    // Read the live tip BEFORE taking the config lock: chain_health() locks a separate
    // RwLock, and reading it while holding the config mutex risks a borrow/lock conflict
    // (and, more importantly, checking order matters for correctness here).
    let real_height = state.pool_manager.chain_health().height;
    let mut config = state.config.lock().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    tracing::info!("Config lock acquired");

    let mut network_changed = false;
    let old_network = config.network.network_type.data_dir_name().to_string();
    let network_editable = !crate::discovery::is_umbrel_mode();

    if network_editable {
        if let Some(ref net) = req.network {
            let new_network = net.to_lowercase();
            if new_network != old_network {
                network_changed = true;
            }
            config.network.network_type = match new_network.as_str() {
                "mainnet" => crate::config::NetworkType::Mainnet,
                "testnet4" => crate::config::NetworkType::Testnet4,
                "signet" => crate::config::NetworkType::Signet,
                _ => config.network.network_type.clone(),
            };
        }
    }

    let indexer_updated = if network_changed {
        tracing::info!("Network changed — scanning LAN for indexer on new network");
        let found = crate::discovery::apply_indexer_discovery(&mut config);
        found && config.indexer.is_some()
    } else {
        false
    };
    if let Some(mode) = req.broadcast_mode {
        if let Ok(m) = mode.parse::<crate::config::BroadcastMode>() {
            config.schedule.broadcast_mode = m;
        }
    }
    if let Some(v) = req.default_delay_hours {
        config.schedule.default_delay_hours = v;
    }
    if let Some(v) = req.scheduled_datetime {
        config.schedule.scheduled_datetime = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = req.min_delay_hours {
        config.schedule.min_delay_hours = v;
    }
    if let Some(v) = req.max_delay_hours {
        config.schedule.max_delay_hours = v;
    }
    if let Some(v) = req.min_fee_rate {
        config.schedule.min_fee_rate = v;
    }
    if let Some(v) = req.max_fee_rate {
        config.schedule.max_fee_rate = v;
    }

    // Liana virtual block. Arming validates the height against the live tip and stamps armed_at.
    if let Some(target) = req.liana_vb_target_height {
        config.schedule.liana_virtual_block.target_height = target;
    }
    if let Some(enable) = req.liana_vb_enabled {
        if enable {
            let Some(armed_at) = real_height else {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "Altura de la cadena no disponible; reintenta en unos segundos.".into(),
                ));
            };
            validate_virtual_height(
                config.schedule.liana_virtual_block.target_height,
                real_height,
            )
            .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
            config.schedule.liana_virtual_block.armed_at_height = armed_at;
            config.schedule.liana_virtual_block.enabled = true;
            tracing::info!(
                "Liana virtual block armed: target={} armed_at={}",
                config.schedule.liana_virtual_block.target_height,
                config.schedule.liana_virtual_block.armed_at_height
            );
        } else {
            config.schedule.liana_virtual_block.enabled = false;
        }
    }

    tracing::info!("Config modified");

    crate::discovery::save_config_to_disk(&config, state.db.key())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("File write error: {}", e)))?;

    let response = config_to_response(&config, network_changed);
    let pool_manager = state.pool_manager.clone();

    // Release the config mutex BEFORE reconnecting: reconnect_indexer_from_config()
    // locks the same std::sync::Mutex, so calling it while this guard is held
    // self-deadlocks (non-reentrant) and freezes every task that later needs the
    // config lock — the whole tokio runtime stalls and Sparrow hangs on broadcast.
    drop(config);
    tracing::info!("Config lock dropped");

    if indexer_updated {
        if let Err(e) = pool_manager.reconnect_indexer_from_config() {
            tracing::warn!("Could not reconnect indexer after save: {}", e);
        }
    }

    Ok(Json(response))
}

async fn get_status(State(state): State<AppState>) -> Result<Json<StatusResponse>, (StatusCode, String)> {
    let pool_manager = state.pool_manager.clone();
    let config = state.config.clone();
    let mut reconnect = false;
    if let Ok(mut cfg) = state.config.lock() {
        if crate::discovery::sanitize_umbrel_indexer_config(&mut cfg) {
            let _ = crate::discovery::save_config_to_disk(&cfg, state.db.key());
            reconnect = true;
        }
    }
    if reconnect {
        if let Err(e) = pool_manager.reconnect_indexer_from_config() {
            tracing::warn!("Could not reconnect indexer after status heal: {}", e);
        }
    }

    // Everything below is a cache read: the health poller owns the probing, so /api/status stays
    // fast even when the indexer is unreachable (it used to block up to 8s on a dead socket).
    let result = tokio::task::spawn_blocking(move || {
        let stats = pool_manager.get_stats().map_err(|e| e.to_string())?;
        let health = pool_manager.chain_health();
        let cfg = config.lock().map_err(|e| e.to_string())?;
        let network = cfg.network.network_type.data_dir_name().to_string();
        let network_display = cfg.network.network_type.display_name().to_string();
        let wallet_url = wallet_connect_url(&cfg);
        let indexer_url = cfg
            .indexer
            .as_ref()
            .map(|i| crate::discovery::display_indexer_url(&i.url))
            .unwrap_or_default();
        let liana_vb_enabled = cfg.schedule.liana_virtual_block.enabled;
        let liana_vb_target_height = cfg.schedule.liana_virtual_block.target_height;
        let liana_vb_disarm_height = cfg.schedule.liana_virtual_block.armed_at_height + 10;
        Ok::<_, String>((
            stats,
            health,
            network,
            network_display,
            wallet_url,
            indexer_url,
            liana_vb_enabled,
            liana_vb_target_height,
            liana_vb_disarm_height,
        ))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Task failed: {}", e)))?;

    let (
        pool_stats,
        health,
        network,
        network_display,
        wallet_url,
        indexer_url,
        liana_vb_enabled,
        liana_vb_target_height,
        liana_vb_disarm_height,
    ) = result.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let electrum_connected = health.indexer_up;

    let indexer_status_hint = {
        let cfg = state.config.lock().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        crate::discovery::umbrel_indexer_status_hint(&cfg, electrum_connected)
    };

    Ok(Json(StatusResponse {
        network,
        network_display,
        supported_networks: supported_networks_vec(),
        rpc_connected: health.core_up,
        electrum_connected,
        indexer_height: if electrum_connected { health.height } else { None },
        chain_mtp: health.mtp,
        chain_source: health.source,
        chain_height: health.height,
        indexer_url,
        indexer_software: health.indexer_software.clone(),
        core_ibd: health.core_ibd,
        core_sync_pct: health.core_sync_pct,
        pool_stats,
        retain_by_default: true,
        wallet_connect_url: wallet_url.clone(),
        indexer_status_hint,
        // Wallet-side readiness only (is the connect URL usable). A dead indexer is an
        // infrastructure fault, not a Sparrow misconfiguration, and it now has its own banner:
        // folding it in here told the user to go fix the wallet, which is not where the fault is.
        sparrow_ready: !wallet_url.is_empty() && !wallet_url.contains('<'),
        sparrow_tor_warning: "Disable Sparrow Settings→Network proxy/Tor or broadcasts bypass this pool (mempool.space). Use tcp://LAN:50050 only.".into(),
        liana_vb_enabled,
        liana_vb_target_height,
        liana_vb_disarm_height,
        safe_mode: state.pool_manager.is_safe_mode(),
        tampered_ids: state.pool_manager.tampered_ids(),
        archive_locked: !state.archive_keys.is_unlocked(),
        archive_password_set: state.archive_keys.password_is_set(&state.db).unwrap_or(false),
    }))
}

#[derive(Deserialize)]
struct EstimateFeeRequest {
    tx_hex: String,
}

#[derive(Serialize)]
struct EstimateFeeResponse {
    fee_rate: f64,
    fee_sat: u64,
    vsize: usize,
}

async fn estimate_fee(
    State(state): State<AppState>,
    Json(req): Json<EstimateFeeRequest>,
) -> Result<Json<EstimateFeeResponse>, (StatusCode, String)> {
    // Use spawn_blocking for the synchronous indexer call
    let config_clone = state.config.lock().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?.clone();
    let tx_hex = req.tx_hex.clone();

    let result = tokio::task::spawn_blocking(move || {
        if let Some(ref indexer) = config_clone.indexer {
            if let Ok(electrum) = crate::rpc::ElectrumClient::new(&indexer.url) {
                match electrum.calculate_tx_fee(&tx_hex) {
                    Ok((fee_rate, fee, vsize)) => {
                        return Ok(EstimateFeeResponse { fee_rate, fee_sat: fee, vsize });
                    }
                    Err(e) => {
                        tracing::warn!("Fee estimation failed: {}", e);
                    }
                }
            }
        }

        // Fallback: estimate from TX size
        if let Ok(raw) = hex::decode(&tx_hex) {
            let tx_size = raw.len();
            let vsize = tx_size * 3 / 4;
            return Ok(EstimateFeeResponse {
                fee_rate: 0.0,
                fee_sat: 0,
                vsize,
            });
        }

        Err("Invalid transaction hex".to_string())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    result.map(Json).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

async fn get_indexer_debug(
    State(state): State<AppState>,
) -> Result<Json<crate::discovery::UmbrelIndexerDiagnostics>, (StatusCode, String)> {
    let pool_manager = state.pool_manager.clone();
    let config = state.config.clone();
    let diagnostics = tokio::task::spawn_blocking(move || {
        // Diagnostics are user-triggered, so probe for real rather than trusting the snapshot.
        pool_manager.refresh_chain_health();
        let connected = pool_manager.chain_health().indexer_up;
        let cfg = config.lock().map_err(|e| e.to_string())?;
        Ok(crate::discovery::umbrel_indexer_diagnostics(&cfg, connected))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Task failed: {}", e)))?
    .map_err(|e: String| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(diagnostics))
}

#[derive(Deserialize)]
struct ArchivePasswordRequest {
    password: String,
}

async fn archive_set_password(
    State(state): State<AppState>,
    Json(req): Json<ArchivePasswordRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let already_set = state
        .archive_keys
        .password_is_set(&state.db)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if already_set {
        return Err((
            StatusCode::CONFLICT,
            "archive password is already set".to_string(),
        ));
    }
    state
        .archive_keys
        .set_password(&state.db, &req.password)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn archive_unlock(
    State(state): State<AppState>,
    Json(req): Json<ArchivePasswordRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let is_set = state
        .archive_keys
        .password_is_set(&state.db)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if !is_set {
        return Err((
            StatusCode::BAD_REQUEST,
            "no archive password set".to_string(),
        ));
    }
    let unlocked = state
        .archive_keys
        .unlock(&state.db, &req.password)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "unlocked": unlocked })))
}

async fn archive_lock(State(state): State<AppState>) -> Json<serde_json::Value> {
    state.archive_keys.lock();
    Json(serde_json::json!({ "ok": true }))
}

#[derive(Deserialize)]
struct ArchiveListQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn list_archive(
    State(state): State<AppState>,
    Query(q): Query<ArchiveListQuery>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    // Use key() rather than is_unlocked() here: listing the archive is a genuine
    // archive operation and should count as activity that slides the auto-lock TTL,
    // unlike passive status polling.
    if state.archive_keys.key().is_none() {
        return Ok((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "locked": true })),
        )
            .into_response());
    }

    let network = state
        .config
        .lock()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .network
        .network_type
        .data_dir_name()
        .to_string();

    let limit = q.limit.unwrap_or(50);
    let offset = q.offset.unwrap_or(0);

    let items = state
        .db
        .list_archive(&network, limit, offset)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(items).into_response())
}

async fn get_archive_item(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let Some(key) = state.archive_keys.key() else {
        return Ok((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "locked": true })),
        )
            .into_response());
    };

    let blob = state
        .db
        .get_archive_blob(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(blob) = blob else {
        return Err((StatusCode::NOT_FOUND, "archive item not found".to_string()));
    };

    let plaintext = crate::crypto::open(&key, &blob, id.as_bytes())
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to decrypt archive item".to_string()))?;
    let value: serde_json::Value = serde_json::from_slice(&plaintext)
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to parse archive item".to_string()))?;

    Ok(Json(value).into_response())
}

/// Manually flush terminal (confirmed/failed/broadcast) transactions to the encrypted
/// archive right now, without waiting for the automatic retention window. Requires the
/// archive to be unlocked. The automatic 30-day retention still runs independently.
async fn archive_flush(
    State(state): State<AppState>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let Some(key) = state.archive_keys.key() else {
        return Ok((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "locked": true })),
        )
            .into_response());
    };

    let network = state
        .config
        .lock()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .network
        .network_type
        .data_dir_name()
        .to_string();

    // cutoff = now → every terminal row (updated_at strictly in the past) is archived.
    let now = chrono::Utc::now().to_rfc3339();
    let archived = state
        .db
        .archive_terminal_older_than(&key, &network, &now)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(serde_json::json!({ "archived": archived })).into_response())
}

async fn restart_daemon() -> impl IntoResponse {
    let handle = tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        tracing::info!("Daemon restart triggered");
        std::process::exit(0);
    });
    let _ = handle.await;
    "Restarting"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::pool::manager::PoolManager;

    fn test_state() -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Arc::new(Database::open(&dir.path().join("test.db")).expect("db open"));
        let config = Arc::new(std::sync::Mutex::new(Config::default_config()));
        let pool_manager = Arc::new(PoolManager::new(db.clone(), None, None, config.clone()));
        let archive_keys = Arc::new(ArchiveKeyStore::new());
        (
            AppState {
                pool_manager,
                db,
                config,
                archive_keys,
            },
            dir,
        )
    }

    #[tokio::test]
    async fn list_archive_returns_locked_when_no_key_cached() {
        let (state, _dir) = test_state();
        let resp = list_archive(State(state), Query(ArchiveListQuery { limit: None, offset: None }))
            .await
            .expect("handler ok")
            .into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn get_archive_item_returns_locked_when_no_key_cached() {
        let (state, _dir) = test_state();
        let resp = get_archive_item(State(state), axum::extract::Path("some-id".to_string()))
            .await
            .expect("handler ok")
            .into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn archive_unlock_without_password_set_returns_bad_request() {
        let (state, _dir) = test_state();
        let err = archive_unlock(
            State(state),
            Json(ArchivePasswordRequest {
                password: "whatever".to_string(),
            }),
        )
        .await
        .expect_err("should reject when no password configured");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn set_password_then_unlock_then_lock_flow() {
        let (state, _dir) = test_state();
        let ok = archive_set_password(
            State(state.clone()),
            Json(ArchivePasswordRequest {
                password: "hunter2".to_string(),
            }),
        )
        .await
        .expect("set-password ok");
        assert_eq!(ok.0["ok"], serde_json::json!(true));

        // Setting again should now be rejected (v1: only-set-if-not-exists).
        let err = archive_set_password(
            State(state.clone()),
            Json(ArchivePasswordRequest {
                password: "other".to_string(),
            }),
        )
        .await
        .expect_err("second set-password should be rejected");
        assert_eq!(err.0, StatusCode::CONFLICT);

        let wrong = archive_unlock(
            State(state.clone()),
            Json(ArchivePasswordRequest {
                password: "wrong".to_string(),
            }),
        )
        .await
        .expect("unlock handler ok even on wrong password");
        assert_eq!(wrong.0["unlocked"], serde_json::json!(false));

        let right = archive_unlock(
            State(state.clone()),
            Json(ArchivePasswordRequest {
                password: "hunter2".to_string(),
            }),
        )
        .await
        .expect("unlock handler ok");
        assert_eq!(right.0["unlocked"], serde_json::json!(true));
        assert!(state.archive_keys.is_unlocked());

        let _ = archive_lock(State(state.clone())).await;
        assert!(!state.archive_keys.is_unlocked());
    }

    #[tokio::test]
    async fn archive_flush_requires_unlock_then_archives_terminal() {
        let (state, _dir) = test_state();

        // Locked → 401.
        let resp = archive_flush(State(state.clone())).await.expect("ok").into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // Set password + unlock.
        let _ = archive_set_password(
            State(state.clone()),
            Json(ArchivePasswordRequest { password: "pw".to_string() }),
        )
        .await
        .expect("set-password");
        let _ = archive_unlock(
            State(state.clone()),
            Json(ArchivePasswordRequest { password: "pw".to_string() }),
        )
        .await
        .expect("unlock");

        // Insert a terminal (confirmed) tx on the configured network.
        let network = state
            .config
            .lock()
            .unwrap()
            .network
            .network_type
            .data_dir_name()
            .to_string();
        let new_tx = crate::db::models::NewBroadcastTx {
            tx_hex: "00".to_string(), network: network.clone(), nlocktime: None,
            broadcast_mode: Some("scheduled".to_string()), scheduled_time: None,
            target_fee_rate: None, source_label: None, destination_address: None,
            utxo_count: Some(1), total_value_btc: None, replacement_of: None,
        };
        let stored = state.db.insert_broadcast_tx(&new_tx).expect("insert");
        state
            .db
            .update_tx_status(&stored.id, crate::db::models::TxStatus::Confirmed, None)
            .expect("confirm");

        // Manual flush now → the terminal tx moves to the archive immediately.
        let resp = archive_flush(State(state.clone())).await.expect("ok").into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(state.db.get_broadcast_tx_by_id(&stored.id).is_err(), "gone from active pool");
        assert_eq!(state.db.list_archive(&network, 10, 0).expect("list").len(), 1);
    }

    #[test]
    fn virtual_height_must_be_future() {
        assert!(validate_virtual_height(0, Some(100)).is_err()); // unset
        assert!(validate_virtual_height(100, Some(100)).is_err()); // equal to tip
        assert!(validate_virtual_height(90, Some(100)).is_err()); // below tip
        assert!(validate_virtual_height(150, Some(100)).is_ok()); // future
        assert!(validate_virtual_height(150, None).is_ok()); // no height known → allow
    }

    #[test]
    fn virtual_height_gap_is_capped() {
        let real = 100u64;
        // Within the allowed gap → Ok.
        assert!(validate_virtual_height(real + MAX_VIRTUAL_BLOCK_GAP, Some(real)).is_ok());
        // Just beyond the allowed gap → Err.
        assert!(validate_virtual_height(real + MAX_VIRTUAL_BLOCK_GAP + 1, Some(real)).is_err());
        // Absurdly far beyond → Err.
        assert!(validate_virtual_height(real + 10_000_000, Some(real)).is_err());
        // No real height known → gap check doesn't apply (existing None => Ok behaviour).
        assert!(validate_virtual_height(real + 10_000_000, None).is_ok());
    }
}
