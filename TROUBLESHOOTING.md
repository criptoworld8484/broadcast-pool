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

## 7. Sparrow conecta pero las txs no llegan al broadcast pool

### Diagnóstico rápido (ejecutar en el nodo Umbrel)

```bash
# 1. Logs recientes del contenedor (últimas 200 líneas)
sudo docker logs sparrow-broadcast-pool_web_1 --tail 200 2>&1

# 2. Verificar que el servidor Electrum escucha en 50050
sudo ss -tlnp | grep 50050

# 3. Versión de la imagen desplegada
sudo docker inspect sparrow-broadcast-pool_web_1 --format '{{.Config.Image}}'

# 4. Variables de entorno relevantes
sudo docker inspect sparrow-broadcast-pool_web_1 --format '{{range .Config.Env}}{{println .}}{{end}}' | grep -E "ELECTRS|NETWORK|UMBREL|LAN_IP"

# 5. Boot log (últimas 20 líneas)
sudo cat $(sudo docker inspect sparrow-broadcast-pool_web_1 --format '{{range .Mounts}}{{if eq .Destination "/home/app/data"}}{{.Source}}{{end}}{{end}}')/umbrel-boot.log 2>/dev/null | tail -20

# 6. Test manual de broadcast vía netcat
echo '{"jsonrpc":"2.0","method":"blockchain.transaction.broadcast","params":["010000000100000000000000000000000000000000000000000000000000000000000000000000000000ffffffff0200f2052a01000000001976a914000000000000000000000000000000000000000088ac000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"],"id":1}' | nc -w 5 127.0.0.1 50050
```

### Qué buscar en los logs

| Log esperado | Significado |
|-------------|-------------|
| `Electrum client connected from IP [sparrow]` | Sparrow se conectó correctamente |
| `Electrum incoming [sparrow] from IP` | Línea RPC recibida de Sparrow |
| `INTERCEPTED broadcast RPC` | Broadcast detectado y procesado |
| `Pre-ack virtual mempool: txid=...` | Tx almacenada en mempool virtual |
| `Broadcast ack sent to wallet` | Ack enviado a Sparrow |
| `Electrum RPC from IP: [blockchain.transaction.broadcast]` | Broadcast detectado vía subrequests |
| `Unparsed broadcast request` | Broadcast detectado pero no parseable |
| `Invalid broadcast params` | Formato de params no reconocido |

### Si NO hay logs de "INTERCEPTED broadcast"

El broadcast no está llegando al servidor Electrum. Causas:

1. **Sparrow envía a otro servidor** — verificar en Sparrow: Settings → Server → URL debe ser `tcp://IP-LAN:50050` (NO electrs `:50001`)
2. **Tor/Proxy activo en Sparrow** — Settings → Network → desactivar proxy/Tor
3. **Firewall bloquea puerto 50050** — `sudo iptables -L -n | grep 50050`
4. **Imagen antigua** — `docker inspect` debe mostrar tag `≥ 0.3.3`

> **Caso típico:** El log muestra `Sparrow session ended ... without any broadcast RPC` con solo ~12 líneas manejadas (handshake + fee queries). Esto confirma que Sparrow cierra la conexión sin enviar el broadcast — el proxy/Tor está redirigiendo el broadcast a mempool.space.

### Si hay "Unparsed broadcast request"

El broadcast se detectó pero `extract_broadcast_hex` no pudo extraer el hex. Causa probable: Sparrow envía params en formato no esperado. Pegar la línea completa del log para diagnosticar.

---

## 8. Sparrow no conecta a broadcast pool

### Diagnóstico rápido (ejecutar en el nodo Umbrel)

```bash
# 1. Logs recientes del contenedor
sudo docker logs sparrow-broadcast-pool_web_1 --tail 50 2>&1

# 2. Versión de imagen desplegada
sudo docker inspect sparrow-broadcast-pool_web_1 --format '{{.Config.Image}}'

# 3. Puerto escuchando
sudo ss -tlnp | grep 50050

# 4. Contenedor activo
sudo docker ps --filter "name=sparrow-broadcast-pool" --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}"

# 5. Connectivity test desde otra máquina en la LAN
nc -zv IP-NODO-UMBREL 50050
```

### Qué buscar en los logs

| Log esperado | Significado |
|-------------|-------------|
| `Electrum listener [sparrow] bound on 0.0.0.0:50050` | Servidor Electrum escuchando |
| `Electrum client connected from IP [sparrow]` | Sparrow se conectó |
| `Electrum session started for IP [sparrow] (v0.3.3, sync RPC responses)` | Versión correcta con fix |
| `Electrum RPC from IP: [server.version]` | Handshake completado |
| `Sparrow session ended ... without any broadcast RPC` | Sparrow se desconectó sin enviar tx |

### Si NO hay "Electrum client connected"

1. **Puerto cerrado** — `sudo ss -tlnp | grep 50050` debe mostrar docker-proxy
2. **URL incorrecta en Sparrow** — debe ser `tcp://192.168.50.26:50050` (NO `:50001`)
3. **Firewall** — `sudo iptables -L -n | grep 50050`
4. **Contenedor caído** — `sudo docker ps -a --filter "name=sparrow-broadcast-pool"`

### Si hay conexión pero Sparrow muestra "Connecting…"

1. Imagen debe ser **≥ 0.3.3** (ver `docker inspect`)
2. Log debe decir **`(sync RPC responses)`**
3. Si hay decenas de `blockchain.scripthash.subscribe` sin respuesta → versión antigua
4. Actualizar la app desde Umbrel

### Si Sparrow conecta pero no envía broadcasts

Ver sección **7. Sparrow conecta pero las txs no llegan** (Tor/proxy activo).

---

## 9. Debug completo en Umbrel (logs debug)

### Paso 1: Obtener credenciales reales del contenedor

```bash
# Ver RPC URL real
docker inspect sparrow-broadcast-pool_web_1 --format '{{range .Config.Env}}{{println .}}{{end}}' | grep RPC_URL

# Ver RPC password real
docker inspect sparrow-broadcast-pool_web_1 --format '{{range .Config.Env}}{{println .}}{{end}}' | grep RPC_PASS

# Ver data dir real
docker inspect sparrow-broadcast-pool_web_1 --format '{{range .Mounts}}{{if eq .Destination "/home/app/data"}}{{.Source}}{{end}}{{end}}'
```

### Paso 2: Ejecutar debug (credenciales del nodo ejemplo)

> **IMPORTANTE:** Parar el contenedor PRIMERO, esperar 5s, confirmar puerto libre, LUEGO ejecutar debug.
> Ajustar `RPC_URL`, `RPC_USER`, `RPC_PASS` según los valores reales del paso 1.

```bash
# 1. Parar el contenedor original
docker stop sparrow-broadcast-pool_web_1

# 2. Esperar a que suelte los puertos
sleep 5

# 3. Confirmar que 50050 está libre
ss -tlnp | grep 50050

# 4. Ejecutar debug (CREDENCIALES REALES)
docker run --rm \
  --name bp-debug \
  --network host \
  -e BROADCAST_POOL_DATA_DIR=/home/app/data \
  -e BROADCAST_POOL_NETWORK=signet \
  -e BROADCAST_POOL_RPC_URL="http://10.21.21.8:8332" \
  -e BROADCAST_POOL_RPC_USER="umbrel" \
  -e BROADCAST_POOL_RPC_PASS="PfVlfvcGB8tWH2q-BfG_64kn3zjcCFYiwZg_VLgisD4=" \
  -e APP_ELECTRS_NODE_IP=10.21.21.10 \
  -e APP_ELECTRS_NODE_PORT=50001 \
  -e BROADCAST_POOL_ELECTRUM_HOST=0.0.0.0 \
  -e BROADCAST_POOL_ELECTRUM_PORT=50050 \
  -e BROADCAST_POOL_LAN_IP=192.168.50.26 \
  -e BROADCAST_POOL_UMBREL=1 \
  -e BROADCAST_POOL_WEB_HOST=0.0.0.0 \
  -e BROADCAST_POOL_WEB_PORT=8080 \
  -v "/home/umbrel/umbrel/app-data/sparrow-broadcast-pool/data":/home/app/data \
  -e RUST_LOG=debug \
  ghcr.io/criptoworld8484/broadcast-pool-umbrel:0.3.2 \
  broadcast-pool start --foreground 2>&1 | tee /tmp/bp-debug.log

# 5. Después de probar, restaurar el contenedor original
docker start sparrow-broadcast-pool_web_1
```

### Paso 3: Ver los logs debug

```bash
# Últimas 100 líneas
cat /tmp/bp-debug.log | tail -100

# Filtrar solo broadcast/errores
grep -iE "(broadcast|INTERCEPTED|error|panic|tx_rpc|incoming)" /tmp/bp-debug.log | tail -50
```

### Errores comunes

| Error | Causa | Solución |
|-------|-------|----------|
| `Address already in use (os error 98)` | Contenedor original aún corriendo | `docker stop` + `sleep 5` antes del debug |
| `No route to host` (RPC) | RPC_URL o IP incorrecta | Verificar con `docker inspect` las variables reales |
| `No such container` en `sleep` | Comando `sleep` ejecutado como contenedor | Usar `sleep 5` como comando separado, no dentro de `docker` |

---

## 10. Frigate en Umbrel - Troubleshooting

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

---

## 11. Sparrow no conecta (debug rápido en Umbrel)

### Diagnóstico paso a paso (ejecutar en el nodo)

```bash
# 1. Ver estado de todos los contenedores
docker ps -a --format '{{.Names}} | {{.Status}}' | grep -i 'sparrow\|broadcast'

# 2. Ver qué puertos están en uso
ss -tlnp | grep -E '50050|8080'

# 3. Ver logs recientes del contenedor
docker logs sparrow-broadcast-pool_web_1 --tail 50 2>&1

# 4. Si hay un proceso debug bloqueando puertos, matarlo
pkill -f 'broadcast-pool.*--foreground' || true
sleep 3

# 5. Reiniciar el contenedor original
docker start sparrow-broadcast-pool_web_1

# 6. Verificar que arrancó
docker logs sparrow-broadcast-pool_web_1 --tail 10 2>&1

# 7. Verificar que el puerto escucha
ss -tlnp | grep 50050
```

### Causas más comunes de "no conecta"

| Causa | Verificar | Solución |
|-------|-----------|----------|
| Debug process sigue corriendo | `ps aux \| grep broadcast-pool` | `pkill -f broadcast-pool` y luego `docker start` |
| Puerto 50050 ocupado | `ss -tlnp \| grep 50050` | Matar proceso que lo bloquea |
| Contenedor detenido | `docker ps -a \| grep web_1` | `docker start sparrow-broadcast-pool_web_1` |
| Imagen corrupta | `docker inspect ... --format '{{.Config.Image}}'` | Reinstalar app desde Umbrel |
| IP incorrecta en Sparrow | Sparrow → Settings → Server URL | Debe ser `tcp://192.168.50.26:50050` |