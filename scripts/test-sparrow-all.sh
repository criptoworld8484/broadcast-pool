#!/bin/bash
# Full Sparrow compatibility: connect handshake + all broadcast modes.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
pkill -f fake-electrs.py 2>/dev/null || true
pkill -9 -f 'broadcast-pool' 2>/dev/null || true
fuser -k 50050/tcp 59999/tcp 18080/tcp 2>/dev/null || true
sleep 2
echo "=== Sparrow connect (headers.subscribe height > 0) ==="
bash "$ROOT/scripts/test-sparrow-lines.sh"
sleep 2
pkill -f fake-electrs.py 2>/dev/null || true
pkill -9 -f 'broadcast-pool' 2>/dev/null || true
fuser -k 50050/tcp 59999/tcp 18080/tcp 2>/dev/null || true
sleep 2
echo ""
echo "=== Sparrow broadcast (manual, timestamp, by_block) ==="
bash "$ROOT/scripts/test-sparrow-broadcast.sh"
echo ""
echo "ALL SPARROW TESTS PASSED (v$(grep '^version' "$ROOT/Cargo.toml" | head -1 | cut -d'"' -f2))"
