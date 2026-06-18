---
name: umbrel-apps-issue
description: >-
  Diagnose and fix Umbrel Community App Store issues: install stuck at 1%,
  GHCR manifest unknown, docker compose errors in app-data, missing containers,
  exports.sh, prebuilt vs on-device Rust builds. Use when Umbrel app install
  fails, update stalls, docker logs fail, or publishing to criptoworld8484/umbrel-apps.
---

# Umbrel apps — troubleshooting & publish

Skill for apps in [criptoworld8484/umbrel-apps](https://github.com/criptoworld8484/umbrel-apps) (store id `sparrow` / BitcoinApps).

## When to use

- Install/update stuck at **1%** or never creates containers
- `docker compose` in `~/umbrel/app-data/{app-id}/` fails with unset `APP_*` vars
- `manifest unknown` pulling `ghcr.io/...`
- Wrong container name / user confused with another app on the node
- Publishing or syncing app package from main repo to `umbrel-apps`
- **Sparrow stuck on "Connecting to tcp://…:50050"** on Umbrel (see [reference.md — Sparrow Connecting](reference.md#sparrow-stuck-on-connecting-to-tcplan50050-2026-06))

## Quick diagnosis (Umbrel node)

Run on the node (use **`sudo`** for docker if `umbrel` user lacks socket access):

```bash
# 1. Does our app have any container?
sudo docker ps -a | grep -i {app-id}

# 2. Is the GHCR image published?
sudo docker pull ghcr.io/criptoworld8484/{image-name}:{tag}

# 3. Logs (only if container exists)
sudo docker logs {app-id}_web_1 --tail 200
```

**Do not** rely on `docker compose logs` inside `~/umbrel/app-data/{app-id}/` — Umbrel injects env vars and `app_proxy`; manual compose fails with:

- `APP_DATA_DIR variable is not set`
- `app_proxy has neither an image nor a build context` ← **expected outside Umbrel**

Prefer: **Settings → Troubleshoot → App**, or `docker logs` on the real container name.

## Install stuck at ~1%

| Cause | Fix |
|-------|-----|
| `docker-compose.yml` uses `build:` (Rust compile on Pi) | Use **prebuilt** `image: ghcr.io/...` (CI builds amd64+arm64) |
| Image tag in compose ≠ tag on GHCR | Align tag with CI output or use tag that exists (`docker pull` test) |
| CI not finished | Wait for workflow **Publish *-umbrel image** (~45–50 min) |
| `manifest unknown` | Tag not pushed yet; check [Actions](https://github.com/criptoworld8484/umbrel-apps/actions) |

**Rule:** Never ship Rust Umbrel apps with on-device `build:` unless explicitly required. Raspberry Pi OOM/timeout looks like a frozen install.

## GHCR tag mismatch (common)

Workflow reads version from **`{app-id}/Cargo.toml`**, not `umbrel-app.yml`:

```bash
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' sparrow-broadcast-pool/Cargo.toml | head -1)
# → tags ghcr.io/.../broadcast-pool-umbrel:$VERSION and :latest
```

If `umbrel-app.yml` says `0.2.0` but `Cargo.toml` is `0.1.0`, compose referencing `:0.2.0` yields **manifest unknown** while `:0.1.0` may contain the new code.

**Fix:** Bump `Cargo.toml` version **before** CI, or point compose at an existing tag until CI completes.

## Package layout (`umbrel-apps/{app-id}/`)

```
{app-id}/
├── umbrel-app.yml
├── docker-compose.yml    # image: ghcr.io/... NOT build: for Rust
├── exports.sh            # host-side env (LAN IP, static Docker IPs if allocated)
├── Dockerfile            # used by CI only
├── Cargo.toml / Cargo.lock / src/   # self-contained build context
├── rust-toolchain.toml
├── .dockerignore
└── icon.png
```

Sync from main repo (example **sparrow-broadcast-pool**):

```bash
SRC=/path/to/Mywalletcompromise
DEST=/path/to/umbrel-apps/sparrow-broadcast-pool
cp -r "$SRC/umbrel-app/sparrow-broadcast-pool/"* "$DEST/"
cp "$SRC/Cargo.toml" "$SRC/Cargo.lock" "$DEST/"
cp -r "$SRC/src" "$DEST/"
```

## docker-compose patterns (Umbrel)

```yaml
services:
  app_proxy:
    environment:
      APP_HOST: {app-id}_web_1
      APP_PORT: 8080
  web:
    image: ghcr.io/criptoworld8484/{image}:{tag}
    ports:
      - "50050:50050"   # wallet Electrum — single port for Sparrow/Liana
    environment:
      APP_ELECTRS_NODE_IP: "${APP_ELECTRS_NODE_IP}"
      APP_ELECTRS_NODE_PORT: "${APP_ELECTRS_NODE_PORT}"
      BROADCAST_POOL_LAN_IP: "${APP_SPARROW_BROADCAST_POOL_LAN_IP:-}"  # from exports.sh
```

- **`app_proxy`**: no `image` in app repo — Umbrel adds it at deploy time.
- **Indexer** (electrs/fulcrum): internal Docker IP, ports **50001/50002** — not the wallet URL.
- **Wallet URL**: node **LAN IP:50050** only (via `exports.sh` → env).

## exports.sh (LAN IP example)

```sh
export APP_SPARROW_BROADCAST_POOL_LAN_IP="$(ip -o route get to 8.8.8.8 2>/dev/null | sed -n 's/.*src \([0-9.]\+\).*/\1/p')"
```

Wire in compose: `BROADCAST_POOL_LAN_IP: "${APP_SPARROW_BROADCAST_POOL_LAN_IP:-}"`

Reference app with static Docker IPs: `sparrow-frigate/exports.sh` (manifest `1.1.0`).

## CI workflow

Path: `.github/workflows/publish-{app}-umbrel.yml`

Triggers on push to `master` when `{app-id}/**` changes. After push, verify **success** before telling user to reinstall.

## Publish checklist (criptoworld8484)

```
- [ ] docker-compose uses prebuilt image (not build:)
- [ ] Cargo.toml version matches compose image tag (or use :latest / existing tag)
- [ ] No secrets in package (no RPC passwords in default.toml)
- [ ] exports.sh present if LAN IP needed
- [ ] CI workflow green
- [ ] sudo docker pull ghcr.io/...:{tag} works on node
- [ ] User: refresh store → reinstall app
```

## Security

- Never commit `.env`, RPC passwords, or `config/default.toml` with credentials.
- Umbrel injects `APP_BITCOIN_*` at runtime; local dev config should omit `[bitcoin_rpc]` secrets.
- Indexing/broadcast uses **Electrs/Fulcrum**, not RPC.

## Case study

Full **Broadcast Pool v0.2.0** timeline (1% stall, manifest unknown, wrong docker compose usage): [reference.md](reference.md)
