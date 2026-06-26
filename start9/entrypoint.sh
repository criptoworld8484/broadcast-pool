#!/bin/sh
set -eu

# StartOS entrypoint for broadcast-pool.
#
# Translates the StartOS environment into the binary's generic BROADCAST_POOL_* contract.
# The network is NOT set here on purpose: the binary auto-detects it from Bitcoin Core
# (discovery::apply_network_from_rpc, via getblockchaininfo.chain), so the same image works
# on mainnet, testnet4 and signet without changes.
#
# Connection details default to the StartOS internal service hostnames but can be overridden
# by main.ts (which can read the real RPC/electrs addresses from the StartOS dependency config).

DATA_DIR="${BROADCAST_POOL_DATA_DIR:-/data}"
BITCOIN_DATA_DIR="${BITCOIN_DATA_DIR:-/mnt/bitcoind}"
# StartOS normalizes the Bitcoin Core RPC port to 8332 on every network (verified:
# bitcoin.conf has "[testnet4] rpcbind=0.0.0.0:8332"), so this default is correct for
# mainnet/testnet4/signet. The indexer is Fulcrum (Electrum TCP on 50001).
BITCOIN_RPC_URL="${BROADCAST_POOL_RPC_URL:-http://bitcoind.startos:8332}"
ELECTRS_URL="${BROADCAST_POOL_INDEXER_URL:-tcp://fulcrum.startos:50001}"

BOOT_LOG="${DATA_DIR}/startos-boot.log"
mkdir -p "${DATA_DIR}"

log() { echo "$(date -Iseconds) $*" >> "${BOOT_LOG}"; }

log "=== broadcast-pool StartOS boot ==="
log "BITCOIN_RPC_URL=${BITCOIN_RPC_URL}"
log "ELECTRS_URL=${ELECTRS_URL}"
log "BITCOIN_DATA_DIR=${BITCOIN_DATA_DIR}"

# --- Resolve the bitcoind RPC cookie (auth) ---------------------------------
# Bitcoin Core writes a .cookie file in its (network-specific) data dir as
# "__cookie__:<password>". The cookie may live in a network subdirectory
# (e.g. signet/.cookie), so fall back to a recursive find like the frigate package.
resolve_cookie_file() {
  if [ -f "${BITCOIN_DATA_DIR}/.cookie" ]; then
    echo "${BITCOIN_DATA_DIR}/.cookie"
    return 0
  fi
  find "${BITCOIN_DATA_DIR}" -maxdepth 4 -name '.cookie' -type f 2>/dev/null | head -1
}

# Wait for the cookie to appear (bitcoind may still be starting).
WAIT_MAX="${BITCOIN_WAIT_MAX:-180}"
WAIT_INTERVAL="${BITCOIN_WAIT_INTERVAL:-5}"
elapsed=0
COOKIE_FILE=""
while [ "${elapsed}" -lt "${WAIT_MAX}" ]; do
  COOKIE_FILE="$(resolve_cookie_file || true)"
  if [ -n "${COOKIE_FILE}" ] && [ -f "${COOKIE_FILE}" ]; then
    break
  fi
  log "Waiting for Bitcoin Core cookie under ${BITCOIN_DATA_DIR} (${elapsed}s/${WAIT_MAX}s)"
  sleep "${WAIT_INTERVAL}"
  elapsed=$((elapsed + WAIT_INTERVAL))
done

if [ -z "${COOKIE_FILE}" ] || [ ! -f "${COOKIE_FILE}" ]; then
  echo "ERROR: Bitcoin Core cookie not found under ${BITCOIN_DATA_DIR} after ${WAIT_MAX}s" >&2
  log "ERROR: cookie not found after ${WAIT_MAX}s"
  exit 1
fi

COOKIE="$(cat "${COOKIE_FILE}")"
RPC_USER="${COOKIE%%:*}"
RPC_PASS="${COOKIE#*:}"
log "Cookie resolved from ${COOKIE_FILE} (user=${RPC_USER})"

# --- Export the binary's generic contract -----------------------------------
export BROADCAST_POOL_DATA_DIR="${DATA_DIR}"
export BROADCAST_POOL_RPC_URL="${BITCOIN_RPC_URL}"
export BROADCAST_POOL_RPC_USER="${RPC_USER}"
export BROADCAST_POOL_RPC_PASS="${RPC_PASS}"
export BROADCAST_POOL_INDEXER_URL="${ELECTRS_URL}"
export BROADCAST_POOL_ELECTRUM_HOST="${BROADCAST_POOL_ELECTRUM_HOST:-0.0.0.0}"
export BROADCAST_POOL_ELECTRUM_PORT="${BROADCAST_POOL_ELECTRUM_PORT:-50050}"
export BROADCAST_POOL_WEB_HOST="${BROADCAST_POOL_WEB_HOST:-0.0.0.0}"
export BROADCAST_POOL_WEB_PORT="${BROADCAST_POOL_WEB_PORT:-8080}"
# Deliberately NOT set: BROADCAST_POOL_NETWORK (auto-detected), BROADCAST_POOL_UMBREL.

log "Starting broadcast-pool (network auto-detected from Bitcoin Core)"
exec broadcast-pool start --foreground
