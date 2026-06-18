# Reference — Broadcast Pool Umbrel investigation (2026-06)

Case study for `sparrow-broadcast-pool` on criptoworld8484/umbrel-apps.

## Symptoms reported

1. Install stops at **1%**
2. `docker compose logs web` in `app-data/sparrow-broadcast-pool` → warnings + `app_proxy has neither an image nor a build context`
3. `sudo docker logs sparrow-broadcast-pool_web_1` → **No such container**
4. `sudo docker pull ghcr.io/criptoworld8484/broadcast-pool-umbrel:0.2.0` → **manifest unknown**
5. User also had **semillabitcoin-broadcast-pool** running — unrelated app (Python, port 50005)

## Root causes

### 1. On-device Rust build (v0.2.0 regression)

v0.1.0 compose:

```yaml
web:
  image: ghcr.io/criptoworld8484/broadcast-pool-umbrel:0.1.0
```

v0.2.0 compose (broken on Umbrel):

```yaml
web:
  build:
    context: .
    dockerfile: Dockerfile
```

Compiling Rust on Umbrel (especially Pi) takes 30–60+ min or OOM → UI shows ~1% indefinitely.

**Fix:** Restore `image: ghcr.io/...` built by GitHub Actions (amd64 + arm64, ~50 min).

### 2. GHCR tag mismatch

CI workflow `publish-broadcast-pool-umbrel.yml`:

```yaml
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' sparrow-broadcast-pool/Cargo.toml | head -1)
tags:
  ghcr.io/criptoworld8484/broadcast-pool-umbrel:${{ steps.ver.outputs.version }}
  ghcr.io/criptoworld8484/broadcast-pool-umbrel:latest
```

Commit `6e89181` (v0.2.0 features) shipped while `Cargo.toml` still said `0.1.0` → image tagged **0.1.0**, not 0.2.0.

Compose referenced `:0.2.0` → **manifest unknown** until CI with `Cargo.toml` 0.2.0 completed.

**Interim fix:** Point compose to `:0.1.0` (image exists, contains v0.2.0 code from that build).

### 3. Manual docker compose is invalid on Umbrel

Running compose from `app-data/` without Umbrel orchestration:

- `APP_DATA_DIR`, `APP_BITCOIN_NODE_IP`, `APP_ELECTRS_*` unset
- `app_proxy` service incomplete

This is **not** proof the app is misconfigured — it's the wrong diagnostic path.

## Correct diagnostics on Umbrel

```bash
sudo docker ps -a | grep sparrow-broadcast-pool
sudo docker pull ghcr.io/criptoworld8484/broadcast-pool-umbrel:0.1.0
sudo docker pull ghcr.io/criptoworld8484/broadcast-pool-umbrel:0.2.0   # after CI success
sudo docker logs sparrow-broadcast-pool_web_1 --tail 200
```

UI: Settings → Troubleshoot → App → Broadcast Pool

## Architecture notes (Broadcast Pool)

| Connection | Port | Who connects |
|------------|------|--------------|
| Wallet Electrum (pool) | **50050** | Sparrow, Liana — URL = **LAN IP:50050** |
| Indexer electrs/fulcrum | 50001 or 50002 | broadcast-pool internal only |
| Dashboard | 8080 via app_proxy | Browser |

- Auto-detect network: Bitcoin RPC `getblockchaininfo` (Umbrel env)
- Auto-detect indexer: probe TCP 50001/50002 + genesis hash match
- LAN IP: `exports.sh` → `BROADCAST_POOL_LAN_IP`

Do **not** document separate Liana port 50051 for users — app uses **50050** for both wallets.

## Commits timeline (umbrel-apps)

| Commit | Change |
|--------|--------|
| `6e89181` | v0.2.0 features; compose still had `build:` |
| `178992d` | Restore prebuilt image `:0.2.0`; bump Cargo.toml 0.2.0 |
| `b250320` | Compose `:0.1.0` until `:0.2.0` manifest available |

## CI outcomes observed

- Successful multi-arch build: **~44–50 minutes**
- Workflow: `Publish broadcast-pool-umbrel image`
- Failed builds often: wrong Rust version in Dockerfile vs dependencies (need 1.86+)

## Related repos & paths

- Fork: `https://github.com/criptoworld8484/umbrel-apps`
- App dir: `sparrow-broadcast-pool/`
- Main source: `Mywalletcompromise/umbrel-app/sparrow-broadcast-pool/`
- Publish skill: `~/.agents/skills/github-publish-criptoworld/`

## User recovery steps (after fix pushed)

1. Wait for GHCR workflow **success**
2. `sudo docker pull ghcr.io/criptoworld8484/broadcast-pool-umbrel:{tag}`
3. Refresh BitcoinApps store on Umbrel
4. Uninstall failed install if present
5. Reinstall Broadcast Pool
6. Settings → copy **IP-LAN:50050** for wallet (not electrs IP)

---

## Sparrow stuck on "Connecting to tcp://LAN:50050" (2026-06)

**Working fix as of v0.2.16** (`dbe33f28` on umbrel-apps). Sparrow connects; do not regress below this without re-running `scripts/test-sparrow-lines.sh`.

### Symptoms

- Sparrow UI: `Connecting to tcp://192.168.x.x:50050...` indefinitely
- Umbrel logs show `Electrum client connected from 192.168.x.x` (client IP, **not** Umbrel IP)
- Logs list many RPC lines, often **dozens of repeated** `blockchain.scripthash.subscribe`
- App works when run **locally** on dev machine; fails only on Umbrel

### What is NOT the problem

| Checked | Result |
|---------|--------|
| Docker port `0.0.0.0:50050:50050` | OK — TCP reaches container |
| `BROADCAST_POOL_ELECTRUM_HOST=0.0.0.0` | OK |
| LAN firewall | OK if logs show client connected |
| Wrong wallet URL | User must use **Umbrel LAN IP:50050**, not electrs `:50001` |

### Root cause (final)

Sparrow sends **one JSON-RPC method per line** (not a JSON array batch):

```
server.version → server.features → blockchain.headers.subscribe → … → blockchain.scripthash.subscribe (×N)
```

On Umbrel, electrs connects instantly via Docker (`10.21.21.10:50001`). Versions **v0.2.13–v0.2.15** could forward client RPCs on a persistent upstream stream **without immediate JSON response**, interleave electrs bytes with handshake data, or block on slow scripthash history fetches. Sparrow retries `scripthash.subscribe` → log flood → stays on Connecting.

### Fix by version

| Version | Outcome |
|---------|---------|
| v0.2.10–v0.2.12 | Partial handshake fixes |
| v0.2.13 | JSON-RPC batch parse (Sparrow often uses lines, not batches) |
| v0.2.14–v0.2.15 | Local handshake tweaks; still broken on Umbrel |
| **v0.2.16** | **Sync response per client line — production fix** |

### v0.2.16 code (`src/electrum_server/mod.rs`)

1. `handle_connection`: simple read loop (no select + upstream reader for client RPCs)
2. `process_client_line`: always `write_client_responses` immediately via `forward_subrequest_sync`
3. `scripthash.subscribe`: skip `fetch_scripthash_history_sync` when pool has no pending txs
4. Log: `Electrum session started for … (sync RPC responses)`

### Log patterns

**Healthy:** `(sync RPC responses)`, one log line per RPC method, no scripthash flood.

**Broken:** `(indexer after handshake)`, 50+ `scripthash.subscribe` in a row.

### IPs in logs

- **`.26`** (example) = Umbrel → Sparrow connects **to** this
- **`.68`** (example) in logs = Sparrow PC → TCP **source** (peer address)

### Tests before publish

```bash
bash scripts/test-sparrow-lines.sh       # primary — one RPC per line
bash scripts/test-umbrel-handshake.sh    # batch JSON alternative
```

### Umbrel verify

```bash
sudo docker inspect sparrow-broadcast-pool_web_1 --format '{{.Config.Image}}'
sudo docker logs sparrow-broadcast-pool_web_1 --tail 50 | grep -E 'sync RPC|Electrum RPC'
```

### Key commits (umbrel-apps)

| Commit | Version |
|--------|---------|
| `dbe33f28` | **0.2.16** — sync per-line (keep) |
| `a1e2cfb7` | 0.2.15 |
| `1a8908e2` | 0.2.14 |
| `043561b7` | 0.2.13 |
