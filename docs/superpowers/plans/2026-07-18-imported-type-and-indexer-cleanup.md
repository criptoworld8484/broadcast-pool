# Imported TX Type + Indexer Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the external/secondary indexer UI+wiring, and make imported txs a first-class "Importada" type schedulable by both date/time and price (parity with "Manual").

**Architecture:** `imported` becomes a scheduling-synonym of `manual` (identical eligibility everywhere), distinct only in the TYPE label. The external-indexer override is dead code after the `remove-indexer-fields` merge; this plan finishes deleting it. Auto-discovery and the env-pin path are untouched.

**Tech Stack:** Rust (axum, rusqlite), embedded HTML/JS dashboard, SQLite.

## Global Constraints

- Base branch: `feature/imported-type-and-cleanup` (already has Task A1 merged: secondary indexer removed).
- Keep `IndexerConfig.manual_override` and the `BROADCAST_POOL_INDEXER_URL` env-pin path — they remain the Umbrel/Start9 auto-config mechanism.
- Do NOT change behaviour of wallet txs (`immediate` / `scheduled` / `by_block`) or of `manual` txs.
- Run `cargo build` and `cargo test` after every code task; both must pass.
- Commit after each task with the shown message; end commit messages with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

---

### Task 1: Remove external-indexer dead code (Task A2)

**Files:**
- Modify: `src/api/mod.rs` (routes ~39-40, handlers `test_indexer` ~854, `discover_indexer` ~950, `save_config` branch ~555-608, `SaveConfigRequest` ~504)
- Modify: `src/api/dashboard.html` (i18n ~1291/~1417, dead JS block ~2412-2424)

**Interfaces:**
- Produces: nothing new; removes routes `/api/test-indexer`, `/api/discover-indexer` and `SaveConfigRequest.indexer_url` / `.indexer_use_ssl`.

- [ ] **Step 1: Remove the two routes**

In `src/api/mod.rs`, delete these lines (~39-40):
```rust
        .route("/api/test-indexer", post(test_indexer))
        .route("/api/discover-indexer", post(discover_indexer))
```

- [ ] **Step 2: Remove the `save_config` manual-override branch**

In `src/api/mod.rs`, the `indexer_updated` binding currently has three arms. Replace the whole `else if let Some(url) = req.indexer_url { … }` arm (lines ~555-608) so only these remain:
```rust
    let indexer_updated = if network_changed {
        tracing::info!("Network changed — scanning LAN for indexer on new network");
        let found = crate::discovery::apply_indexer_discovery(&mut config);
        found && config.indexer.is_some()
    } else {
        false
    };
```

- [ ] **Step 3: Remove request fields no longer used**

In `src/api/mod.rs`, in `struct SaveConfigRequest` (~504) delete the `indexer_url: Option<String>,` field and, if present, `indexer_use_ssl` (it was only read by the branch removed in Step 2 — confirm with `grep -n indexer_use_ssl src/api/mod.rs`; remove the field only if the sole remaining hit is its declaration).

- [ ] **Step 4: Remove the now-unused handlers**

In `src/api/mod.rs`, delete the entire `async fn test_indexer(…) { … }` (~854) and `async fn discover_indexer(…) { … }` (~950) function bodies, plus any request/response structs used **only** by them (e.g. a `TestIndexerRequest`/response near them — verify each struct has no other `grep` hits before deleting).

- [ ] **Step 5: Remove dead i18n keys and JS block in the dashboard**

In `src/api/dashboard.html`:
- On the two i18n lines (~1291 en, ~1417 es) delete the `cfg_indexer_external: …, cfg_indexer_test: …, cfg_indexer_discover: …` entries (keep `cfg_indexer_server`).
- Delete the dead block that references removed DOM (`cfg-external-indexer-group`, `btn-discover-indexer`, `indexer-fallback-hint`), ~2412-2424:
```javascript
            const externalGroup = document.getElementById('cfg-external-indexer-group');
            if (externalGroup) {
                externalGroup.style.display = appUmbrelMode ? 'none' : '';
            }
            const discoverBtn = document.getElementById('btn-discover-indexer');
            if (discoverBtn) {
                discoverBtn.style.display = appUmbrelMode ? 'none' : '';
            }
            const fallbackHint = document.getElementById('indexer-fallback-hint');
            if (fallbackHint) {
                fallbackHint.style.display = appUmbrelMode ? 'block' : 'none';
            }
```
Keep the `appUmbrelMode = !!cfg.umbrel_mode;` line above it.

- [ ] **Step 6: Build, and clean any unused-fn warnings**

Run: `cargo build 2>&1 | grep -E "^(error|warning: (unused|function).*(indexer))"`
Expected: no errors. If `normalize_indexer_url_with_scheme` / `resolve_working_indexer_url` / `extract_indexer_host` / `is_mistaken_umbrel_lan_override` become newly unused, they live in `discovery.rs` and may still be used by auto-discovery — only remove a function if `grep -rn "<fnname>" src/` shows no remaining caller.

- [ ] **Step 7: Verify no live external-indexer references remain**

Run: `grep -rniE "test-indexer|discover-indexer|cfg_indexer_external|cfg-external-indexer|indexer_url:\s*Option|req\.indexer_url" src/`
Expected: no matches (the degraded-banner `status.indexer_url` read and the read-only `indexer_url` status field are fine and will not match these patterns).

- [ ] **Step 8: Test + commit**

Run: `cargo test 2>&1 | tail -3`
Expected: PASS, 0 failed.
```bash
git add src/api/mod.rs src/api/dashboard.html
git commit -m "cleanup(indexer): remove external-indexer override UI, routes, and save branch

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Imported txs get their own mode, schedulable like manual (backend)

**Files:**
- Modify: `src/api/mod.rs` (`import_transaction` ~321-333)
- Modify: `src/pool/manager.rs` (add predicate; guards at ~137, ~177, ~806)
- Modify: `src/db/mod.rs` (due query ~374)
- Test: `src/pool/manager.rs` `#[cfg(test)]` module

**Interfaces:**
- Produces: `PoolManager` accepts and correctly schedules txs with `broadcast_mode = Some("imported")`.
- Consumes: existing `test_manager()` helper and `db.insert_broadcast_tx(&NewBroadcastTx)`.

- [ ] **Step 1: Write failing tests**

Add to the `#[cfg(test)] mod tests` in `src/pool/manager.rs`:
```rust
    fn insert_imported(pm: &PoolManager, nlocktime: Option<u64>) -> String {
        let new_tx = crate::db::models::NewBroadcastTx {
            tx_hex: "00".to_string(),
            network: "testnet4".to_string(),
            nlocktime,
            broadcast_mode: Some("imported".to_string()),
            scheduled_time: None,
            target_fee_rate: None,
            source_label: None,
            destination_address: None,
            utxo_count: Some(1),
            total_value_btc: None,
            replacement_of: None,
        };
        pm.get_db().insert_broadcast_tx(&new_tx).expect("insert").id
    }

    #[test]
    fn imported_tx_can_be_price_scheduled() {
        let (pm, _dir) = test_manager();
        let id = insert_imported(&pm, None);
        // Imported must be accepted by the price-trigger scheduler, exactly like manual.
        pm.schedule_by_price(&id, 100_000.0, "usd", "above", 5.0)
            .expect("imported tx should accept a price trigger");
        let tx = pm.get_db().get_broadcast_tx_by_id(&id).expect("reload");
        assert_eq!(tx.schedule_trigger.as_deref(), Some("price"));
    }

    #[test]
    fn immediate_tx_still_rejects_price_schedule() {
        let (pm, _dir) = test_manager();
        let new_tx = crate::db::models::NewBroadcastTx {
            tx_hex: "00".to_string(), network: "testnet4".to_string(), nlocktime: None,
            broadcast_mode: Some("immediate".to_string()), scheduled_time: None,
            target_fee_rate: None, source_label: None, destination_address: None,
            utxo_count: Some(1), total_value_btc: None, replacement_of: None,
        };
        let id = pm.get_db().insert_broadcast_tx(&new_tx).expect("insert").id;
        assert!(pm.schedule_by_price(&id, 100_000.0, "usd", "above", 5.0).is_err());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p broadcast-pool imported_tx_can_be_price_scheduled 2>&1 | tail -5`
Expected: FAIL — `schedule_by_price` bails with "only available for manual".

- [ ] **Step 3: Add the shared predicate**

In `src/pool/manager.rs`, near the top of `impl PoolManager` (or as a free `fn` above it), add:
```rust
/// A tx the user schedules by hand from the dashboard: retained wallet txs under manual mode and
/// dashboard-imported txs. Both offer date/time AND price triggers; they differ only in the label.
pub fn is_user_scheduled_mode(mode: Option<&str>) -> bool {
    matches!(mode, Some("manual") | Some("imported"))
}
```

- [ ] **Step 4: Use the predicate at the three manager guards**

In `src/pool/manager.rs`:
- `schedule_by_price` guard (~177): replace
  `if tx.broadcast_mode.as_deref() != Some("manual") {`
  with
  `if !is_user_scheduled_mode(tx.broadcast_mode.as_deref()) {`
  and update the bail message to `"Price trigger scheduling is only available for manual/imported (pending) transactions"`.
- `is_reschedule` in `schedule_at` (~137): replace the line
  `|| tx.broadcast_mode.as_deref() == Some("manual");`
  with
  `|| is_user_scheduled_mode(tx.broadcast_mode.as_deref());`
- `tx_has_broadcast_schedule` (~806): replace
  `tx.broadcast_mode.as_deref() == Some("manual")`
  with
  `is_user_scheduled_mode(tx.broadcast_mode.as_deref())`
  (keep the surrounding `|| tx.broadcast_mode.as_deref() == Some("scheduled")` etc.).

- [ ] **Step 5: Add `imported` to the due SQL**

In `src/db/mod.rs` `get_pending_by_scheduled_time` (~374), change
`broadcast_mode IN ('scheduled', 'manual')`
to
`broadcast_mode IN ('scheduled', 'manual', 'imported')`.

- [ ] **Step 6: Import sets the explicit mode**

In `src/api/mod.rs` `import_transaction` (~325), change `broadcast_mode: None,` to `broadcast_mode: Some("imported".to_string()),`.

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p broadcast-pool 2>&1 | tail -3`
Expected: PASS, 0 failed.

- [ ] **Step 8: Commit**
```bash
git add src/pool/manager.rs src/db/mod.rs src/api/mod.rs
git commit -m "feat(pool): treat imported txs like manual for scheduling (date/time + price)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Migrate existing imported rows

**Files:**
- Modify: `src/db/schema.rs` (add `MIGRATION_007`)
- Modify: `src/db/mod.rs` (wire migration after 006)
- Test: `src/db/mod.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: existing migration runner pattern (`conn.execute_batch(schema::MIGRATION_00N)`).

- [ ] **Step 1: Write failing test**

Add to `src/db/mod.rs` tests (create a `#[cfg(test)] mod tests` block if none exists, mirroring `manager.rs` test style with `tempfile::tempdir()`):
```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p broadcast-pool migration_reclassifies 2>&1 | tail -5`
Expected: FAIL — `run_data_migrations` does not exist.

- [ ] **Step 3: Add the migration SQL**

In `src/db/schema.rs` after `MIGRATION_006`:
```rust
pub const MIGRATION_007: &str = r#"
UPDATE broadcast_pool SET broadcast_mode = 'imported'
WHERE broadcast_mode = 'immediate' AND status = 'pending';
"#;
```

- [ ] **Step 4: Run it as an idempotent data migration**

In `src/db/mod.rs`, add a method and call it from the same init path that runs migrations 004-006 (right after the Migration 006 block, before `Ok(())`):
```rust
        // Migration 007: reclassify legacy imported rows (persisted as pending "immediate").
        if let Err(e) = conn.execute_batch(schema::MIGRATION_007) {
            tracing::warn!("Migration 007 warning (non-fatal): {}", e);
        }
```
And expose a test-callable wrapper so the test can invoke it directly:
```rust
    #[cfg(test)]
    pub fn run_data_migrations(&self) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute_batch(schema::MIGRATION_007).context("Migration 007")?;
        Ok(())
    }
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p broadcast-pool migration_reclassifies 2>&1 | tail -3`
Expected: PASS.

- [ ] **Step 6: Full test run + commit**

Run: `cargo test 2>&1 | tail -3`
Expected: PASS, 0 failed.
```bash
git add src/db/schema.rs src/db/mod.rs
git commit -m "feat(db): migration 007 — reclassify legacy pending 'immediate' rows as 'imported'

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: Dashboard — "Importada" label, badge, and scheduling gates

**Files:**
- Modify: `src/api/dashboard.html` (i18n ~1345/~1479 region, CSS ~409-412, TYPE render, `canUsePriceTrigger`/`shouldShowSchedule`/`shouldShowReschedule`)

**Interfaces:**
- Consumes: `tx.broadcast_mode` now carries `"imported"` for imported txs.

- [ ] **Step 1: Add badge CSS**

In `src/api/dashboard.html` after `.type-manual` (~412):
```css
        .type-imported { background: #4ECDC422; color: #4ECDC4; }
```
(distinct from `.type-manual`'s purple).

- [ ] **Step 2: Add i18n labels for the two types (en + es)**

In the English i18n object add `type_imported: 'Imported', type_manual: 'Manual',` and in the Spanish object add `type_imported: 'Importada', type_manual: 'Manual',` (place alongside other `cfg_`/`detail_` keys).

- [ ] **Step 3: Render the label via i18n in the TYPE column**

In the row template where the TYPE badge is built (`const typeLabel = tx.broadcast_mode || '-';`), replace with:
```javascript
                    const typeKey = tx.broadcast_mode ? `type_${tx.broadcast_mode}` : '';
                    const typeLabel = typeKey && i18n[currentLang][typeKey] ? t(typeKey) : (tx.broadcast_mode || '-');
                    const typeClass = tx.broadcast_mode ? `type-${tx.broadcast_mode}` : '';
```
(so `manual`→"Manual"/"Importada" via i18n, unknown modes fall back to the raw value.)

- [ ] **Step 4: Add a JS predicate and use it in the three gates**

In `src/api/dashboard.html`, above `canUsePriceTrigger`:
```javascript
        // Mirror of the backend is_user_scheduled_mode: manual and imported schedule identically.
        function isUserScheduledMode(mode) { return mode === 'manual' || mode === 'imported'; }
```
Then:
- `canUsePriceTrigger`: replace `if (tx.broadcast_mode !== 'manual') return false;` with `if (!isUserScheduledMode(tx.broadcast_mode)) return false;`
- `shouldShowSchedule`: replace `if (tx.broadcast_mode === 'manual') return !tx.scheduled_time;` with `if (isUserScheduledMode(tx.broadcast_mode)) return !tx.scheduled_time;`
- `shouldShowReschedule`: replace `if (tx.broadcast_mode === 'manual' && tx.status === 'pending') {` with `if (isUserScheduledMode(tx.broadcast_mode) && tx.status === 'pending') {`

- [ ] **Step 5: Build (embedded HTML must still compile into the binary)**

Run: `cargo build 2>&1 | tail -1`
Expected: `Finished`.

- [ ] **Step 6: Manual verification (no JS test harness in this repo)**

Run the binary or deploy to the node, then in the dashboard:
1. Import a tx → TYPE shows **"Importada"** with the teal badge.
2. Click **Programar** on it → both **Date & Time** and **Fiat Price** tabs appear.
3. Schedule by price → saves without the "only available for manual" error.
4. A manual tx still shows **"Manual"** and behaves as before.

- [ ] **Step 7: Commit**
```bash
git add src/api/dashboard.html
git commit -m "feat(dashboard): show imported txs as 'Importada' and allow price scheduling

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: Version bump to 0.3.20 and release prep

**Files:**
- Modify: `Cargo.toml` (version)
- Modify: `umbrel-app/sparrow-broadcast-pool/umbrel-app.yml` (version) and `.../docker-compose.yml` (image tag)
- Modify: `start9/` version files (per existing release convention)

**Interfaces:** none (release metadata only).

- [ ] **Step 1: Bump the crate version**

In `Cargo.toml` set `version = "0.3.20"`. Run `cargo build` to sync `Cargo.lock`.

- [ ] **Step 2: Bump Umbrel app + compose**

In `umbrel-app/sparrow-broadcast-pool/umbrel-app.yml` bump `version` to `0.3.20`; in `.../docker-compose.yml` bump the `broadcast-pool-umbrel` image tag to `0.3.20`.

- [ ] **Step 3: Bump Start9 package**

Update the Start9 version to `0.3.20` following the same fields the `0.3.19` release commit touched (`git show 0aa3741 --stat` shows the exact files).

- [ ] **Step 4: Verify + commit**

Run: `cargo build 2>&1 | tail -1` (Expected: `Finished`) and `cargo test 2>&1 | tail -3` (Expected: 0 failed).
```bash
git add Cargo.toml Cargo.lock umbrel-app start9
git commit -m "chore(release): 0.3.20 — imported tx type + external/secondary indexer cleanup

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

- [ ] **Step 5: Handoff for node testing**

Do NOT push tags or deploy automatically. Report to the user that the branch is ready; the release/deploy (tag → GHCR build → node pull) is user-driven per the release-flow memory.

---

## Self-Review notes

- **Spec coverage:** A2 external-indexer removal → Task 1. Imported mode + parity → Task 2. Migration → Task 3. TYPE label/badge + UI gates → Task 4. Version/release → Task 5. A1 (secondary) already merged. All spec sections covered.
- **Type consistency:** `is_user_scheduled_mode(Option<&str>) -> bool` (Rust) and `isUserScheduledMode(mode)` (JS) used consistently; `broadcast_mode = "imported"` string is the single source of truth end to end.
- **Placeholder scan:** none — every code step shows concrete code.
