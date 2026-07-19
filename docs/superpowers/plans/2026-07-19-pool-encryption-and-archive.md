# Cifrado del pool + archivo histórico cifrado — Plan de implementación

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cifrar en reposo los datos sensibles del pool activo (keyfile + integridad por fila con modo seguro) y añadir un archivo histórico cifrado con password propio, con retención a 30 días y visor en la UI.

**Architecture:** Un módulo cripto compartido (`src/crypto`) con AEAD (ChaCha20-Poly1305), Argon2id y HMAC-SHA256. La `Database` sostiene la clave-keyfile y cifra/descifra campos sensibles de forma transparente en insert/read; una columna `row_mac` da integridad y dispara un modo seguro global ante manipulación. El archivo vive en una tabla `archive_pool` cifrada con una clave derivada de un password propio, cacheada en memoria tras desbloquear; un loop de retención mueve las txs terminadas.

**Tech Stack:** Rust, rusqlite (SQLite), axum, RustCrypto (`chacha20poly1305`, `argon2`), `bitcoin::hashes` (HMAC), `hex`, `rand`.

Spec: `docs/superpowers/specs/2026-07-19-pool-encryption-and-archive-design.md`.

## Global Constraints

- **Cripto pura Rust, sin dependencias C.** Solo `chacha20poly1305`, `argon2`, y `bitcoin::hashes` (ya presente vía `bitcoin`). NO SQLCipher.
- **AEAD:** ChaCha20-Poly1305, nonce aleatorio de 96 bits, `aad = id` de la fila. Blob en disco = `nonce(12) ‖ ciphertext‖tag`, codificado en **hex** con prefijo de versión: `enc:v1:<hex>` (no se añade dependencia base64; `hex` ya está).
- **KDF:** Argon2id, perfil medio (m=19456 KiB, t=2, p=1, salida 32 bytes).
- **Compatibilidad hacia atrás:** filas/valores en claro (legado, sin prefijo `enc:`) deben seguir leyéndose y migrarse de forma idempotente. No romper `config.toml` existentes.
- **Modo desatendido:** el pool activo se cifra/descifra con la keyfile (sin intervención). La clave del archivo NO se persiste; vive en memoria solo tras desbloqueo.
- **Retención:** usa el campo existente `[pool] expiry_days` (default nuevo 30). Solo actúa si el archivo está desbloqueado.
- TDD estricto: test que falla → implementación mínima → test verde → commit. Cada tarea termina con build + tests en verde.
- `#[cfg(test)]` para helpers de test; no filtrar secretos en logs (nunca loggear claves, passwords ni plaintext de tx).

---

## File Structure

- `src/crypto/mod.rs` (nuevo) — primitivas: `seal`, `open`, `derive_key`, `mac`, `generate_key`, `encode_blob`/`decode_blob` (prefijo `enc:v1:`).
- `src/db/mod.rs` — la `Database` sostiene `key: [u8;32]` (keyfile); cifra en `insert_broadcast_tx`, descifra en `map_broadcast_row`; `row_mac` (Migración 008); métodos `get_config_value`/`set_config_value`; métodos de archivo (Migración 009): `insert_archive`, `list_archive`, `get_archive`, `select_terminal_older_than`, `delete_broadcast_tx`.
- `src/db/schema.rs` — `MIGRATION_008` (row_mac), `MIGRATION_009` (archive_pool).
- `src/db/keyfile.rs` (nuevo) — `load_or_create_keyfile(path) -> [u8;32]`.
- `src/config.rs` / `src/discovery.rs` — cifrado/descifrado del `bitcoin_rpc.password`.
- `src/pool/manager.rs` — estado de modo seguro (`safe_mode: Arc<AtomicBool>`, `tampered_ids`), verificación de `row_mac` al actuar; gate en la difusión.
- `src/pool/scheduler.rs` — gate de modo seguro en `run_broadcast_loop`; nuevo `run_retention_loop`.
- `src/api/mod.rs` — `AppState` gana un `ArchiveKeyStore` (clave en memoria + expiración); rutas `/api/archive/*`, `/api/security/acknowledge`; campos nuevos en `/api/status`.
- `src/api/dashboard.html` — pestaña "Archivo", alerta de modo seguro, i18n.

---

## FASE 1 — Módulo cripto

### Task 1: Módulo `src/crypto` (AEAD + KDF + HMAC)

**Files:**
- Modify: `Cargo.toml` (deps)
- Create: `src/crypto/mod.rs`
- Modify: `src/main.rs` (declarar `mod crypto;`)

**Interfaces:**
- Produces:
  - `crypto::generate_key() -> [u8; 32]`
  - `crypto::seal(key: &[u8;32], plaintext: &[u8], aad: &[u8]) -> Vec<u8>` (= `nonce‖ct`)
  - `crypto::open(key: &[u8;32], blob: &[u8], aad: &[u8]) -> anyhow::Result<Vec<u8>>`
  - `crypto::derive_key(password: &str, salt: &[u8]) -> [u8;32]`
  - `crypto::mac(key: &[u8;32], data: &[u8]) -> [u8;32]`
  - `crypto::encode_field(key, plaintext, aad) -> String` (→ `"enc:v1:<hex>"`)
  - `crypto::decode_field(key, s: &str, aad) -> anyhow::Result<String>` (si no tiene prefijo `enc:`, devuelve `s` tal cual — legado)
  - `crypto::is_encoded(s: &str) -> bool`

- [ ] **Step 1: Añadir dependencias**

En `Cargo.toml`, bajo `[dependencies]` (tras `hex = "0.4"`):

```toml
# Encryption at rest
chacha20poly1305 = "0.10"
argon2 = "0.5"
```

- [ ] **Step 2: Declarar el módulo**

En `src/main.rs`, junto a los otros `mod` (p. ej. tras `mod config;`):

```rust
mod crypto;
```

- [ ] **Step 3: Escribir los tests que fallan** (`src/crypto/mod.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip() {
        let key = generate_key();
        let blob = seal(&key, b"hello world", b"aad-1");
        assert_ne!(&blob[..], b"hello world");
        assert_eq!(open(&key, &blob, b"aad-1").unwrap(), b"hello world");
    }

    #[test]
    fn open_fails_on_tamper() {
        let key = generate_key();
        let mut blob = seal(&key, b"secret", b"id");
        let last = blob.len() - 1;
        blob[last] ^= 0x01;
        assert!(open(&key, &blob, b"id").is_err());
    }

    #[test]
    fn open_fails_on_wrong_aad() {
        let key = generate_key();
        let blob = seal(&key, b"secret", b"id-A");
        assert!(open(&key, &blob, b"id-B").is_err());
    }

    #[test]
    fn derive_key_is_deterministic() {
        let salt = [7u8; 16];
        assert_eq!(derive_key("pw", &salt), derive_key("pw", &salt));
        assert_ne!(derive_key("pw", &salt), derive_key("other", &salt));
    }

    #[test]
    fn mac_detects_change() {
        let key = generate_key();
        assert_eq!(mac(&key, b"abc"), mac(&key, b"abc"));
        assert_ne!(mac(&key, b"abc"), mac(&key, b"abd"));
    }

    #[test]
    fn encode_decode_field_roundtrip_and_legacy() {
        let key = generate_key();
        let enc = encode_field(&key, "1600 Pennsylvania Ave", b"row-1");
        assert!(enc.starts_with("enc:v1:"));
        assert_eq!(decode_field(&key, &enc, b"row-1").unwrap(), "1600 Pennsylvania Ave");
        // Legacy plaintext (no prefix) passes through unchanged.
        assert_eq!(decode_field(&key, "plain-value", b"row-1").unwrap(), "plain-value");
    }
}
```

- [ ] **Step 4: Ejecutar los tests y verificar que fallan**

Run: `cargo test crypto::`
Expected: fallo de compilación (funciones no definidas).

- [ ] **Step 5: Implementar el módulo** (`src/crypto/mod.rs`, arriba del `mod tests`)

```rust
//! At-rest encryption primitives (pure Rust): ChaCha20-Poly1305 AEAD, Argon2id KDF,
//! HMAC-SHA256. Blobs are `nonce(12) ‖ ciphertext‖tag`; encoded fields are `enc:v1:<hex>`.

use anyhow::{anyhow, Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

const FIELD_PREFIX: &str = "enc:v1:";

pub fn generate_key() -> [u8; 32] {
    rand::random()
}

pub fn seal(key: &[u8; 32], plaintext: &[u8], aad: &[u8]) -> Vec<u8> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let nonce_bytes: [u8; 12] = rand::random();
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), Payload { msg: plaintext, aad })
        .expect("chacha20poly1305 encryption cannot fail with a valid key/nonce");
    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    out
}

pub fn open(key: &[u8; 32], blob: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < 12 {
        return Err(anyhow!("ciphertext blob too short"));
    }
    let (nonce_bytes, ct) = blob.split_at(12);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(Nonce::from_slice(nonce_bytes), Payload { msg: ct, aad })
        .map_err(|_| anyhow!("AEAD decryption/authentication failed"))
}

pub fn derive_key(password: &str, salt: &[u8]) -> [u8; 32] {
    use argon2::{Algorithm, Argon2, Params, Version};
    let params = Params::new(19456, 2, 1, Some(32)).expect("valid argon2 params");
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .expect("argon2 derivation");
    key
}

pub fn mac(key: &[u8; 32], data: &[u8]) -> [u8; 32] {
    use bitcoin::hashes::{hmac::Hmac, hmac::HmacEngine, sha256, Hash, HashEngine};
    let mut engine = HmacEngine::<sha256::Hash>::new(key);
    engine.input(data);
    Hmac::<sha256::Hash>::from_engine(engine).to_byte_array()
}

pub fn is_encoded(s: &str) -> bool {
    s.starts_with(FIELD_PREFIX)
}

pub fn encode_field(key: &[u8; 32], plaintext: &str, aad: &[u8]) -> String {
    let blob = seal(key, plaintext.as_bytes(), aad);
    format!("{}{}", FIELD_PREFIX, hex::encode(blob))
}

pub fn decode_field(key: &[u8; 32], s: &str, aad: &[u8]) -> Result<String> {
    match s.strip_prefix(FIELD_PREFIX) {
        None => Ok(s.to_string()), // legacy plaintext
        Some(h) => {
            let blob = hex::decode(h).context("invalid hex in encoded field")?;
            let pt = open(key, &blob, aad)?;
            String::from_utf8(pt).context("decrypted field is not valid UTF-8")
        }
    }
}
```

- [ ] **Step 6: Ejecutar tests y verificar que pasan**

Run: `cargo test crypto::`
Expected: PASS (6 tests).

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock src/crypto/mod.rs src/main.rs
git commit -m "feat(crypto): AEAD + Argon2id + HMAC at-rest primitives"
```

---

## FASE 2 — Feature A: cifrado del pool activo

### Task 2: Keyfile y clave en la `Database`

**Files:**
- Create: `src/db/keyfile.rs`
- Modify: `src/db/mod.rs` (campo `key`, carga en `open`, declarar `mod keyfile;`)

**Interfaces:**
- Consumes: `crypto::generate_key`
- Produces:
  - `db::keyfile::load_or_create(path: &Path) -> anyhow::Result<[u8;32]>`
  - `Database.key: [u8; 32]` (privado) + `Database::key(&self) -> &[u8;32]` (pub(crate) para tests/uso interno)

- [ ] **Step 1: Test que falla** (`src/db/keyfile.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_then_reuses_keyfile_with_600_perms() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.key");
        let k1 = load_or_create(&path).unwrap();
        assert!(path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let k2 = load_or_create(&path).unwrap();
        assert_eq!(k1, k2, "second call must reuse the same key");
    }
}
```

- [ ] **Step 2: Ejecutar y ver fallo**

Run: `cargo test db::keyfile::` → FAIL (no definido).

- [ ] **Step 3: Implementar** (`src/db/keyfile.rs`)

```rust
//! Loads (or creates on first run) the 32-byte keyfile used to encrypt sensitive
//! active-pool fields. Stored with 0600 perms next to the database.

use anyhow::{Context, Result};
use std::path::Path;

pub fn load_or_create(path: &Path) -> Result<[u8; 32]> {
    if path.exists() {
        let bytes = std::fs::read(path).with_context(|| format!("read keyfile {}", path.display()))?;
        let arr: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("keyfile {} is not 32 bytes", path.display()))?;
        return Ok(arr);
    }
    let key = crate::crypto::generate_key();
    std::fs::write(path, key).with_context(|| format!("write keyfile {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .context("chmod 600 keyfile")?;
    }
    Ok(key)
}
```

- [ ] **Step 4: Declarar el módulo y cargar la clave en `Database::open`** (`src/db/mod.rs`)

Añadir `pub mod keyfile;` junto a `pub mod models;`. Añadir campo a la struct:

```rust
pub struct Database {
    conn: Mutex<Connection>,
    key: [u8; 32],
}
```

En `open`, tras crear la conexión y antes de `run_migrations`, cargar el keyfile adyacente:

```rust
let key_path = db_path
    .parent()
    .unwrap_or_else(|| std::path::Path::new("."))
    .join("pool.key");
let key = keyfile::load_or_create(&key_path)?;

let db = Self { conn: Mutex::new(conn), key };
```

Añadir accessor:

```rust
pub(crate) fn key(&self) -> &[u8; 32] {
    &self.key
}
```

Corregir cualquier otro sitio que construya `Self { conn: ... }` (buscar `Self {` en el fichero) para incluir `key`. En tests que abren `Database::open` sobre un tempdir, el keyfile se crea solo (sin cambios en esos tests).

- [ ] **Step 5: Ejecutar tests y build**

Run: `cargo test db::` y `cargo build`
Expected: PASS; compila.

- [ ] **Step 6: Commit**

```bash
git add src/db/keyfile.rs src/db/mod.rs
git commit -m "feat(db): load/create 0600 keyfile and hold key in Database"
```

### Task 3: Cifrar campos sensibles en insert y descifrar en read

**Files:**
- Modify: `src/db/mod.rs` (`insert_broadcast_tx`, `map_broadcast_row`)

**Interfaces:**
- Consumes: `Database.key`, `crypto::encode_field`, `crypto::decode_field`
- Los campos `tx_hex`, `destination_address`, `source_label` se guardan cifrados (`aad = id`) y se devuelven en claro al leer.

**Nota sobre `map_broadcast_row`:** hoy es una función libre `fn map_broadcast_row(row) -> rusqlite::Result<BroadcastTx>`. Necesita la clave para descifrar. Cambiaremos su firma a `fn map_broadcast_row(row, key: &[u8;32])` y actualizaremos todas las llamadas (hay varias: `get_broadcast_tx_by_id`, `list_broadcast_txs`, y las consultas de "due"/scheduled). Buscar `map_broadcast_row(` para localizarlas todas.

- [ ] **Step 1: Test que falla** (añadir en `src/db/mod.rs` mod tests)

```rust
#[test]
fn sensitive_fields_are_encrypted_at_rest_and_decrypted_on_read() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("e.db")).unwrap();
    let hex = tx_hex_with_output_sats(&[10_000]);
    let new_tx = crate::db::models::NewBroadcastTx {
        tx_hex: hex.clone(), network: "testnet4".into(), nlocktime: None,
        broadcast_mode: Some("imported".into()), scheduled_time: None,
        target_fee_rate: None, source_label: Some("wallet-A".into()),
        destination_address: Some("tb1qexampleaddr".into()),
        utxo_count: Some(1), total_value_btc: None, replacement_of: None,
    };
    let stored = db.insert_broadcast_tx(&new_tx).unwrap();

    // Read-back returns plaintext.
    let got = db.get_broadcast_tx_by_id(&stored.id).unwrap();
    assert_eq!(got.tx_hex, hex);
    assert_eq!(got.destination_address.as_deref(), Some("tb1qexampleaddr"));
    assert_eq!(got.source_label.as_deref(), Some("wallet-A"));

    // Raw column is NOT plaintext.
    let raw: String = db.lock_conn().unwrap()
        .query_row("SELECT destination_address FROM broadcast_pool WHERE id=?1",
            params![stored.id], |r| r.get(0)).unwrap();
    assert!(raw.starts_with("enc:v1:"), "stored value must be encrypted, got {raw}");
    assert!(!raw.contains("tb1qexampleaddr"));
}
```

- [ ] **Step 2: Ejecutar y ver fallo**

Run: `cargo test sensitive_fields_are_encrypted_at_rest` → FAIL (raw no empieza por `enc:`).

- [ ] **Step 3: Cifrar en `insert_broadcast_tx`**

En `insert_broadcast_tx`, tras calcular `id` y antes del `INSERT`, preparar los campos cifrados (usar `id` como `aad`):

```rust
let enc_tx_hex = crate::crypto::encode_field(&self.key, &tx.tx_hex, id.as_bytes());
let enc_dest = tx.destination_address.as_ref()
    .map(|d| crate::crypto::encode_field(&self.key, d, id.as_bytes()));
let enc_source = tx.source_label.as_ref()
    .map(|s| crate::crypto::encode_field(&self.key, s, id.as_bytes()));
```

Sustituir en los `params!` del INSERT: `tx.tx_hex` → `enc_tx_hex`, `tx.source_label` → `enc_source`, `tx.destination_address` → `enc_dest`. (El resto igual, incluida la derivación de `total_value_btc` que ya lee `tx.tx_hex` en claro — se calcula ANTES de cifrar, así que se mantiene usando `tx.tx_hex`.)

- [ ] **Step 4: Descifrar en `map_broadcast_row`**

Cambiar la firma a `fn map_broadcast_row(row: &rusqlite::Row, key: &[u8; 32]) -> rusqlite::Result<BroadcastTx>`. Para `id` (col 0) leerlo primero, luego descifrar los campos usando `id` como aad. Los `decode_field` devuelven `Result<String>`; mapear el error a `rusqlite::Error` (p. ej. `rusqlite::Error::InvalidColumnType`) o, más simple, con un helper que en caso de error registre y devuelva el valor crudo. Implementación recomendada:

```rust
let id: String = row.get(0)?;
let dec = |s: String| crate::crypto::decode_field(key, &s, id.as_bytes())
    .unwrap_or_else(|e| { tracing::error!("field decrypt failed for {}: {}", id, e); s });
// tx_hex (col 1):
tx_hex: dec(row.get::<_, String>(1)?),
// source_label (col 13) y destination_address (col 14): Option<String>
source_label: row.get::<_, Option<String>>(13)?.map(dec),
destination_address: row.get::<_, Option<String>>(14)?.map(dec),
```

Actualizar TODAS las llamadas a `map_broadcast_row(row)` → `map_broadcast_row(row, &self.key)` (o `self.key()` según contexto). Buscar con `grep -n "map_broadcast_row(" src/db/mod.rs`.

- [ ] **Step 5: Ejecutar tests**

Run: `cargo test db::` → PASS (incluye el nuevo). Verificar que los tests previos (valor, imported, migración) siguen verdes.

- [ ] **Step 6: Commit**

```bash
git add src/db/mod.rs
git commit -m "feat(db): encrypt tx_hex/destination/source at rest, decrypt on read"
```

### Task 4: Integridad por fila (`row_mac`, Migración 008)

**Files:**
- Modify: `src/db/schema.rs` (MIGRATION_008), `src/db/mod.rs` (migración, cálculo y verificación)

**Interfaces:**
- Produces:
  - `Database::compute_row_mac(id, status, broadcast_mode, scheduled_time, nlocktime, target_price, schedule_trigger, price_condition, enc_tx_hex, enc_dest) -> String` (hex del HMAC)
  - `Database::verify_row_mac(&BroadcastTx, raw_row_fields) -> bool` — o más práctico: `BroadcastTx` gana un campo runtime `tampered: Option<bool>` que `map_broadcast_row` rellena verificando el `row_mac` leído contra el recomputado.

**Decisión de diseño:** verificar el MAC dentro de `map_broadcast_row` es lo más robusto (toda lectura queda verificada). Para ello el MAC debe calcularse sobre los valores **tal cual están en las columnas** (los cifrados), de modo que `map_broadcast_row` pueda recomputarlo con lo que lee sin re-cifrar. El canonical string se construye concatenando los campos con un separador que no aparezca en ellos (p. ej. `\x1f`).

- [ ] **Step 1: Test que falla**

```rust
#[test]
fn tampering_schedule_is_detected_by_row_mac() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("t.db")).unwrap();
    let new_tx = crate::db::models::NewBroadcastTx {
        tx_hex: tx_hex_with_output_sats(&[10_000]), network: "testnet4".into(),
        nlocktime: None, broadcast_mode: Some("scheduled".into()),
        scheduled_time: Some(chrono::Utc::now()), target_fee_rate: None,
        source_label: None, destination_address: None, utxo_count: Some(1),
        total_value_btc: None, replacement_of: None,
    };
    let stored = db.insert_broadcast_tx(&new_tx).unwrap();
    // Untampered read: not flagged.
    assert_ne!(db.get_broadcast_tx_by_id(&stored.id).unwrap().tampered, Some(true));
    // Tamper the scheduled_time column directly.
    db.lock_conn().unwrap().execute(
        "UPDATE broadcast_pool SET scheduled_time='1999-01-01T00:00:00Z' WHERE id=?1",
        params![stored.id]).unwrap();
    assert_eq!(db.get_broadcast_tx_by_id(&stored.id).unwrap().tampered, Some(true));
}
```

- [ ] **Step 2: Ejecutar y ver fallo** (campo `tampered` no existe / no se verifica).

- [ ] **Step 3: Migración 008** (`src/db/schema.rs`)

```rust
pub const MIGRATION_008: &str = r#"
ALTER TABLE broadcast_pool ADD COLUMN row_mac TEXT;
"#;
```

Registrarla en `run_migrations` con el patrón no-fatal existente:

```rust
if let Err(e) = conn.execute_batch(schema::MIGRATION_008) {
    tracing::warn!("Migration 008 warning (non-fatal): {}", e);
}
```

- [ ] **Step 4: Campo runtime `tampered` en el modelo**

En `src/db/models.rs`, añadir a `BroadcastTx` (junto a los otros campos runtime como `locktime_waiting`): `pub tampered: Option<bool>,` y `#[serde(...)]` acorde. Inicializarlo a `None` en todos los constructores/`map_broadcast_row`.

- [ ] **Step 5: Helper de canonical + MAC** (`src/db/mod.rs`)

```rust
fn row_mac_input(
    id: &str, status: &str, mode: &str, scheduled: &str, nlocktime: i64,
    target_price: &str, schedule_trigger: &str, price_condition: &str,
    enc_tx_hex: &str, enc_dest: &str,
) -> Vec<u8> {
    [id, status, mode, scheduled, &nlocktime.to_string(), target_price,
     schedule_trigger, price_condition, enc_tx_hex, enc_dest].join("\x1f").into_bytes()
}
```

(Para valores `NULL` usar cadena vacía de forma consistente entre cálculo y verificación.)

- [ ] **Step 6: Calcular `row_mac` en insert**

En `insert_broadcast_tx`, tras cifrar los campos, calcular el MAC con los MISMOS valores que van a las columnas (cifrados y normalizados) y añadir `row_mac` a las columnas del INSERT (añadir la columna a la lista y el `?N`). Usar `crate::crypto::mac(&self.key, &row_mac_input(...))` y guardarlo como `hex::encode(...)`.

- [ ] **Step 7: Verificar en `map_broadcast_row`**

Tras construir el `BroadcastTx`, leer la columna `row_mac` (nueva, índice al final del `BROADCAST_SELECT` — añadir `row_mac` a esa constante). Recomputar con los valores crudos leídos de las columnas (¡los cifrados, antes de descifrar!) y comparar. Si difiere o es `NULL` en una fila que debería tenerlo, marcar `tampered = Some(true)`; si coincide, `Some(false)`. Actualizar `BROADCAST_SELECT` para incluir `row_mac` y ajustar índices en `map_broadcast_row`.

- [ ] **Step 8: Ejecutar tests y build** → PASS. Revisar que los tests previos siguen verdes (los índices de columnas cambiaron por añadir `row_mac`).

- [ ] **Step 9: Commit**

```bash
git add src/db/schema.rs src/db/mod.rs src/db/models.rs
git commit -m "feat(db): per-row HMAC integrity (migration 008) with tamper flag"
```

### Task 5: Modo seguro (halt global + acknowledge)

**Files:**
- Modify: `src/pool/manager.rs` (estado + gate), `src/pool/scheduler.rs` (gate en broadcast loop), `src/api/mod.rs` (endpoint + status)

**Interfaces:**
- Produces:
  - `PoolManager.safe_mode: Arc<AtomicBool>` + `PoolManager::is_safe_mode() -> bool`, `PoolManager::enter_safe_mode(id: &str)`, `PoolManager::clear_safe_mode()`, `PoolManager::tampered_ids() -> Vec<String>`.
  - Ruta `POST /api/security/acknowledge`.

- [ ] **Step 1: Test que falla** (`src/pool/manager.rs` tests)

```rust
#[test]
fn safe_mode_toggles_and_reports_ids() {
    let pm = /* construir PoolManager de test como en los tests existentes */;
    assert!(!pm.is_safe_mode());
    pm.enter_safe_mode("abc");
    assert!(pm.is_safe_mode());
    assert_eq!(pm.tampered_ids(), vec!["abc".to_string()]);
    pm.clear_safe_mode();
    assert!(!pm.is_safe_mode());
    assert!(pm.tampered_ids().is_empty());
}
```

(Usar el mismo patrón de construcción de `PoolManager` que los tests ya presentes en `manager.rs`.)

- [ ] **Step 2: Ejecutar y ver fallo.**

- [ ] **Step 3: Implementar el estado en `PoolManager`**

Añadir campos: `safe_mode: Arc<std::sync::atomic::AtomicBool>` y `tampered_ids: Arc<Mutex<Vec<String>>>`, inicializados en `new`. Métodos:

```rust
pub fn is_safe_mode(&self) -> bool {
    self.safe_mode.load(std::sync::atomic::Ordering::Relaxed)
}
pub fn enter_safe_mode(&self, id: &str) {
    self.safe_mode.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut v) = self.tampered_ids.lock() {
        if !v.iter().any(|x| x == id) { v.push(id.to_string()); }
    }
    tracing::error!("SAFE MODE: tampered row {} detected — broadcasting halted", id);
}
pub fn clear_safe_mode(&self) {
    self.safe_mode.store(false, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut v) = self.tampered_ids.lock() { v.clear(); }
}
pub fn tampered_ids(&self) -> Vec<String> {
    self.tampered_ids.lock().map(|v| v.clone()).unwrap_or_default()
}
```

- [ ] **Step 4: Disparar el modo seguro al detectar manipulación**

En el punto donde el scheduler recoge las txs a difundir (la ruta `broadcast_due_transactions` / `get_due_transactions` y el `run_scheduler_tick`), tras leer cada `BroadcastTx`, si `tx.tampered == Some(true)` llamar `pm.enter_safe_mode(&tx.id)` y saltar esa tx. Además, al principio del tick de difusión, si `pm.is_safe_mode()` → no difundir nada y registrar `warn` (halt global).

- [ ] **Step 5: Gate en `run_broadcast_loop`** (`src/pool/scheduler.rs`)

Al inicio de cada iteración del loop de difusión, si `self.pool_manager.is_safe_mode()` → saltar la difusión (log `warn!` una vez) y continuar durmiendo. Las demás tareas (health, price) siguen.

- [ ] **Step 6: Endpoint `acknowledge`** (`src/api/mod.rs`)

Añadir ruta `.route("/api/security/acknowledge", post(security_acknowledge))` y handler:

```rust
async fn security_acknowledge(State(state): State<AppState>) -> Json<serde_json::Value> {
    state.pool_manager.clear_safe_mode();
    Json(serde_json::json!({ "ok": true }))
}
```

- [ ] **Step 7: Exponer en `/api/status`**

En el handler `get_status`, añadir a la respuesta `safe_mode: state.pool_manager.is_safe_mode()` y `tampered_ids: state.pool_manager.tampered_ids()`.

- [ ] **Step 8: Tests + build** → PASS.

- [ ] **Step 9: Commit**

```bash
git add src/pool/manager.rs src/pool/scheduler.rs src/api/mod.rs
git commit -m "feat(pool): global safe-mode halt on tamper + acknowledge endpoint"
```

### Task 6: Cifrar el RPC password del `config.toml`

**Files:**
- Modify: `src/config.rs` (descifrado al cargar), `src/discovery.rs` (cifrado al guardar)

**Interfaces:**
- Consumes: `crypto::encode_field`/`decode_field` con la keyfile. **Problema:** `config.rs` no tiene la keyfile. Solución: cargar la keyfile en `main.rs` (ya se hace en `Database::open`) y aplicar el cifrado del password en el punto de guardado/carga que sí tenga acceso a la clave, o exponer la clave desde `Database`. Enfoque simple: pasar la clave a un helper de config.

**Decisión:** añadir a `discovery::save_config_to_disk` un parámetro `key: &[u8;32]` (o una variante `save_config_to_disk_encrypted`) que cifra `bitcoin_rpc.password` (si no está ya con prefijo) antes de serializar; y al cargar (`Config::load`) NO se descifra (no hay clave allí). En su lugar, tras abrir la DB en `main.rs`, descifrar en memoria el password del config usando `db.key()`. El pin por entorno mantiene prioridad.

- [ ] **Step 1: Test que falla** (`src/config.rs` o test de integración en `crypto`/`db`)

Test unitario del helper de (des)cifrado de password:

```rust
#[test]
fn rpc_password_encrypt_decrypt_roundtrip_and_legacy() {
    let key = crate::crypto::generate_key();
    let enc = crate::config::encrypt_rpc_password(&key, "s3cret");
    assert!(enc.starts_with("enc:v1:"));
    assert_eq!(crate::config::decrypt_rpc_password(&key, &enc), "s3cret");
    // Legacy plaintext passes through.
    assert_eq!(crate::config::decrypt_rpc_password(&key, "plain"), "plain");
}
```

- [ ] **Step 2: Ver fallo.**

- [ ] **Step 3: Implementar helpers en `config.rs`**

```rust
pub fn encrypt_rpc_password(key: &[u8; 32], pw: &str) -> String {
    if pw.is_empty() || crate::crypto::is_encoded(pw) { return pw.to_string(); }
    crate::crypto::encode_field(key, pw, b"bitcoin_rpc.password")
}
pub fn decrypt_rpc_password(key: &[u8; 32], stored: &str) -> String {
    crate::crypto::decode_field(key, stored, b"bitcoin_rpc.password")
        .unwrap_or_else(|_| stored.to_string())
}
```

- [ ] **Step 4: Descifrar al arrancar** (`src/main.rs`)

Tras `let db = Arc::new(Database::open(&db_path)?);`, si hay `config.bitcoin_rpc` con password y el pin de entorno no está, descifrar en memoria:

```rust
if let Some(rpc) = config.bitcoin_rpc.as_mut() {
    if std::env::var("BROADCAST_POOL_RPC_PASS").is_err() {
        rpc.password = config_decrypt_helper(db.key(), &rpc.password); // usa decrypt_rpc_password
    }
}
```

(Asegurar que `config` es mutable en ese punto.)

- [ ] **Step 5: Cifrar al guardar** (`src/discovery.rs::save_config_to_disk`)

Antes de `toml::to_string_pretty`, clonar el config y, si tiene `bitcoin_rpc.password` en claro, cifrarlo con la keyfile. Como `save_config_to_disk` no recibe la clave hoy, añadir el parámetro `key: &[u8;32]` y actualizar sus llamadas (buscar `save_config_to_disk(`), pasando `db.key()` / la clave disponible. Para la llamada del virtual-block (que está en `electrum_server`), pasar la clave desde el estado.

- [ ] **Step 6: Tests + build** → PASS. Verificar arranque local no rompe (config sin bitcoin_rpc también válido).

- [ ] **Step 7: Commit**

```bash
git add src/config.rs src/discovery.rs src/main.rs src/electrum_server/mod.rs
git commit -m "feat(config): encrypt bitcoin_rpc password at rest with keyfile"
```

### Task 7: Migración de arranque — cifrar filas legado + sellar `row_mac`

**Files:**
- Modify: `src/db/mod.rs` (nuevo método `encrypt_legacy_rows`, llamado en `open`)

**Interfaces:**
- Produces: `Database::encrypt_legacy_rows(&self) -> Result<usize>` (idempotente).

- [ ] **Step 1: Test que falla**

```rust
#[test]
fn legacy_plaintext_rows_get_encrypted_and_macked() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("l.db")).unwrap();
    // Insert a row, then force its columns back to plaintext + null mac (simulate legacy).
    let stored = db.insert_broadcast_tx(&crate::db::models::NewBroadcastTx{
        tx_hex: tx_hex_with_output_sats(&[10_000]), network:"testnet4".into(), nlocktime:None,
        broadcast_mode:Some("scheduled".into()), scheduled_time:Some(chrono::Utc::now()),
        target_fee_rate:None, source_label:Some("L".into()), destination_address:Some("addrX".into()),
        utxo_count:Some(1), total_value_btc:None, replacement_of:None }).unwrap();
    db.lock_conn().unwrap().execute(
        "UPDATE broadcast_pool SET destination_address='addrX', row_mac=NULL WHERE id=?1",
        params![stored.id]).unwrap();

    let n = db.encrypt_legacy_rows().unwrap();
    assert_eq!(n, 1);
    // Now stored encrypted, read decrypts, and mac verifies (not tampered).
    let raw: String = db.lock_conn().unwrap().query_row(
        "SELECT destination_address FROM broadcast_pool WHERE id=?1", params![stored.id],
        |r| r.get(0)).unwrap();
    assert!(raw.starts_with("enc:v1:"));
    let got = db.get_broadcast_tx_by_id(&stored.id).unwrap();
    assert_eq!(got.destination_address.as_deref(), Some("addrX"));
    assert_ne!(got.tampered, Some(true));
}
```

- [ ] **Step 2: Ver fallo.**

- [ ] **Step 3: Implementar `encrypt_legacy_rows`**

Seleccionar filas cuyo `tx_hex` no empiece por `enc:` O `row_mac IS NULL`. Para cada una: leer los campos en claro, cifrar `tx_hex`/`destination_address`/`source_label` con `aad=id`, recomputar `row_mac` con los valores cifrados/normalizados, y `UPDATE` con los cifrados + `row_mac`. Idempotente. Devolver el nº de filas tocadas.

- [ ] **Step 4: Llamar en `open`** (no fatal), tras las migraciones y el backfill de valor:

```rust
if let Err(e) = db.encrypt_legacy_rows() {
    tracing::warn!("Legacy encryption migration warning (non-fatal): {}", e);
}
```

- [ ] **Step 5: Tests + build** → PASS.

- [ ] **Step 6: Commit**

```bash
git add src/db/mod.rs
git commit -m "feat(db): startup migration encrypts legacy rows and seals row_mac"
```

---

## FASE 3 — Feature B: archivo histórico cifrado

### Task 8: Esquema `archive_pool` + `config_store` + métodos DB

**Files:**
- Modify: `src/db/schema.rs` (MIGRATION_009), `src/db/mod.rs` (métodos)

**Interfaces:**
- Produces:
  - `Database::get_config_value(key) -> Result<Option<String>>`, `Database::set_config_value(key, value) -> Result<()>`
  - `Database::insert_archive(id, network, archived_at, blob: &[u8]) -> Result<()>`
  - `Database::list_archive(network, limit, offset) -> Result<Vec<ArchiveMeta>>` (`ArchiveMeta { id, network, archived_at }`)
  - `Database::get_archive_blob(id) -> Result<Option<Vec<u8>>>`
  - `Database::select_terminal_older_than(network, cutoff_rfc3339) -> Result<Vec<BroadcastTx>>`
  - `Database::delete_broadcast_tx(id) -> Result<()>`

- [ ] **Step 1: Tests que fallan** (config_store + archive CRUD)

```rust
#[test]
fn config_store_get_set() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("c.db")).unwrap();
    assert_eq!(db.get_config_value("k").unwrap(), None);
    db.set_config_value("k", "v").unwrap();
    assert_eq!(db.get_config_value("k").unwrap(), Some("v".into()));
    db.set_config_value("k", "v2").unwrap(); // upsert
    assert_eq!(db.get_config_value("k").unwrap(), Some("v2".into()));
}

#[test]
fn archive_insert_list_get() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("a.db")).unwrap();
    db.insert_archive("id1", "testnet4", "2026-06-01T00:00:00Z", b"blob-bytes").unwrap();
    let list = db.list_archive("testnet4", 10, 0).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "id1");
    assert_eq!(db.get_archive_blob("id1").unwrap().unwrap(), b"blob-bytes");
}
```

- [ ] **Step 2: Ver fallo.**

- [ ] **Step 3: Migración 009** (`src/db/schema.rs`) — tabla del spec (§ "Esquema (Migración 009)"). Registrarla no-fatal en `run_migrations`.

- [ ] **Step 4: Modelo `ArchiveMeta`** en `src/db/models.rs` (`#[derive(Serialize)]`, campos `id`, `network`, `archived_at`).

- [ ] **Step 5: Implementar los métodos** en `src/db/mod.rs` (config_store con `INSERT ... ON CONFLICT(key) DO UPDATE`; archive con los SELECT/INSERT/DELETE indicados). `select_terminal_older_than` filtra `status IN ('confirmed','failed','broadcast') AND updated_at < ?cutoff AND network=?net` y mapea con `map_broadcast_row(row, &self.key)` (descifra al vuelo).

- [ ] **Step 6: Tests + build** → PASS.

- [ ] **Step 7: Commit**

```bash
git add src/db/schema.rs src/db/mod.rs src/db/models.rs
git commit -m "feat(db): archive_pool schema + config_store + archive/retention DB methods"
```

### Task 9: Password del archivo + almacén de clave en memoria

**Files:**
- Create: `src/api/archive_key.rs` (o dentro de `api/mod.rs`)
- Modify: `src/api/mod.rs` (`AppState` gana el key store)

**Interfaces:**
- Produces:
  - `ArchiveKeyStore` con `set_password(db, password)`, `unlock(db, password) -> bool`, `lock()`, `key() -> Option<[u8;32]>` (respeta expiración), `is_unlocked()`, `password_is_set(db) -> bool`.
  - Config store keys: `archive_salt` (hex), `archive_verifier` (Argon2 PHC).

- [ ] **Step 1: Tests que fallan** (unlock correcto/incorrecto, expiración)

```rust
#[test]
fn archive_password_set_unlock_wrong_and_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let db = std::sync::Arc::new(Database::open(&dir.path().join("k.db")).unwrap());
    let store = ArchiveKeyStore::new();
    assert!(!store.password_is_set(&db).unwrap());
    store.set_password(&db, "hunter2").unwrap();
    assert!(store.password_is_set(&db).unwrap());
    assert!(!store.unlock(&db, "wrong").unwrap());
    assert!(store.key().is_none());
    assert!(store.unlock(&db, "hunter2").unwrap());
    assert!(store.key().is_some());
    store.lock();
    assert!(store.key().is_none());
}
```

- [ ] **Step 2: Ver fallo.**

- [ ] **Step 3: Implementar `ArchiveKeyStore`**

- `set_password`: genera `salt` (16 bytes) → guarda `archive_salt` (hex) y `archive_verifier` (PHC de Argon2 vía `argon2::PasswordHasher` con `SaltString`). No guarda la clave.
- `unlock`: valida el password contra `archive_verifier`; si OK, deriva `key = crypto::derive_key(password, salt)` con el `archive_salt` guardado y la cachea en `Arc<Mutex<Option<(key, Instant expires_at)>>>` con TTL (p. ej. 15 min).
- `key()`: devuelve la clave solo si no ha expirado; si expiró, la borra y devuelve `None`. Renueva `expires_at` en cada acceso válido (sliding).
- `lock()`: limpia la clave.

- [ ] **Step 4: Añadir a `AppState`**

`pub archive_keys: std::sync::Arc<ArchiveKeyStore>` y construirlo donde se crea el `AppState`.

- [ ] **Step 5: Tests + build** → PASS.

- [ ] **Step 6: Commit**

```bash
git add src/api/mod.rs src/api/archive_key.rs
git commit -m "feat(api): archive password (Argon2 verifier) + in-memory key store with TTL"
```

### Task 10: Loop de retención

**Files:**
- Modify: `src/pool/scheduler.rs` (nuevo `run_retention_loop`), `src/config.rs` (default `expiry_days` → 30), y el punto de arranque que tiene acceso al `ArchiveKeyStore`.

**Nota de acceso:** el `Scheduler` hoy tiene `pool_manager` y `config`, no el `ArchiveKeyStore` ni la `db` directamente. Para la retención se necesita la clave del archivo (memoria) + la db. Pasar al `Scheduler` (o al nuevo loop) un `Arc<Database>` y un `Arc<ArchiveKeyStore>`. Ampliar `Scheduler::new` o crear el loop en `main.rs` con esas dependencias (elección del implementador; preferible ampliar `Scheduler` para mantener los loops juntos).

**Interfaces:**
- Consumes: `ArchiveKeyStore::key()`, `Database::select_terminal_older_than`, `crypto::seal`, `Database::insert_archive`, `Database::delete_broadcast_tx`.

- [ ] **Step 1: Test que falla** — test unitario de la función de un tick de retención (extraer la lógica a `Database::archive_terminal_older_than(key, cutoff)` para poder testearla sin loop):

```rust
#[test]
fn retention_archives_terminal_and_deletes_from_pool() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("r.db")).unwrap();
    let archive_key = crate::crypto::generate_key();
    // Insert a confirmed tx with an old updated_at.
    let stored = db.insert_broadcast_tx(&/* confirmed-ish new tx */).unwrap();
    db.update_tx_status(&stored.id, TxStatus::Confirmed, None).unwrap();
    db.lock_conn().unwrap().execute(
        "UPDATE broadcast_pool SET updated_at='2000-01-01T00:00:00Z' WHERE id=?1",
        params![stored.id]).unwrap();

    let moved = db.archive_terminal_older_than(&archive_key, "testnet4", "2026-01-01T00:00:00Z").unwrap();
    assert_eq!(moved, 1);
    assert!(db.get_broadcast_tx_by_id(&stored.id).is_err()); // gone from active pool
    let list = db.list_archive("testnet4", 10, 0).unwrap();
    assert_eq!(list.len(), 1);
    // Blob does not contain plaintext.
    let blob = db.get_archive_blob(&list[0].id).unwrap().unwrap();
    assert!(!String::from_utf8_lossy(&blob).contains("testnet4-plaintext-marker"));
}
```

- [ ] **Step 2: Ver fallo.**

- [ ] **Step 3: Implementar `Database::archive_terminal_older_than(key, network, cutoff)`**

Selecciona terminales > cutoff (`select_terminal_older_than`), y por cada `BroadcastTx` (ya descifrado): serializa a JSON, `blob = crypto::seal(key, json, aad=nuevo_id)`, `insert_archive(nuevo_id, network, archived_at=now, &blob)`, `delete_broadcast_tx(id_original)`. Devuelve el nº movidas. (Usar un `id` de archivo nuevo/opaco para no revelar el original.)

- [ ] **Step 4: `run_retention_loop`** (`src/pool/scheduler.rs`)

Loop con `interval` diario (86400s; para test/manual, configurable). En cada tick: si `archive_keys.key()` es `Some(key)` → por cada red configurada, `cutoff = now - expiry_days`, llamar `db.archive_terminal_older_than(&key, net, &cutoff)`, loggear el nº. Si `None` → `debug!("archivo bloqueado, retención en pausa")`. Registrar el loop en `start_all_loops`.

- [ ] **Step 5: Default `expiry_days` → 30** en `src/config.rs` (donde hoy pone `expiry_days: 14`).

- [ ] **Step 6: Tests + build** → PASS.

- [ ] **Step 7: Commit**

```bash
git add src/pool/scheduler.rs src/db/mod.rs src/config.rs src/main.rs
git commit -m "feat(pool): daily retention loop archives terminal txs when unlocked"
```

### Task 11: Endpoints del archivo + estado

**Files:**
- Modify: `src/api/mod.rs`

**Interfaces (rutas):**
- `POST /api/archive/set-password {password}` → guarda salt+verifier (rechaza si ya hay password salvo que se implemente cambio; para v1: solo set si no existe).
- `POST /api/archive/unlock {password}` → `{unlocked: bool}`; 200 aunque sea incorrecto con `{unlocked:false}`.
- `POST /api/archive/lock` → `{ok:true}`.
- `GET /api/archive?limit&offset` → 401 `{locked:true}` si no hay clave; si no, `[ArchiveMeta]`.
- `GET /api/archive/{id}` → 401 si bloqueado; si no, descifra el blob y devuelve el registro JSON.
- `/api/status`: añadir `archive_locked`, `archive_password_set`.

- [ ] **Step 1: Test que falla** (test de handler o de integración ligera; si el proyecto no tiene tests de axum, testear la lógica subyacente del `ArchiveKeyStore` + `list_archive` ya cubierta — aquí basta un test que verifique que `GET /api/archive` sin clave responde locked). Si montar axum en test es costoso, cubrir con un test del flujo a nivel de `ArchiveKeyStore`/DB y validar los handlers manualmente en el paso de verificación.

- [ ] **Step 2: Implementar los handlers** usando `state.archive_keys` y `state.db`. Para `set-password` usar `ArchiveKeyStore::set_password`. Para `GET` sin clave devolver `(StatusCode::UNAUTHORIZED, Json(json!({"locked":true})))`. Para el detalle, `crypto::open(&key, &blob, aad=id)` → `serde_json::from_slice`.

- [ ] **Step 3: Registrar rutas** en el `Router` de `create_router`/`app`.

- [ ] **Step 4: Ampliar `/api/status`** con `archive_locked = !state.archive_keys.is_unlocked()` y `archive_password_set = state.archive_keys.password_is_set(&state.db).unwrap_or(false)`.

- [ ] **Step 5: Build + `cargo test`** → PASS. Verificación manual con `curl` en el paso final.

- [ ] **Step 6: Commit**

```bash
git add src/api/mod.rs
git commit -m "feat(api): archive unlock/lock/set-password/list/get endpoints + status"
```

### Task 12: UI — pestaña "Archivo" + alerta de modo seguro + i18n

**Files:**
- Modify: `src/api/dashboard.html`

**Interfaces:** consume los endpoints de Task 5 y 11 y los campos nuevos de `/api/status`.

- [ ] **Step 1: Alerta de modo seguro**

En el render principal (donde se pinta el estado), si `status.safe_mode` es `true`, mostrar un banner rojo prominente con el texto i18n `safe_mode_alert` y la lista `status.tampered_ids`, y un botón "Reconocer" que hace `POST /api/security/acknowledge` y refresca. (Seguir el patrón de banners existentes en el fichero.)

- [ ] **Step 2: Pestaña "Archivo"**

Añadir una pestaña/nav "Archivo" (patrón de las pestañas existentes). Estados:
- Si `!archive_password_set`: formulario para fijar password (`POST /api/archive/set-password`) con aviso i18n `archive_no_recovery` ("si pierdes el password no se puede recuperar").
- Si `archive_locked`: formulario de password → `POST /api/archive/unlock`.
- Si desbloqueado: `GET /api/archive?limit=50&offset=…`, tabla paginada `{archived_at, network, id}`, y al hacer click `GET /api/archive/{id}` → modal con el detalle (reutilizar el modal de detalle de tx existente). Botón "Bloquear" → `POST /api/archive/lock`.

- [ ] **Step 3: i18n (en/es)**

Añadir claves en ambos diccionarios (junto a las existentes):
- `tab_archive`: 'Archive' / 'Archivo'
- `safe_mode_alert`: 'Tampering detected — broadcasting halted. Review and acknowledge.' / 'Manipulación detectada — difusión detenida. Revisa y reconoce.'
- `safe_mode_ack`: 'Acknowledge' / 'Reconocer'
- `archive_set_pw`, `archive_unlock`, `archive_lock`, `archive_locked_msg`, `archive_no_recovery`, `archive_empty` (con textos en/es coherentes).

- [ ] **Step 4: Verificación manual**

Run: `cargo build` y arrancar local (o construir imagen para el nodo). Comprobar: banner de modo seguro aparece si `safe_mode`; pestaña Archivo permite set-password → unlock → listar. (Sin datos archivados aún, la lista sale vacía con `archive_empty`.)

- [ ] **Step 5: Commit**

```bash
git add src/api/dashboard.html
git commit -m "feat(dashboard): Archive tab + safe-mode alert + i18n (en/es)"
```

---

## Verificación final (tras todas las tareas)

- `cargo test` completo en verde (todos los tests nuevos + los 65 previos).
- `cargo build --release` compila.
- Construir imagen de prueba y desplegar al nodo .26 (mismo flujo que la imagen `0.3.21-test` ya usada): importar/confirmar txs, comprobar que en la DB los campos sensibles están `enc:v1:`, que manipular una fila activa dispara el banner de modo seguro, que el archivo pide password y lista tras desbloquear.
- No secretos en logs (revisar `tracing` añadido).
- Bump de versión y release: se decidirá junto al merge de `fix/pool-value-sats` (ver [[broadcast-pool-release-flow]]).
