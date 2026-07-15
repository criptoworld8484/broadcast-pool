# Tercer fallback: indexador secundario en la LAN — Diseño

Fecha: 2026-07-15
Estado: aprobado (pendiente de plan de implementación)
Relacionado: fallback a Bitcoin Core (v0.3.18, `chain_health.rs`), `mejoras.md` §2.

## Problema

Hoy el reloj de la cadena (altura + MTP) que necesitan las programaciones tiene dos fuentes:
el indexador principal (electrs/Fulcrum) y, si cae, Bitcoin Core (`decide_chain_source`,
`src/pool/chain_health.rs:62`). Si **ambos** fallan a la vez —indexador caído y Core caído o en
IBD— no hay reloj y las programaciones se paran (`ChainSource::None`).

Muchos usuarios tienen **otro nodo con su propio indexador en la LAN**. Este diseño añade ese
indexador como **tercer fallback**, para que las programaciones sigan cumpliéndose incluso cuando
el indexador principal y Bitcoin Core están ambos fuera de servicio.

## Decisiones (acordadas con el usuario)

1. **Orden de prioridad:** Principal → Bitcoin Core → **Secundario** (último recurso). El
   secundario solo entra cuando el principal está caído Y Core no sirve (caído o en IBD).
2. **Alcance del secundario:** solo **reloj de cadena + difusión** (altura, MTP,
   `sendrawtransaction`). NO se enruta el sync del monedero (historial de direcciones) al
   secundario; el proxy hacia Sparrow/Liana sigue sin funcionar hasta que vuelva el principal.
   (Mantener las programaciones es el objetivo; el sync del monedero queda fuera de alcance.)
3. **Sondeo perezoso:** el secundario se sondea **solo cuando el principal está caído**. Mientras
   el principal funcione, no se toca el nodo externo.
4. **Pop-up de primer arranque:** modal propio con campo de URL + "Más tarde", detectado por
   bandera en `localStorage` (igual patrón que el wizard actual). Reconfigurable siempre en Ajustes.

**Distinto del "External Indexer" existente.** El campo actual "External Indexer" (`cfg-electrs`)
**sustituye** al indexador principal (`config.indexer` con `manual_override=true`). El secundario
es un campo **nuevo y separado** (`secondary_indexer`), que no reemplaza a nada: solo es el tercer
nivel de fallback.

## Componentes

### 1. Modelo de fuente de la cadena (`src/pool/chain_health.rs`)

- `ChainSource` gana un estado: `Indexer`, `BitcoinCore`, `SecondaryIndexer`, `None`.
- Nueva función de decisión (sustituye/extiende `decide_chain_source`):

  ```rust
  pub fn decide_chain_source(
      indexer_up: bool,
      core_up: bool,
      core_ibd: bool,
      secondary_up: bool,
  ) -> ChainSource {
      if indexer_up {
          ChainSource::Indexer
      } else if core_up && !core_ibd {
          ChainSource::BitcoinCore
      } else if secondary_up {
          ChainSource::SecondaryIndexer
      } else {
          ChainSource::None
      }
  }
  ```

- `ChainHealth` gana: `secondary_up: bool`, `secondary_configured: bool` (hay URL puesta),
  y reutiliza `height`/`mtp` (que ya reflejan la fuente activa). El `source` sigue calculándose en
  `write_chain_health` (`manager.rs`) con la nueva firma.
- `clock_available()` no cambia (sigue siendo `source != None`).

### 2. Config (`src/config.rs`)

- Nuevo campo en `Config` (o en el bloque que agrupe indexadores):

  ```rust
  #[serde(default)]
  pub secondary_indexer: Option<String>,   // "tcp://host:port" o "ssl://host:port"
  ```

- `#[serde(default)]` para no romper configs antiguas. `default_config()` lo inicializa a `None`.
- No se autodescubre en Umbrel/Start9: lo introduce el usuario.

### 3. Sondeo perezoso y salud (`src/pool/manager.rs::refresh_chain_health`)

- El poller de 30s mantiene el orden actual: proba el principal; si el principal responde,
  **NO** toca el secundario (`secondary_up = false`, no probado — se trata como "desconocido pero
  irrelevante mientras el principal esté vivo").
- El secundario se sondea **solo cuando** `indexer_up == false` (el principal está caído). Se
  construye un `ElectrumClient` efímero contra `config.secondary_indexer` y se pide
  `get_block_height` (+ MTP si hace falta), igual que el principal. Fallo → `secondary_up=false`,
  log en debug.
- `write_chain_health` pasa a llamar `decide_chain_source(indexer_up, core_up, core_ibd,
  secondary_up)`. Cuando la fuente es `SecondaryIndexer`, `height` (y MTP) se toman del secundario.
- Reutiliza el `mtp_cache` como el resto: si el secundario da MTP, se calienta la caché.

### 4. Difusión (`src/pool/manager.rs::broadcast_transaction`)

- Orden actual: indexador principal → Core RPC. Se añade el secundario como **último eslabón**:
  principal → Core RPC → **secundario**. Solo se intenta el secundario si los dos anteriores
  fallan. Se construye un `ElectrumClient` efímero contra `secondary_indexer` y se llama
  `broadcast_transaction`. Si tampoco hay secundario configurado o falla, se devuelve el error
  como hoy.

### 5. API (`src/api/mod.rs`)

- `ConfigResponse`: nuevo `secondary_indexer_url: String` (vacío si no configurado).
- `SaveConfigRequest`: nuevo `secondary_indexer: Option<String>` (cadena vacía = borrar). Se
  normaliza/valida el formato de URL (host:port, esquema tcp/ssl) reutilizando los helpers de
  `discovery` que ya validan el indexador externo.
- `/api/status` (`StatusResponse`): `chain_source` ya se serializa; añadir
  `secondary_indexer_url` y (opcional) `secondary_up` para el banner.
- Endpoint de test: reutilizar `/api/test-indexer` (ya existe) para el botón Test del secundario.

### 6. Dashboard (`src/api/dashboard.html`)

- **Ajustes → nuevo campo "Indexador secundario (opcional)"**, separado del "External Indexer",
  con botón Test (reutiliza `testIndexer`/`/api/test-indexer`) y aviso: "Se usa solo si el
  indexador principal y Bitcoin Core fallan, para que las programaciones sigan cumpliéndose."
  i18n ES/EN en los dos bloques.
- **Banner de estado** (junto a los de indexador/Core): cuando `status.chain_source ===
  'secondary_indexer'`, banner ámbar: "Indexador principal y Bitcoin Core no disponibles — usando
  indexador secundario en `$url`. Las programaciones siguen cumpliéndose." Si `source === 'none'`
  y hay secundario configurado pero caído, el banner duro existente ("programaciones en pausa")
  ya cubre el caso.
- **Pop-up de primer arranque**: modal propio (no el wizard) que aparece si
  `localStorage['bp-secondary-indexer-prompt']` no está puesto. Contenido: explica el caso, campo
  de URL (opcional) + botones "Guardar" y "Más tarde". Al cerrar (cualquiera de los dos) se marca
  la bandera para no repetir. "Guardar" hace el POST a `/api/config` con `secondary_indexer`.
  Reconfigurable siempre en Ajustes.

### 7. Detección de primer arranque

- Bandera en `localStorage` (`bp-secondary-indexer-prompt`), coherente con el wizard actual
  (que también usa localStorage). Per-navegador; aceptable (mismo criterio que el onboarding).

## Datos / flujo (cuando principal + Core caen)

```
poller (30s):
  indexer_up? ── sí ──▶ source=Indexer (fin; secundario NO se sondea)
       │ no
       ▼
  core_up && !ibd? ── sí ──▶ source=BitcoinCore (fin; secundario NO se sondea)
       │ no
       ▼
  secondary configurado? ── no ──▶ source=None (programaciones en pausa)
       │ sí
       ▼
  probe secundario (get_block_height/MTP)
       ├─ ok  ──▶ source=SecondaryIndexer  (altura+MTP del secundario; schedules siguen)
       └─ fail ─▶ source=None

difusión: indexer principal → Core RPC → indexer secundario
```

## Manejo de errores / bordes

- **Secundario = mismo host que el principal:** permitido (no lo impedimos); si el usuario apunta
  al mismo nodo, simplemente no aportará resiliencia. No es un error.
- **URL malformada al guardar:** validar y devolver 400 con mensaje claro (reutilizar validación
  del external indexer).
- **Secundario en otra red (mainnet vs testnet4):** riesgo de altura incoherente. Mínimo: al hacer
  Test, si el genesis/red no coincide, avisar. (Si es costoso, dejar el aviso para el plan; no
  bloquea el diseño.)
- **Reinicio del contenedor:** `secondary_indexer` vive en config → sobrevive.
- **Sondeo perezoso y "readiness":** por diseño NO sabemos si el secundario está listo hasta que el
  principal cae. El dashboard no muestra "secundario listo" de antemano (a diferencia de Core). Es
  el compromiso aceptado a cambio de no cargar el nodo externo.

## Pruebas

- **Unitarias (`chain_health`)**: `decide_chain_source` con la 4.ª entrada — secundario nunca gana
  al principal ni a un Core sano; secundario gana solo cuando principal caído + Core no sirve;
  nada configurado → None.
- **Unitarias (salud)**: el snapshot marca `SecondaryIndexer` con altura del secundario; con el
  principal sano, `secondary_up` no se sondea (queda false / no consultado).
- **Unitarias (config)**: `secondary_indexer` serializa/deserializa; ausencia → None (compat).
- **Unitarias (API)**: validación de URL del secundario (vacía = borrar; malformada = error).
- **Integración/manual en el nodo (testnet4)**: parar el indexador principal Y Core (o simular),
  con un segundo indexador en la LAN configurado, y ver `source=SecondaryIndexer`, altura fresca y
  una tx por bloque difundiéndose. Más el pop-up de primer arranque y el banner.

## Fuera de alcance

- Enrutar el sync del monedero (historial de direcciones) al secundario — solo reloj + difusión.
- Sondeo proactivo del secundario (se descartó a favor del perezoso).
- Múltiples indexadores secundarios / lista priorizada — un solo secundario.
- Autodescubrimiento del secundario en la LAN — lo introduce el usuario.
