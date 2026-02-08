//! Crypto session persistence for surviving CLI restarts.
//!
//! This module handles saving and loading Matrix crypto store state
//! so that browser connections can survive CLI restarts.
//!
//! # Security
//!
//! All data is encrypted at rest using AES-256-GCM with a key stored in the
//! consolidated keyring entry. This follows industry best practice
//! (Matrix/Element) for protecting E2E encryption session state.
//!
//! # Storage structure
//!
//! ```text
//! ~/.config/botster/hubs/{hub_id}/
//!     vodozemac_store.enc    # AES-GCM encrypted Matrix crypto state
//!
//! OS Keyring (consolidated):
//!     botster/credentials  # Contains crypto_keys[hub_id] = base64 AES key
//! ```
//!
//! Rust guideline compliant 2025-01

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::keyring::Credentials;

use std::sync::{OnceLock, RwLock};

/// Nonce size for AES-GCM (96 bits = 12 bytes).
const NONCE_SIZE: usize = 12;

/// Cache for encryption keys to avoid repeated keyring access.
/// Maps hub_id -> encryption key.
fn key_cache() -> &'static RwLock<HashMap<String, [u8; 32]>> {
    static CACHE: OnceLock<RwLock<HashMap<String, [u8; 32]>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Encrypted data format stored on disk.
#[derive(Debug, Serialize, Deserialize)]
struct EncryptedData {
    /// Base64-encoded nonce (12 bytes).
    nonce: String,
    /// Base64-encoded ciphertext.
    ciphertext: String,
    /// Version identifier for the encrypted data format.
    version: u8,
}

/// Check if we're in test mode (for deterministic key generation).
/// Returns true for both BOTSTER_ENV=test and BOTSTER_ENV=system_test.
fn is_test_mode() -> bool {
    #[cfg(test)]
    {
        return true;
    }

    #[cfg(not(test))]
    {
        crate::env::should_skip_keyring()
    }
}

/// Get the hub state directory for a given hub_identifier.
///
/// Directory selection priority:
/// 1. `#[cfg(test)]` (unit tests): `tmp/botster-test/hubs`
/// 2. `BOTSTER_CONFIG_DIR` env var: `{custom_dir}/hubs`
/// 3. `BOTSTER_ENV=test`: `tmp/botster-test/hubs` (integration tests)
/// 4. Default: system config directory (e.g., `~/Library/Application Support/botster/hubs`)
fn hub_state_dir(hub_id: &str) -> Result<PathBuf> {
    let base_dir = {
        #[cfg(test)]
        {
            // Use repo's tmp/ directory (already gitignored)
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("cli/ has parent directory")
                .join("tmp/botster-test/hubs")
        }

        #[cfg(not(test))]
        {
            if let Ok(custom_dir) = std::env::var("BOTSTER_CONFIG_DIR") {
                // Explicit override via env var
                PathBuf::from(custom_dir).join("hubs")
            } else if crate::env::should_skip_keyring() {
                // Integration/system tests (BOTSTER_ENV=test or system_test): use repo's tmp/ directory
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .expect("cli/ has parent directory")
                    .join("tmp/botster-test/hubs")
            } else {
                // Production: use system config directory
                dirs::config_dir()
                    .context("Could not determine config directory")?
                    .join("botster")
                    .join("hubs")
            }
        }
    };

    let config_dir = base_dir.join(hub_id);
    fs::create_dir_all(&config_dir).context("Failed to create hub state directory")?;

    Ok(config_dir)
}

/// Get or create the encryption key for a hub.
///
/// Keys are cached in memory after the first load to avoid repeated keyring access.
/// This is important on macOS where excessive keychain access can cause issues.
fn get_or_create_encryption_key(hub_id: &str) -> Result<[u8; 32]> {
    if is_test_mode() {
        // Test mode: use deterministic key derived from hub_id
        let hash = Sha256::digest(format!("test-crypto-key-{hub_id}").as_bytes());
        let mut key = [0u8; 32];
        key.copy_from_slice(&hash[..32]);
        return Ok(key);
    }

    // Check cache first
    {
        let cache = key_cache().read().expect("key cache lock poisoned");
        if let Some(key) = cache.get(hub_id) {
            return Ok(*key);
        }
    }

    // Load from keyring (cache miss)
    let mut creds = Credentials::load().unwrap_or_default();

    // Try to load existing key for this hub
    let key = if let Some(key_b64) = creds.crypto_key(hub_id) {
        let key_bytes = BASE64
            .decode(key_b64)
            .context("Invalid encryption key encoding in credentials")?;
        let key: [u8; 32] = key_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid encryption key length in credentials"))?;
        log::debug!("Loaded encryption key from consolidated credentials");
        key
    } else {
        // Generate new key
        let mut key = [0u8; 32];
        rand::rng().fill_bytes(&mut key);

        // Store in consolidated credentials
        let key_b64 = BASE64.encode(key);
        creds.set_crypto_key(hub_id.to_string(), key_b64);
        creds.save()?;

        log::info!("Generated and stored new encryption key in consolidated credentials");
        key
    };

    // Cache the key
    {
        let mut cache = key_cache().write().expect("key cache lock poisoned");
        cache.insert(hub_id.to_string(), key);
    }

    Ok(key)
}

/// Decrypt data using AES-256-GCM.
fn decrypt_data(key: &[u8; 32], encrypted: &EncryptedData) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("valid key length");

    let nonce_bytes = BASE64
        .decode(&encrypted.nonce)
        .context("Invalid nonce encoding")?;
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = BASE64
        .decode(&encrypted.ciphertext)
        .context("Invalid ciphertext encoding")?;

    let plaintext = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|e| anyhow::anyhow!("Decryption failed: {e}"))?;

    Ok(plaintext)
}

// ============================================================================
// Matrix Crypto Persistence
// ============================================================================

use super::olm_crypto::VodozemacCryptoState;

/// Encrypt data using AES-256-GCM with version marker.
fn encrypt_data_versioned(key: &[u8; 32], plaintext: &[u8]) -> Result<EncryptedData> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("valid key length");

    // Generate random nonce
    let mut nonce_bytes = [0u8; NONCE_SIZE];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    // Encrypt
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("Encryption failed: {e}"))?;

    Ok(EncryptedData {
        nonce: BASE64.encode(nonce_bytes),
        ciphertext: BASE64.encode(ciphertext),
        version: 6, // vodozemac crypto version
    })
}

/// Load a vodozemac crypto store from encrypted storage.
pub fn load_vodozemac_crypto_store(hub_id: &str) -> Result<VodozemacCryptoState> {
    let state_dir = hub_state_dir(hub_id)?;
    let store_path = state_dir.join("vodozemac_store.enc");

    if !store_path.exists() {
        anyhow::bail!(
            "Matrix crypto store not found for hub {}",
            &hub_id[..hub_id.len().min(8)]
        );
    }

    let key = get_or_create_encryption_key(hub_id)?;

    let content = fs::read_to_string(&store_path).context("Failed to read Matrix store file")?;
    let encrypted: EncryptedData =
        serde_json::from_str(&content).context("Failed to parse Matrix store file")?;

    let plaintext = decrypt_data(&key, &encrypted)?;
    let state: VodozemacCryptoState =
        serde_json::from_slice(&plaintext).context("Failed to deserialize Matrix store")?;

    log::info!(
        "Loaded Matrix crypto store (encrypted) for hub {}",
        &hub_id[..hub_id.len().min(8)]
    );
    Ok(state)
}

/// Save a vodozemac crypto store to encrypted storage.
pub fn save_vodozemac_crypto_store(hub_id: &str, state: &VodozemacCryptoState) -> Result<()> {
    let key = get_or_create_encryption_key(hub_id)?;
    let state_dir = hub_state_dir(hub_id)?;
    let store_path = state_dir.join("vodozemac_store.enc");

    let plaintext = serde_json::to_vec(state).context("Failed to serialize Matrix store")?;
    let encrypted = encrypt_data_versioned(&key, &plaintext)?;

    let content =
        serde_json::to_string_pretty(&encrypted).context("Failed to serialize encrypted store")?;

    fs::write(&store_path, content).context("Failed to write Matrix store file")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(&store_path, perms)
            .context("Failed to set Matrix store file permissions")?;
    }

    log::debug!("Saved encrypted Matrix store to {:?}", store_path);
    Ok(())
}

/// Delete all vodozemac crypto state for a hub.
pub fn delete_vodozemac_crypto_store(hub_id: &str) -> Result<()> {
    let state_dir = hub_state_dir(hub_id)?;
    let store_path = state_dir.join("vodozemac_store.enc");

    if store_path.exists() {
        fs::remove_file(&store_path).context("Failed to delete Matrix store file")?;
        log::info!("Deleted Matrix crypto store file");
    }

    Ok(())
}

/// Check if a vodozemac crypto store exists for a hub.
pub(crate) fn vodozemac_crypto_store_exists(hub_id: &str) -> bool {
    hub_state_dir(hub_id)
        .map(|dir| dir.join("vodozemac_store.enc").exists())
        .unwrap_or(false)
}

/// Write the connection URL to a file for external access.
///
/// This allows external tools (like test harnesses) to retrieve the
/// connection URL from a running CLI instance.
pub fn write_connection_url(hub_id: &str, url: &str) -> Result<()> {
    let state_dir = hub_state_dir(hub_id)?;
    let url_path = state_dir.join("connection_url.txt");
    fs::write(&url_path, url).context("Failed to write connection URL")?;
    log::debug!("Wrote connection URL to {:?}", url_path);
    Ok(())
}

/// Read the connection URL from file.
///
/// Returns None if the file doesn't exist (CLI not running or not connected).
pub fn read_connection_url(hub_id: &str) -> Result<Option<String>> {
    let state_dir = hub_state_dir(hub_id)?;
    let url_path = state_dir.join("connection_url.txt");

    if !url_path.exists() {
        return Ok(None);
    }

    let url = fs::read_to_string(&url_path).context("Failed to read connection URL")?;
    Ok(Some(url.trim().to_string()))
}

/// Delete the connection URL file (called on CLI shutdown).
pub fn delete_connection_url(hub_id: &str) -> Result<()> {
    let state_dir = hub_state_dir(hub_id)?;
    let url_path = state_dir.join("connection_url.txt");

    if url_path.exists() {
        fs::remove_file(&url_path).context("Failed to delete connection URL file")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hub_id_for_repo_is_stable() {
        let path = std::path::Path::new("/tmp/test-repo");
        let id1 = crate::hub::hub_id_for_repo(path);
        let id2 = crate::hub::hub_id_for_repo(path);
        assert_eq!(id1, id2);
        assert_eq!(id1.len(), 32); // 16 bytes as hex
    }

    #[test]
    fn test_hub_id_for_repo_differs_by_path() {
        let path1 = std::path::Path::new("/tmp/repo-a");
        let path2 = std::path::Path::new("/tmp/repo-b");
        let id1 = crate::hub::hub_id_for_repo(path1);
        let id2 = crate::hub::hub_id_for_repo(path2);
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = [0u8; 32]; // Test key
        let plaintext = b"Hello, Matrix crypto!";

        let encrypted = encrypt_data_versioned(&key, plaintext).unwrap();
        let decrypted = decrypt_data(&key, &encrypted).unwrap();

        assert_eq!(decrypted, plaintext);
        assert_eq!(encrypted.version, 6);
    }

    #[test]
    fn test_olm_crypto_store_persistence_roundtrip() {
        let hub_id = "test-hub-matrix-store";

        // Create a test store state
        let state = VodozemacCryptoState {
            pickled_account: "test_pickled_account".to_string(),
            hub_id: hub_id.to_string(),
            pickled_session: None,
            peer_identity_key: Some("test_peer_key".to_string()),
        };

        // Save
        save_vodozemac_crypto_store(hub_id, &state).unwrap();

        // Load
        let loaded = load_vodozemac_crypto_store(hub_id).unwrap();
        assert_eq!(loaded.hub_id, hub_id);
        assert_eq!(loaded.pickled_account, "test_pickled_account");
        assert_eq!(loaded.peer_identity_key, Some("test_peer_key".to_string()));

        // Cleanup
        let state_dir = hub_state_dir(hub_id).unwrap();
        let _ = fs::remove_dir_all(state_dir);
    }

    #[test]
    fn test_vodozemac_crypto_store_exists() {
        let hub_id = "test-hub-matrix-exists";

        // Should not exist initially
        assert!(!vodozemac_crypto_store_exists(hub_id));

        // Create and save
        let state = VodozemacCryptoState::default();
        save_vodozemac_crypto_store(hub_id, &state).unwrap();

        // Should exist now
        assert!(vodozemac_crypto_store_exists(hub_id));

        // Cleanup
        let state_dir = hub_state_dir(hub_id).unwrap();
        let _ = fs::remove_dir_all(state_dir);
    }
}
