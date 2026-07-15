# Secondary Indexer Third-Fallback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a user-configured secondary indexer (another node's electrs/Fulcrum on the LAN) as a third chain-clock + broadcast fallback, used only when the primary indexer AND Bitcoin Core are both unavailable.

**Architecture:** Extend `ChainSource`/`decide_chain_source` with a `SecondaryIndexer` state placed last. The 30s health poller probes the secondary lazily (only when the primary indexer is down). Broadcast gains the secondary as its final relay link. A new `secondary_indexer: Option<String>` config, a Settings field (accepts IP or URL), a degraded-mode banner, and a first-run pop-up wire it up.

**Tech Stack:** Rust (`bitcoin`, `bitcoincore-rpc`, `electrum-client`), axum, embedded HTML/JS dashboard.

## Global Constraints

- Fallback priority is strictly **Primary indexer → Bitcoin Core (synced) → Secondary indexer → None**. The secondary NEVER wins over a live primary or a synced Core.
- The secondary is used ONLY for the chain clock (height + MTP) and broadcast (`sendrawtransaction`). Do NOT route wallet address-history queries to it.
- **Lazy probing:** the secondary is probed ONLY when the primary indexer is down. While the primary is up, the external node is never contacted.
- The secondary is a NEW config `secondary_indexer: Option<String>`, distinct from the existing "External Indexer" (`config.indexer` with `manual_override`), which replaces the primary. Do not conflate them.
- The Settings/pop-up field accepts an IP, `host:port`, or a full `tcp://`/`ssl://` URL; normalize with `crate::discovery::normalize_indexer_url`.
- `#[serde(default)]` on the new config field so existing on-disk configs still parse.
- Run `cargo test` (currently 56 passing) after each task; keep it green.

---

### Task 1: Extend ChainSource + decide_chain_source + ChainHealth

**Files:**
- Modify: `src/pool/chain_health.rs` (enum, `decide_chain_source`, `ChainHealth`, existing tests)
- Modify: `src/pool/manager.rs:1010-1016` (`write_chain_health` call site)

**Interfaces:**
- Produces:
  - `ChainSource::SecondaryIndexer` variant (serde `snake_case` → `"secondary_indexer"`).
  - `decide_chain_source(indexer_up: bool, core_up: bool, core_ibd: bool, secondary_up: bool) -> ChainSource`.
  - `ChainHealth` fields `secondary_up: bool`, `secondary_configured: bool`.

- [ ] **Step 1: Update the failing tests**

In `src/pool/chain_health.rs`, replace the existing `decide_chain_source` tests and add secondary cases:

```rust
    #[test]
    fn indexer_wins_when_up() {
        assert_eq!(decide_chain_source(true, true, false, true), ChainSource::Indexer);
        assert_eq!(decide_chain_source(true, false, false, false), ChainSource::Indexer);
    }

    #[test]
    fn core_takes_over_when_indexer_down() {
        assert_eq!(decide_chain_source(false, true, false, true), ChainSource::BitcoinCore);
    }

    #[test]
    fn core_in_ibd_is_not_a_clock() {
        // Core in IBD is skipped; secondary (if up) takes over.
        assert_eq!(decide_chain_source(false, true, true, true), ChainSource::SecondaryIndexer);
        assert_eq!(decide_chain_source(false, true, true, false), ChainSource::None);
    }

    #[test]
    fn secondary_is_last_resort() {
        // Only when primary down AND core unusable.
        assert_eq!(decide_chain_source(false, false, false, true), ChainSource::SecondaryIndexer);
        // Never over a live primary or synced core.
        assert_eq!(decide_chain_source(true, false, false, true), ChainSource::Indexer);
        assert_eq!(decide_chain_source(false, true, false, true), ChainSource::BitcoinCore);
    }

    #[test]
    fn nothing_up_means_no_source() {
        assert_eq!(decide_chain_source(false, false, false, false), ChainSource::None);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p broadcast-pool chain_health -- --nocapture` (or `cargo test decide_chain_source secondary_is_last_resort`)
Expected: FAIL — arity mismatch / `SecondaryIndexer` undefined.

- [ ] **Step 3: Implement enum + function + struct fields**

In `src/pool/chain_health.rs`, add the variant:

```rust
pub enum ChainSource {
    Indexer,
    BitcoinCore,
    SecondaryIndexer,
    None,
}
```

Replace `decide_chain_source`:

```rust
/// A node still in initial block download reports a validated tip far behind the network, so a
/// height-locked tx could look due against a stale height. An IBD node is not a chain clock.
/// The secondary indexer is the last resort: only when the primary is down AND Core is unusable.
pub fn decide_chain_source(
    indexer_up: bool,
    core_up: bool,
    core_ibd: bool,
    secondary_up: bool,
) -> ChainSource {
    if indexer_up {
        ChainSource::Indexer
    } else if core_up && !core_ibd {
        ChainSource::BitcoinCore
    } else if secondary_up {
        ChainSource::SecondaryIndexer
    } else {
        ChainSource::None
    }
}
```

Add fields to `ChainHealth` (after `core_sync_pct`):

```rust
    /// Whether the secondary indexer responded on the last (lazy) probe.
    pub secondary_up: bool,
    /// Whether a secondary indexer URL is configured at all.
    pub secondary_configured: bool,
```

Add them to `impl Default for ChainHealth`:

```rust
            secondary_up: false,
            secondary_configured: false,
```

- [ ] **Step 4: Fix the write_chain_health call site**

In `src/pool/manager.rs`, update `write_chain_health` (line ~1013):

```rust
            health.source = decide_chain_source(
                health.indexer_up,
                health.core_up,
                health.core_ibd,
                health.secondary_up,
            );
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test 2>&1 | tail -3`
Expected: PASS — all green (56 + the updated/added tests).

- [ ] **Step 6: Commit**

```bash
git add src/pool/chain_health.rs src/pool/manager.rs
git commit -m "feat(chain-health): add SecondaryIndexer as last-resort chain source"
```

---

### Task 2: Config field `secondary_indexer`

**Files:**
- Modify: `src/config.rs` (`Config` struct, `default_config()`)
- Test: inline `#[cfg(test)]` in `src/config.rs`

**Interfaces:**
- Produces: `Config.secondary_indexer: Option<String>` (`#[serde(default)]`).

- [ ] **Step 1: Write the failing test**

Add to `src/config.rs` tests:

```rust
#[cfg(test)]
mod secondary_indexer_tests {
    use super::*;

    #[test]
    fn secondary_indexer_defaults_none_and_roundtrips() {
        let mut c = Config::default_config();
        assert!(c.secondary_indexer.is_none());
        c.secondary_indexer = Some("tcp://192.168.1.50:50001".to_string());
        let toml = toml::to_string(&c).expect("serialize");
        let back: Config = toml::from_str(&toml).expect("parse");
        assert_eq!(back.secondary_indexer.as_deref(), Some("tcp://192.168.1.50:50001"));
    }

    #[test]
    fn old_config_without_secondary_still_parses() {
        // A network+pool+privacy minimal config, no secondary_indexer key.
        let toml = r#"
            [network]
            type = "testnet4"
            [pool]
            max_size_kb = 300
            rebroadcast_interval_minutes = 30
            expiry_days = 14
            [privacy]
            use_tor = false
            tor_socks_port = 9050
            rotate_identity_per_tx = false
        "#;
        let c: Config = toml::from_str(toml).expect("parse");
        assert!(c.secondary_indexer.is_none());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test secondary_indexer_tests -- --nocapture`
Expected: FAIL — no field `secondary_indexer`.

- [ ] **Step 3: Add the field**

In `src/config.rs`, add to `struct Config` (after `indexer`):

```rust
    /// Optional third-fallback indexer on the LAN (another node), used only when the primary
    /// indexer AND Bitcoin Core are both unavailable. Distinct from `indexer` (which the External
    /// Indexer override replaces). "tcp://host:port" / "ssl://host:port".
    #[serde(default)]
    pub secondary_indexer: Option<String>,
```

In `default_config()`'s `Config { ... }` literal, add:

```rust
            secondary_indexer: None,
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test secondary_indexer_tests -- --nocapture`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add optional secondary_indexer URL"
```

---

### Task 3: Lazy secondary probe in the health poller

**Files:**
- Modify: `src/pool/manager.rs` — `refresh_chain_health` (probe section ~1057-1112), add a pure helper.
- Test: inline `#[cfg(test)]` in `src/pool/manager.rs` for the helper.

**Interfaces:**
- Consumes: `crate::config::Config.secondary_indexer`, `crate::discovery::normalize_indexer_url`, `ElectrumClient::new(&str)`, `ChainHealth.secondary_up/secondary_configured`.
- Produces: `fn should_probe_secondary(indexer_up: bool, core_up: bool, core_ibd: bool, secondary_configured: bool) -> bool`.

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` in `src/pool/manager.rs`:

```rust
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
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test secondary_probed_only_when -- --nocapture`
Expected: FAIL — `should_probe_secondary` undefined.

- [ ] **Step 3: Add the helper + wire the probe**

Add the pure helper near `should_disarm` in `src/pool/manager.rs` (module scope):

```rust
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
```

In `refresh_chain_health`, AFTER the `core_up/core_ibd/...` destructuring and BEFORE `self.write_chain_health(...)`, add the lazy secondary probe:

```rust
        // Third fallback: probe the secondary indexer only when the primary is down and Core is
        // unusable. Reads the URL from config; a fresh ElectrumClient per probe (rare path).
        let secondary_url = {
            let cfg = self.config.lock().ok();
            cfg.and_then(|c| c.secondary_indexer.clone())
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
                        let mtp = client.get_median_time_past().ok();
                        tracing::info!("Secondary indexer alive at {} (height {})", url, h);
                        (true, Some(h), mtp)
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
```

Then extend the `self.write_chain_health(|h| { ... })` closure to record the secondary and prefer sources in order (primary → core → secondary):

```rust
        self.write_chain_health(|h| {
            h.indexer_up = indexer_height.is_some();
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
```

Also warm the mtp cache from the secondary when it is the only source (after the existing core_mtp cache-warm block):

```rust
        if core_mtp.is_none() {
            if let Some(mtp) = secondary_mtp {
                if let Ok(mut cache) = self.mtp_cache.lock() {
                    *cache = Some((Instant::now(), mtp));
                }
            }
        }
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test 2>&1 | tail -3`
Expected: PASS — all green. Also `cargo build` clean.

- [ ] **Step 5: Commit**

```bash
git add src/pool/manager.rs
git commit -m "feat(pool): lazily probe the secondary indexer when primary+Core are down"
```

---

### Task 4: Broadcast fallback to the secondary

**Files:**
- Modify: `src/pool/manager.rs` — `broadcast_transaction` (~line 241).

**Interfaces:**
- Consumes: `config.secondary_indexer`, `crate::discovery::normalize_indexer_url`, `ElectrumClient::new`.

- [ ] **Step 1: Add the secondary as the final broadcast link**

In `broadcast_transaction`, replace the tail (from the `if let Some(ref rpc) = self.rpc` block through the final `bail!`) with:

```rust
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
```

Note: the existing code has `if let Some(ref rpc) = self.rpc { return rpc.broadcast_transaction(tx_hex); }` — this task changes it to fall through to the secondary on RPC failure rather than returning the RPC error directly.

- [ ] **Step 2: Verify build + suite**

Run: `cargo build 2>&1 | grep -E "^error" || echo clean` then `cargo test 2>&1 | tail -3`
Expected: build clean; all tests green (this path is exercised live in Task 7 — no unit test since it needs a real electrum peer).

- [ ] **Step 3: Commit**

```bash
git add src/pool/manager.rs
git commit -m "feat(pool): broadcast via secondary indexer when indexer+Core fail"
```

---

### Task 5: API — config read/write, status, validation

**Files:**
- Modify: `src/api/mod.rs` — `ConfigResponse`, `config_to_response`, `SaveConfigRequest`, `save_config`, `StatusResponse`, `get_status`.
- Test: inline `#[cfg(test)]` for the URL validation helper.

**Interfaces:**
- Consumes: `config.secondary_indexer`, `crate::discovery::{normalize_indexer_url, extract_indexer_host}`, `chain_health()`.
- Produces:
  - `fn normalize_secondary_indexer(raw: &str) -> Result<Option<String>, String>` (empty → Ok(None); invalid → Err; valid → Ok(Some(normalized))).
  - `ConfigResponse.secondary_indexer_url: String`.
  - `SaveConfigRequest.secondary_indexer: Option<String>`.
  - `StatusResponse.secondary_indexer_url: String`, `StatusResponse.secondary_up: bool`.

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` in `src/api/mod.rs`:

```rust
    #[test]
    fn secondary_indexer_normalization() {
        assert_eq!(normalize_secondary_indexer("").unwrap(), None);
        assert_eq!(normalize_secondary_indexer("   ").unwrap(), None);
        // IP or host:port normalizes to a tcp:// url with a port.
        let n = normalize_secondary_indexer("192.168.1.50:50001").unwrap().unwrap();
        assert!(n.contains("192.168.1.50:50001"), "got {}", n);
        // Garbage with no host → error.
        assert!(normalize_secondary_indexer("!!!").is_err());
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test secondary_indexer_normalization -- --nocapture`
Expected: FAIL — `normalize_secondary_indexer` undefined.

- [ ] **Step 3: Add the validator**

Add to `src/api/mod.rs` (module scope):

```rust
/// Normalize a user-entered secondary indexer (IP, host:port, or tcp/ssl URL). Empty → None
/// (clears it). A value with no resolvable host → Err with a message. Uses the same normalization
/// as the primary/external indexer so behaviour is consistent.
fn normalize_secondary_indexer(raw: &str) -> Result<Option<String>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let normalized = crate::discovery::normalize_indexer_url(trimmed);
    if crate::discovery::extract_indexer_host(&normalized).is_none() {
        return Err(format!("Dirección de indexador secundario no válida: {}", raw));
    }
    Ok(Some(normalized))
}
```

- [ ] **Step 4: Wire ConfigResponse**

Add to `ConfigResponse` (after the liana_vb fields):

```rust
    secondary_indexer_url: String,
```

Populate in `config_to_response`:

```rust
        secondary_indexer_url: config.secondary_indexer.clone().unwrap_or_default(),
```

- [ ] **Step 5: Wire SaveConfigRequest + save_config**

Add to `SaveConfigRequest`:

```rust
    secondary_indexer: Option<String>,
```

In `save_config`, after the Liana virtual-block handling, add:

```rust
    if let Some(raw) = req.secondary_indexer {
        let normalized = normalize_secondary_indexer(&raw)
            .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        config.secondary_indexer = normalized;
    }
```

- [ ] **Step 6: Expose in StatusResponse**

Add to `StatusResponse` (near `chain_source`):

```rust
    secondary_indexer_url: String,
    secondary_up: bool,
```

Populate in `get_status` from the same config lock + health snapshot already taken there (mirror the `indexer_url`/`liana_vb_*` pattern; the value comes from `cfg.secondary_indexer` and `health.secondary_up`):

```rust
        secondary_indexer_url: /* from cfg */ .secondary_indexer.clone().unwrap_or_default(),
        secondary_up: health.secondary_up,
```
Thread `secondary_indexer_url` through the `spawn_blocking` tuple exactly like `indexer_url`.

- [ ] **Step 7: Run to verify pass**

Run: `cargo test 2>&1 | tail -3`
Expected: PASS — all green; `cargo build` clean.

- [ ] **Step 8: Commit**

```bash
git add src/api/mod.rs
git commit -m "feat(api): read/write/validate secondary indexer; expose in status"
```

---

### Task 6: Dashboard — Settings field, banner, first-run pop-up

**Files:**
- Modify: `src/api/dashboard.html` — Settings (secondary field), status banner, first-run modal, EN/ES i18n, config load/save, JS.
- Test: JS syntax check.

**Interfaces:**
- Consumes: `/api/config` `secondary_indexer_url`; `/api/status` `chain_source`, `secondary_indexer_url`, `secondary_up`; existing `testIndexer()` / `/api/test-indexer`.

- [ ] **Step 1: Add the Settings field**

Inside the right-column stack cell (`id="cfg-delay-side"`), after the External Indexer block, add:

```html
                            <div style="margin-top: 22px;">
                                <label data-i18n="cfg_secondary_indexer">Secondary Indexer (optional)</label>
                                <div style="display: flex; gap: 8px; align-items: center;">
                                    <input type="text" id="cfg-secondary-indexer" placeholder="192.168.x.x:50001" style="flex: 1;">
                                    <button class="btn btn-outline" onclick="testSecondaryIndexer()" style="padding: 16px 20px; white-space: nowrap;" data-i18n="cfg_indexer_test">Test</button>
                                </div>
                                <div class="setting-hint" id="secondary-indexer-test-result" style="margin-top: 8px; display: none;"></div>
                                <div class="setting-hint" data-i18n="cfg_secondary_indexer_hint">Used only if the primary indexer AND Bitcoin Core both fail, so schedules keep running. Another node's electrs/Fulcrum on your LAN (IP or host:port).</div>
                            </div>
```

- [ ] **Step 2: Add the status banner container**

After the existing indexer banner (`id="indexer-banner"`), add:

```html
                <div id="secondary-banner" class="protocol-banner" style="display:none; border-color: var(--amber); margin-top: 12px;">
                    <h3 id="secondary-banner-title"></h3>
                    <p id="secondary-banner-text"></p>
                </div>
```

- [ ] **Step 3: Add the first-run pop-up modal**

Before the closing `</body>` (or alongside other overlays), add:

```html
    <div id="secondary-prompt-overlay" class="overlay" style="display:none;">
        <div class="modal" style="max-width: 520px;">
            <h2 data-i18n="secprompt_title">Add a backup indexer?</h2>
            <p data-i18n="secprompt_body">If you have another node with its own indexer (electrs/Fulcrum) on your LAN, you can set it as a backup. Broadcast Pool will use it only if your primary indexer and Bitcoin Core both fail, so your scheduled transactions still go out on time.</p>
            <input type="text" id="secprompt-url" placeholder="192.168.x.x:50001" style="width:100%; margin:14px 0; background:var(--bg); border:1px solid var(--border); color:var(--text); padding:14px 18px; border-radius:12px; font-family:var(--font-mono);">
            <div style="display:flex; gap:10px; justify-content:flex-end;">
                <button class="btn btn-outline" onclick="dismissSecondaryPrompt()" data-i18n="secprompt_later">Later</button>
                <button class="btn" onclick="saveSecondaryPrompt()" data-i18n="secprompt_save">Save</button>
            </div>
        </div>
    </div>
```

- [ ] **Step 4: Add i18n keys (EN)**

In the EN i18n object:

```js
                cfg_secondary_indexer: 'Secondary Indexer (optional)',
                cfg_secondary_indexer_hint: 'Used only if the primary indexer AND Bitcoin Core both fail, so schedules keep running. Another node’s electrs/Fulcrum on your LAN (IP or host:port).',
                sec_banner_title: 'Primary indexer & Bitcoin Core down — using secondary indexer',
                sec_banner_text: 'Reading block height and median time from your secondary indexer at {url}. Scheduled broadcasts keep running.',
                secprompt_title: 'Add a backup indexer?',
                secprompt_body: 'If you have another node with its own indexer (electrs/Fulcrum) on your LAN, you can set it as a backup. Broadcast Pool will use it only if your primary indexer and Bitcoin Core both fail, so your scheduled transactions still go out on time.',
                secprompt_later: 'Later',
                secprompt_save: 'Save',
```

- [ ] **Step 5: Add i18n keys (ES)**

In the ES i18n object:

```js
                cfg_secondary_indexer: 'Indexador secundario (opcional)',
                cfg_secondary_indexer_hint: 'Se usa solo si el indexador principal Y Bitcoin Core fallan, para que las programaciones sigan cumpliéndose. El electrs/Fulcrum de otro nodo de tu LAN (IP o host:puerto).',
                sec_banner_title: 'Indexador principal y Bitcoin Core caídos — usando indexador secundario',
                sec_banner_text: 'Leyendo la altura de bloque y el tiempo mediano de tu indexador secundario en {url}. Las difusiones programadas siguen cumpliéndose.',
                secprompt_title: '¿Añadir un indexador de respaldo?',
                secprompt_body: 'Si tienes otro nodo con su propio indexador (electrs/Fulcrum) en tu LAN, puedes ponerlo como respaldo. Broadcast Pool lo usará solo si tu indexador principal y Bitcoin Core fallan ambos, para que tus transacciones programadas salgan igualmente a su hora.',
                secprompt_later: 'Más tarde',
                secprompt_save: 'Guardar',
```

- [ ] **Step 6: Add the JS (load, save, test, banner, first-run)**

Add these functions (near the other config/banner functions):

```js
        function testSecondaryIndexer() {
            const url = document.getElementById('cfg-secondary-indexer').value.trim();
            const out = document.getElementById('secondary-indexer-test-result');
            out.style.display = 'block';
            out.textContent = t('cfg_checking');
            fetch('/api/test-indexer', {
                method: 'POST', headers: {'Content-Type': 'application/json'},
                body: JSON.stringify({ url })
            }).then(r => r.json()).then(d => {
                out.textContent = d.ok ? '✓ ' + (d.url || url) : '✗ ' + (d.error || 'unreachable');
                out.style.color = d.ok ? 'var(--green)' : 'var(--red)';
            }).catch(() => { out.textContent = '✗'; out.style.color = 'var(--red)'; });
        }

        function updateSecondaryBanner(status) {
            const el = document.getElementById('secondary-banner');
            if (!el || !status) return;
            if (status.chain_source === 'secondary_indexer') {
                document.getElementById('secondary-banner-title').textContent = t('sec_banner_title');
                document.getElementById('secondary-banner-text').textContent =
                    t('sec_banner_text').replace('{url}', status.secondary_indexer_url || '—');
                el.style.display = 'block';
            } else {
                el.style.display = 'none';
            }
        }

        function maybeShowSecondaryPrompt(cfg) {
            if (localStorage.getItem('bp-secondary-indexer-prompt')) return;
            if (cfg && cfg.secondary_indexer_url) { // already configured → don't nag
                localStorage.setItem('bp-secondary-indexer-prompt', '1');
                return;
            }
            document.getElementById('secondary-prompt-overlay').style.display = 'flex';
        }
        function dismissSecondaryPrompt() {
            localStorage.setItem('bp-secondary-indexer-prompt', '1');
            document.getElementById('secondary-prompt-overlay').style.display = 'none';
        }
        function saveSecondaryPrompt() {
            const url = document.getElementById('secprompt-url').value.trim();
            fetch('/api/config', {
                method: 'POST', headers: {'Content-Type': 'application/json'},
                body: JSON.stringify({ secondary_indexer: url })
            }).then(r => {
                if (!r.ok) return r.text().then(e => { throw new Error(e); });
                toast(t('toast_saved') || 'Saved');
            }).catch(e => toast('Error: ' + e.message))
              .finally(() => dismissSecondaryPrompt());
        }
```

Wire them into existing flows:
- In the config-load block (where `cfg-electrs` etc. are set), add:
  ```js
                  document.getElementById('cfg-secondary-indexer').value = cfg.secondary_indexer_url || '';
                  maybeShowSecondaryPrompt(cfg);
  ```
- In the save payload object (near `broadcast_mode:`), add:
  ```js
                      secondary_indexer: document.getElementById('cfg-secondary-indexer').value.trim(),
  ```
- In the status refresh (where `updateIndexerBanner(status)` is called), add:
  ```js
                      updateSecondaryBanner(status);
  ```

- [ ] **Step 7: JS syntax check**

Run:
```bash
python3 - <<'PY'
import re
html = open('src/api/dashboard.html').read()
blocks = re.findall(r'<script[^>]*>(.*?)</script>', html, re.S)
open('/tmp/dash_sec.js','w').write('\n'.join(blocks))
print('blocks', len(blocks))
PY
node --check /tmp/dash_sec.js && echo "JS OK"
```
Expected: `JS OK`. Also `cargo build` (embedded HTML compiles).

- [ ] **Step 8: Commit**

```bash
git add src/api/dashboard.html
git commit -m "feat(dashboard): secondary indexer field, banner, and first-run prompt"
```

---

### Task 7: End-to-end verification on the node (testnet4) — manual

**Files:** none. Node Umbrel `192.168.50.26` (helper `/home/criptoworld/.claude/jobs/*/tmp/nodessh.py`, web `10.21.21.60:8080`). There is a second indexer on the LAN (the Start9 node's Fulcrum, or the semillabitcoin pool's indexer) usable as the secondary.

- [ ] **Step 1: Build + deploy a test image**

Build/tag an rc image and deploy to the node (as in the release flow), or run the release binary against the node. Confirm a clean start.

- [ ] **Step 2: Configure the secondary + verify it is stored**

```bash
curl -s -XPOST http://<host>:8080/api/config -H 'content-type: application/json' \
  -d '{"secondary_indexer":"<second-node-ip>:50001"}' | python3 -m json.tool
curl -s http://<host>:8080/api/config | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d["secondary_indexer_url"])'
```
Expected: the normalized URL is returned.

- [ ] **Step 3: Force primary + Core down, verify secondary takes over**

Make the primary indexer unreachable (point `APP_ELECTRS_NODE_IP` at a dead IP) AND make Core unreachable (bad `BROADCAST_POOL_RPC_URL`). Then:
```bash
curl -s http://<host>:8080/api/status | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d["chain_source"], d["secondary_up"], d["chain_height"])'
```
Expected: `chain_source == "secondary_indexer"`, `secondary_up true`, a fresh height. App log shows `Secondary indexer alive at ...`.

- [ ] **Step 4: Verify a scheduled tx broadcasts via the secondary**

With a by_block/by_time tx due while only the secondary is up, confirm it broadcasts (log `Broadcast via secondary indexer ...`).

- [ ] **Step 5: Verify lazy probing**

With the primary indexer healthy again, confirm the app log shows NO secondary probe lines over several poll cycles (the external node is not contacted).

- [ ] **Step 6: Verify the banner + first-run pop-up**

With `chain_source == secondary_indexer`, the dashboard shows the amber secondary banner. In a fresh browser profile (cleared localStorage), the first-run pop-up appears; "Later" dismisses permanently; "Save" stores the URL.

- [ ] **Step 7: Record results in the PR description.**

---

## Self-Review notes

- **Spec coverage:** §1 ChainSource (T1), §2 config (T2), §3 lazy probe + health (T3), §4 broadcast (T4), §5 API (T5), §6 dashboard field+banner+pop-up (T6), §7 first-run localStorage (T6), E2E (T7). Network-mismatch warning on Test (spec §"bordes") is deferred: `/api/test-indexer` reports reachability; a genesis/network check is a nice-to-have flagged in T7, not a core task — noted so it is not silently dropped.
- **Placeholder scan:** the `get_status` step 6 shows `/* from cfg */` as a pointer to the existing config-lock tuple pattern (same as the shipped `indexer_url` wiring), not a placeholder for missing logic.
- **Type consistency:** `decide_chain_source(4 args)`, `should_probe_secondary`, `normalize_secondary_indexer`, `secondary_indexer` (config), `secondary_indexer_url` / `secondary_up` (API/JS), `ChainSource::SecondaryIndexer` → `"secondary_indexer"` are used identically across tasks.
