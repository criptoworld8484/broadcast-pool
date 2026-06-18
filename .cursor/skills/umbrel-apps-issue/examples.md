# Examples — umbrel-apps-issue

## Example 1: Install stuck at 1%

**User:** "La app se queda al 1% instalando"

**Agent steps:**

1. Read `docker-compose.yml` in umbrel-apps — look for `build:` vs `image:`
2. Check GHCR: `gh run list --repo criptoworld8484/umbrel-apps --workflow=publish-*-umbrel.yml`
3. Ask user to run on node: `sudo docker pull ghcr.io/criptoworld8484/{image}:{tag}`
4. If `manifest unknown` → tag mismatch or CI pending; align Cargo.toml version with compose tag
5. Fix compose to prebuilt image; push; user refreshes store and reinstalls

## Example 2: docker compose logs fails

**User pastes:**

```
WARN APP_DATA_DIR variable is not set
service "app_proxy" has neither an image nor a build context specified
```

**Response:** Explain this is expected when running compose manually. Direct to:

```bash
sudo docker ps -a | grep {app-id}
sudo docker logs {app-id}_web_1 --tail 100
```

Or Umbrel UI Troubleshoot → App.

## Example 3: Container not found + another app running

**User:**

```
No such container: sparrow-broadcast-pool_web_1
semillabitcoin-broadcast-pool_app_1   Up 36 minutes
```

**Response:** Our app never started (no container). `semillabitcoin-broadcast-pool` is a different product — ignore for this diagnosis. Focus on GHCR pull + reinstall.

## Example 4: Publishing update from main repo

```bash
git clone https://github.com/criptoworld8484/umbrel-apps.git
SRC=~/Documents/OpenCode/Mywalletcompromise
DEST=~/umbrel-apps/sparrow-broadcast-pool
cp -r "$SRC/umbrel-app/sparrow-broadcast-pool/"* "$DEST/"
cp "$SRC/Cargo.toml" "$SRC/Cargo.lock" "$DEST/"
cp -r "$SRC/src" "$DEST/"
# Verify compose has image: not build:
# Verify Cargo.toml version matches image tag
git commit && git push
# Wait for CI ~50min before user reinstalls
```

Use skill `github-publish-criptoworld` for commit confirmation protocol.

## Example 5: Wrong wallet URL documented

**Wrong:** Liana uses IP-LAN:50051, Sparrow uses :50050

**Correct:** Both use **IP-LAN:50050** (single Electrum server port on pool). Indexer electrs uses 50001/50002 internally only.
