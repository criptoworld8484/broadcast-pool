//! Archive password + in-memory key store.
//!
//! The archive password protects a symmetric key used to encrypt archived data.
//! Only a salt (`archive_salt`) and an Argon2 PHC verifier (`archive_verifier`)
//! are ever persisted in `config_store`. The derived key itself lives only in
//! memory, cached with a sliding TTL, and is cleared on `lock()`.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use argon2::password_hash::{rand_core::OsRng, PasswordHash, SaltString};
use argon2::{Argon2, PasswordHasher, PasswordVerifier};

use crate::crypto;
use crate::db::Database;

const ARCHIVE_SALT_KEY: &str = "archive_salt";
const ARCHIVE_VERIFIER_KEY: &str = "archive_verifier";
const TTL: Duration = Duration::from_secs(15 * 60);

struct CachedKey {
    key: [u8; 32],
    expires_at: Instant,
}

pub struct ArchiveKeyStore {
    cached: Mutex<Option<CachedKey>>,
}

impl ArchiveKeyStore {
    pub fn new() -> Self {
        Self {
            cached: Mutex::new(None),
        }
    }

    pub fn password_is_set(&self, db: &Database) -> Result<bool> {
        Ok(db.get_config_value(ARCHIVE_VERIFIER_KEY)?.is_some())
    }

    pub fn set_password(&self, db: &Database, password: &str) -> Result<()> {
        let salt_bytes: [u8; 16] = rand::random();
        let salt_string = SaltString::encode_b64(&salt_bytes)
            .map_err(|e| anyhow!("failed to encode salt: {e}"))?;
        let argon2 = Argon2::default();
        let verifier = argon2
            .hash_password(password.as_bytes(), &salt_string)
            .map_err(|e| anyhow!("failed to hash password: {e}"))?
            .to_string();

        db.set_config_value(ARCHIVE_SALT_KEY, &hex::encode(salt_bytes))?;
        db.set_config_value(ARCHIVE_VERIFIER_KEY, &verifier)?;
        Ok(())
    }

    pub fn unlock(&self, db: &Database, password: &str) -> Result<bool> {
        let verifier = match db.get_config_value(ARCHIVE_VERIFIER_KEY)? {
            Some(v) => v,
            None => return Err(anyhow!("archive password is not set")),
        };
        let salt_hex = match db.get_config_value(ARCHIVE_SALT_KEY)? {
            Some(v) => v,
            None => return Err(anyhow!("archive salt is missing")),
        };
        let salt_bytes = hex::decode(&salt_hex).map_err(|e| anyhow!("invalid archive salt: {e}"))?;

        let parsed_hash =
            PasswordHash::new(&verifier).map_err(|e| anyhow!("invalid archive verifier: {e}"))?;
        let ok = Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok();

        if !ok {
            return Ok(false);
        }

        let key = crypto::derive_key(password, &salt_bytes);
        let mut guard = self.cached.lock().unwrap();
        *guard = Some(CachedKey {
            key,
            expires_at: Instant::now() + TTL,
        });
        Ok(true)
    }

    pub fn key(&self) -> Option<[u8; 32]> {
        let mut guard = self.cached.lock().unwrap();
        match guard.as_mut() {
            Some(cached) => {
                if Instant::now() >= cached.expires_at {
                    *guard = None;
                    None
                } else {
                    cached.expires_at = Instant::now() + TTL;
                    Some(cached.key)
                }
            }
            None => None,
        }
    }

    pub fn lock(&self) {
        let mut guard = self.cached.lock().unwrap();
        *guard = None;
    }

    /// Non-sliding peek: reports whether a key is currently cached and unexpired,
    /// WITHOUT renewing the TTL. Used by passive checks (e.g. `/api/status` polling)
    /// so that merely observing lock state doesn't keep the archive unlocked forever.
    /// If the cached key has expired, it is cleared as a side effect.
    pub fn is_unlocked(&self) -> bool {
        let mut guard = self.cached.lock().unwrap();
        match guard.as_ref() {
            Some(cached) => {
                if Instant::now() >= cached.expires_at {
                    *guard = None;
                    false
                } else {
                    true
                }
            }
            None => false,
        }
    }
}

impl Default for ArchiveKeyStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use std::sync::Arc;

    #[test]
    fn archive_password_set_unlock_wrong_and_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&dir.path().join("k.db")).unwrap());
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

    #[test]
    fn archive_key_expires_after_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&dir.path().join("k.db")).unwrap());
        let store = ArchiveKeyStore::new();
        store.set_password(&db, "hunter2").unwrap();
        assert!(store.unlock(&db, "hunter2").unwrap());
        assert!(store.is_unlocked());

        // Force expiry by rewriting the cached entry's expires_at into the past.
        {
            let mut guard = store.cached.lock().unwrap();
            if let Some(cached) = guard.as_mut() {
                cached.expires_at = Instant::now() - Duration::from_secs(1);
            }
        }
        assert!(store.key().is_none());
        assert!(!store.is_unlocked());
    }

    #[test]
    fn is_unlocked_does_not_slide_ttl_but_key_does() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&dir.path().join("k.db")).unwrap());
        let store = ArchiveKeyStore::new();
        store.set_password(&db, "hunter2").unwrap();
        assert!(store.unlock(&db, "hunter2").unwrap());

        let initial_expires_at = store.cached.lock().unwrap().as_ref().unwrap().expires_at;

        // Passive peeks must NOT renew expires_at.
        for _ in 0..5 {
            assert!(store.is_unlocked());
        }
        let after_peeks = store.cached.lock().unwrap().as_ref().unwrap().expires_at;
        assert_eq!(
            initial_expires_at, after_peeks,
            "is_unlocked() must not slide the TTL"
        );

        // A genuine access via key() must renew expires_at.
        assert!(store.key().is_some());
        let after_key = store.cached.lock().unwrap().as_ref().unwrap().expires_at;
        assert!(
            after_key > after_peeks,
            "key() must slide the TTL forward"
        );
    }
}
