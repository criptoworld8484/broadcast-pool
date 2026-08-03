use anyhow::Result;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::{sleep, Instant};

use crate::api::ArchiveKeyStore;
use crate::config::Config;
use crate::db::Database;
use crate::pool::manager::{PoolManager, NO_CHAIN_SOURCE};

const BROADCAST_CHECK_INTERVAL: Duration = Duration::from_secs(15);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_secs(30);
const RETENTION_INTERVAL: Duration = Duration::from_secs(86_400);

pub struct Scheduler {
    pool_manager: Arc<PoolManager>,
    config: Arc<Mutex<Config>>,
    db: Arc<Database>,
    archive_keys: Arc<ArchiveKeyStore>,
}

impl Scheduler {
    pub fn new(
        pool_manager: Arc<PoolManager>,
        config: Arc<Mutex<Config>>,
        db: Arc<Database>,
        archive_keys: Arc<ArchiveKeyStore>,
    ) -> Self {
        Self {
            pool_manager,
            config,
            db,
            archive_keys,
        }
    }

    pub async fn run_broadcast_loop(&self) -> Result<()> {
        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(60);

        tracing::info!(
            "Starting broadcast scheduler loop (interval: {}s)",
            BROADCAST_CHECK_INTERVAL.as_secs()
        );

        loop {
            let start = Instant::now();

            if self.pool_manager.is_safe_mode() {
                tracing::warn!(
                    "SAFE MODE active — broadcast loop paused (tampered row detected, awaiting admin acknowledge via /api/security/acknowledge)"
                );
                let elapsed = start.elapsed();
                if elapsed < BROADCAST_CHECK_INTERVAL {
                    sleep(BROADCAST_CHECK_INTERVAL - elapsed).await;
                }
                continue;
            }

            let pool_manager = self.pool_manager.clone();

            let tick = tokio::task::spawn_blocking(move || pool_manager.run_scheduler_tick()).await;

            match tick {
                Ok(Ok(results)) => {
                    backoff = Duration::from_secs(1);
                    for (id, result) in results {
                        match result {
                            Ok(txid) => tracing::info!("Broadcast {} -> txid: {}", id, txid),
                            Err(e) => tracing::debug!("Broadcast deferred for {}: {}", id, e),
                        }
                    }
                }
                Ok(Err(e)) => {
                    if e.to_string().contains(NO_CHAIN_SOURCE) {
                        tracing::warn!(
                            "No chain data source (indexer and Bitcoin Core both unusable), backing off for {:?}",
                            backoff
                        );
                        sleep(backoff).await;
                        backoff = (backoff * 2).min(max_backoff);
                    } else {
                        tracing::error!("Error in broadcast loop: {}", e);
                    }
                }
                Err(e) => tracing::error!("Broadcast scheduler task failed: {}", e),
            }

            let elapsed = start.elapsed();
            if elapsed < BROADCAST_CHECK_INTERVAL {
                sleep(BROADCAST_CHECK_INTERVAL - elapsed).await;
            }
        }
    }

    pub async fn run_block_height_monitor(&self) -> Result<()> {
        let check_interval = Duration::from_secs(60);
        tracing::info!("Starting block height monitor loop (interval: 60s)");

        loop {
            let pool_manager = self.pool_manager.clone();
            let config = self.config.clone();

            let result = tokio::task::spawn_blocking(move || -> Result<(), anyhow::Error> {
                let network = {
                    let config = config.lock().map_err(|e| anyhow::anyhow!("Config lock: {}", e))?;
                    config.network.network_type.data_dir_name().to_string()
                };

                if !pool_manager.chain_clock_available() {
                    return Ok(());
                }

                match pool_manager.check_block_height()? {
                    Some(current_height) => {
                        let pending_txs = pool_manager.get_pending_by_block_height(&network)?;
                        for tx in pending_txs {
                            if tx.broadcast_mode.as_deref() != Some("by_block") {
                                continue;
                            }
                            if let Some(nlocktime) = tx.nlocktime {
                                if nlocktime > 0
                                    && nlocktime < 500_000_000
                                    && current_height >= nlocktime
                                {
                                    tracing::info!(
                                        "Transaction {} now due (block height {} reached)",
                                        tx.id,
                                        current_height
                                    );
                                    if let Err(e) = pool_manager.mark_as_due(&tx.id) {
                                        tracing::error!("Failed to mark {} as due: {}", tx.id, e);
                                    }
                                }
                            }
                        }
                    }
                    None => tracing::debug!("Could not get block height"),
                }
                Ok(())
            })
            .await;

            if let Err(e) = result {
                tracing::error!("Block height monitor task failed: {}", e);
            } else if let Ok(Err(e)) = result {
                tracing::error!("Block height monitor error: {}", e);
            }

            sleep(check_interval).await;
        }
    }

    pub async fn run_rebroadcast_loop(&self) -> Result<()> {
        let interval = {
            let config = self.config.lock().map_err(|e| anyhow::anyhow!("Config lock: {}", e))?;
            Duration::from_secs(config.pool.rebroadcast_interval_minutes * 60)
        };
        tracing::info!(
            "Starting rebroadcast loop (interval: {}m)",
            interval.as_secs() / 60
        );

        loop {
            let pool_manager = self.pool_manager.clone();
            let tick = tokio::task::spawn_blocking(move || {
                if !pool_manager.chain_clock_available() {
                    return Ok(Vec::new());
                }
                pool_manager.rebroadcast_pending()
            })
            .await;

            match tick {
                Ok(Ok(results)) => {
                    for (id, result) in results {
                        match result {
                            Ok(txid) => tracing::debug!("Rebroadcast {} -> txid: {}", id, txid),
                            Err(e) => tracing::warn!("Rebroadcast failed for {}: {}", id, e),
                        }
                    }
                }
                Ok(Err(e)) => tracing::error!("Error in rebroadcast loop: {}", e),
                Err(e) => tracing::error!("Rebroadcast task failed: {}", e),
            }

            sleep(interval).await;
        }
    }

    pub async fn run_confirmation_checker(&self) -> Result<()> {
        let interval = Duration::from_secs(120);
        tracing::info!("Starting confirmation checker loop (interval: 120s)");

        loop {
            let pool_manager = self.pool_manager.clone();
            let tick = tokio::task::spawn_blocking(move || {
                if !pool_manager.chain_clock_available() {
                    return Ok(Vec::new());
                }
                pool_manager.check_confirmations()
            })
            .await;

            match tick {
                Ok(Ok(results)) => {
                    for (id, confirmed, height) in results {
                        if confirmed {
                            tracing::info!(
                                "Transaction {} confirmed at block {}",
                                id,
                                height.unwrap_or(0)
                            );
                        }
                    }
                }
                Ok(Err(e)) => tracing::error!("Error checking confirmations: {}", e),
                Err(e) => tracing::error!("Confirmation checker task failed: {}", e),
            }

            sleep(interval).await;
        }
    }

    pub async fn run_price_monitor(&self) -> Result<()> {
        let interval = Duration::from_secs(60);
        tracing::info!("Starting BTC/fiat price monitor loop (interval: 60s)");

        loop {
            let pool_manager = self.pool_manager.clone();
            let price_feed = pool_manager.price_feed().clone();

            let fetch_and_check = async move {
                let snapshot = price_feed.fetch_snapshot().await?;
                if snapshot.stale {
                    tracing::warn!(
                        "Price monitor using stale cache from {} — skipping trigger evaluation",
                        snapshot.source
                    );
                    return Ok(0usize);
                }
                tracing::debug!(
                    "BTC prices from {} (EUR={:?}, USD={:?})",
                    snapshot.source,
                    snapshot.prices.get("eur"),
                    snapshot.prices.get("usd")
                );
                let prices = snapshot.prices;
                let triggered = tokio::task::spawn_blocking(move || {
                    pool_manager.check_price_triggers(&prices)
                })
                .await??;
                Ok::<usize, anyhow::Error>(triggered)
            };

            match fetch_and_check.await {
                Ok(n) if n > 0 => tracing::info!("Price monitor marked {} tx(s) as due", n),
                Ok(_) => {}
                Err(e) => tracing::warn!("Price monitor tick failed: {}", e),
            }

            sleep(interval).await;
        }
    }

    /// Keeps the chain-health snapshot fresh so the other loops never pay a blocking probe.
    /// Polls Bitcoin Core even while the indexer is healthy, so the dashboard can tell the user
    /// whether the fallback is actually ready *before* it is needed.
    pub async fn run_health_poller(&self) -> Result<()> {
        tracing::info!(
            "Starting chain health poller (interval: {}s)",
            HEALTH_POLL_INTERVAL.as_secs()
        );

        loop {
            sleep(HEALTH_POLL_INTERVAL).await;

            let pool_manager = self.pool_manager.clone();
            let config = self.config.clone();
            if let Err(e) = tokio::task::spawn_blocking(move || {
                pool_manager.refresh_chain_health();
                // Keep the tip served to wallets fresh. Without this the cache only ever moved
                // when a wallet itself asked for headers — and since that request is answered
                // from cache *before* the refresh lands, the wallet always got the previous
                // value. Observed 71 blocks (~12h) stale in the field.
                let indexer_url =
                    crate::electrum_server::resolve_live_indexer_url(&pool_manager, &config);
                crate::electrum_server::refresh_chain_tip_cache(&indexer_url, &pool_manager);
            })
            .await
            {
                tracing::error!("Chain health poller task failed: {}", e);
            }
        }
    }

    /// Daily retention sweep: while the archive is unlocked (key present in memory), moves
    /// terminal txs older than `expiry_days` into the encrypted archive and deletes them from
    /// the active pool. While locked, this is a no-op tick — retention simply pauses rather than
    /// erroring, since the key may become available again on a later tick.
    pub async fn run_retention_loop(&self) -> Result<()> {
        tracing::info!(
            "Starting retention loop (interval: {}s)",
            RETENTION_INTERVAL.as_secs()
        );

        loop {
            let archive_keys = self.archive_keys.clone();
            let db = self.db.clone();
            let config = self.config.clone();

            let result = tokio::task::spawn_blocking(move || -> Result<()> {
                let key = match archive_keys.key() {
                    Some(key) => key,
                    None => {
                        tracing::debug!("archive locked, retention paused");
                        return Ok(());
                    }
                };

                let (network, expiry_days) = {
                    let config = config
                        .lock()
                        .map_err(|e| anyhow::anyhow!("Config lock: {}", e))?;
                    (
                        config.network.network_type.data_dir_name().to_string(),
                        config.pool.expiry_days,
                    )
                };

                let cutoff = (chrono::Utc::now() - chrono::Duration::days(expiry_days as i64))
                    .to_rfc3339();

                let moved = db.archive_terminal_older_than(&key, &network, &cutoff)?;
                tracing::info!("Retention: archived {} terminal tx(s) for {}", moved, network);
                Ok(())
            })
            .await;

            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::error!("Retention loop tick failed: {}", e),
                Err(e) => tracing::error!("Retention loop task failed: {}", e),
            }

            sleep(RETENTION_INTERVAL).await;
        }
    }

    /// Drains the broadcast-notification channel and delivers each event to the enabled
    /// enabled channel. Runs entirely outside the broadcast path: a slow or unreachable relay
    /// can only delay other notifications, never a broadcast.
    pub async fn run_notification_dispatcher(&self) -> Result<()> {
        let mut rx = self.pool_manager.subscribe_broadcast_notifications();
        tracing::info!("Starting notification dispatcher");

        loop {
            match rx.recv().await {
                Ok(notification) => {
                    let cfg = {
                        match self.config.lock() {
                            Ok(config) => config.notifications.clone(),
                            Err(e) => {
                                tracing::error!("Config lock in notification dispatcher: {}", e);
                                continue;
                            }
                        }
                    };
                    if !cfg.any_active() {
                        continue;
                    }
                    crate::notify::deliver(&cfg, &notification).await;
                }
                // Slow dispatcher (e.g. a long relay timeout) while broadcasts pile up. The
                // broadcasts themselves are unaffected; we just lost the notifications.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Notification dispatcher lagged, dropped {} event(s)", n);
                }
                // The PoolManager is gone — the process is shutting down.
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    tracing::info!("Notification channel closed, dispatcher stopping");
                    return Ok(());
                }
            }
        }
    }

    pub async fn start_all_loops(&self) -> Result<()> {
        let pool_manager = self.pool_manager.clone();
        let config = self.config.clone();
        let db = self.db.clone();
        let archive_keys = self.archive_keys.clone();

        tokio::spawn(async move {
            let scheduler = Scheduler::new(pool_manager, config, db, archive_keys);
            if let Err(e) = scheduler.run_health_poller().await {
                tracing::error!("Chain health poller error: {}", e);
            }
        });

        let pool_manager = self.pool_manager.clone();
        let config = self.config.clone();
        let db = self.db.clone();
        let archive_keys = self.archive_keys.clone();

        tokio::spawn(async move {
            let scheduler = Scheduler::new(pool_manager, config, db, archive_keys);
            if let Err(e) = scheduler.run_broadcast_loop().await {
                tracing::error!("Broadcast loop error: {}", e);
            }
        });

        let pool_manager = self.pool_manager.clone();
        let config = self.config.clone();
        let db = self.db.clone();
        let archive_keys = self.archive_keys.clone();

        tokio::spawn(async move {
            let scheduler = Scheduler::new(pool_manager, config, db, archive_keys);
            if let Err(e) = scheduler.run_block_height_monitor().await {
                tracing::error!("Block height monitor error: {}", e);
            }
        });

        let pool_manager = self.pool_manager.clone();
        let config = self.config.clone();
        let db = self.db.clone();
        let archive_keys = self.archive_keys.clone();

        tokio::spawn(async move {
            let scheduler = Scheduler::new(pool_manager, config, db, archive_keys);
            if let Err(e) = scheduler.run_rebroadcast_loop().await {
                tracing::error!("Rebroadcast loop error: {}", e);
            }
        });

        let pool_manager = self.pool_manager.clone();
        let config = self.config.clone();
        let db = self.db.clone();
        let archive_keys = self.archive_keys.clone();

        tokio::spawn(async move {
            let scheduler = Scheduler::new(pool_manager, config, db, archive_keys);
            if let Err(e) = scheduler.run_confirmation_checker().await {
                tracing::error!("Confirmation checker error: {}", e);
            }
        });

        let pool_manager = self.pool_manager.clone();
        let config = self.config.clone();
        let db = self.db.clone();
        let archive_keys = self.archive_keys.clone();

        tokio::spawn(async move {
            let scheduler = Scheduler::new(pool_manager, config, db, archive_keys);
            if let Err(e) = scheduler.run_price_monitor().await {
                tracing::error!("Price monitor error: {}", e);
            }
        });

        let pool_manager = self.pool_manager.clone();
        let config = self.config.clone();
        let db = self.db.clone();
        let archive_keys = self.archive_keys.clone();

        tokio::spawn(async move {
            let scheduler = Scheduler::new(pool_manager, config, db, archive_keys);
            if let Err(e) = scheduler.run_retention_loop().await {
                tracing::error!("Retention loop error: {}", e);
            }
        });

        let pool_manager = self.pool_manager.clone();
        let config = self.config.clone();
        let db = self.db.clone();
        let archive_keys = self.archive_keys.clone();

        tokio::spawn(async move {
            let scheduler = Scheduler::new(pool_manager, config, db, archive_keys);
            if let Err(e) = scheduler.run_notification_dispatcher().await {
                tracing::error!("Notification dispatcher error: {}", e);
            }
        });

        tracing::info!("All scheduler loops started");
        Ok(())
    }
}
