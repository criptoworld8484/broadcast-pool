# Diseño: Adaptación de Broadcast Pool a Start9 (StartOS)

Fecha: 2026-06-25 · Rama: `start9-adaptation`

## Contexto

Broadcast Pool ya funciona empaquetado para Umbrel (`umbrel-app/sparrow-broadcast-pool/`).
El binario Rust `broadcast-pool` es un intermediario Electrum entre wallets (Sparrow/Liana)
y un indexador (electrs/Fulcrum), con mempool virtual para programar/retrasar broadcasts.

Queremos ofrecerlo también en **Start9 (StartOS)** sin afectar al proyecto Umbrel. El binario
es **agnóstico de distro**: su superficie de integración es su contrato de variables de entorno
(`BROADCAST_POOL_*`). Se verificó que funciona en modo **genérico** (sin `BROADCAST_POOL_UMBREL=1`):
`BROADCAST_POOL_INDEXER_URL` se lee directo y la resolución de genesis usa solo `BROADCAST_POOL_RPC_*`.

**Objetivo:** un paquete `.s9pk` que envuelve el mismo binario, declarando dependencias de
`bitcoind` + `electrs` y exponiendo el proxy Electrum (Sparrow/Liana) y el dashboard web.

**Decisiones tomadas (brainstorming):**
- Mismo repo, **rama aislada** `start9-adaptation`, todo bajo `start9/` (no toca archivos Umbrel).
- Nodo de pruebas Start9 disponible con acceso SSH/web, indexador **electrs**, red **mainnet**.
- Verificación = **pruebas de humo seguras** (conexión, genesis, handshake, dashboard, que
  Sparrow/Liana conecten). **NO** se difundirá una tx de valor real.

## Arquitectura

Patrón espejo de `criptoworld8484/sparrow-frigate-startos` (StartOS TypeScript SDK), que ya
depende de bitcoind+electrs y expone una interfaz Electrum — forma casi idéntica a la nuestra.

```
Sparrow / Liana ──TCP LAN:50050──▶ broadcast-pool (.s9pk)
                                      │  (mismo binario Rust; entrypoint StartOS)
                                      ├──▶ electrs.startos:50001   (upstream índice)
                                      └──▶ bitcoind.startos:8332   (genesis/red/altura, cookie auth)
Navegador ──LAN/Tor HTTPS──▶ dashboard web :8080
```

El binario Rust **no se modifica** (salvo un posible ajuste menor: que una `INDEXER_URL`
explícita no sea pisada por el LAN-scan en modo no-Umbrel; se valida en implementación).

## Componentes (todo bajo `start9/`)

Estructura TS SDK (como frigate):
- `start9/startos/manifest/index.ts` — id `broadcast-pool`, imágenes (dockerBuild), deps bitcoind+electrs.
- `start9/startos/dependencies.ts` — bitcoind (running, prune=0) + electrs (running).
- `start9/startos/interfaces.ts` — **dos** interfaces:
  - `electrum` (TCP, sin SSL) en 50050 — Sparrow/Liana.
  - `ui` (web) en 8080 — dashboard.
- `start9/startos/main.ts` — subcontenedor, monta volumen `main`→`/data` y bitcoind RO→`/mnt/bitcoind`,
  daemon `entrypoint.sh`, health-check `checkPortListening(50050)`.
- `start9/startos/utils.ts` — constantes de puerto (`electrumPort=50050`, `webPort=8080`).
- `start9/startos/{index,versions/current,init,backups,actions,sdk,i18n}.ts` — plumbing estándar.
- `start9/Dockerfile` — build multi-stage del binario Rust (context = raíz del repo), arch x86_64+aarch64.
- `start9/entrypoint.sh` — traduce StartOS → env vars del binario (ver mapeo).
- `start9/{Makefile,s9pk.mk,package.json,tsconfig.json}` + `.github/workflows/build-s9pk.yml`.

## Mapeo de configuración (entrypoint.sh StartOS → binario)

| Binario (env)                     | Valor en StartOS                                   |
|-----------------------------------|----------------------------------------------------|
| `BROADCAST_POOL_NETWORK`          | `mainnet` (red del nodo Start9)                    |
| `BROADCAST_POOL_DATA_DIR`         | `/data`                                            |
| `BROADCAST_POOL_RPC_URL`          | `http://bitcoind.startos:8332`                     |
| `BROADCAST_POOL_RPC_USER`/`PASS`  | leídos del cookie `/mnt/bitcoind/.cookie` (`__cookie__:<pass>`) |
| `BROADCAST_POOL_INDEXER_URL`      | `tcp://electrs.startos:50001`                      |
| `BROADCAST_POOL_ELECTRUM_HOST`    | `0.0.0.0`                                           |
| `BROADCAST_POOL_ELECTRUM_PORT`    | `50050`                                            |
| `BROADCAST_POOL_WEB_HOST`/`PORT`  | `0.0.0.0` / `8080`                                 |
| `BROADCAST_POOL_UMBREL`           | **no se define** (modo genérico)                   |

El cookie de bitcoind se resuelve igual que frigate (`/mnt/bitcoind/.cookie`, con fallback a
`find`), se parte por `:` en usuario/contraseña, y se espera a que exista antes de arrancar.

## Manejo de errores
- Esperar el cookie de bitcoind (bucle con timeout) antes de lanzar el binario.
- Health-check de StartOS sobre el puerto 50050: el servicio aparece "starting" hasta que escucha.
- Si electrs aún sincroniza, el binario ya tolera respuestas lentas/erróneas del indexador
  (timeouts + fallback de la mempool virtual); StartOS marca la dependencia electrs por su propio health.

## Pruebas / Verificación (mainnet, sin mover fondos)
1. Build local del `.s9pk` (Makefile) y/o vía workflow `build-s9pk.yml`.
2. Sideload/instalar en el nodo Start9; comprobar que el servicio arranca y la interfaz Electrum
   queda **verde** (health).
3. Smoke test del proxy en la dirección LAN:50050:
   - `server.version` responde.
   - `blockchain.block.header(0)` → hash = genesis mainnet
     `000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f`.
   - `server.features.genesis_hash` coincide con el del nodo.
4. Apuntar **Sparrow** (y opcionalmente **Liana en mainnet**) a la URL Electrum LAN → conecta y
   carga el monedero (solo lectura). **No** difundir una tx real.
5. Dashboard web accesible por la interfaz `ui`.

Tests automatizados del binario Rust: sin cambios (la suite existente cubre la lógica; el
empaquetado no añade lógica Rust nueva salvo el posible ajuste de INDEXER_URL, que llevaría su test).

## Alcance / YAGNI
- v1 expone **un** puerto Electrum (50050) para Sparrow y Liana; el `liana_port` separado
  (PoC de altura/anti-fee-sniping) **no** se expone en v1.
- Solo red **mainnet** (es la del nodo Start9); el binario soporta otras redes si se cambia el env.
- Sin acciones/actions extra más allá de las de plumbing en v1.

## Riesgos
- Posible necesidad de un pequeño ajuste en `discovery.rs`/`main.rs` para que una `INDEXER_URL`
  explícita no sea sobrescrita por el LAN-scan en modo no-Umbrel. Se valida al implementar; si
  hace falta, es un cambio mínimo y testeado, compartido con Umbrel (no lo rompe).
- Versionado/SDK de StartOS: usar la misma versión del `@start9labs/start-sdk` que frigate para
  evitar incompatibilidades.
