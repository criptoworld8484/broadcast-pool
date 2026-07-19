pub mod keyfile;
pub mod models;
pub mod schema;

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;
use uuid::Uuid;

use self::models::*;

const BROADCAST_SELECT: &str = "id, tx_hex, txid, status, network, nlocktime, broadcast_mode, scheduled_time, broadcast_at, confirmed_at, block_height, target_fee_rate, actual_fee_rate, source_label, destination_address, utxo_count, total_value_btc, replacement_of, error_message, retry_count, broadcast_missed_at, original_scheduled_time, defer_until, schedule_trigger, target_price, price_currency, price_condition, created_at, updated_at, row_mac";

/// Canonical byte string used both when computing `row_mac` at insert time and when
/// re-verifying it at read time. Fields are joined with `\x1f` (a byte that cannot appear in
/// any of them); NULL-valued fields must be normalized to the empty string identically at
/// both call sites, or verification will always fail.
fn row_mac_input(
    id: &str,
    status: &str,
    mode: &str,
    scheduled: &str,
    nlocktime: i64,
    target_price: &str,
    schedule_trigger: &str,
    price_condition: &str,
    enc_tx_hex: &str,
    enc_dest: &str,
) -> Vec<u8> {
    [
        id,
        status,
        mode,
        scheduled,
        &nlocktime.to_string(),
        target_price,
        schedule_trigger,
        price_condition,
        enc_tx_hex,
        enc_dest,
    ]
    .join("\x1f")
    .into_bytes()
}

pub struct Database {
    conn: Mutex<Connection>,
    key: [u8; 32],
}

impl Database {
    pub(crate) fn key(&self) -> &[u8; 32] {
        &self.key
    }

    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|e| {
            // #region agent log
            crate::utils::debug_log::agent_log(
                "H1",
                "db/mod.rs:lock_conn",
                "database mutex poisoned",
                serde_json::json!({ "error": e.to_string() }),
            );
            // #endregion
            anyhow::anyhow!("Database lock poisoned: {}", e)
        })
    }

    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open database at {}", db_path.display()))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .context("Failed to set pragmas")?;

        let key_path = db_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("pool.key");
        let key = keyfile::load_or_create(&key_path)?;

        let db = Self {
            conn: Mutex::new(conn),
            key,
        };
        db.run_migrations()?;
        // Backfill total_value_btc for rows persisted before value-derivation existed (non-fatal).
        if let Err(e) = db.backfill_output_values() {
            tracing::warn!("Output-value backfill warning (non-fatal): {}", e);
        }
        // Encrypt any legacy plaintext rows and seal their row_mac (non-fatal).
        if let Err(e) = db.encrypt_legacy_rows() {
            tracing::warn!("Legacy encryption migration warning (non-fatal): {}", e);
        }
        Ok(db)
    }

    /// Recompute total_value_btc from the stored tx hex for rows that have no value yet
    /// (legacy rows persisted as 0). Returns the number of rows updated.
    pub fn backfill_output_values(&self) -> Result<usize> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, tx_hex FROM broadcast_pool WHERE total_value_btc = 0 OR total_value_btc IS NULL",
        )?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);

        let mut updated = 0;
        for (id, hex) in rows {
            let hex = crate::crypto::decode_field(&self.key, &hex, id.as_bytes()).unwrap_or_else(|e| {
                tracing::error!("field decrypt failed for {}: {}", id, e);
                hex
            });
            if let Some(btc) = output_value_btc_from_hex(&hex) {
                if btc > 0.0 {
                    conn.execute(
                        "UPDATE broadcast_pool SET total_value_btc = ?1 WHERE id = ?2",
                        params![btc, id],
                    )?;
                    updated += 1;
                }
            }
        }
        Ok(updated)
    }

    /// Encrypt sensitive columns (`tx_hex`/`destination_address`/`source_label`) for rows
    /// persisted before field-level encryption existed, and seal their `row_mac`. Idempotent:
    /// a row is only selected if `tx_hex` isn't already `enc:`-prefixed or `row_mac` is NULL;
    /// each field is only encrypted if it isn't already encoded, so re-running never
    /// double-encrypts. Non-fatal at the call site (see `open`). Returns the number of rows
    /// touched.
    pub fn encrypt_legacy_rows(&self) -> Result<usize> {
        let rows: Vec<(String, String, Option<String>, Option<String>)> = {
            let conn = self.lock_conn()?;
            let mut stmt = conn.prepare(
                "SELECT id, tx_hex, destination_address, source_label FROM broadcast_pool \
                 WHERE tx_hex NOT LIKE 'enc:%' OR row_mac IS NULL",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .filter_map(|r| r.ok())
                .collect();
            rows
        };

        let mut updated = 0;
        for (id, tx_hex, dest, source) in rows {
            let enc_tx_hex = if crate::crypto::is_encoded(&tx_hex) {
                tx_hex
            } else {
                crate::crypto::encode_field(&self.key, &tx_hex, id.as_bytes())
            };
            let enc_dest = dest.map(|d| {
                if crate::crypto::is_encoded(&d) {
                    d
                } else {
                    crate::crypto::encode_field(&self.key, &d, id.as_bytes())
                }
            });
            let enc_source = source.map(|s| {
                if crate::crypto::is_encoded(&s) {
                    s
                } else {
                    crate::crypto::encode_field(&self.key, &s, id.as_bytes())
                }
            });

            {
                let conn = self.lock_conn()?;
                conn.execute(
                    "UPDATE broadcast_pool SET tx_hex = ?1, destination_address = ?2, source_label = ?3 WHERE id = ?4",
                    params![enc_tx_hex, enc_dest, enc_source, id],
                )
                .context("Failed to encrypt legacy row")?;
            }
            self.reseal_row_mac(&id)?;
            updated += 1;
        }
        Ok(updated)
    }

    fn run_migrations(&self) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute_batch(schema::MIGRATION_001)
            .context("Failed to run migrations")?;

        // Migration 002: add nlocktime column if it doesn't exist
        let add_col_result: Result<(), _> = conn.execute_batch(schema::MIGRATION_002).context("Migration 002");
        if let Err(e) = add_col_result {
            let err_str = e.to_string();
            if !err_str.contains("duplicate column") && !err_str.contains("already exists") {
                tracing::warn!("Migration 002 warning (non-fatal): {}", e);
            } else {
                tracing::debug!("nlocktime column already exists, skipping migration 002");
            }
        }

        // Migration 003: create nlocktime index (idempotent)
        let add_idx_result: Result<(), _> = conn.execute_batch(schema::MIGRATION_003).context("Migration 003");
        if let Err(e) = add_idx_result {
            let err_str = e.to_string();
            if !err_str.contains("already exists") {
                tracing::warn!("Migration 003 warning (non-fatal): {}", e);
            }
        }

        // Migration 004: add broadcast_mode column (older DBs created before this field)
        let add_mode_result: Result<(), _> =
            conn.execute_batch(schema::MIGRATION_004).context("Migration 004");
        if let Err(e) = add_mode_result {
            let err_str = e.to_string();
            if !err_str.contains("duplicate column") && !err_str.contains("already exists") {
                tracing::warn!("Migration 004 warning (non-fatal): {}", e);
            } else {
                tracing::debug!("broadcast_mode column already exists, skipping migration 004");
            }
        }

        // Migration 005: deferred broadcast tracking columns
        let add_defer_result: Result<(), _> =
            conn.execute_batch(schema::MIGRATION_005).context("Migration 005");
        if let Err(e) = add_defer_result {
            let err_str = e.to_string();
            if !err_str.contains("duplicate column") && !err_str.contains("already exists") {
                tracing::warn!("Migration 005 warning (non-fatal): {}", e);
            } else {
                tracing::debug!("defer columns already exist, skipping migration 005");
            }
        }

        // Migration 006: fiat price trigger columns
        let add_price_result: Result<(), _> =
            conn.execute_batch(schema::MIGRATION_006).context("Migration 006");
        if let Err(e) = add_price_result {
            let err_str = e.to_string();
            if !err_str.contains("duplicate column") && !err_str.contains("already exists") {
                tracing::warn!("Migration 006 warning (non-fatal): {}", e);
            } else {
                tracing::debug!("price trigger columns already exist, skipping migration 006");
            }
        }

        // Migration 007: reclassify legacy imported rows (persisted as pending "immediate").
        if let Err(e) = conn.execute_batch(schema::MIGRATION_007) {
            tracing::warn!("Migration 007 warning (non-fatal): {}", e);
        }

        // Migration 008: per-row integrity MAC column.
        if let Err(e) = conn.execute_batch(schema::MIGRATION_008) {
            tracing::warn!("Migration 008 warning (non-fatal): {}", e);
        }

        // Migration 009: encrypted archive table for retired terminal-status transactions.
        if let Err(e) = conn.execute_batch(schema::MIGRATION_009) {
            tracing::warn!("Migration 009 warning (non-fatal): {}", e);
        }

        Ok(())
    }

    #[cfg(test)]
    pub fn run_data_migrations(&self) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute_batch(schema::MIGRATION_007).context("Migration 007")?;
        Ok(())
    }

    // ── Broadcast Pool ──────────────────────────────────────────────

    pub fn insert_broadcast_tx(&self, tx: &NewBroadcastTx) -> Result<BroadcastTx> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let status = TxStatus::Pending.as_str();
        let scheduled = tx.scheduled_time.map(|t| t.to_rfc3339());
        let nlocktime = tx.nlocktime.unwrap_or(0);
        let broadcast_mode = tx.broadcast_mode.clone().unwrap_or_else(|| "immediate".to_string());
        // Every add path (API import, electrum intercept, virtual-block) passes total_value_btc:
        // None; derive it from the tx's outputs so the pool-value stat isn't stuck at 0. An
        // explicit value (e.g. from the migration importer) is preserved.
        let total_value_btc = tx
            .total_value_btc
            .or_else(|| output_value_btc_from_hex(&tx.tx_hex))
            .unwrap_or(0.0);

        // Encrypt sensitive fields at rest. Computed AFTER the plaintext-dependent
        // total_value_btc derivation above, using the row id as AEAD associated data.
        let enc_tx_hex = crate::crypto::encode_field(&self.key, &tx.tx_hex, id.as_bytes());
        let enc_dest = tx.destination_address.as_ref()
            .map(|d| crate::crypto::encode_field(&self.key, d, id.as_bytes()));
        let enc_source = tx.source_label.as_ref()
            .map(|s| crate::crypto::encode_field(&self.key, s, id.as_bytes()));

        // Integrity MAC over the scheduling fields plus the encrypted sensitive fields, exactly
        // as they will land in the columns. schedule_trigger/target_price/price_condition are
        // not part of this INSERT (they take their schema defaults: 'datetime' / NULL / NULL),
        // so the same defaults are used here — must match map_broadcast_row's read-back exactly.
        let row_mac = hex::encode(crate::crypto::mac(
            &self.key,
            &row_mac_input(
                &id,
                status,
                &broadcast_mode,
                scheduled.as_deref().unwrap_or(""),
                nlocktime as i64,
                "",
                "datetime",
                "",
                &enc_tx_hex,
                enc_dest.as_deref().unwrap_or(""),
            ),
        ));

        {
            let conn = self.lock_conn()?;
            conn.execute(
                "INSERT INTO broadcast_pool (id, tx_hex, status, network, nlocktime, broadcast_mode, scheduled_time, target_fee_rate, source_label, destination_address, utxo_count, total_value_btc, replacement_of, row_mac, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    id,
                    enc_tx_hex,
                    status,
                    tx.network,
                    nlocktime,
                    broadcast_mode,
                    scheduled,
                    tx.target_fee_rate,
                    enc_source,
                    enc_dest,
                    tx.utxo_count.unwrap_or(1),
                    total_value_btc,
                    tx.replacement_of,
                    row_mac,
                    now,
                    now,
                ],
            )
            .context("Failed to insert broadcast tx")?;
        }

        self.get_broadcast_tx_by_id(&id)
    }

    pub fn get_broadcast_tx_by_id(&self, id: &str) -> Result<BroadcastTx> {
        let conn = self.lock_conn()?;
        conn.query_row(
            &format!("SELECT {BROADCAST_SELECT} FROM broadcast_pool WHERE id = ?1"),
            params![id],
            |row| map_broadcast_row(row, &self.key),
        )
        .context("Failed to get broadcast tx")
    }

    /// Recompute `row_mac` from the CURRENTLY stored MAC-covered column values for `id` and
    /// rewrite it. Must be called by every mutator that changes a MAC-covered column
    /// (status, broadcast_mode, scheduled_time, nlocktime, target_price, schedule_trigger,
    /// price_condition, tx_hex, destination_address) so a legitimate write doesn't leave
    /// `row_mac` stale and get flagged as tampered on the next read.
    ///
    /// Callers MUST have already released their own `lock_conn()` guard before calling this —
    /// `Mutex<Connection>` is not reentrant, so an overlapping lock would deadlock.
    fn reseal_row_mac(&self, id: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        let (status, mode, scheduled, nlocktime, target_price, schedule_trigger, price_condition, enc_tx_hex, enc_dest): (
            String,
            Option<String>,
            Option<String>,
            i64,
            Option<f64>,
            Option<String>,
            Option<String>,
            String,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT status, broadcast_mode, scheduled_time, nlocktime, target_price, schedule_trigger, price_condition, tx_hex, destination_address FROM broadcast_pool WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ))
                },
            )
            .context("Failed to load row for row_mac reseal")?;

        let row_mac = hex::encode(crate::crypto::mac(
            &self.key,
            &row_mac_input(
                id,
                &status,
                mode.as_deref().unwrap_or(""),
                scheduled.as_deref().unwrap_or(""),
                nlocktime,
                &target_price.map(|p| p.to_string()).unwrap_or_default(),
                schedule_trigger.as_deref().unwrap_or(""),
                price_condition.as_deref().unwrap_or(""),
                &enc_tx_hex,
                enc_dest.as_deref().unwrap_or(""),
            ),
        ));

        conn.execute(
            "UPDATE broadcast_pool SET row_mac = ?1 WHERE id = ?2",
            params![row_mac, id],
        )
        .context("Failed to reseal row_mac")?;

        Ok(())
    }

    pub fn list_broadcast_txs(&self, status_filter: Option<&str>, network: &str, limit: i32) -> Result<Vec<BroadcastTx>> {
        let conn = self.lock_conn()?;

        let map_row = |row: &rusqlite::Row| map_broadcast_row(row, &self.key);

        let mut txs = Vec::new();

        if let Some(status) = status_filter {
            let sql = format!("SELECT {BROADCAST_SELECT} FROM broadcast_pool WHERE network = ?1 AND status = ?2 ORDER BY created_at DESC LIMIT ?3");
            let mut stmt = conn.prepare(&sql).context("Failed to prepare list query")?;
            let rows = stmt.query_map(rusqlite::params![network, status, limit], map_row)
                .map_err(|e| anyhow::anyhow!("Failed to query broadcast txs: {}", e))?;
            for row in rows {
                txs.push(row.context("Failed to read row")?);
            }
        } else {
            let sql = format!("SELECT {BROADCAST_SELECT} FROM broadcast_pool WHERE network = ?1 ORDER BY created_at DESC LIMIT ?2");
            let mut stmt = conn.prepare(&sql).context("Failed to prepare list query")?;
            let rows = stmt.query_map(rusqlite::params![network, limit], map_row)
                .map_err(|e| anyhow::anyhow!("Failed to query broadcast txs: {}", e))?;
            for row in rows {
                txs.push(row.context("Failed to read row")?);
            }
        }

        Ok(txs)
    }

    pub fn update_tx_status(&self, id: &str, status: TxStatus, error: Option<&str>) -> Result<()> {
        {
            let conn = self.lock_conn()?;
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE broadcast_pool SET status = ?1, error_message = ?2, updated_at = ?3 WHERE id = ?4",
                params![status.as_str(), error, now, id],
            )
            .context("Failed to update tx status")?;
        }
        self.reseal_row_mac(id)?;
        Ok(())
    }

    /// Move a failed tx back to scheduled so the scheduler can retry broadcast.
    pub fn reset_failed_to_scheduled(&self, id: &str) -> Result<()> {
        {
            let conn = self.lock_conn()?;
            let now = Utc::now().to_rfc3339();
            let updated = conn
                .execute(
                    "UPDATE broadcast_pool SET status = 'scheduled', error_message = NULL, updated_at = ?1 WHERE id = ?2 AND status = 'failed'",
                    params![now, id],
                )
                .context("Failed to reset failed tx to scheduled")?;
            if updated == 0 {
                anyhow::bail!("Transaction {} is not in failed state", id);
            }
        }
        self.reseal_row_mac(id)?;
        Ok(())
    }

    pub fn mark_broadcast(&self, id: &str, txid: &str, fee_rate: f64) -> Result<()> {
        {
            let conn = self.lock_conn()?;
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE broadcast_pool SET status = 'broadcast', txid = ?1, actual_fee_rate = ?2, broadcast_at = ?3, updated_at = ?3, broadcast_missed_at = NULL, defer_until = NULL WHERE id = ?4",
                params![txid, fee_rate, now, id],
            )
            .context("Failed to mark as broadcast")?;
        }
        self.reseal_row_mac(id)?;
        Ok(())
    }

    pub fn get_tx_hex_by_txid(&self, txid: &str) -> Result<Option<String>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare("SELECT id, tx_hex FROM broadcast_pool WHERE txid = ?1 AND status IN ('pending', 'scheduled') LIMIT 1")
            .context("Failed to prepare get_tx_hex query")?;
        let mut rows = stmt
            .query_map(params![txid], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
            .context("Failed to query tx_hex by txid")?;
        match rows.next() {
            Some(Ok((id, tx_hex))) => {
                let decoded = crate::crypto::decode_field(self.key(), &tx_hex, id.as_bytes())
                    .context("Failed to decrypt tx_hex")?;
                Ok(Some(decoded))
            }
            _ => Ok(None),
        }
    }

    pub fn mark_confirmed(&self, id: &str, block_height: u64) -> Result<()> {
        {
            let conn = self.lock_conn()?;
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE broadcast_pool SET status = 'confirmed', block_height = ?1, confirmed_at = ?2, updated_at = ?2 WHERE id = ?3",
                params![block_height, now, id],
            )
            .context("Failed to mark as confirmed")?;
        }
        self.reseal_row_mac(id)?;
        Ok(())
    }

    pub fn get_due_transactions(&self, network: &str) -> Result<Vec<BroadcastTx>> {
        let conn = self.lock_conn()?;
        let sql = format!(
            "SELECT {BROADCAST_SELECT} FROM broadcast_pool WHERE status = 'scheduled' AND network = ?1"
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("Failed to prepare due query")?;

        let rows = stmt
            .query_map(params![network], |row| map_broadcast_row(row, &self.key))
            .context("Failed to query due txs")?;

        let mut txs = Vec::new();
        for row in rows {
            txs.push(row.context("Failed to read row")?);
        }
        Ok(txs)
    }

    pub fn record_broadcast_miss(
        &self,
        id: &str,
        missed_at: &str,
        original_scheduled: Option<&str>,
    ) -> Result<()> {
        let conn = self.lock_conn()?;
        let now = Utc::now().to_rfc3339();
        if let Some(orig) = original_scheduled {
            conn.execute(
                "UPDATE broadcast_pool SET broadcast_missed_at = ?1, original_scheduled_time = COALESCE(original_scheduled_time, ?2), updated_at = ?3 WHERE id = ?4 AND broadcast_missed_at IS NULL",
                params![missed_at, orig, now, id],
            )
            .context("Failed to record broadcast miss")?;
        } else {
            conn.execute(
                "UPDATE broadcast_pool SET broadcast_missed_at = ?1, updated_at = ?2 WHERE id = ?3 AND broadcast_missed_at IS NULL",
                params![missed_at, now, id],
            )
            .context("Failed to record broadcast miss")?;
        }
        Ok(())
    }

    pub fn update_reschedule(
        &self,
        id: &str,
        scheduled_time: &str,
        defer_until: Option<&str>,
        fee_rate: f64,
    ) -> Result<()> {
        {
            let conn = self.lock_conn()?;
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE broadcast_pool SET status = 'scheduled', scheduled_time = ?1, defer_until = ?2, target_fee_rate = ?3, error_message = NULL, schedule_trigger = 'datetime', target_price = NULL, price_currency = NULL, price_condition = NULL, updated_at = ?4 WHERE id = ?5",
                params![scheduled_time, defer_until, fee_rate, now, id],
            )
            .context("Failed to update reschedule")?;
        }
        self.reseal_row_mac(id)?;
        Ok(())
    }

    pub fn update_price_schedule(
        &self,
        id: &str,
        target_price: f64,
        price_currency: &str,
        price_condition: &str,
        fee_rate: f64,
    ) -> Result<()> {
        {
            let conn = self.lock_conn()?;
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE broadcast_pool SET status = 'pending', scheduled_time = NULL, defer_until = NULL, target_fee_rate = ?1, error_message = NULL, schedule_trigger = 'price', target_price = ?2, price_currency = ?3, price_condition = ?4, updated_at = ?5 WHERE id = ?6",
                params![fee_rate, target_price, price_currency, price_condition, now, id],
            )
            .context("Failed to update price schedule")?;
        }
        self.reseal_row_mac(id)?;
        Ok(())
    }

    pub fn get_price_triggered_pending(&self, network: &str) -> Result<Vec<BroadcastTx>> {
        let conn = self.lock_conn()?;
        let sql = format!(
            "SELECT {BROADCAST_SELECT} FROM broadcast_pool WHERE status IN ('pending', 'scheduled') AND schedule_trigger = 'price' AND target_price IS NOT NULL AND network = ?1"
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("Failed to prepare price-triggered query")?;

        let rows = stmt
            .query_map(params![network], |row| map_broadcast_row(row, &self.key))
            .context("Failed to query price-triggered txs")?;

        let mut txs = Vec::new();
        for row in rows {
            txs.push(row.context("Failed to read row")?);
        }
        Ok(txs)
    }

    pub fn get_pending_by_block_height(&self, network: &str) -> Result<Vec<BroadcastTx>> {
        let conn = self.lock_conn()?;
        let sql = format!(
            "SELECT {BROADCAST_SELECT} FROM broadcast_pool WHERE status = 'pending' AND broadcast_mode = 'by_block' AND nlocktime > 0 AND nlocktime < 500000000 AND network = ?1"
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("Failed to prepare pending by block height query")?;

        let rows = stmt
            .query_map(params![network], |row| map_broadcast_row(row, &self.key))
            .context("Failed to query pending by block height txs")?;

        let mut txs = Vec::new();
        for row in rows {
            txs.push(row.context("Failed to read row")?);
        }
        Ok(txs)
    }

    pub fn get_pending_by_scheduled_time(&self, network: &str) -> Result<Vec<BroadcastTx>> {
        let conn = self.lock_conn()?;
        let now = Utc::now();
        let sql = format!(
            "SELECT {BROADCAST_SELECT} FROM broadcast_pool WHERE status = 'pending' AND broadcast_mode IN ('scheduled', 'manual', 'imported') AND scheduled_time IS NOT NULL AND network = ?1"
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("Failed to prepare pending by scheduled time query")?;

        let rows = stmt
            .query_map(params![network], |row| map_broadcast_row(row, &self.key))
            .context("Failed to query pending by scheduled time txs")?;

        let mut txs = Vec::new();
        for row in rows {
            let tx = row.context("Failed to read row")?;
            if tx
                .scheduled_time
                .as_ref()
                .is_some_and(|t| *t <= now)
            {
                txs.push(tx);
            }
        }
        Ok(txs)
    }

    pub fn get_pending_rebroadcast(&self, interval_minutes: i32, network: &str) -> Result<Vec<BroadcastTx>> {
        let conn = self.lock_conn()?;
        let cutoff = Utc::now()
            .checked_sub_signed(chrono::Duration::minutes(interval_minutes as i64))
            .ok_or_else(|| anyhow::anyhow!("Invalid rebroadcast interval"))?
            .to_rfc3339();

        let sql = format!(
            "SELECT {BROADCAST_SELECT} FROM broadcast_pool WHERE status = 'broadcast' AND confirmed_at IS NULL AND (broadcast_at IS NULL OR broadcast_at < ?1) AND network = ?2"
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("Failed to prepare rebroadcast query")?;

        let rows = stmt
            .query_map(params![cutoff, network], |row| map_broadcast_row(row, &self.key))
            .context("Failed to query rebroadcast txs")?;

        let mut txs = Vec::new();
        for row in rows {
            txs.push(row.context("Failed to read row")?);
        }
        Ok(txs)
    }

    pub fn mark_due(&self, id: &str) -> Result<()> {
        {
            let conn = self.lock_conn()?;
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE broadcast_pool SET status = 'scheduled', scheduled_time = ?1, updated_at = ?1 WHERE id = ?2",
                params![now, id],
            )
            .context("Failed to mark tx as due")?;
        }
        self.reseal_row_mac(id)?;
        Ok(())
    }

    /// Price trigger fired: ready for broadcast loop (clears price-only waiting state).
    pub fn mark_due_from_price_trigger(&self, id: &str) -> Result<()> {
        {
            let conn = self.lock_conn()?;
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE broadcast_pool SET status = 'scheduled', scheduled_time = ?1, schedule_trigger = 'datetime', updated_at = ?2 WHERE id = ?3",
                params![now, now, id],
            )
            .context("Failed to mark price-triggered tx as due")?;
        }
        self.reseal_row_mac(id)?;
        Ok(())
    }

    pub fn mark_due_with_schedule(&self, id: &str, scheduled_time: &DateTime<Utc>) -> Result<()> {
        {
            let conn = self.lock_conn()?;
            let now = Utc::now().to_rfc3339();
            let scheduled = scheduled_time.to_rfc3339();
            conn.execute(
                "UPDATE broadcast_pool SET status = 'scheduled', scheduled_time = ?1, updated_at = ?2 WHERE id = ?3",
                params![scheduled, now, id],
            )
            .context("Failed to mark tx as due with schedule")?;
        }
        self.reseal_row_mac(id)?;
        Ok(())
    }

    pub fn remove_broadcast_tx(&self, id: &str) -> Result<usize> {
        let conn = self.lock_conn()?;
        let n = conn
            .execute("DELETE FROM broadcast_pool WHERE id = ?1", params![id])
            .context("Failed to remove broadcast tx")?;
        Ok(n)
    }

    pub fn get_pool_stats(&self, network: &str) -> Result<PoolStats> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT status, COUNT(*), COALESCE(SUM(total_value_btc), 0.0)
                 FROM broadcast_pool WHERE network = ?1 GROUP BY status",
            )
            .context("Failed to prepare stats query")?;

        let mut stats = PoolStats {
            total_transactions: 0,
            pending: 0,
            scheduled: 0,
            broadcast: 0,
            confirmed: 0,
            failed: 0,
            total_value_btc: 0.0,
        };

        let rows = stmt
            .query_map(params![network], |row| {
                let status: String = row.get(0)?;
                let count: i32 = row.get(1)?;
                let value: f64 = row.get(2)?;
                Ok((status, count, value))
            })
            .context("Failed to query stats")?;

        for row in rows {
            let (status, count, value) = row?;
            stats.total_transactions += count as usize;
            // Failed txs never leave the pool, so their value doesn't count toward the
            // "waiting or broadcast" pool total (counts above still include them).
            if status != "failed" {
                stats.total_value_btc += value;
            }
            match status.as_str() {
                "pending" => stats.pending = count as usize,
                "scheduled" => stats.scheduled = count as usize,
                "broadcast" => stats.broadcast = count as usize,
                "confirmed" => stats.confirmed = count as usize,
                "failed" => stats.failed = count as usize,
                _ => {}
            }
        }

        Ok(stats)
    }

    // ── Migration Plans ─────────────────────────────────────────────

    pub fn insert_migration_plan(&self, plan: &NewMigrationPlan) -> Result<MigrationPlan> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let status = PlanStatus::Draft.as_str();

        {
            let conn = self.lock_conn()?;
            conn.execute(
                "INSERT INTO migration_plans (id, name, source_wallet, destination_wallet, network, status, min_delay_hours, max_delay_hours, min_fee_rate, max_fee_rate, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    id,
                    plan.name,
                    plan.source_wallet,
                    plan.destination_wallet,
                    plan.network,
                    status,
                    plan.min_delay_hours.unwrap_or(2),
                    plan.max_delay_hours.unwrap_or(72),
                    plan.min_fee_rate.unwrap_or(1.0),
                    plan.max_fee_rate.unwrap_or(50.0),
                    now,
                    now,
                ],
            )
            .context("Failed to insert migration plan")?;
        }

        self.get_migration_plan_by_id(&id)
    }

    pub fn get_migration_plan_by_id(&self, id: &str) -> Result<MigrationPlan> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT id, name, source_wallet, destination_wallet, network, status, min_delay_hours, max_delay_hours, min_fee_rate, max_fee_rate, total_transactions, completed_transactions, total_value_migrated_btc, created_at, updated_at
             FROM migration_plans WHERE id = ?1",
            params![id],
            |row| {
                Ok(MigrationPlan {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    source_wallet: row.get(2)?,
                    destination_wallet: row.get(3)?,
                    network: row.get(4)?,
                    status: PlanStatus::from_str(&row.get::<_, String>(5)?),
                    min_delay_hours: row.get(6)?,
                    max_delay_hours: row.get(7)?,
                    min_fee_rate: row.get(8)?,
                    max_fee_rate: row.get(9)?,
                    total_transactions: row.get(10)?,
                    completed_transactions: row.get(11)?,
                    total_value_migrated_btc: row.get(12)?,
                    created_at: parse_datetime(&row.get::<_, String>(13)?),
                    updated_at: parse_datetime(&row.get::<_, String>(14)?),
                })
            },
        )
        .context("Failed to get migration plan")
    }

    pub fn list_migration_plans(&self, network: &str) -> Result<Vec<MigrationPlan>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, name, source_wallet, destination_wallet, network, status, min_delay_hours, max_delay_hours, min_fee_rate, max_fee_rate, total_transactions, completed_transactions, total_value_migrated_btc, created_at, updated_at
                 FROM migration_plans WHERE network = ?1 ORDER BY created_at DESC",
            )
            .context("Failed to prepare list plans query")?;

        let rows = stmt
            .query_map(params![network], |row| {
                Ok(MigrationPlan {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    source_wallet: row.get(2)?,
                    destination_wallet: row.get(3)?,
                    network: row.get(4)?,
                    status: PlanStatus::from_str(&row.get::<_, String>(5)?),
                    min_delay_hours: row.get(6)?,
                    max_delay_hours: row.get(7)?,
                    min_fee_rate: row.get(8)?,
                    max_fee_rate: row.get(9)?,
                    total_transactions: row.get(10)?,
                    completed_transactions: row.get(11)?,
                    total_value_migrated_btc: row.get(12)?,
                    created_at: parse_datetime(&row.get::<_, String>(13)?),
                    updated_at: parse_datetime(&row.get::<_, String>(14)?),
                })
            })
            .context("Failed to query plans")?;

        let mut plans = Vec::new();
        for row in rows {
            plans.push(row.context("Failed to read row")?);
        }
        Ok(plans)
    }

    pub fn update_plan_status(&self, id: &str, status: PlanStatus) -> Result<()> {
        let conn = self.lock_conn()?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE migration_plans SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status.as_str(), now, id],
        )
        .context("Failed to update plan status")?;
        Ok(())
    }

    // ── Migration UTXOs ─────────────────────────────────────────────

    pub fn insert_migration_utxo(&self, plan_id: &str, utxo: &NewMigrationUtxo) -> Result<MigrationUtxo> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        {
            let conn = self.lock_conn()?;
            conn.execute(
                "INSERT INTO migration_utxos (id, plan_id, txid, vout, value_btc, address, label, source_label, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id,
                    plan_id,
                    utxo.txid,
                    utxo.vout,
                    utxo.value_btc,
                    utxo.address,
                    utxo.label,
                    utxo.source_label,
                    now,
                ],
            )
            .context("Failed to insert migration utxo")?;
        }

        self.get_migration_utxo_by_id(&id)
    }

    pub fn get_migration_utxo_by_id(&self, id: &str) -> Result<MigrationUtxo> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT id, plan_id, txid, vout, value_btc, address, label, source_label, broadcast_pool_id, created_at
             FROM migration_utxos WHERE id = ?1",
            params![id],
            |row| {
                Ok(MigrationUtxo {
                    id: row.get(0)?,
                    plan_id: row.get(1)?,
                    txid: row.get(2)?,
                    vout: row.get(3)?,
                    value_btc: row.get(4)?,
                    address: row.get(5)?,
                    label: row.get(6)?,
                    source_label: row.get(7)?,
                    broadcast_pool_id: row.get(8)?,
                    created_at: parse_datetime(&row.get::<_, String>(9)?),
                })
            },
        )
        .context("Failed to get migration utxo")
    }

    pub fn list_migration_utxos(&self, plan_id: &str) -> Result<Vec<MigrationUtxo>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, plan_id, txid, vout, value_btc, address, label, source_label, broadcast_pool_id, created_at
                 FROM migration_utxos WHERE plan_id = ?1 ORDER BY created_at ASC",
            )
            .context("Failed to prepare list utxos query")?;

        let rows = stmt
            .query_map(params![plan_id], |row| {
                Ok(MigrationUtxo {
                    id: row.get(0)?,
                    plan_id: row.get(1)?,
                    txid: row.get(2)?,
                    vout: row.get(3)?,
                    value_btc: row.get(4)?,
                    address: row.get(5)?,
                    label: row.get(6)?,
                    source_label: row.get(7)?,
                    broadcast_pool_id: row.get(8)?,
                    created_at: parse_datetime(&row.get::<_, String>(9)?),
                })
            })
            .context("Failed to query utxos")?;

        let mut utxos = Vec::new();
        for row in rows {
            utxos.push(row.context("Failed to read row")?);
        }
        Ok(utxos)
    }

    pub fn link_utxo_to_pool(&self, utxo_id: &str, pool_id: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE migration_utxos SET broadcast_pool_id = ?1 WHERE id = ?2",
            params![pool_id, utxo_id],
        )
        .context("Failed to link utxo to pool")?;
        Ok(())
    }

    pub fn update_plan_total(&self, plan_id: &str, total: i32) -> Result<()> {
        let conn = self.lock_conn()?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE migration_plans SET total_transactions = ?1, updated_at = ?2 WHERE id = ?3",
            params![total, now, plan_id],
        )
        .context("Failed to update plan total")?;
        Ok(())
    }

    pub fn execute_raw(&self, sql: &str, params: &[&dyn rusqlite::types::ToSql]) -> Result<usize> {
        let conn = self.lock_conn()?;
        conn.execute(sql, params).context("Failed to execute raw SQL")
    }

    // ── Config store ────────────────────────────────────────────────

    pub fn get_config_value(&self, key: &str) -> Result<Option<String>> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT value FROM config_store WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .context("Failed to read config value")
    }

    pub fn set_config_value(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO config_store (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value, now],
        )
        .context("Failed to upsert config value")?;
        Ok(())
    }

    // ── Encrypted archive ───────────────────────────────────────────

    pub fn insert_archive(&self, id: &str, network: &str, archived_at: &str, blob: &[u8]) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO archive_pool (id, network, archived_at, blob) VALUES (?1, ?2, ?3, ?4)",
            params![id, network, archived_at, blob],
        )
        .context("Failed to insert archive row")?;
        Ok(())
    }

    pub fn list_archive(&self, network: &str, limit: i64, offset: i64) -> Result<Vec<ArchiveMeta>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, network, archived_at FROM archive_pool WHERE network = ?1 ORDER BY archived_at DESC LIMIT ?2 OFFSET ?3",
            )
            .context("Failed to prepare archive list query")?;

        let rows = stmt
            .query_map(params![network, limit, offset], |row| {
                Ok(ArchiveMeta {
                    id: row.get(0)?,
                    network: row.get(1)?,
                    archived_at: row.get(2)?,
                })
            })
            .context("Failed to query archive list")?;

        let mut items = Vec::new();
        for row in rows {
            items.push(row.context("Failed to read archive row")?);
        }
        Ok(items)
    }

    pub fn get_archive_blob(&self, id: &str) -> Result<Option<Vec<u8>>> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT blob FROM archive_pool WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()
        .context("Failed to read archive blob")
    }

    // ── Retention ───────────────────────────────────────────────────

    /// Terminal-status (confirmed/failed/broadcast) rows not updated since `cutoff_rfc3339`,
    /// eligible for archival. Returned rows are decrypted with the pool's own key.
    pub fn select_terminal_older_than(&self, network: &str, cutoff_rfc3339: &str) -> Result<Vec<BroadcastTx>> {
        let conn = self.lock_conn()?;
        let sql = format!(
            "SELECT {BROADCAST_SELECT} FROM broadcast_pool WHERE status IN ('confirmed', 'failed', 'broadcast') AND updated_at < ?1 AND network = ?2"
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("Failed to prepare terminal-older-than query")?;

        let rows = stmt
            .query_map(params![cutoff_rfc3339, network], |row| {
                map_broadcast_row(row, self.key())
            })
            .context("Failed to query terminal-older-than txs")?;

        let mut txs = Vec::new();
        for row in rows {
            txs.push(row.context("Failed to read row")?);
        }
        Ok(txs)
    }

    pub fn delete_broadcast_tx(&self, id: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute("DELETE FROM broadcast_pool WHERE id = ?1", params![id])
            .context("Failed to delete broadcast tx")?;
        Ok(())
    }

    /// Moves terminal-status rows older than `cutoff_rfc3339` into the encrypted archive.
    ///
    /// Each row is sealed with a brand-new, opaque archive id (never the original pool id,
    /// so archive rows can't be correlated back to pool history at a glance) used as AEAD aad.
    /// The insert-into-archive + delete-from-pool pair for a given row runs inside a single
    /// SQLite transaction so a crash can never drop a pool row without its archive blob having
    /// been durably written first. `crypto::seal` runs before the write transaction opens, so
    /// the connection mutex is never held across the (CPU-bound) sealing step.
    pub fn archive_terminal_older_than(
        &self,
        key: &[u8; 32],
        network: &str,
        cutoff_rfc3339: &str,
    ) -> Result<usize> {
        let txs = self.select_terminal_older_than(network, cutoff_rfc3339)?;
        let mut count = 0usize;

        for tx in txs {
            let json = serde_json::to_vec(&tx).context("Failed to serialize tx for archive")?;
            let archive_id = Uuid::new_v4().to_string();
            let blob = crate::crypto::seal(key, &json, archive_id.as_bytes());
            let archived_at = Utc::now().to_rfc3339();

            let mut conn = self.lock_conn()?;
            let txn = conn
                .transaction()
                .context("Failed to start archive transaction")?;
            txn.execute(
                "INSERT INTO archive_pool (id, network, archived_at, blob) VALUES (?1, ?2, ?3, ?4)",
                params![archive_id, network, archived_at, blob],
            )
            .context("Failed to insert archive row")?;
            txn.execute("DELETE FROM broadcast_pool WHERE id = ?1", params![tx.id])
                .context("Failed to delete broadcast tx")?;
            txn.commit().context("Failed to commit archive transaction")?;
            drop(conn);

            count += 1;
        }

        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_fields_are_encrypted_at_rest_and_decrypted_on_read() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("e.db")).unwrap();
        let hex = tx_hex_with_output_sats(&[10_000]);
        let new_tx = crate::db::models::NewBroadcastTx {
            tx_hex: hex.clone(), network: "testnet4".into(), nlocktime: None,
            broadcast_mode: Some("imported".into()), scheduled_time: None,
            target_fee_rate: None, source_label: Some("wallet-A".into()),
            destination_address: Some("tb1qexampleaddr".into()),
            utxo_count: Some(1), total_value_btc: None, replacement_of: None,
        };
        let stored = db.insert_broadcast_tx(&new_tx).unwrap();

        // Read-back returns plaintext.
        let got = db.get_broadcast_tx_by_id(&stored.id).unwrap();
        assert_eq!(got.tx_hex, hex);
        assert_eq!(got.destination_address.as_deref(), Some("tb1qexampleaddr"));
        assert_eq!(got.source_label.as_deref(), Some("wallet-A"));

        // Raw column is NOT plaintext.
        let raw: String = db.lock_conn().unwrap()
            .query_row("SELECT destination_address FROM broadcast_pool WHERE id=?1",
                params![stored.id], |r| r.get(0)).unwrap();
        assert!(raw.starts_with("enc:v1:"), "stored value must be encrypted, got {raw}");
        assert!(!raw.contains("tb1qexampleaddr"));
    }

    // Before Task 2, dashboard-imported txs were persisted with broadcast_mode = "immediate"
    // (the DB default for a None mode). A real "immediate" tx is broadcast at once and never
    // lingers as pending, so any row that is both "immediate" and "pending" is safely known to
    // be a legacy import; migration 007 reclassifies it as "imported".
    #[test]
    fn migration_reclassifies_pending_immediate_as_imported() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("m.db")).unwrap();
        // A pending tx stored under the old default mode "immediate" (how imports used to persist).
        let new_tx = crate::db::models::NewBroadcastTx {
            tx_hex: "00".into(), network: "testnet4".into(), nlocktime: None,
            broadcast_mode: Some("immediate".into()), scheduled_time: None,
            target_fee_rate: None, source_label: None, destination_address: None,
            utxo_count: Some(1), total_value_btc: None, replacement_of: None,
        };
        let id = db.insert_broadcast_tx(&new_tx).unwrap().id;
        db.run_data_migrations().unwrap();
        let tx = db.get_broadcast_tx_by_id(&id).unwrap();
        assert_eq!(tx.broadcast_mode.as_deref(), Some("imported"));
    }

    // Build a consensus-encodable tx (1 dummy input to avoid the 0-input/segwit-marker
    // ambiguity) with the given output values in sats; return its hex.
    #[cfg(test)]
    fn tx_hex_with_output_sats(sats: &[u64]) -> String {
        use bitcoin::absolute::LockTime;
        use bitcoin::transaction::Version;
        use bitcoin::{Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness};
        let tx = Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: sats
                .iter()
                .map(|s| TxOut {
                    value: Amount::from_sat(*s),
                    script_pubkey: ScriptBuf::new(),
                })
                .collect(),
        };
        hex::encode(bitcoin::consensus::serialize(&tx))
    }

    // When a tx is inserted without an explicit total_value_btc (all API/electrum/import paths
    // pass None), the value must be derived from the tx hex — the sum of its outputs — instead of
    // silently defaulting to 0. Otherwise the dashboard's pool-value stat is always 0.
    #[test]
    fn insert_derives_total_value_from_outputs_when_none() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("v.db")).unwrap();
        let hex = tx_hex_with_output_sats(&[12_345, 67_890]); // 80_235 sats total
        let new_tx = crate::db::models::NewBroadcastTx {
            tx_hex: hex, network: "testnet4".into(), nlocktime: None,
            broadcast_mode: Some("imported".into()), scheduled_time: None,
            target_fee_rate: None, source_label: None, destination_address: None,
            utxo_count: Some(1), total_value_btc: None, replacement_of: None,
        };
        let stored = db.insert_broadcast_tx(&new_tx).unwrap();
        assert_eq!((stored.total_value_btc * 1e8).round() as u64, 80_235);
    }

    // An explicitly provided value must be preserved (not overwritten by the hex-derived sum),
    // since the migration importer sets total_value_btc from its own source.
    #[test]
    fn insert_preserves_explicit_total_value() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("ve.db")).unwrap();
        let hex = tx_hex_with_output_sats(&[50_000]);
        let new_tx = crate::db::models::NewBroadcastTx {
            tx_hex: hex, network: "testnet4".into(), nlocktime: None,
            broadcast_mode: Some("imported".into()), scheduled_time: None,
            target_fee_rate: None, source_label: None, destination_address: None,
            utxo_count: Some(1), total_value_btc: Some(1.5), replacement_of: None,
        };
        let stored = db.insert_broadcast_tx(&new_tx).unwrap();
        assert_eq!(stored.total_value_btc, 1.5);
    }

    // get_tx_hex_by_txid serves the raw hex over the wallet-facing electrum transaction.get
    // path; it must decrypt the stored ciphertext, not hand back "enc:v1:..." verbatim.
    #[test]
    fn get_tx_hex_by_txid_returns_decrypted_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.db")).unwrap();
        let hex = tx_hex_with_output_sats(&[10_000]);
        let new_tx = crate::db::models::NewBroadcastTx {
            tx_hex: hex.clone(), network: "testnet4".into(), nlocktime: None,
            broadcast_mode: Some("imported".into()), scheduled_time: None,
            target_fee_rate: None, source_label: None, destination_address: None,
            utxo_count: Some(1), total_value_btc: None, replacement_of: None,
        };
        let stored = db.insert_broadcast_tx(&new_tx).unwrap();
        let txid = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        db.lock_conn().unwrap()
            .execute("UPDATE broadcast_pool SET txid = ?1 WHERE id = ?2", params![txid, stored.id])
            .unwrap();

        let got = db.get_tx_hex_by_txid(txid).unwrap();
        assert_eq!(got, Some(hex), "must return decrypted plaintext hex");
        assert!(!got.unwrap().starts_with("enc:"), "must not leak ciphertext prefix");
    }

    // Rows persisted before value-derivation existed have total_value_btc = 0. The backfill
    // recomputes them from the stored hex; a row that already has a value is left untouched.
    #[test]
    fn backfill_recomputes_zero_value_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("b.db")).unwrap();
        let new_tx = crate::db::models::NewBroadcastTx {
            tx_hex: tx_hex_with_output_sats(&[123_456]), network: "testnet4".into(), nlocktime: None,
            broadcast_mode: Some("imported".into()), scheduled_time: None,
            target_fee_rate: None, source_label: None, destination_address: None,
            utxo_count: Some(1), total_value_btc: None, replacement_of: None,
        };
        let stored = db.insert_broadcast_tx(&new_tx).unwrap();
        // Simulate a legacy row: force its value back to 0.
        db.lock_conn().unwrap()
            .execute("UPDATE broadcast_pool SET total_value_btc = 0 WHERE id = ?1", params![stored.id])
            .unwrap();

        let updated = db.backfill_output_values().unwrap();
        assert_eq!(updated, 1);
        let row = db.get_broadcast_tx_by_id(&stored.id).unwrap();
        assert_eq!((row.total_value_btc * 1e8).round() as u64, 123_456);
    }

    // The pool-value stat counts sats waiting to be broadcast and already broadcast, but NOT
    // failed txs (they will never leave the pool). Counts still include every row.
    #[test]
    fn pool_stats_value_excludes_failed() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("s.db")).unwrap();
        let mk = || crate::db::models::NewBroadcastTx {
            tx_hex: tx_hex_with_output_sats(&[50_000]), network: "testnet4".into(), nlocktime: None,
            broadcast_mode: Some("imported".into()), scheduled_time: None,
            target_fee_rate: None, source_label: None, destination_address: None,
            utxo_count: Some(1), total_value_btc: None, replacement_of: None,
        };
        let keep = db.insert_broadcast_tx(&mk()).unwrap();
        let fail = db.insert_broadcast_tx(&mk()).unwrap();
        let _ = keep;
        db.update_tx_status(&fail.id, TxStatus::Failed, Some("boom")).unwrap();

        let stats = db.get_pool_stats("testnet4").unwrap();
        assert_eq!(stats.total_transactions, 2, "counts include failed rows");
        assert_eq!(stats.failed, 1);
        assert_eq!((stats.total_value_btc * 1e8).round() as u64, 50_000, "value excludes failed");
    }

    // The row_mac (HMAC over the scheduling fields + encrypted sensitive fields) must catch
    // direct tampering of a column via SQL (bypassing the app layer entirely).
    #[test]
    fn tampering_schedule_is_detected_by_row_mac() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.db")).unwrap();
        let new_tx = crate::db::models::NewBroadcastTx {
            tx_hex: tx_hex_with_output_sats(&[10_000]), network: "testnet4".into(),
            nlocktime: None, broadcast_mode: Some("scheduled".into()),
            scheduled_time: Some(chrono::Utc::now()), target_fee_rate: None,
            source_label: None, destination_address: None, utxo_count: Some(1),
            total_value_btc: None, replacement_of: None,
        };
        let stored = db.insert_broadcast_tx(&new_tx).unwrap();
        // Untampered read: not flagged.
        assert_ne!(db.get_broadcast_tx_by_id(&stored.id).unwrap().tampered, Some(true));
        // Tamper the scheduled_time column directly.
        db.lock_conn().unwrap().execute(
            "UPDATE broadcast_pool SET scheduled_time='1999-01-01T00:00:00Z' WHERE id=?1",
            params![stored.id]).unwrap();
        assert_eq!(db.get_broadcast_tx_by_id(&stored.id).unwrap().tampered, Some(true));
    }

    // A legitimate status transition (pending -> ... -> confirmed) must NOT be flagged as
    // tampered: row_mac has to be recomputed on every MAC-covered-column update, not just at
    // insert. Otherwise normal state transitions would falsely trip tamper detection and, via
    // Task 5's safe-mode, halt all broadcasting.
    #[test]
    fn status_update_does_not_falsely_flag_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("rm1.db")).unwrap();
        let new_tx = crate::db::models::NewBroadcastTx {
            tx_hex: tx_hex_with_output_sats(&[10_000]), network: "testnet4".into(),
            nlocktime: None, broadcast_mode: Some("imported".into()),
            scheduled_time: None, target_fee_rate: None,
            source_label: None, destination_address: None, utxo_count: Some(1),
            total_value_btc: None, replacement_of: None,
        };
        let stored = db.insert_broadcast_tx(&new_tx).unwrap();

        db.update_tx_status(&stored.id, TxStatus::Confirmed, None).unwrap();
        let tx = db.get_broadcast_tx_by_id(&stored.id).unwrap();
        assert_ne!(tx.tampered, Some(true), "legitimate status update must not be flagged as tampered");
        assert_eq!(tx.status, TxStatus::Confirmed);
    }

    // mark_broadcast changes `status` (a MAC-covered column) but not through update_tx_status;
    // it must reseal row_mac too.
    #[test]
    fn mark_broadcast_does_not_falsely_flag_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("rm2.db")).unwrap();
        let new_tx = crate::db::models::NewBroadcastTx {
            tx_hex: tx_hex_with_output_sats(&[10_000]), network: "testnet4".into(),
            nlocktime: None, broadcast_mode: Some("imported".into()),
            scheduled_time: None, target_fee_rate: None,
            source_label: None, destination_address: None, utxo_count: Some(1),
            total_value_btc: None, replacement_of: None,
        };
        let stored = db.insert_broadcast_tx(&new_tx).unwrap();

        db.mark_broadcast(&stored.id, "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef", 5.0).unwrap();
        let tx = db.get_broadcast_tx_by_id(&stored.id).unwrap();
        assert_ne!(tx.tampered, Some(true), "mark_broadcast must not be flagged as tampered");
        assert_eq!(tx.status, TxStatus::Broadcast);
    }

    // update_reschedule and update_price_schedule mutate scheduled_time/schedule_trigger/
    // target_price/price_condition — all MAC-covered. Both must reseal row_mac.
    #[test]
    fn reschedule_and_price_schedule_do_not_falsely_flag_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("rm3.db")).unwrap();
        let new_tx = crate::db::models::NewBroadcastTx {
            tx_hex: tx_hex_with_output_sats(&[10_000]), network: "testnet4".into(),
            nlocktime: None, broadcast_mode: Some("scheduled".into()),
            scheduled_time: Some(chrono::Utc::now()), target_fee_rate: None,
            source_label: None, destination_address: None, utxo_count: Some(1),
            total_value_btc: None, replacement_of: None,
        };
        let stored = db.insert_broadcast_tx(&new_tx).unwrap();

        let new_time = (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
        db.update_reschedule(&stored.id, &new_time, None, 3.0).unwrap();
        let tx = db.get_broadcast_tx_by_id(&stored.id).unwrap();
        assert_ne!(tx.tampered, Some(true), "reschedule must not be flagged as tampered");

        db.update_price_schedule(&stored.id, 50000.0, "USD", "above", 3.0).unwrap();
        let tx = db.get_broadcast_tx_by_id(&stored.id).unwrap();
        assert_ne!(tx.tampered, Some(true), "price schedule update must not be flagged as tampered");
    }

    // Running the migration twice must be safe (idempotent) — re-running it on an already
    // reclassified row must not error and must leave the row as "imported".
    #[test]
    fn migration_is_idempotent_when_run_twice() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("m2.db")).unwrap();
        let new_tx = crate::db::models::NewBroadcastTx {
            tx_hex: "00".into(), network: "testnet4".into(), nlocktime: None,
            broadcast_mode: Some("immediate".into()), scheduled_time: None,
            target_fee_rate: None, source_label: None, destination_address: None,
            utxo_count: Some(1), total_value_btc: None, replacement_of: None,
        };
        let id = db.insert_broadcast_tx(&new_tx).unwrap().id;
        db.run_data_migrations().unwrap();
        db.run_data_migrations().unwrap();
        let tx = db.get_broadcast_tx_by_id(&id).unwrap();
        assert_eq!(tx.broadcast_mode.as_deref(), Some("imported"));
    }

    // Startup migration must encrypt legacy plaintext rows (tx_hex not enc:-prefixed, or
    // row_mac NULL) and reseal row_mac so they read back correctly and are not flagged tampered.
    #[test]
    fn legacy_plaintext_rows_get_encrypted_and_macked() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("l.db")).unwrap();
        // Insert a row, then force its columns back to plaintext + null mac (simulate legacy).
        let stored = db.insert_broadcast_tx(&crate::db::models::NewBroadcastTx{
            tx_hex: tx_hex_with_output_sats(&[10_000]), network:"testnet4".into(), nlocktime:None,
            broadcast_mode:Some("scheduled".into()), scheduled_time:Some(chrono::Utc::now()),
            target_fee_rate:None, source_label:Some("L".into()), destination_address:Some("addrX".into()),
            utxo_count:Some(1), total_value_btc:None, replacement_of:None }).unwrap();
        db.lock_conn().unwrap().execute(
            "UPDATE broadcast_pool SET destination_address='addrX', row_mac=NULL WHERE id=?1",
            params![stored.id]).unwrap();

        let n = db.encrypt_legacy_rows().unwrap();
        assert_eq!(n, 1);
        // Now stored encrypted, read decrypts, and mac verifies (not tampered).
        let raw: String = db.lock_conn().unwrap().query_row(
            "SELECT destination_address FROM broadcast_pool WHERE id=?1", params![stored.id],
            |r| r.get(0)).unwrap();
        assert!(raw.starts_with("enc:v1:"));
        let got = db.get_broadcast_tx_by_id(&stored.id).unwrap();
        assert_eq!(got.destination_address.as_deref(), Some("addrX"));
        assert_ne!(got.tampered, Some(true));
    }

    // Running the legacy-encryption migration twice must be a no-op the second time.
    #[test]
    fn legacy_encryption_migration_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("l2.db")).unwrap();
        let stored = db.insert_broadcast_tx(&crate::db::models::NewBroadcastTx{
            tx_hex: tx_hex_with_output_sats(&[10_000]), network:"testnet4".into(), nlocktime:None,
            broadcast_mode:Some("scheduled".into()), scheduled_time:Some(chrono::Utc::now()),
            target_fee_rate:None, source_label:Some("L".into()), destination_address:Some("addrX".into()),
            utxo_count:Some(1), total_value_btc:None, replacement_of:None }).unwrap();
        db.lock_conn().unwrap().execute(
            "UPDATE broadcast_pool SET destination_address='addrX', row_mac=NULL WHERE id=?1",
            params![stored.id]).unwrap();

        assert_eq!(db.encrypt_legacy_rows().unwrap(), 1);
        assert_eq!(db.encrypt_legacy_rows().unwrap(), 0);
        let got = db.get_broadcast_tx_by_id(&stored.id).unwrap();
        assert_eq!(got.destination_address.as_deref(), Some("addrX"));
        assert_ne!(got.tampered, Some(true));
    }

    #[test]
    fn config_store_get_set() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("c.db")).unwrap();
        assert_eq!(db.get_config_value("k").unwrap(), None);
        db.set_config_value("k", "v").unwrap();
        assert_eq!(db.get_config_value("k").unwrap(), Some("v".into()));
        db.set_config_value("k", "v2").unwrap(); // upsert
        assert_eq!(db.get_config_value("k").unwrap(), Some("v2".into()));
    }

    #[test]
    fn archive_insert_list_get() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("a.db")).unwrap();
        db.insert_archive("id1", "testnet4", "2026-06-01T00:00:00Z", b"blob-bytes").unwrap();
        let list = db.list_archive("testnet4", 10, 0).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "id1");
        assert_eq!(db.get_archive_blob("id1").unwrap().unwrap(), b"blob-bytes");
    }

    #[test]
    fn retention_archives_terminal_and_deletes_from_pool() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("r.db")).unwrap();
        let archive_key = crate::crypto::generate_key();
        let stored = db
            .insert_broadcast_tx(&crate::db::models::NewBroadcastTx {
                tx_hex: tx_hex_with_output_sats(&[10_000]),
                network: "testnet4".into(),
                nlocktime: None,
                broadcast_mode: Some("scheduled".into()),
                scheduled_time: Some(chrono::Utc::now()),
                target_fee_rate: None,
                source_label: Some("testnet4-plaintext-marker".into()),
                destination_address: Some("testnet4-plaintext-marker-addr".into()),
                utxo_count: Some(1),
                total_value_btc: None,
                replacement_of: None,
            })
            .unwrap();
        db.update_tx_status(&stored.id, TxStatus::Confirmed, None).unwrap();
        db.lock_conn()
            .unwrap()
            .execute(
                "UPDATE broadcast_pool SET updated_at='2000-01-01T00:00:00Z' WHERE id=?1",
                params![stored.id],
            )
            .unwrap();

        let moved = db
            .archive_terminal_older_than(&archive_key, "testnet4", "2026-01-01T00:00:00Z")
            .unwrap();
        assert_eq!(moved, 1);
        assert!(db.get_broadcast_tx_by_id(&stored.id).is_err()); // gone from active pool
        let list = db.list_archive("testnet4", 10, 0).unwrap();
        assert_eq!(list.len(), 1);
        // Blob does not contain plaintext.
        let blob = db.get_archive_blob(&list[0].id).unwrap().unwrap();
        assert!(!String::from_utf8_lossy(&blob).contains("testnet4-plaintext-marker"));
    }
}

/// Sum a transaction's output values (from its raw hex) and return the total in BTC.
/// Returns None if the hex can't be decoded as a transaction.
fn output_value_btc_from_hex(tx_hex: &str) -> Option<f64> {
    let raw = hex::decode(tx_hex.trim()).ok()?;
    let tx: bitcoin::Transaction = bitcoin::consensus::deserialize(&raw).ok()?;
    let sats: u64 = tx.output.iter().map(|o| o.value.to_sat()).sum();
    Some(sats as f64 / 100_000_000.0)
}

fn parse_datetime(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

fn parse_optional_datetime(s: Option<String>) -> Option<DateTime<Utc>> {
    s.map(|s| parse_datetime(&s))
}

fn map_broadcast_row(row: &rusqlite::Row, key: &[u8; 32]) -> rusqlite::Result<BroadcastTx> {
    let id: String = row.get(0)?;
    let dec = |s: String| {
        crate::crypto::decode_field(key, &s, id.as_bytes()).unwrap_or_else(|e| {
            tracing::error!("field decrypt failed for {}: {}", id, e);
            s
        })
    };

    // Raw (still-encrypted) column values, needed both for decrypting below and for
    // recomputing row_mac exactly as it was computed at insert time.
    let raw_tx_hex: String = row.get(1)?;
    let status_raw: String = row.get::<_, String>(3)?;
    let nlocktime_raw: i64 = row.get(5)?;
    let broadcast_mode_raw: Option<String> = row.get(6)?;
    let scheduled_raw: Option<String> = row.get(7)?;
    let raw_dest: Option<String> = row.get(14)?;
    let schedule_trigger_raw: Option<String> = row.get(23)?;
    let target_price_raw: Option<f64> = row.get(24)?;
    let price_condition_raw: Option<String> = row.get(26)?;
    let stored_row_mac: Option<String> = row.get(29)?;

    let expected_mac = hex::encode(crate::crypto::mac(
        key,
        &row_mac_input(
            &id,
            &status_raw,
            broadcast_mode_raw.as_deref().unwrap_or(""),
            scheduled_raw.as_deref().unwrap_or(""),
            nlocktime_raw,
            &target_price_raw.map(|p| p.to_string()).unwrap_or_default(),
            schedule_trigger_raw.as_deref().unwrap_or(""),
            price_condition_raw.as_deref().unwrap_or(""),
            &raw_tx_hex,
            raw_dest.as_deref().unwrap_or(""),
        ),
    ));
    let tampered = Some(stored_row_mac.as_deref() != Some(expected_mac.as_str()));

    Ok(BroadcastTx {
        tx_hex: dec(raw_tx_hex),
        id: id.clone(),
        txid: row.get(2)?,
        status: TxStatus::from_str(&status_raw),
        network: row.get(4)?,
        nlocktime: row.get(5)?,
        broadcast_mode: broadcast_mode_raw,
        scheduled_time: parse_optional_datetime(scheduled_raw),
        broadcast_at: parse_optional_datetime(row.get::<_, Option<String>>(8)?),
        confirmed_at: parse_optional_datetime(row.get::<_, Option<String>>(9)?),
        block_height: row.get(10)?,
        target_fee_rate: row.get(11)?,
        actual_fee_rate: row.get(12)?,
        source_label: row.get::<_, Option<String>>(13)?.map(dec),
        destination_address: raw_dest.map(dec),
        utxo_count: row.get(15)?,
        total_value_btc: row.get(16)?,
        replacement_of: row.get(17)?,
        error_message: row.get(18)?,
        retry_count: row.get(19)?,
        broadcast_missed_at: parse_optional_datetime(row.get::<_, Option<String>>(20)?),
        original_scheduled_time: parse_optional_datetime(row.get::<_, Option<String>>(21)?),
        defer_until: parse_optional_datetime(row.get::<_, Option<String>>(22)?),
        schedule_trigger: schedule_trigger_raw,
        target_price: target_price_raw,
        price_currency: row.get(25)?,
        price_condition: price_condition_raw,
        created_at: parse_datetime(&row.get::<_, String>(27)?),
        updated_at: parse_datetime(&row.get::<_, String>(28)?),
        locktime_waiting: None,
        locktime_deferred: None,
        can_reschedule: None,
        chain_mtp: None,
        locktime_target: None,
        locktime_remaining_secs: None,
        locktime_satisfied: None,
        current_btc_price: None,
        tampered,
    })
}

use chrono::DateTime;
