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
