# Liana Virtual Block (UTXO cycling) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serve Liana an armed, absolute virtual block height so it signs a refresh tx with `nLockTime` targeting a future block; broadcast pool ingests it as `by_block`, holds it, and relays it when the real chain reaches that height — never affecting Sparrow.

**Architecture:** A new `LianaVirtualBlockConfig` holds the armed state (enabled, target_height, armed_at_height). A pure `virtual_headers` module fabricates synthetic block headers above the real tip (chained prev_hash, zero merkle, +600s/block time, nonce 0 — Liana validates continuity + genesis, not PoW). The Electrum dispatcher, which already has the session and config, detects Liana (no `server.version`) and, when armed, serves fabricated `headers.subscribe` / `block.header` / `block.headers`. Ingest classifies the resulting height-locktime tx as `by_block` and advances the served height by +2. The scheduler auto-disarms at `armed_at_height + 10`.

**Tech Stack:** Rust, `bitcoin = "0.32"` (block::Header fabrication), axum (API), embedded HTML/JS dashboard.

## Global Constraints

- Fabricated headers are served **only** to sessions detected as Liana (`!session.saw_server_version` or client name contains "liana" on the shared `sparrow` port) **and** only while `config.schedule` armed state is enabled. Sparrow always receives real chain data.
- The real-tip cache (`PoolManager::cached_chain_tip`) must **never** be written with a virtual height. Virtual heights are computed per-Liana-response only.
- The scheduler never broadcasts a non-final tx: `is_locktime_satisfied` (`src/pool/manager.rs:558`) already enforces `real_height >= nLockTime`. Do not weaken it.
- Virtual height is **absolute** (user-entered), not an offset. First captured tx serves the configured `V` exactly; each subsequent capture advances the served value by **+2**.
- Auto-disarm when `real_height >= armed_at_height + 10`.
- All new user-facing copy needs both EN and ES i18n entries (the two blocks in `dashboard.html`).
- Rust: keep the existing style; run `cargo test` (currently 43 passing) after each task; never leave it red.

---

### Task 1: Config — `LianaVirtualBlockConfig` armed state

**Files:**
- Modify: `src/config.rs` (add struct + field on `ScheduleConfig` + default)
- Test: `src/config.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Produces:
  - `pub struct LianaVirtualBlockConfig { pub enabled: bool, pub target_height: u64, pub armed_at_height: u64 }` with `Default` (all zero/false) and `Serialize/Deserialize`.
  - Field `pub liana_virtual_block: LianaVirtualBlockConfig` on `ScheduleConfig`, `#[serde(default)]`.

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` in `src/config.rs` (create the module at end of file if none exists):

```rust
#[cfg(test)]
mod liana_vb_tests {
    use super::*;

    #[test]
    fn liana_virtual_block_defaults_disarmed() {
        let c = LianaVirtualBlockConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.target_height, 0);
        assert_eq!(c.armed_at_height, 0);
    }

    #[test]
    fn schedule_config_deserializes_without_liana_block() {
        // Old configs on disk have no liana_virtual_block key; it must default in.
        let toml = r#"
            broadcast_mode = "Immediate"
            default_delay_hours = 24
            min_delay_hours = 2
            max_delay_hours = 72
            min_fee_rate = 1.0
            max_fee_rate = 50.0
        "#;
        let sc: ScheduleConfig = toml::from_str(toml).expect("parse");
        assert!(!sc.liana_virtual_block.enabled);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test liana_vb_tests -- --nocapture`
Expected: FAIL — `LianaVirtualBlockConfig` not found / field missing.

- [ ] **Step 3: Add the struct and field**

In `src/config.rs`, after the `ScheduleConfig` struct definition (around line 359), add:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LianaVirtualBlockConfig {
    /// Tick armed: while true, Liana sessions are served a virtual (future) tip.
    #[serde(default)]
    pub enabled: bool,
    /// Absolute virtual block height to serve to the next Liana tx. Advances +2 per capture.
    #[serde(default)]
    pub target_height: u64,
    /// Real chain height captured when the tick was armed. Auto-disarm at armed_at_height + 10.
    #[serde(default)]
    pub armed_at_height: u64,
}
```

Add the field to `ScheduleConfig` (after `scheduled_datetime`):

```rust
    #[serde(default)]
    pub liana_virtual_block: LianaVirtualBlockConfig,
```

Add it to the `impl Default for ScheduleConfig` block:

```rust
            liana_virtual_block: LianaVirtualBlockConfig::default(),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test liana_vb_tests -- --nocapture`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add Liana virtual-block armed state"
```

---

### Task 2: Synthetic header fabrication module

**Files:**
- Create: `src/electrum_server/virtual_headers.rs`
- Modify: `src/electrum_server/mod.rs` (add `mod virtual_headers;` near the top, after the existing module attributes)
- Test: inline `#[cfg(test)]` in `virtual_headers.rs`

**Interfaces:**
- Consumes: `bitcoin = "0.32"` (`bitcoin::block::Header`, `bitcoin::consensus::encode`, `bitcoin::hashes::Hash`).
- Produces:
  - `pub fn fabricate_headers(real_tip_height: u64, real_tip_header_hex: &str, up_to_height: u64) -> anyhow::Result<Vec<(u64, String)>>` — returns `(height, header_hex)` for each height in `real_tip_height+1..=up_to_height`, chained from the real tip. Empty vec if `up_to_height <= real_tip_height`.
  - `pub fn header_hex_at(real_tip_height: u64, real_tip_header_hex: &str, height: u64) -> anyhow::Result<Option<String>>` — the fabricated header hex for one height above the tip, or `Ok(None)` if `height <= real_tip_height` (caller passes those through to electrs).

- [ ] **Step 1: Write the failing test**

Create `src/electrum_server/virtual_headers.rs` with only the tests first:

```rust
//! Fabricates synthetic block headers above the real chain tip, for the Liana virtual-block
//! feature. Liana validates chain continuity (prev_blockhash) and the genesis hash, but NOT
//! proof-of-work, so a chained header with zeroed merkle root and nonce 0 is accepted.

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::consensus::encode;
    use bitcoin::block::Header;

    // A real testnet4 header hex (80 bytes) — height is irrelevant to the math here.
    const TIP_HEX: &str = "0000002094d8...";

    fn decode(hex: &str) -> Header {
        encode::deserialize(&hex::decode(hex).unwrap()).unwrap()
    }

    #[test]
    fn empty_when_target_not_above_tip() {
        let out = fabricate_headers(100, TIP_HEX, 100).unwrap();
        assert!(out.is_empty());
        let out = fabricate_headers(100, TIP_HEX, 99).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn fabricates_chained_headers() {
        let tip = decode(TIP_HEX);
        let out = fabricate_headers(100, TIP_HEX, 103).unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].0, 101);
        assert_eq!(out[2].0, 103);

        // First synthetic header chains from the real tip.
        let h101 = decode(&out[0].1);
        assert_eq!(h101.prev_blockhash, tip.block_hash());
        // Each subsequent header chains from the previous synthetic one.
        let h102 = decode(&out[1].1);
        assert_eq!(h102.prev_blockhash, h101.block_hash());
        let h103 = decode(&out[2].1);
        assert_eq!(h103.prev_blockhash, h102.block_hash());

        // Time advances 600s/block from the tip; merkle zeroed; nonce 0; bits/version copied.
        assert_eq!(h101.time, tip.time + 600);
        assert_eq!(h103.time, tip.time + 1800);
        assert_eq!(h101.merkle_root, bitcoin::TxMerkleNode::all_zeros());
        assert_eq!(h101.nonce, 0);
        assert_eq!(h101.bits, tip.bits);
        assert_eq!(h101.version, tip.version);
        // 80-byte header → 160 hex chars.
        assert_eq!(out[0].1.len(), 160);
    }

    #[test]
    fn header_hex_at_passes_through_below_tip() {
        assert_eq!(header_hex_at(100, TIP_HEX, 100).unwrap(), None);
        assert_eq!(header_hex_at(100, TIP_HEX, 50).unwrap(), None);
        let one = header_hex_at(100, TIP_HEX, 101).unwrap();
        assert!(one.is_some());
    }
}
```

Before running: replace `TIP_HEX` with a real 80-byte testnet4 header. Get one on the node:
```bash
python3 /home/criptoworld/.claude/jobs/*/tmp/nodessh.py \
  "curl -s http://10.21.21.60:8080/api/status | python3 -c 'import json,sys;print(json.load(sys.stdin))'" 30
```
or query the indexer directly for a `blockchain.block.header` result. Any valid mainnet/testnet header hex works for the math; paste it into `TIP_HEX`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test virtual_headers -- --nocapture`
Expected: FAIL — `fabricate_headers` / `header_hex_at` not defined.

- [ ] **Step 3: Implement the module**

Add above the `#[cfg(test)]` block in `virtual_headers.rs`:

```rust
use anyhow::{Context, Result};
use bitcoin::block::Header;
use bitcoin::consensus::encode;
use bitcoin::hashes::Hash;
use bitcoin::{BlockHash, TxMerkleNode};

const SECS_PER_BLOCK: u32 = 600;

fn decode_header(hex: &str) -> Result<Header> {
    let bytes = hex::decode(hex.trim()).context("virtual_headers: bad hex")?;
    encode::deserialize::<Header>(&bytes).context("virtual_headers: not an 80-byte header")
}

/// Fabricate `(height, header_hex)` for every height in `real_tip_height+1..=up_to_height`,
/// each chained from the previous. Empty when `up_to_height <= real_tip_height`.
pub fn fabricate_headers(
    real_tip_height: u64,
    real_tip_header_hex: &str,
    up_to_height: u64,
) -> Result<Vec<(u64, String)>> {
    if up_to_height <= real_tip_height {
        return Ok(Vec::new());
    }
    let tip = decode_header(real_tip_header_hex)?;
    let mut out = Vec::with_capacity((up_to_height - real_tip_height) as usize);
    let mut prev_hash = tip.block_hash();
    for h in (real_tip_height + 1)..=up_to_height {
        let steps = (h - real_tip_height) as u32;
        let header = Header {
            version: tip.version,
            prev_blockhash: prev_hash,
            merkle_root: TxMerkleNode::all_zeros(),
            time: tip.time.saturating_add(SECS_PER_BLOCK.saturating_mul(steps)),
            bits: tip.bits,
            nonce: 0,
        };
        prev_hash = header.block_hash();
        out.push((h, encode::serialize_hex(&header)));
    }
    Ok(out)
}

/// The fabricated header for a single height above the tip. `Ok(None)` for heights at/below the
/// real tip — the caller forwards those to electrs unchanged.
pub fn header_hex_at(
    real_tip_height: u64,
    real_tip_header_hex: &str,
    height: u64,
) -> Result<Option<String>> {
    if height <= real_tip_height {
        return Ok(None);
    }
    let headers = fabricate_headers(real_tip_height, real_tip_header_hex, height)?;
    Ok(headers.into_iter().last().map(|(_, hex)| hex))
}
```

Add to `src/electrum_server/mod.rs` (near the top, with any other `mod` declarations):

```rust
mod virtual_headers;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test virtual_headers -- --nocapture`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add src/electrum_server/virtual_headers.rs src/electrum_server/mod.rs
git commit -m "feat(electrum): synthetic header fabrication for Liana virtual block"
```

---

### Task 3: Serve virtual headers to armed Liana sessions

**Files:**
- Modify: `src/electrum_server/mod.rs` — `handle_headers_subscribe` (`:958`), its call site (`:1188`), and add interception of `blockchain.block.header` / `blockchain.block.headers` in `handle_single_request` (`:1085`).
- Test: inline `#[cfg(test)]` — a helper `virtual_tip_for_session` that computes the served height.

**Interfaces:**
- Consumes: `virtual_headers::fabricate_headers`, `virtual_headers::header_hex_at`; `SessionState::effective_source`; `PoolManager::get_cached_chain_tip`; `config.schedule.liana_virtual_block`.
- Produces:
  - `fn virtual_tip_height(cfg: &Config, is_liana: bool) -> Option<u64>` — `Some(target_height)` when `is_liana && cfg.schedule.liana_virtual_block.enabled && target_height > 0`, else `None`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `src/electrum_server/mod.rs`:

```rust
#[test]
fn virtual_tip_only_for_armed_liana() {
    let mut cfg = Config::default();
    cfg.schedule.liana_virtual_block.enabled = true;
    cfg.schedule.liana_virtual_block.target_height = 950430;

    assert_eq!(virtual_tip_height(&cfg, true), Some(950430));
    // Never for Sparrow.
    assert_eq!(virtual_tip_height(&cfg, false), None);

    // Disarmed → nothing.
    cfg.schedule.liana_virtual_block.enabled = false;
    assert_eq!(virtual_tip_height(&cfg, true), None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test virtual_tip_only_for_armed_liana -- --nocapture`
Expected: FAIL — `virtual_tip_height` not defined.

- [ ] **Step 3: Implement the gate + wire it into header serving**

Add the helper near `handle_headers_subscribe` in `src/electrum_server/mod.rs`:

```rust
/// The virtual tip height to serve, or None when the feature is disarmed or the peer is Sparrow.
fn virtual_tip_height(cfg: &Config, is_liana: bool) -> Option<u64> {
    let vb = &cfg.schedule.liana_virtual_block;
    if is_liana && vb.enabled && vb.target_height > 0 {
        Some(vb.target_height)
    } else {
        None
    }
}
```

Change the `handle_headers_subscribe` signature and call site to pass the virtual tip. New signature:

```rust
async fn handle_headers_subscribe(
    request: &JsonRpcRequest,
    pool_manager: &Arc<PoolManager>,
    indexer_url: &str,
    virtual_tip: Option<u64>,
) -> Result<serde_json::Value> {
```

At the very start of `handle_headers_subscribe`, before the cache-first block, add:

```rust
    // Liana virtual block: serve a fabricated tip ABOVE the real one. Never touches the shared
    // real-tip cache (that is what Sparrow reads), so it can only lie to this one Liana session.
    if let Some(vtip) = virtual_tip {
        if let Some((real_h, real_hex)) = pool_manager.get_cached_chain_tip() {
            let up_to = vtip.max(real_h + 1);
            match virtual_headers::header_hex_at(real_h, &real_hex, up_to) {
                Ok(Some(hex)) => {
                    tracing::info!("Serving Liana virtual tip height={} (real={})", up_to, real_h);
                    return Ok(serde_json::json!({
                        "jsonrpc": "2.0",
                        "result": { "height": up_to, "hex": hex },
                        "id": request.id
                    }));
                }
                Ok(None) => {} // vtip <= real tip: fall through to the real tip below.
                Err(e) => tracing::warn!("virtual header fabrication failed: {}", e),
            }
        }
    }
```

Update the call site at `:1188`. Replace:

```rust
    if request.method == "blockchain.headers.subscribe" {
        return handle_headers_subscribe(request, pool_manager, indexer_url).await;
    }
```

with (note `config` and `session`/`source_label` are already parameters of `handle_single_request`):

```rust
    if request.method == "blockchain.headers.subscribe" {
        let vtip = {
            let cfg = config.lock().map_err(|e| anyhow::anyhow!("Config lock: {}", e))?;
            let is_liana = session.effective_source(source_label) == "liana";
            virtual_tip_height(&cfg, is_liana)
        };
        return handle_headers_subscribe(request, pool_manager, indexer_url, vtip).await;
    }
```

Add interception of single/range header requests just BEFORE the `blockchain.headers.subscribe` block, in `handle_single_request`:

```rust
    if request.method == "blockchain.block.header"
        || request.method == "blockchain.block.headers"
    {
        let vtip = {
            let cfg = config.lock().map_err(|e| anyhow::anyhow!("Config lock: {}", e))?;
            let is_liana = session.effective_source(source_label) == "liana";
            virtual_tip_height(&cfg, is_liana)
        };
        if vtip.is_some() {
            if let Some(resp) = serve_virtual_block_header(request, pool_manager, vtip) {
                return Ok(resp);
            }
        }
    }
```

Add the helper (near `virtual_tip_height`):

```rust
/// Answer `blockchain.block.header` (single) for a Liana virtual height above the real tip.
/// Heights at/below the real tip, and `block.headers` ranges, return None → normal forwarding.
fn serve_virtual_block_header(
    request: &JsonRpcRequest,
    pool_manager: &Arc<PoolManager>,
    _virtual_tip: Option<u64>,
) -> Option<serde_json::Value> {
    if request.method != "blockchain.block.header" {
        return None; // block.headers ranges: forward for now (Liana backfills via subscribe).
    }
    let params = request.params.as_ref()?.as_array()?;
    let height = params.first()?.as_u64()?;
    let (real_h, real_hex) = pool_manager.get_cached_chain_tip()?;
    match virtual_headers::header_hex_at(real_h, &real_hex, height) {
        Ok(Some(hex)) => Some(serde_json::json!({
            "jsonrpc": "2.0",
            "result": hex,
            "id": request.id
        })),
        _ => None,
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -- --nocapture 2>&1 | tail -5`
Expected: PASS — all tests green including `virtual_tip_only_for_armed_liana`.

- [ ] **Step 5: Commit**

```bash
git add src/electrum_server/mod.rs
git commit -m "feat(electrum): serve virtual headers to armed Liana sessions"
```

---

### Task 4: Ingest a Liana virtual-block tx as by_block + advance +2

**Files:**
- Modify: `src/electrum_server/mod.rs` — `resolve_ingest_plan` (`:2154`), and the ingest persist path (`:2263`) to advance `target_height`.
- Test: inline `#[cfg(test)]` for `resolve_ingest_plan`.

**Interfaces:**
- Consumes: `config.schedule.liana_virtual_block`, `BroadcastMode::ByBlock`.
- Produces: no new public symbols; changes `resolve_ingest_plan` behaviour when armed.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `src/electrum_server/mod.rs`:

```rust
#[test]
fn armed_liana_height_locktime_becomes_by_block() {
    let mut cfg = Config::default();
    cfg.schedule.liana_virtual_block.enabled = true;
    cfg.schedule.liana_virtual_block.target_height = 950430;

    // Liana tx with a by-height nLockTime, armed → by_block.
    let (mode, sched) = resolve_ingest_plan("liana", 950430, &cfg);
    assert_eq!(mode, BroadcastMode::ByBlock);
    assert!(sched.is_none());
}

#[test]
fn disarmed_liana_height_locktime_stays_manual() {
    let cfg = Config::default(); // liana_virtual_block disarmed by default
    let (mode, _) = resolve_ingest_plan("liana", 950430, &cfg);
    assert_eq!(mode, BroadcastMode::Manual);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test armed_liana_height_locktime_becomes_by_block disarmed_liana_height_locktime_stays_manual -- --nocapture`
Expected: FAIL — armed case returns `Manual`.

- [ ] **Step 3: Modify `resolve_ingest_plan`**

In `src/electrum_server/mod.rs`, replace the Liana branch at the top of `resolve_ingest_plan`:

```rust
    if source_label == "liana" {
        tracing::info!("Liana ingest → manual scheduling (pending until user sets date/price)");
        return (BroadcastMode::Manual, None);
    }
```

with:

```rust
    if source_label == "liana" {
        let vb = &config.schedule.liana_virtual_block;
        // Armed virtual block + a by-height nLockTime = the user's UTXO-cycling intent: hold as
        // by_block targeting that height, not manual. is_locktime_satisfied still gates broadcast.
        if vb.enabled && nlocktime > 0 && nlocktime <= 500_000_000 {
            tracing::info!("Liana virtual-block ingest → by_block target {}", nlocktime);
            return (BroadcastMode::ByBlock, None);
        }
        tracing::info!("Liana ingest → manual scheduling (pending until user sets date/price)");
        return (BroadcastMode::Manual, None);
    }
```

- [ ] **Step 4: Advance target_height +2 after a virtual-block capture**

In the ingest persist path, after `resolve_ingest_plan` is called (`:2263`), add the advance. Locate:

```rust
    let (broadcast_mode, scheduled_time) = {
        let cfg = config.lock().map_err(|e| anyhow::anyhow!("Config lock: {}", e))?;
        resolve_ingest_plan(source_label, nlocktime, &cfg)
    };
```

and append right after it:

```rust
    // After capturing a Liana virtual-block tx, advance the served height by +2 so the next
    // Liana tx gets a distinct, higher locktime (decorrelation). Persist so it survives restart.
    if broadcast_mode == BroadcastMode::ByBlock && source_label == "liana" {
        let mut cfg = config.lock().map_err(|e| anyhow::anyhow!("Config lock: {}", e))?;
        if cfg.schedule.liana_virtual_block.enabled {
            cfg.schedule.liana_virtual_block.target_height =
                cfg.schedule.liana_virtual_block.target_height.saturating_add(2);
            let snapshot = cfg.clone();
            drop(cfg);
            if let Err(e) = crate::discovery::save_config_to_disk(&snapshot) {
                tracing::warn!("Failed to persist advanced virtual-block height: {}", e);
            }
        }
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -- --nocapture 2>&1 | tail -5`
Expected: PASS — all green.

- [ ] **Step 6: Commit**

```bash
git add src/electrum_server/mod.rs
git commit -m "feat(electrum): ingest armed Liana tx as by_block and advance served height +2"
```

---

### Task 5: Auto-disarm at armed_at_height + 10

**Files:**
- Modify: `src/pool/manager.rs` — add `maybe_disarm_virtual_block(&self)`, call it from `refresh_chain_health` (`:1031`, after the health snapshot write).
- Test: inline `#[cfg(test)]` in `src/pool/manager.rs`.

**Interfaces:**
- Consumes: `config.schedule.liana_virtual_block`, `chain_health().height`, `crate::discovery::save_config_to_disk`.
- Produces: `pub fn maybe_disarm_virtual_block(&self)` on `PoolManager`; pure helper `fn should_disarm(enabled: bool, armed_at: u64, real_height: u64) -> bool`.

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` in `src/pool/manager.rs`:

```rust
#[test]
fn virtual_block_disarms_after_ten_blocks() {
    assert!(!should_disarm(false, 100, 200));          // disabled → never
    assert!(!should_disarm(true, 100, 109));           // 9 blocks in → still armed
    assert!(should_disarm(true, 100, 110));            // exactly +10 → disarm
    assert!(should_disarm(true, 100, 130));            // well past → disarm
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test virtual_block_disarms_after_ten_blocks -- --nocapture`
Expected: FAIL — `should_disarm` not defined.

- [ ] **Step 3: Implement the helper and the method**

Add near the bottom of `src/pool/manager.rs` (module scope, not in `impl`):

```rust
/// Auto-disarm the Liana virtual block once the real chain has advanced 10 blocks past arming.
/// Bounds the window in which we serve fake heights, even if the user forgets to unset the tick.
fn should_disarm(enabled: bool, armed_at_height: u64, real_height: u64) -> bool {
    enabled && real_height >= armed_at_height + 10
}
```

Add the method inside `impl PoolManager` (near `refresh_chain_health`):

```rust
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
```

Call it at the end of `refresh_chain_health`, just before the closing of the method (after the `health_refresh_in_flight.store(false, ...)` line):

```rust
        self.maybe_disarm_virtual_block();
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -- --nocapture 2>&1 | tail -5`
Expected: PASS — all green.

- [ ] **Step 5: Commit**

```bash
git add src/pool/manager.rs
git commit -m "feat(pool): auto-disarm Liana virtual block at armed_at + 10 blocks"
```

---

### Task 6: API — expose and arm/disarm via config; status reflects state

**Files:**
- Modify: `src/api/mod.rs` — `ConfigResponse` (`:416`), `get_config` (`:442`), `SaveConfigRequest` (`:466`), `save_config` (`:479`), `StatusResponse`/`get_status` for state readout.
- Test: inline `#[cfg(test)]` in `src/api/mod.rs` for the arming validation helper.

**Interfaces:**
- Consumes: `config.schedule.liana_virtual_block`, `pool_manager.chain_health().height`.
- Produces:
  - `fn validate_virtual_height(target: u64, real_height: Option<u64>) -> Result<(), String>` — Err when `target == 0` or `target <= real_height`.
  - `ConfigResponse` fields: `liana_vb_enabled: bool`, `liana_vb_target_height: u64`, `liana_vb_armed_at_height: u64`.
  - `SaveConfigRequest` fields: `liana_vb_enabled: Option<bool>`, `liana_vb_target_height: Option<u64>`.

- [ ] **Step 1: Write the failing test**

Add to (or create) `#[cfg(test)] mod tests` in `src/api/mod.rs`:

```rust
#[test]
fn virtual_height_must_be_future() {
    assert!(validate_virtual_height(0, Some(100)).is_err());       // unset
    assert!(validate_virtual_height(100, Some(100)).is_err());     // equal to tip
    assert!(validate_virtual_height(90, Some(100)).is_err());      // below tip
    assert!(validate_virtual_height(150, Some(100)).is_ok());      // future
    assert!(validate_virtual_height(150, None).is_ok());           // no height known → allow
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test virtual_height_must_be_future -- --nocapture`
Expected: FAIL — `validate_virtual_height` not defined.

- [ ] **Step 3: Add the validator**

Add to `src/api/mod.rs` (module scope):

```rust
/// A virtual block height must be a real future block. Reject 0 (unset) and anything at/below
/// the current tip (it would be non-final immediately, defeating the point).
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
    }
    Ok(())
}
```

- [ ] **Step 4: Wire ConfigResponse (readout)**

Add fields to `ConfigResponse` (after `scheduled_datetime`):

```rust
    liana_vb_enabled: bool,
    liana_vb_target_height: u64,
    liana_vb_armed_at_height: u64,
```

Populate them in `get_config` where the response is built (mirror the existing `broadcast_mode: config.schedule.broadcast_mode.to_string()` line):

```rust
        liana_vb_enabled: config.schedule.liana_virtual_block.enabled,
        liana_vb_target_height: config.schedule.liana_virtual_block.target_height,
        liana_vb_armed_at_height: config.schedule.liana_virtual_block.armed_at_height,
```

- [ ] **Step 5: Wire SaveConfigRequest (arm/disarm)**

Add to `SaveConfigRequest`:

```rust
    liana_vb_enabled: Option<bool>,
    liana_vb_target_height: Option<u64>,
```

In `save_config`, after the existing `broadcast_mode` handling (around `:568`), add:

```rust
    // Liana virtual block. Arming validates the height against the live tip and stamps armed_at.
    if let Some(target) = req.liana_vb_target_height {
        config.schedule.liana_virtual_block.target_height = target;
    }
    if let Some(enable) = req.liana_vb_enabled {
        if enable {
            let real_height = state.pool_manager.chain_health().height;
            validate_virtual_height(
                config.schedule.liana_virtual_block.target_height,
                real_height,
            )
            .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
            config.schedule.liana_virtual_block.armed_at_height = real_height.unwrap_or(0);
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
```

Note: `state` is in scope in `save_config` (it takes `State(state)`), but the current handler shadows `config` from `state.config.lock()`. Confirm `state.pool_manager` is reachable; it is (`AppState` holds both). If a borrow conflict arises with the held `config` lock, read `chain_health().height` into a local BEFORE locking config.

- [ ] **Step 6: Expose state in StatusResponse**

Add to `StatusResponse` (near `chain_source`):

```rust
    liana_vb_enabled: bool,
    liana_vb_target_height: u64,
    liana_vb_disarm_height: u64,
```

Populate in `get_status` (near the `chain_source: health.source` line), reading config once:

```rust
        liana_vb_enabled: /* cfg */ .schedule.liana_virtual_block.enabled,
        liana_vb_target_height: /* cfg */ .schedule.liana_virtual_block.target_height,
        liana_vb_disarm_height: /* cfg */ .schedule.liana_virtual_block.armed_at_height + 10,
```

Use the same config lock already taken in `get_status`'s `spawn_blocking` (it locks `config` for network/wallet_url). Add these three reads there and return them through the tuple, following the existing pattern used for `indexer_url`.

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -- --nocapture 2>&1 | tail -5`
Expected: PASS — all green.

- [ ] **Step 8: Commit**

```bash
git add src/api/mod.rs
git commit -m "feat(api): arm/disarm Liana virtual block via config, expose state in status"
```

---

### Task 7: Dashboard UI — tick, virtual height, approx date, red flag, i18n

**Files:**
- Modify: `src/api/dashboard.html` — settings block under Broadcast Mode (`:919-927`), `toggleScheduledFields()`, config load (`:2352`) and save (`:2520`), EN i18n (`:1214`), ES i18n (`:1336`).
- Test: manual (browser) + JS syntax check.

**Interfaces:**
- Consumes: `/api/config` fields `liana_vb_enabled`, `liana_vb_target_height`; `/api/status` `liana_vb_*`.
- Produces: UI only.

- [ ] **Step 1: Add the markup**

Insert after the Broadcast Mode `setting-group` (after line 927, before `cfg-delay-group`):

```html
                        <div class="setting-group" id="cfg-liana-vb-group" style="display:none;">
                            <label style="display:flex; align-items:center; gap:8px; cursor:pointer;">
                                <input type="checkbox" id="cfg-liana-vb-enabled" onchange="toggleLianaVb()">
                                <span data-i18n="cfg_liana_vb_tick">Programar ciclado UTXO de Liana</span>
                            </label>
                            <div id="cfg-liana-vb-fields" style="display:none; margin-top:12px;">
                                <label data-i18n="cfg_liana_vb_height">Altura de bloque virtual</label>
                                <input type="number" id="cfg-liana-vb-height" min="1" step="1"
                                       oninput="updateLianaVbDate()"
                                       style="width:100%; background:var(--bg); border:1px solid var(--border); color:var(--text); padding:16px 20px; border-radius:12px; font-family:var(--font-mono); font-size:16px;">
                                <div class="setting-hint" id="cfg-liana-vb-date">&nbsp;</div>
                                <div class="setting-hint" style="color:var(--amber); margin-top:8px;" data-i18n="cfg_liana_vb_warn">
                                    Introduce la altura de bloque virtual que se pasará a Liana. Debe ser un bloque futuro anterior a la expiración del ciclado de tu UTXO y a que el recovery path miniscript pueda gastar. Con esta opción activa, Liana mostrará una altura falsa; cualquier tx que construya es no-final hasta esa altura.
                                </div>
                            </div>
                        </div>
```

- [ ] **Step 2: Add i18n keys (EN)**

In the EN i18n object (near `:1214`), add:

```js
                cfg_liana_vb_tick: 'Schedule Liana UTXO cycling',
                cfg_liana_vb_height: 'Virtual block height',
                cfg_liana_vb_warn: 'Enter the virtual block height that will be passed to Liana. It must be a future block, before your UTXO cycling expires and before the miniscript recovery path can spend. With this on, Liana shows a false height; any tx it builds is non-final until that height.',
                cfg_liana_vb_approx: 'approx.',
```

- [ ] **Step 3: Add i18n keys (ES)**

In the ES i18n object (near `:1336`), add:

```js
                cfg_liana_vb_tick: 'Programar ciclado UTXO de Liana',
                cfg_liana_vb_height: 'Altura de bloque virtual',
                cfg_liana_vb_warn: 'Introduce la altura de bloque virtual que se pasará a Liana. Debe ser un bloque futuro anterior a la expiración del ciclado de tu UTXO y a que el recovery path miniscript pueda gastar. Con esta opción activa, Liana mostrará una altura falsa; cualquier tx que construya es no-final hasta esa altura.',
                cfg_liana_vb_approx: 'aprox.',
```

- [ ] **Step 4: Add the JS behaviour**

Add these functions (near `toggleScheduledFields`):

```js
        function toggleLianaVb() {
            const on = document.getElementById('cfg-liana-vb-enabled').checked;
            document.getElementById('cfg-liana-vb-fields').style.display = on ? 'block' : 'none';
            if (on) updateLianaVbDate();
        }

        // ~10 min/block from the current tip → approximate calendar date of the virtual height.
        function updateLianaVbDate() {
            const el = document.getElementById('cfg-liana-vb-date');
            const target = parseInt(document.getElementById('cfg-liana-vb-height').value, 10);
            const tip = window.__realTipHeight || 0;
            if (!target || !tip || target <= tip) { el.textContent = ' '; return; }
            const mins = (target - tip) * 10;
            const when = new Date(Date.now() + mins * 60000);
            el.textContent = `≈ ${t('cfg_liana_vb_approx')} ${when.toLocaleString('es-ES', {day:'2-digit', month:'2-digit', year:'numeric', hour:'2-digit', minute:'2-digit'})} (block #${target})`;
        }
```

In `toggleScheduledFields()`, show the Liana group only when mode is `scheduled`. Find the function and add, using the existing mode variable:

```js
            const lianaGrp = document.getElementById('cfg-liana-vb-group');
            if (lianaGrp) lianaGrp.style.display = (mode === 'scheduled') ? 'block' : 'none';
```

- [ ] **Step 5: Load and save the values**

In the config-load block (near `:2352`, where `cfg-broadcast-mode` is set), add:

```js
                document.getElementById('cfg-liana-vb-enabled').checked = !!cfg.liana_vb_enabled;
                if (cfg.liana_vb_target_height) document.getElementById('cfg-liana-vb-height').value = cfg.liana_vb_target_height;
                toggleLianaVb();
```

Capture the real tip for the date estimate — in the status refresh (`refresh()` / `updateIndexerStatus`), where `status.chain_height` is used, add:

```js
                    window.__realTipHeight = status.chain_height || window.__realTipHeight || 0;
```

In the save payload (near `:2520`, the object with `broadcast_mode:`), add:

```js
                    liana_vb_enabled: document.getElementById('cfg-liana-vb-enabled').checked,
                    liana_vb_target_height: parseInt(document.getElementById('cfg-liana-vb-height').value, 10) || 0,
```

- [ ] **Step 6: JS syntax check**

Run:
```bash
python3 - <<'PY'
import re
html = open('src/api/dashboard.html').read()
blocks = re.findall(r'<script[^>]*>(.*?)</script>', html, re.S)
open('/tmp/dash_check.js','w').write('\n'.join(blocks))
print('blocks', len(blocks))
PY
node --check /tmp/dash_check.js && echo "JS OK"
```
Expected: `JS OK`.

- [ ] **Step 7: Commit**

```bash
git add src/api/dashboard.html
git commit -m "feat(dashboard): Liana virtual-block tick, height, approx date, red flag, i18n"
```

---

### Task 8: End-to-end verification on the node (testnet4) — manual

**Files:** none (verification only). Node: Umbrel `192.168.50.26`, user `umbrel`, container `sparrow-broadcast-pool_web_1`, web on `10.21.21.60:8080`, Electrum host `:50050`. Helper: `/home/criptoworld/.claude/jobs/*/tmp/nodessh.py`.

This task ships a new image (see release flow) OR runs a locally-built binary against the node's Bitcoin Core + Fulcrum. Because it depends on the real Liana wallet, it is manual.

- [ ] **Step 1: Build and smoke-check locally**

Run: `cargo build --release 2>&1 | tail -3` — expect a clean build.

- [ ] **Step 2: Arm via API and verify status**

With the app running against the node, arm:
```bash
curl -s -XPOST http://<host>:8080/api/config -H 'content-type: application/json' \
  -d '{"liana_vb_target_height": <real_tip+20>, "liana_vb_enabled": true}' | python3 -m json.tool
curl -s http://<host>:8080/api/status | python3 -c 'import json,sys;d=json.load(sys.stdin);print({k:d[k] for k in d if k.startswith("liana_vb")})'
```
Expected: `liana_vb_enabled=true`, `target_height` set, `disarm_height = armed_at + 10`.

- [ ] **Step 3: Drive Liana**

Point the real Liana wallet at `<node-lan-ip>:50050`. Confirm in the app log:
`Serving Liana virtual tip height=<target> (real=<tip>)`. Build a refresh tx in Liana; it should sign with `nLockTime = <target>` and "broadcast" it to the pool.

- [ ] **Step 4: Verify ingest + hold + relay**

```bash
curl -s http://<host>:8080/api/transactions | python3 -c 'import json,sys;[print(t["id"],t["broadcast_mode"],t["nlocktime"],t["status"]) for t in json.load(sys.stdin)]'
```
Expected: a `by_block` tx with `nlocktime = <target>`, status `pending`/`scheduled` (held). It must NOT broadcast until the real chain reaches `<target>`. A second Liana tx (nothing changed) should carry `<target>+2`.

- [ ] **Step 5: Verify Sparrow is unaffected**

With the tick still armed, connect Sparrow to `:50050` and Test Connection. Its `headers.subscribe` must return the REAL height (check the app log — no "Serving Liana virtual tip" line for the Sparrow session). Sparrow syncs normally.

- [ ] **Step 6: Verify auto-disarm**

Wait until the real chain passes `armed_at + 10` (~100 min on testnet4) or set a low target for testing. Confirm the log line `Liana virtual block auto-disarmed` and `/api/status` shows `liana_vb_enabled=false`.

- [ ] **Step 7: Record results**

Note outcomes in the PR description. If Liana rejects the fabricated headers (e.g. requests a `block.headers` range we forwarded), capture the exact request from the log and extend `serve_virtual_block_header` to fabricate ranges — that is the one known risk this verification exists to surface.

---

## Self-Review notes

- **Spec coverage:** config/armed-state (T1), header fabrication (T2), Liana-only serving + no cache poisoning (T3), by_block ingest + `+2` (T4), auto-disarm at +10 (T5), API arm/validate/status (T6), UI tick+height+date+red flag+i18n (T7), node E2E incl. Sparrow-safety + reschedule/delete-by-reuse (T8). Reschedule/delete reuse the existing `/schedule` and `/remove` endpoints — no new task needed; the nLockTime-immutability caveat is documented in the spec.
- **Known risk carried into T8:** `block.headers` (range) is forwarded, not fabricated. If the real Liana needs the range, T8 step 7 extends `serve_virtual_block_header`. Kept out of the core tasks to avoid speculative code before observing Liana's real requests.
- **Type consistency:** `virtual_tip_height`, `fabricate_headers`, `header_hex_at`, `should_disarm`, `validate_virtual_height`, and the `liana_vb_*` field names are used identically across tasks.
