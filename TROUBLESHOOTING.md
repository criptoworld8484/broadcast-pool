# Broadcast Pool - Troubleshooting Commands

## 1. Compilar e Instalar

```bash
cd /home/criptoworld/Documents/OpenCode/Mywalletcompromise
cargo build --release
pkill broadcast-pool 2>/dev/null
cp target/release/broadcast-pool ~/.local/bin/broadcast-pool
```

## 2. Ejecutar con Debug (para ver requests de Sparrow)

```bash
pkill broadcast-pool 2>/dev/null
RUST_LOG=debug,broadcast_pool::rpc::electrum=warn,broadcast_pool::pool::scheduler=warn broadcast-pool start 2>&1 | tee /tmp/broadcast.log
```

## 3. Ver logs filtrados

```bash
# Ver solo requests Electrum
grep -E "(Received Electrum|broadcast|handle_broadcast|Method)" /tmp/broadcast.log
```

## 4. Verificar conexiones activas

```bash
ss -tlnp | grep 50050
```

## 5. Borrar DB (si hay error de columnas)

```bash
pkill broadcast-pool 2>/dev/null
rm -rf ~/.local/share/broadcast-pool/testnet4/*.db*
rm ~/.config/broadcast-pool/config.toml
broadcast-pool start
```

## 6. Test de conexión manual

```bash
nc -zv 127.0.0.1 50050
```

## 6b. Sparrow "Connecting…" en Umbrel (solución v0.2.16+)

Si Sparrow se queda en `Connecting to tcp://IP:50050` pero los logs muestran `Electrum client connected from …`:

1. Confirmar imagen **≥ 0.2.16**: `sudo docker inspect sparrow-broadcast-pool_web_1 --format '{{.Config.Image}}'`
2. Log debe decir **`(sync RPC responses)`**, no `(indexer after handshake)`
3. Si hay decenas de `blockchain.scripthash.subscribe` seguidos → versión antigua; actualizar/reinstalar

Documentación completa: [`.cursor/skills/umbrel-apps-issue/reference.md`](.cursor/skills/umbrel-apps-issue/reference.md) → sección *Sparrow stuck on Connecting*.

Tests locales antes de publicar:

```bash
bash scripts/test-sparrow-lines.sh    # un RPC por línea (Sparrow real)
bash scripts/test-umbrel-handshake.sh  # batch JSON (alternativo)
```

--- (fecha/hora o precio BTC)

TX en modo `manual` (Liana, Sparrow sin nLockTime, o nLockTime de bloque ya alcanzado) pueden programarse desde el dashboard o la API:

| Criterio | API body (`POST /api/transactions/{id}/schedule`) |
|----------|---------------------------------------------------|
| **Fecha y hora** | `{ "scheduled_time": "2026-06-15T14:30:00Z", "fixed_fee_rate": 5 }` |
| **Precio fiat** | `{ "target_price": 95000, "price_currency": "eur", "price_condition": "above", "fixed_fee_rate": 5 }` |

Variables de entorno para el feed de precios (opcional):

| Variable | Valor | Descripción |
|----------|-------|-------------|
| `BROADCAST_POOL_CMC_API_KEY` | clave API | Respaldo opcional CoinMarketCap (4.º en la cadena) |

Cadena de fallback automática (cada 60s): **Kraken → CoinGecko → Bitstamp → CoinMarketCap** (si hay clave).
Si todas fallan, se usa caché reciente (hasta 10 min, marcada como stale; no dispara triggers).

Programación por **precio** solo para TX **manual** con **nLockTime = 0** (típ. wallet con locktime desactivado).
Liana (anti-fee-sniping con altura en nLockTime) solo admite programación por **fecha/hora**.

---

## 7. Frigate en Umbrel - Troubleshooting

### Verificar estado del contenedor

```bash
ssh umbrel@umbrel.local
docker ps -a --filter "name=sparrow-frigate" --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}"
```

### Ver logs del contenedor

```bash
docker logs sparrow-frigate_frigate_1 2>&1 | tail -50
```

### Ver logs completos

```bash
docker logs sparrow-frigate_frigate_1 2>&1
```

### Reiniciar el contenedor

```bash
docker restart sparrow-frigate_frigate_1
```

### Eliminar la app y reinstalar

```bash
rm -rf ~/umbrel/app-data/sparrow-frigate
```

### Verificar que la imagen Docker existe

```bash
docker pull ghcr.io/criptoworld8484/frigate-umbrel/frigate-umbrel:latest
```

### Verificar variables de entorno de Umbrel

```bash
docker inspect sparrow-frigate_frigate_1 --format '{{range .Config.Env}}{{println .}}{{end}}'
```

### Entrar al contenedor para debug

```bash
docker exec -it sparrow-frigate_frigate_1 /bin/bash
```

### Verificar conexión a Bitcoin Core

```bash
# Desde dentro del contenedor
curl -s http://127.0.0.1:48332/ -u user:pass -d '{"jsonrpc":"1.0","id":"test","method":"getblockchaininfo"}' -H 'Content-Type: application/json'
```