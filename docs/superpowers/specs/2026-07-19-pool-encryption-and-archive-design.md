# Diseño: cifrado del pool + archivo histórico cifrado

Fecha: 2026-07-19
Rama: `feature/pool-encryption` (basada en `fix/pool-value-sats`)
Base conceptual: [[broadcast-pool-security-plan]]

## Contexto y línea base

Hoy broadcast-pool no cifra nada: la DB es un SQLite en claro
(`data_dir/broadcast-pool-<red>.db`), no hay autenticación propia (el control de acceso lo
pone solo el proxy de Umbrel/Start9), el `config.toml` guarda la password RPC de Bitcoin Core
en claro, y **no existe ninguna retención/limpieza** (`expiry_days` está en config pero no se
consume; `TxStatus::Expired` existe pero nada lo asigna). Cualquiera con acceso al filesystem
del nodo lee y puede manipular la DB.

Dos mejoras, independientes pero con una capa cripto compartida:

- **A** — Cifrar en reposo los datos sensibles del **pool activo** y detectar manipulación de
  los campos de programación. Modo desatendido (clave en keyfile).
- **B** — Tras un mes, mover las txs terminadas a un **archivo cifrado con password propio**,
  borrarlas del pool activo y poder consultarlas desde la UI solo con ese password.

## Modelo de amenaza y alcance (explícito)

- **Adversario cubierto:** quien obtiene acceso de lectura/escritura al filesystem del nodo
  sin ser el admin — otra app comprometida, un backup filtrado, robo del disco, un SSH
  puntual. Objetivo: que ese acceso no revele destinos/importes ni permita alterar la
  programación sin ser detectado; y que el histórico solo lo lea quien tiene el password.
- **Fuera de alcance (imposible en modo desatendido):** un host **totalmente comprometido**
  mientras el servicio corre. El proceso debe poder descifrar el pool activo solo para difundir
  a su hora, así que la clave del pool activo vive en el disco (keyfile) y en memoria del
  proceso; quien controle el host en ejecución puede leerla. Esto se documenta, no se oculta.

## Decisiones (acordadas con el usuario 2026-07-19)

- **Cripto:** RustCrypto puro, sin dependencias C: `chacha20poly1305` (AEAD = confidencialidad
  + detección de manipulación), `argon2` (Argon2id: password→clave), `rand` (nonces/salt).
  **No** SQLCipher (build/imagen más pesados).
- **Pool activo:** cifrado por campo con clave en **keyfile** (`600`), desatendido.
- **Archivo:** **tabla cifrada en la misma DB**, con **password propio** de broadcast-pool.
- **Clave del archivo:** derivada del password y cacheada **en memoria tras desbloquear**; la
  retención corre solo mientras está desbloqueado. Tras un reinicio, la limpieza se pausa hasta
  el siguiente desbloqueo.

## Capa cripto compartida (`src/crypto/mod.rs`, nuevo)

Primitivas reutilizadas por A y B:

- `fn generate_key() -> [u8;32]` (aleatoria, `OsRng`).
- `fn seal(key: &[u8;32], plaintext: &[u8], aad: &[u8]) -> Vec<u8>` → devuelve
  `nonce(12) ‖ ciphertext‖tag`. XChaCha20 no; usamos ChaCha20-Poly1305 con nonce aleatorio de
  96 bits (riesgo de colisión despreciable a este volumen) — o XChaCha20-Poly1305 (nonce 192b)
  si se prefiere margen; decisión menor, por defecto ChaCha20-Poly1305.
- `fn open(key: &[u8;32], blob: &[u8], aad: &[u8]) -> Result<Vec<u8>>` (falla si hay
  manipulación).
- `fn derive_key(password: &str, salt: &[u8]) -> [u8;32]` (Argon2id, parámetros medios:
  m=19456 KiB, t=2, p=1 — perfil "medio", ajustable).
- `fn mac(key: &[u8;32], data: &[u8]) -> [u8;32]` para el `row_mac`: **HMAC-SHA256** usando
  `bitcoin_hashes` (ya es dependencia vía `bitcoin`), clave = keyfile.

El `aad` (additional authenticated data) liga cada ciphertext a su fila (p. ej. el `id` de la
tx) para que un blob no pueda moverse de una fila a otra sin fallar la autenticación.

## Feature A — cifrado del pool activo (keyfile)

### Ciclo de vida de la clave

- Al arrancar, si no existe `data_dir/pool.key`, se genera (32 bytes aleatorios) y se escribe
  con permisos `0o600`. Si existe, se carga. La clave se mantiene en memoria del proceso.
- El keyfile vive junto a la DB por defecto. (Nota operativa documentada: para protección real
  frente a robo de disco conviene tenerlo en otro volumen; no lo forzamos.)

### Campos cifrados

Se cifran (blob `nonce‖ct`, `aad = id`): `tx_hex`, `destination_address`, `source_label`.
El resto de columnas quedan en claro porque el scheduler las filtra/ordena: `status`,
`network`, `scheduled_time`, `broadcast_mode`, `target_price`, `nlocktime`, timestamps.

Almacenamiento: se reutilizan las columnas `TEXT` existentes guardando el blob en base64 con un
prefijo de versión, p. ej. `enc:v1:<base64>`. Así una fila sin cifrar (legado) se distingue de
una cifrada por el prefijo, y el lector decide. Esto evita cambios de esquema para el cifrado de
campos (solo se añade `row_mac`).

### Integridad anti-manipulación (`row_mac`)

- Nueva columna `row_mac TEXT` (Migración 008).
- En cada `INSERT`/`UPDATE` de una fila del pool se calcula
  `row_mac = mac(keyfile_key, canonical(id, status, broadcast_mode, scheduled_time, nlocktime,
  target_price, price_condition, schedule_trigger, tx_hex_cipher, destination_cipher))`.
- Al leer una fila para **actuar** (scheduler: decidir difusión) se verifica el `row_mac`. Si no
  cuadra → la fila se marca en memoria como `tampered`, **no se difunde**, se registra
  `tracing::error!` y se expone un flag en la API/UI ("registro manipulado, revisar"). No se
  borra el dato.
- Filas legado sin `row_mac`: se les calcula al vuelo en la migración de arranque (ver abajo);
  a partir de ahí quedan protegidas.

### Password RPC en `config.toml` (A3)

- El campo `bitcoin_rpc.password` se persiste cifrado con la keyfile como `enc:v1:<base64>`.
- Al cargar config, si el valor tiene ese prefijo se descifra en memoria; si está en claro
  (legado) se usa y se reescribe cifrado en el siguiente guardado. El pin por entorno
  (`BROADCAST_POOL_RPC_PASS`) sigue teniendo prioridad y no se persiste.

### Migración de datos (arranque, idempotente)

Migración en Rust (patrón del backfill de valor ya implementado): recorre `broadcast_pool`;
para cada fila cuyos campos sensibles no tengan el prefijo `enc:`, los cifra y calcula su
`row_mac`; `UPDATE`. Idempotente (si ya tiene prefijo, se salta). No fatal.

## Feature B — archivo histórico cifrado con password propio

### Esquema (Migración 009)

```sql
CREATE TABLE IF NOT EXISTS archive_pool (
    id           TEXT PRIMARY KEY,   -- id opaco (uuid nuevo, no revela el original)
    network      TEXT NOT NULL,      -- en claro, para filtrar por red
    archived_at  TEXT NOT NULL,      -- en claro, para listar/paginar/ordenar
    blob         BLOB NOT NULL,      -- seal(archive_key, JSON(registro completo), aad=id)
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_archive_net_time ON archive_pool(network, archived_at);
```

El `blob` contiene el registro completo (todos los campos, ya descifrados desde el pool activo y
re-serializados a JSON) cifrado con la `archive_key`. En claro solo `network` y `archived_at`.

### Password del archivo (setup y verificación)

- El admin define el password desde la UI. Se guarda en `config_store`:
  - `archive_salt` (aleatorio, 16 bytes, base64) — para Argon2id.
  - `archive_verifier` = Argon2id-PHC del password (solo para validar el password al
    desbloquear; **no** permite descifrar).
- La `archive_key = derive_key(password, archive_salt)` **nunca se persiste**.

### Desbloqueo y clave en memoria

- Endpoint `POST /api/archive/unlock {password}`: valida contra `archive_verifier`; si OK,
  deriva `archive_key` y la guarda en un estado en memoria (`Arc<Mutex<Option<UnlockState>>>`)
  con `expires_at` (p. ej. 15 min de inactividad, renovable). Respuesta: `{unlocked:true}`.
- `POST /api/archive/lock`: borra la clave de memoria.
- La clave se mantiene solo en RAM; un reinicio la pierde (hay que volver a desbloquear).

### Job de retención

- Config: usar el campo existente `[pool] expiry_days` (hoy inerte), subiendo su default a 30.
  No se renombra (evita romper `config.toml` existentes).
- Bucle diario (junto a los scheduler loops existentes): si el archivo está **desbloqueado**
  (hay `archive_key` en memoria), selecciona txs en estado terminal (`confirmed`, `failed`,
  `broadcast`) con `updated_at` anterior a `now - archive_after_days`; por cada una: construir
  el JSON del registro (descifrando los campos del pool activo con la keyfile), cifrar con
  `archive_key`, `INSERT` en `archive_pool`, y `DELETE` de `broadcast_pool`. Transaccional por
  fila (o por lote) para no perder datos si falla a medias.
- Si está **bloqueado**, el job no hace nada y registra un `debug` ("archivo bloqueado, retención
  en pausa"). No borra nada sin haber cifrado.

### Consulta desde la UI

- `GET /api/archive?limit&offset` (requiere desbloqueo): lista `{id, network, archived_at}` en
  claro (paginado). Sin desbloqueo → `401` con `{locked:true}`.
- `GET /api/archive/{id}` (requiere desbloqueo): descifra el `blob` y devuelve el registro
  completo. Sin clave → `401`.
- La UI añade una pestaña "Archivo" con un candado: si está bloqueado, pide el password
  (`unlock`); si está desbloqueado, lista y permite ver el detalle de cada tx archivada.

## Cambios de API / UI (resumen)

- Nuevas rutas: `POST /api/archive/unlock`, `POST /api/archive/lock`,
  `GET /api/archive`, `GET /api/archive/{id}`, y `POST /api/archive/set-password` (setup /
  cambio de password del archivo).
- `dashboard.html`: nueva pestaña "Archivo" con estados bloqueado/desbloqueado, formulario de
  password, listado paginado y modal de detalle; i18n en/es. Indicador de "registro manipulado"
  en la tabla del pool cuando `row_mac` falla.
- `/api/status`: exponer `archive_locked: bool` y `archive_password_set: bool` para que la UI
  sepa qué mostrar.

## No incluido (YAGNI)

- No cifrado de DB completa (SQLCipher) ni de columnas no sensibles del pool.
- No multi-usuario ni roles; "autorizado" = quien tiene el password del archivo (y/o pasa el
  proxy del nodo).
- No rotación de claves por UI (se puede añadir después; la keyfile y el password del archivo se
  pueden rotar manualmente con un procedimiento documentado más adelante).
- No recuperación del password del archivo: si se pierde, el histórico cifrado no se puede leer
  (se documenta claramente en la UI al fijarlo).
- No protección frente a host en ejecución totalmente comprometido (ver alcance).

## Riesgos

- **Pérdida del keyfile** → el pool activo cifrado se vuelve ilegible y el servicio no puede
  difundir. Mitigación: la keyfile se genera y persiste en el data-dir (mismo ciclo de vida que
  la DB); documentar que un backup de la DB debe incluir la keyfile.
- **Pérdida del password del archivo** → histórico irrecuperable (por diseño). Aviso explícito
  en la UI.
- **Retención pausada** si nadie desbloquea tras un reinicio → el pool activo retiene txs viejas
  más tiempo. Aceptado por diseño (se prioriza "solo el admin").
- **Migración de cifrado a medias** (corte de energía) → la migración es idempotente y por fila;
  al rearrancar continúa. Riesgo bajo.
- **Nonce de 96 bits** con clave fija del pool: a volúmenes reales (miles de filas) la colisión
  es despreciable; si preocupa, usar XChaCha20-Poly1305 (nonce 192b). Decisión menor.

## Estrategia de test (TDD)

- `crypto`: round-trip `seal/open`; `open` falla con blob manipulado; `open` falla con `aad`
  distinto; `derive_key` determinista con misma salt; `verifier` valida/rechaza.
- **A**: insertar con campos sensibles → en DB quedan con prefijo `enc:` (no en claro);
  lectura devuelve el plaintext original; `row_mac` presente; manipular una columna de
  programación en crudo → la verificación falla y la fila se marca `tampered` y no se difunde;
  config RPC password se persiste cifrado y se descifra al cargar; migración idempotente cifra
  filas legado.
- **B**: `set-password` guarda salt+verifier y no la clave; `unlock` con password correcto/incorrecto;
  retención mueve solo terminales > N días y borra del pool; con archivo bloqueado la retención
  no toca nada; `GET /api/archive*` exige desbloqueo; el `blob` no contiene texto en claro
  (comprobar que no aparece la dirección/hex en el BLOB); expiración de la clave en memoria.

## Fases para el plan (implementación incremental)

1. **Módulo cripto** (`src/crypto`) + tests. Base de todo.
2. **Feature A**: keyfile, cifrado de campos en insert/read, `row_mac` (Migración 008),
   cifrado del RPC password, migración de arranque. Entregable testable e independiente.
3. **Feature B**: esquema `archive_pool`, password (salt/verifier), unlock/lock en memoria,
   job de retención, endpoints y pestaña UI. Depende de la keyfile de A para descifrar al
   archivar.

Cada fase deja software funcionando y probado. En el paso de "writing-plans" se decidirá si van
en un único plan de 3 fases o en dos planes (A, luego B).
