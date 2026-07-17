# Diseño: tipo "importada" + limpieza de indexadores externos

Fecha: 2026-07-18
Rama: `feature/imported-type-and-cleanup`
Base: `origin/master` (v0.3.19)

## Contexto

Dos mejoras independientes que se liberan juntas:

- **Tarea A** — Eliminar del dashboard el indexador **secundario** (tercer fallback) y el **externo**
  (override manual). En Umbrel y Start9 un indexador externo (LAN o internet) no funciona, así que
  ambos campos sobran. Queda: **indexador primario (auto-descubierto) + Bitcoin Core de backup**,
  como antes de esas mejoras.
- **Tarea B** — Las tx **importadas** deben (1) mostrarse como tipo **"Importada"** en la columna
  TYPE, distinto de **"Manual"**, y (2) poder programarse por **fecha/hora Y por precio**, igual que
  las Manual. Hoy solo permiten fecha/hora.

## Tarea A — Eliminación de indexadores secundario y externo

### A1 — Indexador secundario (HECHO)

Merge de la rama `remove-indexer-fields` (revert de 1 commit ya en `origin`). Elimina:
`config.secondary_indexer`, los campos de API (`secondary_indexer_url`, `secondary_up`),
`ChainSource::SecondaryIndexer` y `ChainHealth.secondary_*`, y los campos del dashboard del
secundario. Verificado: compila, 56 tests en verde.

### A2 — Indexador externo (override manual), eliminación completa

El campo **"External Indexer (optional)"** (input `cfg-electrs`, botones Test/Discover) sirve para
apuntar manualmente a un indexador en modo dev/LAN; **ya está oculto en Umbrel/Start9**. Decisión del
usuario (2026-07-18): **eliminarlo por completo**. En dev/bare-metal el indexador queda solo por
auto-discovery (sin override manual en la UI).

**Frontend (`src/api/dashboard.html`):**
- Eliminar el grupo `cfg-external-indexer-group` (input `cfg-electrs`), el botón
  `btn-discover-indexer` y el botón Test asociado.
- Eliminar las funciones JS `testIndexer()` y `discoverIndexer()` y todo su cableado (listeners,
  lecturas de `cfg-electrs`).
- Eliminar las claves i18n `cfg_indexer_external`, `cfg_indexer_test`, `cfg_indexer_discover`
  (en/es) y el `indexer-fallback-hint` si solo servía a este campo.
- En `saveConfig`, dejar de enviar `indexer_url`.

**Backend (`src/api/mod.rs`):**
- Eliminar las rutas `/api/test-indexer` y `/api/discover-indexer` y sus handlers `test_indexer` /
  `discover_indexer`.
- En `save_config`, eliminar la rama `else if let Some(url) = req.indexer_url { … manual_override:
  true … }` y el campo `indexer_url` de `SaveConfigRequest`.
- Mantener el resto del status (`indexer_url` de solo lectura para el banner degradado sigue siendo
  útil y se calcula desde el indexador activo).

**Config / discovery:**
- **Se conserva** el campo `IndexerConfig.manual_override` y la vía de pin por entorno
  (`BROADCAST_POOL_INDEXER_URL`), que es como Umbrel/Start9 fijan el indexador; ya no lo activa
  ninguna UI, pero sigue siendo la fuente para el modo auto-config. No se toca la lógica de
  auto-discovery ni el `sanitize` de `discovery.rs`.

**Criterio de hecho A2:** un grep de `test-indexer|discover-indexer|cfg-electrs|cfg_indexer_external|
External Indexer|indexer_url:` en `src/` no devuelve referencias vivas de UI/override manual;
compila; los tests pasan; en modo dev el dashboard ya no muestra ningún campo de indexador externo.

## Tarea B — Tipo "importada" y paridad de programación con "manual"

### Causa raíz

Al importar (`POST /api/transactions/import`), el backend crea `NewBroadcastTx` con
`broadcast_mode: None`, y el INSERT lo **defaultea a `"immediate"`** (`db/mod.rs:120`). Consecuencias:

- La columna TYPE muestra el modo por defecto, no "importada" (`typeLabel = tx.broadcast_mode || '-'`,
  `dashboard.html:2070`).
- `canUsePriceTrigger(tx)` exige `broadcast_mode === 'manual'` (`dashboard.html:1758`) → la importada
  solo ofrece fecha/hora.
- `PoolManager::schedule_by_price()` rechaza si el modo no es `"manual"` (`manager.rs:177`).

### Enfoque: `imported` como sinónimo de programación de `manual`, distinto solo en la etiqueta

**Regla:** a efectos de **programación**, `imported` se comporta **idéntico** a `manual` (ambos
criterios: fecha/hora y precio). A efectos de **tipo/etiqueta**, se distinguen: "Importada" vs
"Manual". No se toca el flujo de `manual` ni el de las wallet-txs.

**1. Import fija el modo explícito (`src/api/mod.rs`):**
- `import_transaction` crea el `NewBroadcastTx` con `broadcast_mode: Some("imported")` en vez de
  `None`.

**2. Predicado único de "programable por el usuario" (`src/pool/manager.rs`):**
- Introducir `fn is_user_scheduled_mode(mode: Option<&str>) -> bool` que devuelve `true` para
  `Some("manual") | Some("imported")`.
- Sustituir los `== Some("manual")` que gobiernan **elegibilidad de programación** por ese predicado:
  - `schedule_by_price` (guard de línea ~177).
  - `is_reschedule` en `schedule_at` (~137).
  - `tx_has_broadcast_schedule` (~806).
- **Consultas SQL de "due"** (`db/mod.rs`): las que filtran `broadcast_mode IN ('scheduled',
  'manual')` (p. ej. `get_pending_by_scheduled_time`, línea ~374) pasan a incluir `'imported'`.

**3. Columna TYPE (`src/api/dashboard.html`):**
- Etiqueta legible por modo: `imported` → **"Importada"/"Imported"**; `manual` → **"Manual"** (hoy
  se pinta el valor crudo del modo). Mapear vía i18n (`type_imported`, `type_manual`, y de paso los
  demás modos para consistencia).
- Añadir clase de badge `.type-imported` con color propio (distinto del morado de `.type-manual`).

**4. Puertas de UI (`src/api/dashboard.html`):**
- `canUsePriceTrigger`, `shouldShowSchedule`, `shouldShowReschedule`: donde comprueban
  `broadcast_mode === 'manual'`, aceptar también `'imported'` (helper JS
  `isUserScheduledMode(mode)`).

**5. Migración de datos existentes (`src/db/mod.rs`):**
- Migración idempotente: `UPDATE broadcast_pool SET broadcast_mode = 'imported' WHERE broadcast_mode
  = 'immediate' AND status = 'pending'`. Una tx "immediate" real no queda pendiente (se difunde al
  instante), así que el criterio no captura falsos positivos.

**Criterio de hecho B:**
- Importar una tx → aparece en TYPE como "Importada".
- Pulsar "Programar" en una importada → ofrece **fecha/hora Y precio** (igual que una Manual).
- Programar por precio una importada → el backend la acepta y el scheduler la dispara al cumplirse.
- Una tx Manual sigue mostrándose como "Manual" y comportándose como hoy.

## No incluido (YAGNI)

- No se elimina el campo `manual_override` ni el pin por entorno `BROADCAST_POOL_INDEXER_URL`
  (sigue siendo la vía de auto-config en Umbrel/Start9).
- No se refactoriza la lógica de auto-discovery ni `discovery.rs` más allá de quitar referencias
  muertas del override manual de UI.
- No se cambia el comportamiento de las wallet-txs (`immediate`/`scheduled`/`by_block`).

## Riesgos

- **A2**: arrancar el override manual de UI podría dejar referencias colgadas (JS/rutas). Mitigación:
  grep de cierre en el criterio de hecho.
- **B/migración**: el criterio `immediate + pending` para reclasificar a `imported` asume que no hay
  otra fuente de tx pending con modo immediate. Verificado contra el flujo de ingesta; en el peor
  caso solo cambia una etiqueta, sin pérdida de fondos ni de programación.

## Versión / release

Tras implementar y verificar en local + nodo: bump de versión (0.3.19 → 0.3.20) en `Cargo.toml`,
`umbrel-app/` y `start9/`, y seguir el flujo de release habitual. La imagen a probar en el nodo se
construye desde esta rama.
