//! At-rest encryption for the few secrets the service must be able to read back (game passwords
//! it hands to the client, the SOAP password): ChaCha20-Poly1305 under a master key that lives
//! only in the state volume. Losing `master.key` loses those secrets — but every one of them is
//! re-creatable (`rotate` in the panel, `reset-admin`), so it is not backed up specially.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit, OsRng};
use chacha20poly1305::{AeadCore, ChaCha20Poly1305, Key, Nonce};

#[derive(Clone)]
pub struct Keyring {
    cipher: ChaCha20Poly1305,
}

impl Keyring {
    /// Load `master.key` from `state_dir`, creating it (0600) when absent.
    pub fn load_or_create(state_dir: &Path) -> Result<Self> {
        let path = state_dir.join("master.key");
        let key = if path.exists() {
            let hex = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            hex::decode(hex.trim()).context("master.key is not hex")?
        } else {
            let key = ChaCha20Poly1305::generate_key(&mut OsRng);
            write_private(&path, &hex::encode(key))?;
            key.to_vec()
        };
        if key.len() != 32 {
            return Err(anyhow!("master.key must be 32 bytes"));
        }
        Ok(Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(&key)),
        })
    }

    pub fn from_bytes(key: &[u8; 32]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(key)),
        }
    }

    /// `(ciphertext, nonce)`.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ct = self
            .cipher
            .encrypt(&nonce, plaintext)
            .map_err(|_| anyhow!("encrypt failed"))?;
        Ok((ct, nonce.to_vec()))
    }

    pub fn decrypt(&self, ciphertext: &[u8], nonce: &[u8]) -> Result<Vec<u8>> {
        if nonce.len() != 12 {
            return Err(anyhow!("bad nonce length"));
        }
        self.cipher
            .decrypt(Nonce::from_slice(nonce), ciphertext)
            .map_err(|_| anyhow!("decrypt failed (wrong master key?)"))
    }

    pub fn decrypt_string(&self, ciphertext: &[u8], nonce: &[u8]) -> Result<String> {
        String::from_utf8(self.decrypt(ciphertext, nonce)?).context("secret is not UTF-8")
    }
}

/// Write a file readable by its owner only.
pub fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    f.write_all(contents.as_bytes())?;
    Ok(())
}

/// `len` random characters from `alphabet`.
pub fn random_string(alphabet: &[u8], len: usize) -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| alphabet[rng.gen_range(0..alphabet.len())] as char)
        .collect()
}

pub const ALNUM_UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
pub const ALNUM: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let k = Keyring::from_bytes(&[7u8; 32]);
        let (ct, nonce) = k.encrypt(b"hunter2").unwrap();
        assert_eq!(k.decrypt_string(&ct, &nonce).unwrap(), "hunter2");
        assert!(Keyring::from_bytes(&[8u8; 32])
            .decrypt(&ct, &nonce)
            .is_err());
    }

    #[test]
    fn key_file_is_created_once() {
        let dir = tempfile::tempdir().unwrap();
        let a = Keyring::load_or_create(dir.path()).unwrap();
        let (ct, nonce) = a.encrypt(b"x").unwrap();
        let b = Keyring::load_or_create(dir.path()).unwrap();
        assert_eq!(b.decrypt(&ct, &nonce).unwrap(), b"x");
    }
}
