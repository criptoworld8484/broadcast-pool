use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rand::Rng;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::db::models::*;
use crate::db::Database;
use crate::pool::chain_health::{decide_chain_source, ChainHealth, ChainSource};
use crate::price::PriceFeed;
use crate::rpc::BitcoinRpc;
use crate::rpc::ElectrumClient;

/// Error emitted by the scheduler tick when neither the indexer nor Bitcoin Core can serve a
/// block height. Matched by the broadcast loop to decide whether to back off.
pub const NO_CHAIN_SOURCE: &str = "no chain data source available";

#[derive(Clone)]
pub struct PendingTxOutput {
    pub output_index: u32,
    pub value: u64, // satoshis
    pub scripthash: String,
}

#[derive(Clone)]
pub struct PendingTxInfo {
    pub tx_hex: String,
    pub scripthashes: Vec<String>,
    pub outputs: Vec<PendingTxOutput>,
}

/// Notification sent when scripthash status changes in the virtual mempool.
#[derive(Clone, Debug)]
pub struct ScripthashNotification {
    pub scripthash: String,
}

pub struct PoolManager {
    db: Arc<Database>,
    rpc: Option<Arc<BitcoinRpc>>,
    indexer: Mutex<Option<Arc<ElectrumClient>>>,
    config: Arc<Mutex<Config>>,
    pending_txs: Arc<Mutex<HashMap<String, PendingTxInfo>>>,
    mtp_cache: Arc<Mutex<Option<(Instant, u64)>>>,
    /// Last successful `blockchain.headers.subscribe` from electrs (height, header hex).
    cached_chain_tip: Arc<Mutex<Option<(u64, String)>>>,
    /// Which backend the chain clock is reading from. Refreshed by the health poller, read
    /// (without I/O) by the scheduler gates and `/api/status`.
    chain_health: Arc<RwLock<ChainHealth>>,
    /// Keeps concurrent `refresh_chain_health()` calls from stacking probes.
    health_refresh_in_flight: Arc<AtomicBool>,
    price_feed: PriceFeed,
    /// Broadcast channel for scripthash state changes (virtual mempool add/remove).
    scripthash_notifications: tokio::sync::broadcast::Sender<ScripthashNotification>,
}

impl PoolManager {
    pub fn new(
        db: Arc<Database>,
        rpc: Option<Arc<BitcoinRpc>>,
        indexer: Option<Arc<ElectrumClient>>,
        config: Arc<Mutex<Config>>,
    ) -> Self {
        let (scripthash_notifications, _) = tokio::sync::broadcast::channel(256);
        Self {
            db,
            rpc,
            indexer: Mutex::new(indexer),
            config,
            pending_txs: Arc::new(Mutex::new(HashMap::new())),
            mtp_cache: Arc::new(Mutex::new(None)),
            cached_chain_tip: Arc::new(Mutex::new(None)),
            chain_health: Arc::new(RwLock::new(ChainHealth::default())),
            health_refresh_in_flight: Arc::new(AtomicBool::new(false)),
            price_feed: PriceFeed::new(),
            scripthash_notifications,
        }
    }

    /// Subscribe to scripthash state change notifications.
    pub fn subscribe_scripthash_changes(&self) -> tokio::sync::broadcast::Receiver<ScripthashNotification> {
        self.scripthash_notifications.subscribe()
    }

    pub fn cache_chain_tip(&self, height: u64, hex: String) {
        if height == 0 || hex.is_empty() {
            return;
        }
        if let Ok(mut cache) = self.cached_chain_tip.lock() {
            *cache = Some((height, hex));
        }
    }

    pub fn get_cached_chain_tip(&self) -> Option<(u64, String)> {
        self.cached_chain_tip.lock().ok()?.clone()
    }

    fn require_rpc(&self) -> Result<&BitcoinRpc> {
        self.rpc.as_ref().map(|r| r.as_ref()).context("RPC not available - this command requires a Bitcoin Core connection")
    }

    pub fn rpc_available(&self) -> bool {
        self.rpc.is_some() || self.get_indexer().is_some()
    }

    pub fn get_db(&self) -> &Arc<Database> {
        &self.db
    }

    pub fn schedule_at(&self, id: &str, scheduled_time: DateTime<Utc>, fee_rate: f64) -> Result<BroadcastTx> {
        let tx = self.db.get_broadcast_tx_by_id(id)?;

        let now = Utc::now();
        if scheduled_time <= now {
            anyhow::bail!("Scheduled time must be in the future");
        }

        if let Some(n) = tx.nlocktime.filter(|&n| n > 500_000_000) {
            let sched_unix = scheduled_time.timestamp().max(0) as u64;
            if sched_unix < n {
                anyhow::bail!(
                    "Scheduled time cannot be before nLockTime (unix {}). The network will only accept this transaction when chain MTP reaches the signed nLockTime.",
                    n
                );
            }
        }

        let scheduled_str = scheduled_time.to_rfc3339();
        let is_reschedule = tx.broadcast_missed_at.is_some()
            || tx.defer_until.is_some()
            || tx.scheduled_time.is_some()
            || tx.schedule_trigger.as_deref() == Some("price")
            || tx.broadcast_mode.as_deref() == Some("scheduled")
            || tx.broadcast_mode.as_deref() == Some("manual");

        let defer_until = if is_reschedule {
            Some(scheduled_str.as_str())
        } else {
            None
        };

        self.db
            .update_reschedule(id, &scheduled_str, defer_until, fee_rate)?;

        let mut updated = self.db.get_broadcast_tx_by_id(id)?;
        self.enrich_tx_locktime(&mut updated);
        Ok(updated)
    }

    pub fn schedule_by_price(
        &self,
        id: &str,
        target_price: f64,
        price_currency: &str,
        price_condition: &str,
        fee_rate: f64,
    ) -> Result<BroadcastTx> {
        if target_price <= 0.0 {
            anyhow::bail!("Target price must be positive");
        }
        let currency = match price_currency.to_lowercase().as_str() {
            "eur" | "usd" => price_currency.to_lowercase(),
            _ => anyhow::bail!("price_currency must be eur or usd"),
        };
        let condition = match price_condition.to_lowercase().as_str() {
            "above" | "below" => price_condition.to_lowercase(),
            _ => anyhow::bail!("price_condition must be above or below"),
        };

        let tx = self.db.get_broadcast_tx_by_id(id)?;
        if !matches!(tx.status, TxStatus::Pending | TxStatus::Scheduled) {
            anyhow::bail!("Cannot set price trigger on transaction in status {}", tx.status.as_str());
        }
        if tx.broadcast_mode.as_deref() != Some("manual") {
            anyhow::bail!("Price trigger scheduling is only available for manual (pending) transactions");
        }
        if tx.nlocktime.is_some_and(|n| n > 0) {
            anyhow::bail!("Price trigger scheduling is only available when nLockTime is disabled (0)");
        }

        self.db
            .update_price_schedule(id, target_price, &currency, &condition, fee_rate)?;

        let mut updated = self.db.get_broadcast_tx_by_id(id)?;
        self.enrich_tx_locktime(&mut updated);
        tracing::info!(
            "Transaction {} waiting for BTC {} {} {:.2}",
            id,
            condition,
            currency.to_uppercase(),
            target_price
        );
        Ok(updated)
    }

    pub fn check_price_triggers(&self, prices: &std::collections::HashMap<String, f64>) -> Result<usize> {
        let network = {
            let config = self.config.lock().map_err(|e| anyhow::anyhow!("Config lock failed: {}", e))?;
            config.network.network_type.data_dir_name().to_string()
        };

        let mut triggered = 0;
        for tx in self.db.get_price_triggered_pending(&network)? {
            let currency = tx.price_currency.as_deref().unwrap_or("usd");
            let target = match tx.target_price {
                Some(t) => t,
                None => continue,
            };
            let condition = tx.price_condition.as_deref().unwrap_or("above");
            let current = match prices.get(currency) {
                Some(p) => *p,
                None => continue,
            };

            if PriceFeed::price_condition_met(current, target, condition) {
                tracing::info!(
                    "Price trigger met for {} (BTC/{} = {:.2}, target {} {:.2})",
                    tx.id,
                    currency.to_uppercase(),
                    current,
                    condition,
                    target
                );
                if let Err(e) = self.mark_due_from_price_trigger(&tx.id) {
                    tracing::error!("Failed to mark price-triggered tx {} as due: {}", tx.id, e);
                } else {
                    triggered += 1;
                }
            }
        }
        Ok(triggered)
    }

    pub fn price_feed(&self) -> &PriceFeed {
        &self.price_feed
    }

    pub fn broadcast_transaction(&self, tx_hex: &str) -> Result<String> {
        let mut indexer_err = None;
        // A broadcast error does not imply the indexer is down (it may be alive and rejecting the
        // tx as non-final or duplicate), so this only skips an already-known-dead one — it never
        // demotes the indexer on its own.
        if self.chain_health().should_try_indexer() {
            if let Some(indexer) = self.get_indexer() {
                match indexer.broadcast_transaction(tx_hex) {
                    Ok(txid) => return Ok(txid),
                    Err(e) => {
                        tracing::warn!("Indexer broadcast failed: {}, trying RPC...", e);
                        indexer_err = Some(e);
                    }
                }
            }
        }
        if let Some(ref rpc) = self.rpc {
            match rpc.broadcast_transaction(tx_hex) {
                Ok(txid) => return Ok(txid),
                Err(e) => {
                    tracing::warn!("Core RPC broadcast failed: {}, trying secondary indexer...", e);
                    indexer_err = indexer_err.or(Some(e));
                }
            }
        }
        // Last resort: a secondary indexer on the LAN.
        let secondary_url = {
            let cfg = self.config.lock().ok();
            cfg.and_then(|c| c.secondary_indexer.clone())
        };
        if let Some(raw) = secondary_url {
            let url = crate::discovery::normalize_indexer_url(&raw);
            if let Ok(client) = ElectrumClient::new(&url) {
                match client.broadcast_transaction(tx_hex) {
                    Ok(txid) => {
                        tracing::info!("Broadcast via secondary indexer {}", url);
                        return Ok(txid);
                    }
                    Err(e) => tracing::warn!("Secondary indexer broadcast failed: {}", e),
                }
            }
        }
        if let Some(e) = indexer_err {
            return Err(e);
        }
        anyhow::bail!("No broadcast backend available (indexer, Core, nor secondary)")
    }

    pub fn import_transaction(&self, new_tx: &NewBroadcastTx) -> Result<BroadcastTx> {
        let tx = self.db.insert_broadcast_tx(new_tx)?;
        tracing::info!(
            "Imported transaction {} into broadcast pool (network: {}, status: {})",
            tx.id,
            tx.network,
            tx.status.as_str()
        );
        Ok(tx)
    }

    pub fn mark_broadcast(&self, id: &str, txid: &str, fee_rate: f64) -> Result<()> {
        self.db.mark_broadcast(id, txid, fee_rate)?;
        self.remove_pending_tx(txid);
        Ok(())
    }

    pub fn has_pending_tx(&self, txid: &str) -> bool {
        self.get_pending_tx_hex(txid).is_some()
    }

    pub fn has_pending_for_scripthash(&self, scripthash: &str) -> bool {
        !self.get_pending_txids_for_scripthash(scripthash).is_empty()
    }

    /// Rehydrate in-memory virtual mempool from DB pending/scheduled txs (survives restarts)
    pub fn load_pending_from_db(&self) -> Result<usize> {
        let network = {
            let config = self
                .config
                .lock()
                .map_err(|e| anyhow::anyhow!("Config lock failed: {}", e))?;
            config.network.network_type.data_dir_name().to_string()
        };

        let indexer_url = self
            .get_indexer_url()
            .context("No indexer configured for pending tx rehydration")?;
        let indexer_addr = super::virtual_mempool::strip_indexer_host(&indexer_url);

        let pending = self.db.list_broadcast_txs(Some("pending"), &network, 10_000)?;
        let scheduled = self.db.list_broadcast_txs(Some("scheduled"), &network, 10_000)?;

        let mut count = 0;
        for tx in pending.into_iter().chain(scheduled) {
            let txid = match tx.txid {
                Some(ref id) => id.clone(),
                None => match super::virtual_mempool::compute_txid(&tx.tx_hex) {
                    Ok(id) => id,
                    Err(e) => {
                        tracing::warn!("Skipping rehydrate for pool tx {}: {}", tx.id, e);
                        continue;
                    }
                },
            };

            match super::virtual_mempool::extract_affected_scripthashes(
                &tx.tx_hex,
                &indexer_addr,
            ) {
                Ok(scripthashes) => {
                    let outputs = super::virtual_mempool::extract_outputs(&tx.tx_hex)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(output_index, value, scripthash)| PendingTxOutput {
                            output_index,
                            value,
                            scripthash,
                        })
                        .collect();
                    self.store_pending_tx(&txid, &tx.tx_hex, scripthashes, outputs);
                    count += 1;
                }
                Err(e) => {
                    tracing::warn!("Failed to rehydrate pending tx {}: {}", txid, e);
                }
            }
        }

        if count > 0 {
            tracing::info!("Rehydrated {} pending/scheduled txs into virtual mempool", count);
        }
        Ok(count)
    }

    pub fn get_tx_hex_by_txid(&self, txid: &str) -> Result<Option<String>> {
        self.db.get_tx_hex_by_txid(txid)
    }

    pub fn schedule_transaction(
        &self,
        id: &str,
        min_delay_hours: Option<u64>,
        max_delay_hours: Option<u64>,
        min_fee_rate: Option<f64>,
        max_fee_rate: Option<f64>,
        fixed_fee_rate: Option<f64>,
    ) -> Result<BroadcastTx> {
        let config = self.config.lock().map_err(|e| anyhow::anyhow!("Config lock failed: {}", e))?;
        let min_delay = min_delay_hours.unwrap_or(config.schedule.min_delay_hours as u64);
        let max_delay_val = max_delay_hours.unwrap_or(config.schedule.max_delay_hours as u64);
        let min_fee = min_fee_rate.unwrap_or(config.schedule.min_fee_rate);
        let max_fee = max_fee_rate.unwrap_or(config.schedule.max_fee_rate);
        drop(config);

        let mut rng = rand::thread_rng();
        let delay_hours = rng.gen_range(min_delay..=max_delay_val.max(min_delay));
        let fee_rate = if let Some(fixed) = fixed_fee_rate {
            fixed
        } else {
            rng.gen_range(min_fee..=max_fee.max(min_fee))
        };

        let scheduled_time = Utc::now()
            .checked_add_signed(chrono::Duration::hours(delay_hours as i64))
            .context("Failed to calculate scheduled time")?;

        let scheduled_time_str = scheduled_time.to_rfc3339();
        let now = Utc::now().to_rfc3339();

        self.db.execute_raw(
            "UPDATE broadcast_pool SET status = 'scheduled', scheduled_time = ?1, target_fee_rate = ?2, updated_at = ?3 WHERE id = ?4",
            &[&scheduled_time_str.as_str(), &fee_rate.to_string().as_str(), &now.as_str(), &id],
        )?;

        tracing::info!(
            "Scheduled transaction {} for {} (fee: {:.2} sat/vB, delay: {}h)",
            id,
            scheduled_time.format("%Y-%m-%d %H:%M UTC"),
            fee_rate,
            delay_hours
        );

        self.db.get_broadcast_tx_by_id(id)
    }

    pub fn schedule_all_pending(&self, network: &str) -> Result<Vec<BroadcastTx>> {
        let pending_txs = self
            .db
            .list_broadcast_txs(Some("pending"), network, 1000)?;

        let mut scheduled = Vec::new();
        for tx in pending_txs {
            let scheduled_tx = self.schedule_transaction(&tx.id, None, None, None, None, None)?;
            scheduled.push(scheduled_tx);
        }

        tracing::info!(
            "Scheduled {} pending transactions on {}",
            scheduled.len(),
            network
        );
        Ok(scheduled)
    }

    pub fn broadcast_due_transactions(&self) -> Result<Vec<(String, Result<String>)>> {
        let network = {
            let config = self.config.lock().map_err(|e| anyhow::anyhow!("Config lock failed: {}", e))?;
            config.network.network_type.data_dir_name().to_string()
        };
        let now = Utc::now();
        let candidate_txs = self.db.get_due_transactions(&network)?;
        let due_txs: Vec<BroadcastTx> = candidate_txs
            .into_iter()
            .filter(|tx| self.is_tx_due_for_broadcast(tx, now).unwrap_or(false))
            .collect();
        let mut results = Vec::new();

        for tx in due_txs {
            if !self.is_locktime_satisfied(tx.nlocktime)? {
                if tx.nlocktime.is_some_and(|n| n > 500_000_000) {
                    if tx.broadcast_missed_at.is_none() {
                        let missed_at = now.to_rfc3339();
                        let original = tx.scheduled_time.map(|t| t.to_rfc3339());
                        if let Err(e) = self.db.record_broadcast_miss(
                            &tx.id,
                            &missed_at,
                            original.as_deref(),
                        ) {
                            tracing::warn!("Failed to record broadcast miss for {}: {}", tx.id, e);
                        }
                    }
                    if let Ok(mtp) = self.get_median_time_past_cached() {
                        let remaining = tx.nlocktime.unwrap_or(0).saturating_sub(mtp);
                        tracing::info!(
                            "Tx {} missed scheduled broadcast — chain MTP not ready (nLockTime={}, chain MTP={}, ~{}s remaining)",
                            tx.id,
                            tx.nlocktime.unwrap_or(0),
                            mtp,
                            remaining
                        );
                    } else {
                        tracing::info!(
                            "Tx {} missed scheduled broadcast — chain MTP not ready (nLockTime={:?})",
                            tx.id,
                            tx.nlocktime
                        );
                    }
                } else if let Some(n) = tx.nlocktime.filter(|&n| n > 0 && n <= 500_000_000) {
                    tracing::info!(
                        "Tx {} waiting for block height locktime (nLockTime={})",
                        tx.id,
                        n
                    );
                } else {
                    tracing::info!(
                        "Tx {} waiting for chain locktime (nLockTime={:?})",
                        tx.id,
                        tx.nlocktime
                    );
                }
                continue;
            }

            tracing::info!("Broadcasting due transaction {}", tx.id);

            match self.broadcast_transaction(&tx.tx_hex) {
                Ok(txid) => {
                    let fee_rate = tx.target_fee_rate.unwrap_or(0.0);
                    self.mark_broadcast(&tx.id, &txid, fee_rate)?;
                    tracing::info!("Successfully broadcast {} (txid: {})", tx.id, txid);
                    results.push((tx.id, Ok(txid)));
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    if is_retriable_broadcast_error(&err_msg) {
                        tracing::info!(
                            "Broadcast deferred for {} (will retry): {}",
                            tx.id,
                            err_msg
                        );
                        results.push((tx.id.clone(), Err(e)));
                        continue;
                    }
                    self.db.update_tx_status(&tx.id, TxStatus::Failed, Some(&err_msg))?;
                    tracing::error!("Failed to broadcast {}: {}", tx.id, err_msg);
                    results.push((tx.id, Err(e)));
                }
            }
        }

        Ok(results)
    }

    fn is_tx_due_for_broadcast(&self, tx: &BroadcastTx, now: DateTime<Utc>) -> Result<bool> {
        // Waiting for fiat price condition (monitor marks due separately).
        if tx.schedule_trigger.as_deref() == Some("price")
            && matches!(tx.status, TxStatus::Pending)
        {
            return Ok(false);
        }

        let locktime_ok = self.is_locktime_satisfied(tx.nlocktime)?;

        if let Some(defer_until) = tx.defer_until {
            if now < defer_until {
                return Ok(false);
            }
            return Ok(locktime_ok);
        }

        if tx
            .scheduled_time
            .as_ref()
            .is_some_and(|t| *t <= now)
        {
            return Ok(locktime_ok);
        }

        Ok(false)
    }

    pub fn requeue_retriable_failures(&self) -> Result<usize> {
        let network = {
            let config = self.config.lock().map_err(|e| anyhow::anyhow!("Config lock failed: {}", e))?;
            config.network.network_type.data_dir_name().to_string()
        };
        let failed = self.db.list_broadcast_txs(Some("failed"), &network, 1000)?;
        let mut count = 0;
        for tx in failed {
            let err = tx.error_message.as_deref().unwrap_or("");
            let retriable = is_retriable_broadcast_error(err)
                || (tx.nlocktime.is_some_and(|n| n > 0) && err.contains("Failed to broadcast"));
            if retriable {
                self.db.reset_failed_to_scheduled(&tx.id)?;
                count += 1;
                tracing::info!("Requeued failed tx {} for retry", tx.id);
            }
        }
        Ok(count)
    }

    pub fn is_locktime_satisfied(&self, nlocktime: Option<u64>) -> Result<bool> {
        let nlocktime = match nlocktime {
            Some(n) if n > 0 => n,
            _ => return Ok(true),
        };

        if nlocktime > 500_000_000 {
            let mtp = self.get_median_time_past_cached()?;
            Ok(mtp >= nlocktime)
        } else {
            match self.check_block_height()? {
                Some(height) => Ok(height >= nlocktime),
                None => Ok(false),
            }
        }
    }

    /// One scheduler tick: mark pending due, requeue failures, broadcast scheduled txs.
    pub fn run_scheduler_tick(&self) -> Result<Vec<(String, Result<String>)>> {
        // #region agent log
        crate::utils::debug_log::agent_log(
            "H5",
            "pool/manager.rs:run_scheduler_tick",
            "tick start",
            serde_json::json!({}),
        );
        // #endregion
        // The scheduler needs a height and an MTP, not the address index — so Bitcoin Core alone
        // is enough to keep honouring schedules while electrs/Fulcrum is down.
        if !self.chain_clock_available() {
            anyhow::bail!(NO_CHAIN_SOURCE);
        }

        let network = {
            let config = self.config.lock().map_err(|e| anyhow::anyhow!("Config lock failed: {}", e))?;
            config.network.network_type.data_dir_name().to_string()
        };

        for tx in self.get_pending_by_scheduled_time(&network)? {
            if !matches!(
                tx.broadcast_mode.as_deref(),
                Some("scheduled") | Some("manual")
            ) {
                continue;
            }
            if tx.schedule_trigger.as_deref() == Some("price") {
                continue;
            }
            tracing::info!("Pending tx {} has scheduled_time reached, marking as due", tx.id);
            if let Err(e) = self.mark_as_due(&tx.id) {
                tracing::error!("Failed to mark {} as due: {}", tx.id, e);
            }
        }

        if let Err(e) = self.requeue_retriable_failures() {
            tracing::warn!("Failed to requeue retriable failures: {}", e);
        }

        self.broadcast_due_transactions()
    }

    fn get_median_time_past(&self) -> Result<u64> {
        // Core's `mediantime` is the authoritative BIP-113 value, so it wins whenever the node
        // answers. No pre-flight `test_connection()`: that was a second getblockchaininfo per call.
        if let Some(ref rpc) = self.rpc {
            match rpc.get_median_time() {
                Ok(mtp) => return Ok(mtp),
                Err(e) => tracing::debug!("RPC median time failed: {}", e),
            }
        }
        if self.chain_health().should_try_indexer() {
            if let Some(indexer) = self.get_indexer() {
                match indexer.get_median_time_past() {
                    Ok(mtp) => return Ok(mtp),
                    Err(e) => {
                        tracing::debug!("Indexer median time failed: {}", e);
                        self.note_indexer_failure();
                    }
                }
            }
        }
        anyhow::bail!("No backend available to read median time past")
    }

    pub fn get_chain_mtp(&self) -> Result<u64> {
        self.get_median_time_past_cached()
    }

    pub fn get_median_time_past_cached(&self) -> Result<u64> {
        // Matches the health poller interval. MTP only advances with a new block (~10 min) and is
        // monotonic non-decreasing, so a stale value can only delay a broadcast, never fire one early.
        const TTL: Duration = Duration::from_secs(30);
        if let Ok(cache) = self.mtp_cache.lock() {
            if let Some((fetched_at, mtp)) = *cache {
                if fetched_at.elapsed() < TTL {
                    return Ok(mtp);
                }
            }
        }
        let mtp = self.get_median_time_past()?;
        if let Ok(mut cache) = self.mtp_cache.lock() {
            *cache = Some((Instant::now(), mtp));
        }
        Ok(mtp)
    }

    pub fn retry_failed_transaction(&self, id: &str) -> Result<BroadcastTx> {
        let tx = self.db.get_broadcast_tx_by_id(id)?;
        if tx.status != TxStatus::Failed {
            anyhow::bail!("Transaction {} is not in failed state", id);
        }
        self.db.reset_failed_to_scheduled(id)?;
        let mut tx = self.db.get_broadcast_tx_by_id(id)?;
        self.enrich_tx_locktime(&mut tx);
        Ok(tx)
    }

    pub fn rebroadcast_pending(&self) -> Result<Vec<(String, Result<String>)>> {
        let (interval, network) = {
            let config = self.config.lock().map_err(|e| anyhow::anyhow!("Config lock failed: {}", e))?;
            (config.pool.rebroadcast_interval_minutes as i32, config.network.network_type.data_dir_name().to_string())
        };
        let pending_txs = self.db.get_pending_rebroadcast(interval, &network)?;
        let mut results = Vec::new();

        for tx in pending_txs {
            tracing::debug!("Rebroadcasting transaction {}", tx.id);

            match self.broadcast_transaction(&tx.tx_hex) {
                Ok(txid) => {
                    let now = Utc::now().to_rfc3339();
                    self.db.execute_raw(
                        "UPDATE broadcast_pool SET broadcast_at = ?1, updated_at = ?1 WHERE id = ?2",
                        &[&now.as_str(), &tx.id.as_str()],
                    )?;
                    results.push((tx.id.clone(), Ok(txid)));
                }
                Err(e) => {
                    tracing::warn!("Rebroadcast failed for {}: {}", tx.id, e);
                    results.push((tx.id.clone(), Err(e)));
                }
            }
        }

        Ok(results)
    }

    pub fn check_confirmations(&self) -> Result<Vec<(String, bool, Option<u64>)>> {
        let network = {
            let config = self.config.lock().map_err(|e| anyhow::anyhow!("Config lock failed: {}", e))?;
            config.network.network_type.data_dir_name().to_string()
        };
        let broadcast_txs = self.db.list_broadcast_txs(Some("broadcast"), &network, 1000)?;
        let mut results = Vec::new();

        // Try RPC first (it can actually check confirmations per tx)
        if let Some(ref rpc) = self.rpc {
            if rpc.test_connection().unwrap_or(false) {
                for tx in &broadcast_txs {
                    if let Some(ref txid) = tx.txid {
                        match rpc.get_transaction(txid) {
                            Ok(raw_tx) => {
                                if let Some(ref blockhash) = raw_tx.blockhash {
                                    // Transaction is in a block
                                    if let Ok(height) = rpc.get_block_count() {
                                        let _ = self.db.mark_confirmed(&tx.id, height);
                                        results.push((tx.id.clone(), true, Some(height)));
                                    } else {
                                        results.push((tx.id.clone(), true, None));
                                    }
                                } else {
                                    // In mempool, not confirmed yet
                                    results.push((tx.id.clone(), false, None));
                                }
                            }
                            Err(_) => {
                                // Can't find tx, might be dropped
                                results.push((tx.id.clone(), false, None));
                            }
                        }
                    } else {
                        results.push((tx.id.clone(), false, None));
                    }
                }
                return Ok(results);
            }
        }

        // Fallback to indexer (limited confirmation checking)
        if let Some(indexer) = self.get_indexer() {
            if indexer.test_connection().unwrap_or(false) {
                for tx in broadcast_txs {
                    results.push((tx.id, false, None));
                }
                return Ok(results);
            }
        }

        Ok(results)
    }

    pub fn list_transactions(
        &self,
        status_filter: Option<&str>,
        limit: i32,
    ) -> Result<Vec<BroadcastTx>> {
        let network = {
            let config = self.config.lock().map_err(|e| anyhow::anyhow!("Config lock failed: {}", e))?;
            config.network.network_type.data_dir_name().to_string()
        };
        let mut txs = self.db.list_broadcast_txs(status_filter, &network, limit)?;
        for tx in &mut txs {
            self.enrich_tx_locktime(tx);
        }
        Ok(txs)
    }

    pub fn get_transaction(&self, id: &str) -> Result<BroadcastTx> {
        let mut tx = self.db.get_broadcast_tx_by_id(id)?;
        self.enrich_tx_locktime(&mut tx);
        Ok(tx)
    }

    fn tx_has_broadcast_schedule(tx: &BroadcastTx) -> bool {
        tx.broadcast_mode.as_deref() == Some("scheduled")
            || tx.broadcast_mode.as_deref() == Some("manual")
            || tx.scheduled_time.is_some()
            || tx.defer_until.is_some()
            || tx.broadcast_missed_at.is_some()
            || tx.schedule_trigger.as_deref() == Some("price")
    }

    fn tx_can_reschedule(tx: &BroadcastTx) -> bool {
        matches!(tx.status, TxStatus::Pending | TxStatus::Scheduled)
            && Self::tx_has_broadcast_schedule(tx)
    }

    fn enrich_tx_locktime(&self, tx: &mut BroadcastTx) {
        tx.locktime_waiting = None;
        tx.locktime_deferred = None;
        tx.can_reschedule = None;
        tx.chain_mtp = None;
        tx.locktime_target = None;
        tx.locktime_remaining_secs = None;
        tx.locktime_satisfied = None;

        let emitted = matches!(tx.status, TxStatus::Broadcast | TxStatus::Confirmed);
        let deferred = tx.broadcast_missed_at.is_some() && !emitted;
        tx.locktime_deferred = Some(deferred);
        tx.can_reschedule = Some(!emitted && Self::tx_can_reschedule(tx));

        if tx.schedule_trigger.as_deref() == Some("price") {
            if let Some(currency) = &tx.price_currency {
                if let Some(prices) = self.price_feed.cached_prices() {
                    tx.current_btc_price = prices.get(currency).copied();
                }
            }
        }

        let nlock = match tx.nlocktime {
            Some(n) if n > 0 && n > 500_000_000 => n,
            _ => {
                tx.locktime_waiting = Some(false);
                return;
            }
        };

        tx.locktime_target = Some(nlock);

        let mtp = match self.get_median_time_past_cached() {
            Ok(mtp) => mtp,
            Err(e) => {
                tracing::debug!("Could not read chain MTP for {}: {}", tx.id, e);
                return;
            }
        };

        tx.chain_mtp = Some(mtp);
        let satisfied = mtp >= nlock;
        tx.locktime_satisfied = Some(satisfied);
        if !satisfied {
            tx.locktime_remaining_secs = Some(nlock as i64 - mtp as i64);
        } else {
            tx.locktime_remaining_secs = Some(0);
        }

        let waiting_for_locktime = !emitted
            && matches!(tx.status, TxStatus::Pending | TxStatus::Scheduled)
            && !satisfied;
        tx.locktime_waiting = Some(waiting_for_locktime);
    }

    pub fn remove_transaction(&self, id: &str) -> Result<()> {
        let tx = self.db.get_broadcast_tx_by_id(id)?;
        let txid = tx
            .txid
            .clone()
            .or_else(|| crate::pool::virtual_mempool::compute_txid(&tx.tx_hex).ok());
        let removed = self.db.remove_broadcast_tx(id)?;
        if removed == 0 {
            anyhow::bail!("Transaction not found");
        }
        if let Some(txid) = txid {
            self.remove_pending_tx(&txid);
        }
        tracing::info!("Removed transaction {} from broadcast pool", id);
        Ok(())
    }

    pub fn get_stats(&self) -> Result<PoolStats> {
        let network = {
            let config = self.config.lock().map_err(|e| anyhow::anyhow!("Config lock failed: {}", e))?;
            config.network.network_type.data_dir_name().to_string()
        };
        self.db.get_pool_stats(&network)
    }

    pub fn get_mempool_status(&self) -> MempoolStatus {
        use crate::db::models::MempoolStatus;

        let network = self
            .config
            .lock()
            .map(|c| c.network.network_type.data_dir_name().to_string())
            .unwrap_or_else(|_| "mainnet".to_string());

        let Some(rpc) = self.rpc.as_ref() else {
            return MempoolStatus {
                available: false,
                mempool_tx_count: None,
                fee_rate_sat_vb: None,
                congestion: None,
            };
        };

        let node = match rpc.get_node_status() {
            Ok(n) => n,
            Err(e) => {
                tracing::debug!("Mempool status unavailable: {}", e);
                return MempoolStatus {
                    available: false,
                    mempool_tx_count: None,
                    fee_rate_sat_vb: None,
                    congestion: None,
                };
            }
        };

        let fee_rate = rpc.estimate_smart_fee(6).ok().flatten();
        let congestion = fee_rate.map(|f| classify_mempool_congestion(f, &network).to_string());

        MempoolStatus {
            available: true,
            mempool_tx_count: node.mempool_size,
            fee_rate_sat_vb: fee_rate,
            congestion,
        }
    }

    pub fn get_pending_by_block_height(&self, network: &str) -> Result<Vec<BroadcastTx>> {
        self.db.get_pending_by_block_height(network)
    }

    pub fn get_pending_by_scheduled_time(&self, network: &str) -> Result<Vec<BroadcastTx>> {
        self.db.get_pending_by_scheduled_time(network)
    }

    pub fn mark_as_due(&self, id: &str) -> Result<()> {
        self.db.mark_due(id)
    }

    pub fn mark_due_from_price_trigger(&self, id: &str) -> Result<()> {
        self.db.mark_due_from_price_trigger(id)
    }

    pub fn mark_as_due_with_schedule(&self, id: &str, scheduled_time: &chrono::DateTime<chrono::Utc>) -> Result<()> {
        self.db.mark_due_with_schedule(id, scheduled_time)
    }

    pub fn rpc_connected(&self) -> bool {
        if let Some(ref rpc) = self.rpc {
            rpc.test_connection().unwrap_or(false)
        } else {
            false
        }
    }

    pub fn get_rpc(&self) -> Option<&Arc<BitcoinRpc>> {
        self.rpc.as_ref()
    }

    fn lock_indexer(&self) -> Option<std::sync::MutexGuard<'_, Option<Arc<ElectrumClient>>>> {
        self.indexer.lock().ok()
    }

    pub fn get_indexer(&self) -> Option<Arc<ElectrumClient>> {
        self.lock_indexer()?.clone()
    }

    pub fn set_indexer(&self, client: Option<Arc<ElectrumClient>>) {
        if let Some(mut guard) = self.lock_indexer() {
            *guard = client;
        }
    }

    pub fn reconnect_indexer_from_config(&self) -> Result<()> {
        let url = self
            .get_indexer_url()
            .context("No indexer URL configured")?;
        let network = {
            let cfg = self
                .config
                .lock()
                .map_err(|e| anyhow::anyhow!("Config lock poisoned: {}", e))?;
            cfg.network.network_type.clone()
        };
        let working = crate::discovery::resolve_working_indexer_url(&url, &network)
            .unwrap_or_else(|| url.clone());
        if working != url {
            if let Ok(mut cfg) = self.config.lock() {
                cfg.indexer = Some(crate::config::IndexerConfig {
                    url: working.clone(),
                    manual_override: cfg
                        .indexer
                        .as_ref()
                        .map(|i| i.manual_override)
                        .unwrap_or(false),
                });
            }
        }
        let client = Arc::new(ElectrumClient::new(&working)?);
        self.set_indexer(Some(client));
        Ok(())
    }

    /// Snapshot of the chain-clock health. Pure cache read, no I/O — safe on every tick.
    pub fn chain_health(&self) -> ChainHealth {
        self.chain_health
            .read()
            .map(|h| h.clone())
            .unwrap_or_default()
    }

    pub fn chain_source(&self) -> ChainSource {
        self.chain_health().source
    }

    /// True when either the indexer or a synced Bitcoin Core can serve height and MTP.
    pub fn chain_clock_available(&self) -> bool {
        self.chain_health().clock_available()
    }

    fn write_chain_health(&self, f: impl FnOnce(&mut ChainHealth)) {
        if let Ok(mut health) = self.chain_health.write() {
            f(&mut health);
            health.source = decide_chain_source(
                health.indexer_up,
                health.core_up,
                health.core_ibd,
                health.secondary_up,
            );
        }
    }

    /// Demote the indexer the moment a data path sees it fail, instead of waiting up to a full
    /// poll interval. Failing fast here is what keeps a dead indexer from being retried.
    pub fn note_indexer_failure(&self) {
        let was_up = self.chain_health().indexer_up;
        self.write_chain_health(|h| {
            h.indexer_up = false;
            h.polled = true;
        });
        if was_up {
            tracing::warn!("Indexer marked down; chain source is now {:?}", self.chain_source());
        }
    }

    pub fn note_indexer_success(&self, height: u64) {
        self.write_chain_health(|h| {
            h.indexer_up = true;
            h.polled = true;
            h.height = Some(height);
        });
    }

    /// Probe both backends and refresh the snapshot. The single writer; called by the health
    /// poller every 30s and once synchronously at startup.
    pub fn refresh_chain_health(&self) {
        if self.health_refresh_in_flight.swap(true, Ordering::SeqCst) {
            return;
        }

        let core = self.rpc.as_ref().and_then(|rpc| match rpc.get_chain_status() {
            Ok(status) => Some(status),
            Err(e) => {
                // `{:#}` prints the whole error chain: without the source, an RPC fault is
                // indistinguishable from a deserialization one, and this is the log a user
                // sends us when the fallback did not kick in.
                tracing::debug!("Bitcoin Core chain status failed: {:#}", e);
                None
            }
        });

        let indexer_height = self.get_indexer().and_then(|ix| match ix.get_block_height() {
            Ok(height) => Some(height),
            Err(e) => {
                tracing::debug!("Indexer height probe failed: {}", e);
                None
            }
        });

        // The software name never changes while a server is up, so ask only once per live indexer
        // rather than on every 30s poll.
        let indexer_software = if indexer_height.is_some() && self.chain_health().indexer_software.is_none() {
            self.get_indexer().and_then(|ix| ix.server_software().ok().flatten())
        } else {
            None
        };

        let (core_up, core_ibd, core_sync_pct, core_height, core_mtp) = match core {
            Some((height, mtp, ibd, pct)) => (true, ibd, Some(pct), Some(height), Some(mtp)),
            None => (false, false, None, None, None),
        };

        // Third fallback: probe the secondary indexer only when the primary is down and Core is
        // unusable. Reads the URL + our network from config; a fresh ElectrumClient per probe
        // (rare path).
        let (secondary_url, secondary_network) = {
            match self.config.lock().ok() {
                Some(c) => (c.secondary_indexer.clone(), Some(c.network.network_type.clone())),
                None => (None, None),
            }
        };
        let secondary_configured = secondary_url.is_some();
        let indexer_is_up = indexer_height.is_some();
        let (secondary_up, secondary_height, secondary_mtp) = if should_probe_secondary(
            indexer_is_up,
            core_up,
            core_ibd,
            secondary_configured,
        ) {
            let url = crate::discovery::normalize_indexer_url(secondary_url.as_deref().unwrap_or(""));
            match ElectrumClient::new(&url) {
                Ok(client) => match client.get_block_height() {
                    Ok(h) => {
                        // Reject a wrong-network secondary before trusting its height: a mainnet
                        // indexer answers ~900k blocks, which would mark testnet height-locked txs
                        // due early. Only accept it if its genesis matches our configured network.
                        let net_ok = secondary_network
                            .as_ref()
                            .map(|n| client.genesis_matches_network(n).unwrap_or(false))
                            .unwrap_or(false);
                        if net_ok {
                            let mtp = client.get_median_time_past().ok();
                            tracing::info!("Secondary indexer alive at {} (height {})", url, h);
                            (true, Some(h), mtp)
                        } else {
                            tracing::warn!(
                                "Secondary indexer {} is on a different network (genesis mismatch) — ignoring",
                                url
                            );
                            (false, None, None)
                        }
                    }
                    Err(e) => {
                        tracing::debug!("Secondary indexer probe failed: {}", e);
                        (false, None, None)
                    }
                },
                Err(e) => {
                    tracing::debug!("Secondary indexer client build failed: {}", e);
                    (false, None, None)
                }
            }
        } else {
            (false, None, None)
        };

        self.write_chain_health(|h| {
            h.indexer_up = indexer_height.is_some();
            // Keep the last known name while it is down: the degraded banner names the server the
            // user has to go and check ("your Fulcrum at 10.21.21.9:50002"), which is only useful
            // if we still remember which one it was. The UI never presents it as live.
            if let Some(ref name) = indexer_software {
                h.indexer_software = Some(name.clone());
            }
            h.core_up = core_up;
            h.core_ibd = core_ibd;
            h.core_sync_pct = core_sync_pct;
            h.secondary_configured = secondary_configured;
            h.secondary_up = secondary_up;
            // Prefer the indexer's height, then Core, then the secondary.
            h.height = indexer_height.or(core_height).or(secondary_height);
            h.mtp = core_mtp.or(secondary_mtp).or(h.mtp);
            h.polled = true;
        });

        // Core's `mediantime` is the authoritative BIP-113 MTP, so warm the cache with it and
        // spare the data paths an 11-header walk against the indexer.
        if let Some(mtp) = core_mtp {
            if let Ok(mut cache) = self.mtp_cache.lock() {
                *cache = Some((Instant::now(), mtp));
            }
        }
        if core_mtp.is_none() {
            if let Some(mtp) = secondary_mtp {
                if let Ok(mut cache) = self.mtp_cache.lock() {
                    *cache = Some((Instant::now(), mtp));
                }
            }
        }

        let health = self.chain_health();
        tracing::info!(
            "chain health: source={:?} indexer_up={} core_up={} core_ibd={} height={:?}",
            health.source,
            health.indexer_up,
            health.core_up,
            health.core_ibd,
            health.height
        );

        self.health_refresh_in_flight.store(false, Ordering::SeqCst);

        self.maybe_disarm_virtual_block();
    }

    /// Disarm the virtual-block tick once real_height >= armed_at + 10. Cheap: a lock read plus,
    /// only on the disarm edge, one config write. Called from the health poller.
    pub fn maybe_disarm_virtual_block(&self) {
        let real_height = match self.chain_health().height {
            Some(h) => h,
            None => return,
        };
        let snapshot = {
            let mut cfg = match self.config.lock() {
                Ok(c) => c,
                Err(_) => return,
            };
            let vb = &cfg.schedule.liana_virtual_block;
            if !should_disarm(vb.enabled, vb.armed_at_height, real_height) {
                return;
            }
            tracing::info!(
                "Liana virtual block auto-disarmed (real height {} >= armed {} + 10)",
                real_height,
                cfg.schedule.liana_virtual_block.armed_at_height
            );
            cfg.schedule.liana_virtual_block.enabled = false;
            cfg.clone()
        };
        if let Err(e) = crate::discovery::save_config_to_disk(&snapshot) {
            tracing::warn!("Failed to persist virtual-block disarm: {}", e);
        }
    }

    pub fn check_block_height(&self) -> Result<Option<u64>> {
        // Skip a known-dead indexer: probing it re-resolves every candidate URL (tcp and ssl) and
        // costs a full connect timeout on each call.
        if self.chain_health().should_try_indexer() {
            if let Some(indexer) = self.get_indexer() {
                match indexer.get_block_height() {
                    Ok(height) => {
                        self.note_indexer_success(height);
                        return Ok(Some(height));
                    }
                    Err(e) => {
                        tracing::debug!("Indexer get_height failed: {}", e);
                        self.note_indexer_failure();
                    }
                }
            }
        }
        if let Some(ref rpc) = self.rpc {
            match rpc.get_block_count() {
                Ok(height) => return Ok(Some(height)),
                Err(e) => tracing::debug!("RPC get_block_count failed: {}", e),
            }
        }
        Ok(None)
    }

    pub fn store_pending_tx(&self, txid: &str, tx_hex: &str, scripthashes: Vec<String>, outputs: Vec<PendingTxOutput>) {
        let sh_len = scripthashes.len();
        let out_len = outputs.len();
        let sh_clone = scripthashes.clone();
        let mut pending = match self.lock_pending() {
            Some(p) => p,
            None => return,
        };
        pending.insert(
            txid.to_string(),
            PendingTxInfo {
                tx_hex: tx_hex.to_string(),
                scripthashes,
                outputs,
            },
        );
        tracing::info!(
            "Stored pending tx {} with {} scripthashes, {} outputs",
            txid,
            sh_len,
            out_len
        );
        drop(pending);
        // Notify connected sessions so they see the new pending tx immediately.
        for sh in &sh_clone {
            let _ = self.scripthash_notifications.send(ScripthashNotification {
                scripthash: sh.clone(),
            });
        }
    }

    /// Merge additional scripthashes (e.g. input addresses after electrs enrichment).
    pub fn merge_pending_scripthashes(&self, txid: &str, extra: &[String]) {
        if extra.is_empty() {
            return;
        }
        let newly_added: Vec<String> = {
            let mut pending = match self.lock_pending() {
                Some(p) => p,
                None => return,
            };
            let Some(info) = pending.get_mut(txid) else {
                return;
            };
            let before = info.scripthashes.len();
            let mut added = Vec::new();
            for sh in extra {
                if !info.scripthashes.contains(sh) {
                    info.scripthashes.push(sh.clone());
                    added.push(sh.clone());
                }
            }
            if info.scripthashes.len() > before {
                tracing::info!(
                    "Enriched pending tx {} scripthashes: {} → {}",
                    txid,
                    before,
                    info.scripthashes.len()
                );
            }
            added
        };
        // Notify connected Electrum sessions about the newly affected scripthashes.
        // Input/spending addresses are resolved AFTER the pre-ack output-only store;
        // without this push, Sparrow (subscribed to its spending addresses) never sees
        // the tx hit those addresses and stays stuck on "broadcasting" forever.
        // Sent after dropping the pending lock (mirrors store_pending_tx).
        for sh in &newly_added {
            let _ = self.scripthash_notifications.send(ScripthashNotification {
                scripthash: sh.clone(),
            });
        }
    }

    pub fn lookup_tx_hex(&self, txid: &str) -> Option<String> {
        let normalized = txid.trim().to_lowercase();
        if let Some(hex) = self.get_pending_tx_hex(&normalized) {
            return Some(hex);
        }
        if let Some(alt) = super::virtual_mempool::alternate_txid_format(&normalized) {
            if let Some(hex) = self.get_pending_tx_hex(&alt) {
                return Some(hex);
            }
        }
        if let Ok(Some(hex)) = self.get_tx_hex_by_txid(&normalized) {
            return Some(hex);
        }
        if let Some(alt) = super::virtual_mempool::alternate_txid_format(&normalized) {
            if let Ok(Some(hex)) = self.get_tx_hex_by_txid(&alt) {
                return Some(hex);
            }
        }
        None
    }

    fn lock_pending(&self) -> Option<std::sync::MutexGuard<'_, HashMap<String, PendingTxInfo>>> {
        match self.pending_txs.lock() {
            Ok(guard) => Some(guard),
            Err(e) => {
                // #region agent log
                crate::utils::debug_log::agent_log(
                    "H2",
                    "pool/manager.rs:lock_pending",
                    "pending mutex poisoned",
                    serde_json::json!({ "error": e.to_string() }),
                );
                // #endregion
                tracing::error!("pending_txs mutex poisoned: {}", e);
                None
            }
        }
    }

    pub fn get_pending_tx_hex(&self, txid: &str) -> Option<String> {
        let pending = self.lock_pending()?;
        pending.get(txid).map(|info| info.tx_hex.clone())
    }

    pub fn get_all_pending_txs(&self) -> HashMap<String, PendingTxInfo> {
        self.lock_pending()
            .map(|pending| pending.clone())
            .unwrap_or_default()
    }

    pub fn has_pending_txs(&self) -> bool {
        self.lock_pending()
            .map(|p| !p.is_empty())
            .unwrap_or(false)
    }

    /// Sparrow polls INPUT scripthashes after broadcast; enrich on demand when phase-2 ingest missed.
    pub fn enrich_pending_for_scripthash(&self, scripthash: &str, indexer_url: &str) {
        if scripthash.is_empty() {
            return;
        }
        if !self.has_pending_txs() {
            return;
        }
        let indexer_addr = super::virtual_mempool::strip_indexer_host(indexer_url);
        let budget = std::time::Duration::from_millis(5000);
        let lookup = |id: &str| self.lookup_tx_hex(id);
        let pending = self.get_all_pending_txs();
        for (txid, info) in pending {
            if info.scripthashes.contains(&scripthash.to_string()) {
                continue;
            }
            match super::virtual_mempool::enrich_input_scripthashes(
                &info.tx_hex,
                &indexer_addr,
                budget,
                Some(&lookup),
            ) {
                Ok(extra) if !extra.is_empty() => {
                    self.merge_pending_scripthashes(&txid, &extra);
                }
                Err(e) => {
                    tracing::warn!("On-demand scripthash enrich failed for {}: {}", txid, e);
                }
                Ok(_) => {}
            }
        }
    }

    pub fn get_pending_txids_for_scripthash(&self, scripthash: &str) -> Vec<String> {
        let pending = match self.lock_pending() {
            Some(p) => p,
            None => return Vec::new(),
        };
        pending
            .iter()
            .filter(|(_, info)| info.scripthashes.contains(&scripthash.to_string()))
            .map(|(txid, _)| txid.clone())
            .collect()
    }

    pub fn get_pending_utxos_for_scripthash(&self, scripthash: &str) -> Vec<(String, u32, u64)> {
        let pending = match self.lock_pending() {
            Some(p) => p,
            None => return Vec::new(),
        };
        let mut utxos = Vec::new();
        for (txid, info) in pending.iter() {
            for output in &info.outputs {
                if output.scripthash == scripthash {
                    utxos.push((txid.clone(), output.output_index, output.value));
                }
            }
        }
        utxos
    }

    pub fn get_pending_unconfirmed_value(&self, scripthash: &str) -> u64 {
        let pending = match self.lock_pending() {
            Some(p) => p,
            None => return 0,
        };
        let mut total = 0u64;
        for (_, info) in pending.iter() {
            for output in &info.outputs {
                if output.scripthash == scripthash {
                    total += output.value;
                }
            }
        }
        total
    }

    pub fn remove_pending_tx(&self, txid: &str) -> Vec<String> {
        let removed_sh = {
            let mut pending = match self.lock_pending() {
                Some(p) => p,
                None => return Vec::new(),
            };
            match pending.remove(txid) {
                Some(info) => {
                    tracing::info!("Removed pending tx {}", txid);
                    info.scripthashes
                }
                None => return Vec::new(),
            }
        };
        // Notify all affected scripthashes so connected Electrum sessions get updated status.
        for sh in &removed_sh {
            let _ = self.scripthash_notifications.send(ScripthashNotification {
                scripthash: sh.clone(),
            });
        }
        removed_sh
    }

    pub fn get_indexer_url(&self) -> Option<String> {
        let config = self.config.lock().ok()?;
        config.indexer.as_ref().map(|i| i.url.clone())
    }
}

fn is_retriable_broadcast_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("non-final")
        || m.contains("non final")
        || m.contains("not final")
        || m.contains("locktime")
        || m.contains("too-long-mempool-chain")
}

/// Auto-disarm the Liana virtual block once the real chain has advanced 10 blocks past arming.
/// Bounds the window in which we serve fake heights, even if the user forgets to unset the tick.
fn should_disarm(enabled: bool, armed_at_height: u64, real_height: u64) -> bool {
    enabled && real_height >= armed_at_height + 10
}

/// The secondary indexer is a last resort: probe it only when the primary is down AND Core cannot
/// serve the clock (down or in IBD), and only if one is configured. Keeps the external LAN node
/// untouched while the primary is healthy.
fn should_probe_secondary(
    indexer_up: bool,
    core_up: bool,
    core_ibd: bool,
    secondary_configured: bool,
) -> bool {
    secondary_configured && !indexer_up && !(core_up && !core_ibd)
}

fn classify_mempool_congestion(fee_sat_vb: f64, network: &str) -> &'static str {
    let (low_max, high_min) = match network {
        "mainnet" => (10.0, 50.0),
        _ => (2.0, 15.0),
    };
    if fee_sat_vb < low_max {
        "low"
    } else if fee_sat_vb < high_min {
        "medium"
    } else {
        "high"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db::Database;

    fn test_manager() -> (PoolManager, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Arc::new(Database::open(&dir.path().join("test.db")).expect("db open"));
        let config = Arc::new(Mutex::new(Config::default_config()));
        (PoolManager::new(db, None, None, config), dir)
    }

    // With neither an indexer nor Bitcoin Core, there is no chain clock: the tick must refuse to
    // run rather than silently do nothing, so the broadcast loop can back off and the dashboard
    // can tell the user schedules are paused.
    #[test]
    fn no_backend_means_no_chain_clock() {
        let (pm, _dir) = test_manager();
        pm.refresh_chain_health();

        assert_eq!(pm.chain_source(), ChainSource::None);
        assert!(!pm.chain_clock_available());

        let err = pm.run_scheduler_tick().expect_err("tick must refuse without a chain source");
        assert!(err.to_string().contains(NO_CHAIN_SOURCE));
    }

    // A data path that sees the indexer fail must demote it immediately, or every later call keeps
    // paying the full connect timeout until the next 30s poll.
    #[test]
    fn indexer_failure_demotes_the_snapshot() {
        let (pm, _dir) = test_manager();
        pm.note_indexer_success(900_000);
        assert!(pm.chain_health().indexer_up);
        assert_eq!(pm.chain_source(), ChainSource::Indexer);

        pm.note_indexer_failure();
        let health = pm.chain_health();
        assert!(!health.indexer_up);
        assert!(!health.should_try_indexer());
        assert_eq!(health.source, ChainSource::None);
    }

    // After the pre-ack output-only store, input (spending) scripthashes are resolved
    // later and merged in. Those merged scripthashes MUST emit a subscription push, or
    // Sparrow never learns the tx hit its spending addresses and hangs on "broadcasting".
    #[test]
    fn merge_pending_scripthashes_pushes_new_scripthashes() {
        let (pm, _dir) = test_manager();
        let txid = "a2885907c946130f23ab0fe0d885e4b6276e4f0cd8b15c4b2c04183eeb57d86d";
        let out_sh = "4a6fb09fc2adc3be000000000000000000000000000000000000000000000001";
        pm.store_pending_tx(txid, "00", vec![out_sh.to_string()], vec![]);

        // Subscribe AFTER the output store so we only observe the enrichment push.
        let mut rx = pm.subscribe_scripthash_changes();
        let input_sh = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef0";
        pm.merge_pending_scripthashes(txid, &[input_sh.to_string()]);

        let got = rx.try_recv().expect("push for newly merged input scripthash");
        assert_eq!(got.scripthash, input_sh);
    }

    // Re-merging an already-known scripthash must NOT spam a push (nothing new added).
    #[test]
    fn merge_pending_scripthashes_no_push_when_nothing_new() {
        let (pm, _dir) = test_manager();
        let txid = "b1885907c946130f23ab0fe0d885e4b6276e4f0cd8b15c4b2c04183eeb57d86d";
        let sh = "1111111111111111111111111111111111111111111111111111111111111111";
        pm.store_pending_tx(txid, "00", vec![sh.to_string()], vec![]);

        let mut rx = pm.subscribe_scripthash_changes();
        pm.merge_pending_scripthashes(txid, &[sh.to_string()]); // already present

        assert!(rx.try_recv().is_err(), "no push expected when no new scripthash");
    }

    #[test]
    fn virtual_block_disarms_after_ten_blocks() {
        assert!(!should_disarm(false, 100, 200)); // disabled → never
        assert!(!should_disarm(true, 100, 109)); // 9 blocks in → still armed
        assert!(should_disarm(true, 100, 110)); // exactly +10 → disarm
        assert!(should_disarm(true, 100, 130)); // well past → disarm
    }

    #[test]
    fn secondary_probed_only_when_primary_down_and_core_unusable() {
        // Primary up → never probe the external node.
        assert!(!should_probe_secondary(true, false, false, true));
        // Primary down but Core synced → Core covers it; don't probe secondary.
        assert!(!should_probe_secondary(false, true, false, true));
        // Primary down, Core down → probe secondary (if configured).
        assert!(should_probe_secondary(false, false, false, true));
        // Primary down, Core in IBD → probe secondary.
        assert!(should_probe_secondary(false, true, true, true));
        // Not configured → never probe.
        assert!(!should_probe_secondary(false, false, false, false));
    }
}
