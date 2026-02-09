//! Shared AES-256-GCM encryption primitives.
//!
//! Provides encrypt/decrypt operations and the on-disk encrypted data
//! format used by both `relay::persistence` (crypto session state) and
//! `lua::primitives::secrets` (plugin secrets).
//!
//! # Wire Format
//!
//! Each encrypted file is a JSON object:
//! ```json
//! { "nonce": "<base64>", "ciphertext": "<base64>", "version": <u8> }
//! ```
//!
//! The version field is caller-defined (e.g., 6 for vodozemac state, 1 for secrets).

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use rand::RngCore;
use serde::{Deserialize, Serialize};

/// Nonce size for AES-GCM (96 bits = 12 bytes).
const NONCE_SIZE: usize = 12;

/// Encrypted data envelope stored on disk.
#[derive(Debug, Serialize, Deserialize)]
pub struct EncryptedData {
    /// Base64-encoded nonce (12 bytes).
    pub nonce: String,
    /// Base64-encoded ciphertext.
    pub ciphertext: String,
    /// Version identifier (caller-defined).
    pub version: u8,
}

/// Encrypt plaintext using AES-256-GCM with a random nonce.
///
/// The `version` tag is stored in the envelope for callers to distinguish
/// format versions (e.g., crypto state v6 vs secrets v1).
pub fn encrypt(key: &[u8; 32], plaintext: &[u8], version: u8) -> Result<EncryptedData> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("valid key length");

    let mut nonce_bytes = [0u8; NONCE_SIZE];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("Encryption failed: {e}"))?;

    Ok(EncryptedData {
        nonce: BASE64.encode(nonce_bytes),
        ciphertext: BASE64.encode(ciphertext),
        version,
    })
}

/// Decrypt an `EncryptedData` envelope using AES-256-GCM.
pub fn decrypt(key: &[u8; 32], encrypted: &EncryptedData) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("valid key length");

    let nonce_bytes = BASE64
        .decode(&encrypted.nonce)
        .context("Invalid nonce encoding")?;
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = BASE64
        .decode(&encrypted.ciphertext)
        .context("Invalid ciphertext encoding")?;

    cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|e| anyhow::anyhow!("Decryption failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = [42u8; 32];
        let plaintext = b"Hello, encrypted world!";

        let encrypted = encrypt(&key, plaintext, 1).unwrap();
        assert_eq!(encrypted.version, 1);

        let decrypted = decrypt(&key, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_version_preserved() {
        let key = [0u8; 32];
        let encrypted = encrypt(&key, b"data", 6).unwrap();
        assert_eq!(encrypted.version, 6);
    }

    #[test]
    fn test_wrong_key_fails() {
        let key = [1u8; 32];
        let wrong_key = [2u8; 32];
        let encrypted = encrypt(&key, b"secret", 1).unwrap();
        assert!(decrypt(&wrong_key, &encrypted).is_err());
    }

    #[test]
    fn test_serialization_roundtrip() {
        let key = [7u8; 32];
        let encrypted = encrypt(&key, b"test data", 1).unwrap();

        let json = serde_json::to_string(&encrypted).unwrap();
        let loaded: EncryptedData = serde_json::from_str(&json).unwrap();

        let decrypted = decrypt(&key, &loaded).unwrap();
        assert_eq!(decrypted, b"test data");
    }
}
