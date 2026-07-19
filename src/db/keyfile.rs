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
