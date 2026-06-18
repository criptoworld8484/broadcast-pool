# Broadcast Pool en Umbrel

Paquete para la [Community App Store](https://github.com/criptoworld8484/umbrel-apps) de Umbrel (prefijo `sparrow`).

## Contenido del paquete

```
umbrel-app/sparrow-broadcast-pool/
├── docker-compose.yml   # Servicios Umbrel (web + init + app_proxy)
├── exports.sh           # IP LAN del nodo para URL wallet
├── Dockerfile           # Imagen multi-stage Rust
├── umbrel-app.yml       # Manifest de la tienda
└── icon.png             # Icono de la app (desde icon.png del proyecto)
```

## Publicar en umbrel-apps

1. Sincroniza el paquete (manifest + código fuente para el build Docker):

   ```bash
   SRC=/ruta/a/Mywalletcompromise
   DEST=/ruta/a/umbrel-apps/sparrow-broadcast-pool
   rm -rf "$DEST" && mkdir -p "$DEST"
   cp -r "$SRC/umbrel-app/sparrow-broadcast-pool/"* "$DEST/"
   cp "$SRC/Cargo.toml" "$SRC/Cargo.lock" "$DEST/"
   cp -r "$SRC/src" "$DEST/"
   ```

2. Commit y push a `https://github.com/criptoworld8484/umbrel-apps`

3. En Umbrel: **Ajustes → App Stores → Añadir** la URL del repo de la tienda.

4. Instala **Broadcast Pool** desde la tienda community.

## Dependencias Umbrel

- **Bitcoin Node** (sincronizado)
- **Electrs** (indexador Electrum)

La app usa automáticamente:

| Variable Umbrel | Uso |
|-----------------|-----|
| `APP_BITCOIN_NODE_IP` / RPC | Bitcoin Core |
| `APP_ELECTRS_NODE_IP` | Host del indexador (red Docker interna) |
| `APP_ELECTRS_NODE_PORT` | Puerto electrs (50001 o 50002; se prueban ambos si falla) |
| `APP_SPARROW_BROADCAST_POOL_LAN_IP` | IP LAN del nodo (vía `exports.sh`) → URL wallet |

## Puertos

| Puerto | Uso |
|--------|-----|
| **8080** | Dashboard web (vía proxy Umbrel) |
| **50050** | Servidor Electrum para Sparrow (`IP-LAN:50050`) |
| **50051** | Servidor Electrum dedicado Liana (`IP-LAN:50051`) |

Al arrancar, broadcast-pool:
- Lee la red desde Bitcoin Core (`getblockchaininfo`)
- Detecta electrs/fulcrum probando TCP **50001** y **50002** (genesis hash debe coincidir)
- Muestra en Settings solo **`IP-LAN:50050`** (no IPs Docker internas)

## Sparrow Wallet

1. Instala y abre Broadcast Pool en Umbrel.
2. En el dashboard, copia la URL Electrum (host LAN + puerto 50050).
3. En Sparrow: **Settings → Server** → añade esa URL (no uses electrs directamente).
4. Firma y envía transacciones; el pool las retiene y las difunde según el modo configurado.

### Sparrow no conecta ("Connecting…")

- URL en Sparrow: **`tcp://IP-LAN-UMBREL:50050`** (sin SSL), no electrs `:50001`
- Versión mínima probada en Umbrel: **v0.2.16** (respuesta sync por cada línea RPC)
- Ver troubleshooting: [`TROUBLESHOOTING.md`](TROUBLESHOOTING.md) §6b y [`.cursor/skills/umbrel-apps-issue/reference.md`](.cursor/skills/umbrel-apps-issue/reference.md)

## Desarrollo local (simular Umbrel)

Desde la raíz del proyecto:

```bash
docker compose -f umbrel-app/sparrow-broadcast-pool/docker-compose.yml build
```

> La primera compilación Rust puede tardar varios minutos.

## Imagen Docker precompilada (opcional)

Tras publicar en GHCR, sustituye en `docker-compose.yml` el bloque `build:` por:

```yaml
  web:
    image: ghcr.io/criptoworld8484/broadcast-pool-umbrel:0.1.0
```

El workflow `.github/workflows/umbrel-docker.yml` automatiza el build y push a GHCR.

## Datos persistentes

Volumen Umbrel: `${APP_DATA_DIR}/data` → SQLite y estado del pool en `/home/app/data`.
