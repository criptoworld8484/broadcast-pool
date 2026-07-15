# Bloque virtual para Liana (ciclado de UTXO) — Diseño

Fecha: 2026-07-14
Estado: implementado; premisa corregida tras E2E (2026-07-15).
Relacionado: `mejoras.md` §3 ("nLockTime rancio"), decisión 2026-07-14 "bloque virtual futuro para Liana".

## ADENDA 2026-07-15 — Hallazgo del E2E contra Liana real y pivote a "retener por objetivo de config"

La prueba end-to-end en el nodo (testnet4, Umbrel) reveló que **la premisa original era
falsa para Liana**: se le sirvió la altura virtual (144196) y Liana **aceptó las cabeceras
sintéticas sin PoW** y construyó/envió su tx de refresco — pero la firmó con **`nLockTime = 0`**
(`nSequence = 0xfffffffd`, RBF), no con la altura servida. El ciclado de UTXO de Liana usa un
timelock **relativo** (CSV en el descriptor miniscript), que no necesita `nLockTime` absoluto;
la altura servida le sirve para *ver* la cadena adelantada, pero no acaba en la transacción.

Consecuencias:
- La huella anti-fingerprinting (que el `nLockTime` on-chain parezca reciente) **no se consigue**
  por esta vía: Liana no escribe la altura en el `nLockTime`. (`nLockTime=0` tampoco es la huella
  "rancia" que se quería evitar.)
- El "retener y difundir en el bloque V" **sí se consigue**, desacoplándolo del `nLockTime` de la
  tx.

**Decisión (Opción 1, aprobada por el usuario):** con el tick armado, una tx de Liana —incluida
`nLockTime=0`— se ingesta como `by_block` cuyo objetivo es la **altura de config armada** que se le
sirvió, almacenada en la columna `nlocktime` de la BD. El scheduler `by_block` existente la retiene
hasta que la cadena real alcanza ese objetivo y la difunde (la tx con `nLockTime=0` es válida a
cualquier altura). El escalón +2 por captura se mantiene. Verificado en vivo en la rc2: la tx entra
`mode=by_block, nlocktime=<objetivo>, source=liana` y queda retenida, en vez de caer a manual.

El resto del documento describe el diseño original; la sección de ingesta (§Componentes → Ingesta)
queda sustituida por esta adenda en lo relativo a de dónde sale el objetivo (config, no `nLockTime`).

## Problema

Liana firma transacciones con un `nLockTime` por altura de bloque. Hoy, cuando esa tx llega
a broadcast pool por el puerto Electrum compartido, se ingesta como **manual/pending**
(`resolve_ingest_plan`, `src/electrum_server/mod.rs:2159`) y el usuario le pone criterio a mano.

El caso de uso que no cubrimos es el **ciclado de UTXO de Liana**: el usuario quiere refrescar
un UTXO *antes* de que su recovery path miniscript pase a ser gastable. Para eso necesita que
Liana firme una tx de refresco con un `nLockTime` en un **bloque futuro concreto** (anterior a
la expiración del ciclado), y que alguien la retenga y la difunda cuando llegue ese bloque.
Liana solo construye/firma esa tx si cree que la cadena está en esa altura — de ahí la necesidad
de servirle una **altura de bloque virtual** por delante de la real.

Beneficio secundario (anti-fingerprinting): al servir alturas escalonadas, las tx sucesivas no
comparten `nLockTime`, dificultando enlazarlas como del mismo dueño.

## Modelo de comportamiento (decidido con el usuario)

### Configuración (Ajustes → bajo "Modo de transmisión = Programada")

- Casilla **"Programar ciclado UTXO de Liana"**. Al marcarla se revela:
  - Campo **altura de bloque virtual (absoluta)** — el usuario escribe el número de bloque
    (p. ej. `950430`). **No es un offset** sobre la punta; es la altura objetivo.
  - **Fecha aproximada** de ese bloque, calculada a ~10 min/bloque desde la punta real, para
    que el usuario la alinee con la fecha de expiración de su ciclado.
  - **Red flag** (aviso destacado): "Introduzca la altura de bloque virtual que se pasará a
    Liana. Debe ser un bloque futuro **anterior** a la expiración del ciclado de su UTXO y a que
    el recovery path miniscript pueda gastar. Con esta opción activa, Liana mostrará una altura
    falsa; cualquier tx que construya es no-final hasta esa altura y la rechazará la red si se
    difunde por otra vía."

### Caso canónico (del usuario)

- Altura real 950350. El recovery path se vuelve gastable en 950450.
- El usuario marca el tick y configura altura virtual 950430 (futura, < 950450).
- Liana recibe altura 950430, firma la tx de refresco con `nLockTime = 950430`, la manda al pool.
- El pool la retiene (es no-final hasta 950430) y la difunde cuando la cadena real llega a 950430.
- El UTXO queda refrescado a tiempo; el recovery path nunca llega a poder gastarlo.

### Armado y autodesactivación

- Al marcar el tick y guardar, se registra `H0 = altura real actual` y el estado queda **armado**.
- El tick se **autodesactiva** cuando la cadena real alcanza `H0 + 10` (ventana ~100 min), o
  cuando el usuario lo desmarca. Es la red de seguridad: la ventana en que servimos altura falsa
  es acotada.

### Servido a Liana

- El valor servido arranca en la V configurada. **La 1.ª tx capturada sale con V exactamente**
  (sin +2). Tras capturar una tx, el valor a servir avanza **+2** (2.ª tx → V+2, 3.ª → V+4…).
  El +2 es el escalón de decorrelación entre tx sucesivas.
- Solo se aplica a sesiones detectadas como **Liana**: las que **no** han enviado `server.version`
  (Sparrow siempre lo envía antes de pedir cabeceras). Sparrow recibe **siempre** la altura real.

### Ingesta

- Con el tick armado, una tx de Liana con `nLockTime = V` (locktime por altura, `<= 500_000_000`)
  se clasifica como **`by_block` con objetivo V**, invirtiendo la regla actual que la fuerza a
  manual (`resolve_ingest_plan`). Se retiene y se difunde cuando `altura_real >= V`.
- La tx capturada es **reprogramable y eliminable** desde el dashboard como cualquier programada
  (`/api/transactions/{id}/schedule` y `/remove`, ya existentes). **Matiz:** el `nLockTime`
  firmado es inmutable, así que reprogramar solo puede **diferir** la difusión a V o más tarde
  (nunca antes de V — sería no-final). Borrar funciona igual que hoy.

### Seguridad (invariantes)

1. **Nunca a Sparrow.** Dos capas: (a) el offset solo se sirve con el tick armado (no hay estado
   "siempre activo"); (b) dentro de la ventana, solo a sesiones sin `server.version`.
2. **Nunca difundir no-final.** El scheduler ya comprueba `altura_real >= nLockTime` antes de
   difundir (`is_locktime_satisfied`, `src/pool/manager.rs:558`). Una tx no-final no sale antes
   de tiempo ni por error. Esta es la red de seguridad que hace nuestro enfoque más seguro que el
   `header_faker` de semilla (que crea tx no-finales sin este freno).
3. **No envenenar la caché de tip.** `cached_chain_tip` alimenta el `headers.subscribe`
   instantáneo que ve Sparrow. La altura falsa se calcula **por sesión de Liana** y **jamás** se
   escribe en esa caché compartida.

## Componentes a tocar

### 1. Config y estado armado (`src/config.rs`)

Nuevo bloque de config (persistido, sobrevive a reinicio del contenedor dentro de la ventana):

```
liana_virtual_block:
  enabled: bool          # tick armado
  target_height: u64     # V configurada (absoluta) — próxima a servir
  armed_at_height: u64   # H0, altura real al armar
```

Notas:
- `target_height` avanza +2 tras cada captura de tx de Liana (persistir el nuevo valor).
- Autodesactivación: cuando `altura_real >= armed_at_height + 10`, poner `enabled = false`. La
  comprobación vive donde ya tenemos altura fresca (el health poller / scheduler tick), no en un
  timer aparte.

### 2. Fabricación de cabeceras sintéticas (módulo nuevo, p. ej. `src/electrum_server/virtual_headers.rs`)

Para las alturas `real_tip+1 .. V` no existe cabecera. Hay que fabricarlas:
- `version`: copiar el de la última cabecera real (o un valor plausible).
- `prev_hash`: hash de la cabecera anterior de la cadena sintética (la primera encadena con el
  hash real de `real_tip`).
- `merkle_root`: ceros.
- `time`: `time(real_tip) + 600 * (h - real_tip)`.
- `bits`: copiar el de `real_tip`.
- `nonce`: 0.

Liana valida **continuidad de la cadena** (prev_hash encadenado) y **hash de génesis**, pero
**no** PoW. Este es el punto con más riesgo; se prueba contra la Liana real del nodo.

Funciones esperadas:
- `fabricate_chain(real_tip_height, real_tip_header_hex, up_to_height) -> Vec<(u64, String)>`
  (cabeceras hex de `real_tip+1..=up_to`, encadenadas).
- `tip_response(real_tip, up_to) -> {height, hex}` para `headers.subscribe`.
- Helpers para `blockchain.block.header` (una altura) y `blockchain.block.headers` (un rango).

### 3. Enrutado por sesión (`src/electrum_server/mod.rs`)

- Pasar la **sesión** (o un `is_liana: bool` derivado) hasta `handle_headers_subscribe` (hoy no
  llega, `:958`) y hasta el manejo de `blockchain.block.header` / `block.headers`.
- Cuando `is_liana && config.liana_virtual_block.enabled`:
  - `headers.subscribe` → devolver `{height: V_servida, hex: cabecera_fabricada(V_servida)}`.
  - `block.header` / `block.headers` para alturas `> real_tip` → responder con cabeceras
    fabricadas en vez de reenviar a electrs.
  - Alturas `<= real_tip` → passthrough normal a electrs.
- Detección de Liana: reutilizar `SessionState::effective_source` / `saw_server_version`
  (`:411`). El punto es que aquí "no vi `server.version`" ⇒ Liana.

### 4. Ingesta (`resolve_ingest_plan`, `src/electrum_server/mod.rs:2153`)

- Si `source == "liana"`, `nlocktime` es por altura (`0 < n <= 500_000_000`) y el tick está
  armado ⇒ `(BroadcastMode::ByBlock, None)` con objetivo = `nlocktime`.
- Tras persistir la tx, avanzar `target_height += 2` en la config y persistir.
- Si el tick **no** está armado ⇒ comportamiento actual (manual).

### 5. API y dashboard

- `/api/config` (GET/POST): exponer y guardar `liana_virtual_block` (`enabled`, `target_height`).
  Validaciones: `target_height > altura_real` al armar; entero positivo.
- `/api/status`: incluir estado del tick (armado, `target_height`, `armed_at_height`, bloques
  restantes hasta autodesactivar) para reflejarlo en el dashboard.
- Dashboard (`src/api/dashboard.html`): casilla bajo "Programada", campo de altura, fecha
  aproximada, red flag, e i18n ES/EN en los dos bloques de traducción. Indicador de estado
  "armado — se apaga en el bloque H0+10".

## Datos / flujo

```
Usuario marca tick + V ─▶ config.liana_virtual_block {enabled, target_height=V, armed_at=H0}
                                        │
Liana headers.subscribe ────────────────┤ (sesión sin server.version)
                                        ▼
                        real_tip ← caché/indexer/core (SIN offset)
                        fabricate_chain(real_tip .. V) ─▶ {height:V, hex}
                                        ▼
Liana firma nLockTime=V, "difunde" ─▶ resolve_ingest_plan ─▶ ByBlock(target=V)
                                        │  target_height += 2 (persist)
                                        ▼
scheduler tick: is_locktime_satisfied(V)? (altura_real>=V)
                                        ▼ sí
                        broadcast_transaction ─▶ indexer/Core
```

## Manejo de errores / bordes

- **V <= altura real al armar:** rechazar en `/api/config` con mensaje claro (la altura virtual
  debe ser futura).
- **La cadena pasa de V antes de que Liana firme:** al servir, si `V <= real_tip`, servir
  `real_tip` (no tiene sentido una altura virtual ya alcanzada) — o avisar. Decidir en el plan;
  preferible servir `real_tip+1` mínimo para que la cabecera fabricada sea coherente.
- **Reinicio del contenedor con tick armado:** el estado vive en config → sobrevive. Al arrancar,
  si `altura_real >= armed_at + 10`, desarmar.
- **Fallback a Core activo (indexador caído):** `real_tip` se toma de `chain_health` como el resto
  del proyecto; la fabricación funciona igual. La difusión final usa el mismo `broadcast_transaction`.

## Pruebas

- **Unitarias** (fabricación): continuidad de `prev_hash`, `time` +600/bloque, `merkle_root` ceros,
  longitud 80 bytes/cabecera, encadenado desde un hash real conocido.
- **Unitarias** (estado): armado guarda H0; `target_height += 2` tras captura; autodesactivación al
  llegar a H0+10; V<=real rechazada.
- **Unitarias** (ingesta): con tick armado, Liana + nLockTime por altura ⇒ ByBlock(V); sin armar ⇒
  Manual; Sparrow nunca recibe offset.
- **Integración / manual en el nodo (testnet4):** probar contra la **Liana real** — que acepta la
  altura virtual, firma con nLockTime=V, la tx se retiene y se difunde en V. Este es el
  verificador que importa: la fabricación de cabeceras solo se valida de verdad contra Liana.

## Fuera de alcance

- Sparrow (valida cabeceras; reventaría — excluido por diseño).
- nLockTime por timestamp para este flujo (Liana usa altura).
- Vault NIP-44, i18n del resto de la app, informe de diagnóstico (otros puntos de `mejoras.md`).
