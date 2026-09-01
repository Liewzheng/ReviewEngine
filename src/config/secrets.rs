//! Encryption at rest for secrets persisted by the web UI.
//!
//! `ui-state.toml` stores the UI's live credentials (git platform tokens,
//! webhook secrets, legacy GitLab fields). Historically these were plaintext
//! on disk; this module adds an encrypt-at-rest layer that keeps the
//! in-memory/runtime path plaintext and only encrypts at the persistence
//! boundary.
//!
//! Scheme: one per-config-dir symmetric key file `secrets.key` (32 random
//! bytes, `0600` on Unix, written atomically) plus per-value
//! ChaCha20-Poly1305 with a fresh 12-byte random nonce per secret. On disk a
//! secret reads:
//!
//! ```text
//! enc:<base64( nonce[12] ‖ ciphertext ‖ tag )>
//! ```
//!
//! Values without the `enc:` prefix are legacy plaintext and pass through
//! unchanged (transparent migration: old files load fine, the next save
//! encrypts everything).

use std::path::{Path, PathBuf};

use anyhow::Context;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::RngCore;

/// File name of the local key, in the same directory as `ui-state.toml`.
pub const SECRETS_KEY_FILE_NAME: &str = "secrets.key";

/// Prefix marking an at-rest-encrypted secret.
pub const ENC_PREFIX: &str = "enc:";

/// ChaCha20-Poly1305 nonce length in bytes.
const NONCE_LEN: usize = 12;

/// The key file for `state_file` lives next to it and is named `secrets.key`.
pub fn key_path_for(state_file: &Path) -> PathBuf {
    state_file
        .parent()
        .map(|dir| dir.join(SECRETS_KEY_FILE_NAME))
        .unwrap_or_else(|| PathBuf::from(SECRETS_KEY_FILE_NAME))
}

/// Read the key file if present; `Ok(None)` when it does not exist. A file
/// with the wrong length is treated as corruption, never as a missing key.
pub(crate) fn load_key(key_path: &Path) -> anyhow::Result<Option<[u8; 32]>> {
    match std::fs::read(key_path) {
        Ok(bytes) => {
            let key: [u8; 32] = bytes.try_into().map_err(|original: Vec<u8>| {
                anyhow::anyhow!(
                    "secrets key file {} is corrupted: expected exactly 32 bytes, found {}; \
                     restore the key file or re-enter the secrets in the web UI",
                    key_path.display(),
                    original.len()
                )
            })?;
            Ok(Some(key))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("failed to read secrets key file {}", key_path.display())),
    }
}

/// Load the local key, generating and persisting it on first use. Created
/// keys are 32 random bytes written atomically (same-directory temp file +
/// rename) with `0600` permissions on Unix.
pub fn load_or_create_key(key_path: &Path) -> anyhow::Result<[u8; 32]> {
    if let Some(key) = load_key(key_path)? {
        return Ok(key);
    }
    let mut key = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut key);
    write_key_atomically(key_path, &key)?;
    // A concurrent save may have created the key between our read and our
    // rename; use whatever the file actually holds so the returned key always
    // matches `secrets.key` on disk.
    load_key(key_path)?.ok_or_else(|| anyhow::anyhow!("failed to persist secrets key file {}", key_path.display()))
}

/// Atomically persist a freshly generated key (temp file + rename, `0600` on
/// Unix) so a crash mid-write never leaves a truncated key file.
fn write_key_atomically(key_path: &Path, key: &[u8; 32]) -> anyhow::Result<()> {
    if let Some(parent) = key_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_path = key_path.with_extension("key.tmp");
    std::fs::write(&tmp_path, key)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp_path, key_path)?;
    Ok(())
}

/// Encrypt `plain` into the on-disk form `enc:<base64(nonce ‖ ciphertext‖tag)>`.
///
/// A fresh 12-byte random nonce is generated per call, so encrypting the same
/// plaintext twice yields different ciphertexts.
pub fn encrypt_secret(plain: &str, key: &[u8; 32]) -> anyhow::Result<String> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plain.as_bytes())
        .map_err(|e| anyhow::anyhow!("failed to encrypt secret: {e}"))?;
    let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ciphertext);
    Ok(format!(
        "{ENC_PREFIX}{}",
        base64::engine::general_purpose::STANDARD.encode(blob)
    ))
}

/// Decrypt an on-disk secret. Values without the `enc:` prefix are legacy
/// plaintext and are returned unchanged (transparent migration).
///
/// A decrypt failure (wrong/missing key, corrupted value) is an error, never
/// silently swallowed — the caller surfaces it so the user knows the secret
/// must be re-entered.
pub fn decrypt_secret(value: &str, key: &[u8; 32]) -> anyhow::Result<String> {
    let Some(encoded) = value.strip_prefix(ENC_PREFIX) else {
        return Ok(value.to_string());
    };
    let blob = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| anyhow::anyhow!("encrypted secret is corrupted (invalid base64): {e}"))?;
    if blob.len() < NONCE_LEN {
        anyhow::bail!("encrypted secret is corrupted (payload shorter than the nonce)");
    }
    let (nonce, ciphertext) = blob.split_at(NONCE_LEN);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let plain = cipher.decrypt(Nonce::from_slice(nonce), ciphertext).map_err(|_| {
        anyhow::anyhow!(
            "failed to decrypt an `{ENC_PREFIX}` secret: the local key is missing, rotated, or the value is \
                 corrupted; re-enter the secret in the web UI to restore it"
        )
    })?;
    String::from_utf8(plain).map_err(|e| anyhow::anyhow!("decrypted secret is not valid UTF-8: {e}"))
}
