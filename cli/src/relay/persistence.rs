//! Signal Protocol session persistence for surviving CLI restarts.
//!
//! This module handles saving and loading Signal Protocol store state
//! so that browser connections can survive CLI restarts.
//!
//! # Security
//!
//! All data is encrypted at rest using AES-256-GCM with a key stored in the
//! consolidated keyring entry. This follows industry best practice
//! (Signal, Matrix/Element) for protecting E2E encryption session state.
//!
//! # Storage structure
//!
//! ```text
//! ~/.config/botster/hubs/{hub_id}/
//!     signal_store.enc    # AES-GCM encrypted store state
//!
//! OS Keyring (consolidated):
//!     botster/credentials  # Contains signal_keys[hub_id] = base64 AES key
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

/// Serializable Signal Protocol store state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalStoreState {
    /// Identity key pair (serialized).
    pub identity_key_pair: Vec<u8>,
    /// Registration ID.
    pub registration_id: u32,
    /// Known identities (address -> identity key bytes).
    pub identities: HashMap<String, Vec<u8>>,
    /// Sessions (address -> session record bytes).
    pub sessions: HashMap<String, Vec<u8>>,
    /// PreKeys (id -> record bytes).
    pub pre_keys: HashMap<u32, Vec<u8>>,
    /// Signed PreKeys (id -> record bytes).
    pub signed_pre_keys: HashMap<u32, Vec<u8>>,
    /// Kyber PreKeys (id -> record bytes).
    pub kyber_pre_keys: HashMap<u32, Vec<u8>>,
    /// Sender keys (composite key -> record bytes).
    pub sender_keys: HashMap<String, Vec<u8>>,
    /// Used PreKey IDs (for tracking consumption).
    pub used_pre_keys: Vec<u32>,
}

/// Check if we're in test mode (for deterministic key generation).
fn is_test_mode() -> bool {
    #[cfg(test)]
    {
        return true;
    }

    #[cfg(not(test))]
    {
        crate::env::is_test_mode()
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
            } else if crate::env::is_test_mode() {
                // Integration tests (BOTSTER_ENV=test): use repo's tmp/ directory
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

/// Generate a stable hub_identifier from a repo path.
///
/// Uses SHA256 hash of the absolute path to ensure the same repo
/// always gets the same hub_id, even across CLI restarts.
#[must_use]
pub fn hub_id_for_repo(repo_path: &std::path::Path) -> String {
    let canonical = repo_path
        .canonicalize()
        .unwrap_or_else(|_| repo_path.to_path_buf());

    let hash = Sha256::digest(canonical.to_string_lossy().as_bytes());

    // Use first 16 bytes as hex (32 chars) - enough uniqueness, shorter than UUID
    hash[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Get or create the encryption key for a hub.
///
/// Keys are cached in memory after the first load to avoid repeated keyring access.
/// This is important on macOS where excessive keychain access can cause issues.
fn get_or_create_encryption_key(hub_id: &str) -> Result<[u8; 32]> {
    if is_test_mode() {
        // Test mode: use deterministic key derived from hub_id
        let hash = Sha256::digest(format!("test-signal-key-{hub_id}").as_bytes());
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
    let key = if let Some(key_b64) = creds.signal_key(hub_id) {
        let key_bytes = BASE64
            .decode(key_b64)
            .context("Invalid encryption key encoding in credentials")?;
        let key: [u8; 32] = key_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid encryption key length in credentials"))?;
        log::debug!("Loaded Signal encryption key from consolidated credentials");
        key
    } else {
        // Generate new key
        let mut key = [0u8; 32];
        rand::rng().fill_bytes(&mut key);

        // Store in consolidated credentials
        let key_b64 = BASE64.encode(key);
        creds.set_signal_key(hub_id.to_string(), key_b64);
        creds.save()?;

        log::info!("Generated and stored new Signal encryption key in consolidated credentials");
        key
    };

    // Cache the key
    {
        let mut cache = key_cache().write().expect("key cache lock poisoned");
        cache.insert(hub_id.to_string(), key);
    }

    Ok(key)
}

/// Encrypt data using AES-256-GCM.
fn encrypt_data(key: &[u8; 32], plaintext: &[u8]) -> Result<EncryptedData> {
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
        version: 4, // Signal Protocol version
    })
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

/// Load a Signal Protocol store from encrypted storage.
pub fn load_signal_store(hub_id: &str) -> Result<SignalStoreState> {
    let state_dir = hub_state_dir(hub_id)?;
    let store_path = state_dir.join("signal_store.enc");

    if !store_path.exists() {
        anyhow::bail!("Signal store not found for hub {}", &hub_id[..hub_id.len().min(8)]);
    }

    let key = get_or_create_encryption_key(hub_id)?;

    let content = fs::read_to_string(&store_path).context("Failed to read Signal store file")?;
    let encrypted: EncryptedData =
        serde_json::from_str(&content).context("Failed to parse Signal store file")?;

    let plaintext = decrypt_data(&key, &encrypted)?;
    let state: SignalStoreState =
        serde_json::from_slice(&plaintext).context("Failed to deserialize Signal store")?;

    log::info!(
        "Loaded Signal store (encrypted) for hub {}",
        &hub_id[..hub_id.len().min(8)]
    );
    Ok(state)
}

/// Save a Signal Protocol store to encrypted storage.
pub fn save_signal_store(hub_id: &str, state: &SignalStoreState) -> Result<()> {
    let key = get_or_create_encryption_key(hub_id)?;
    let state_dir = hub_state_dir(hub_id)?;
    let store_path = state_dir.join("signal_store.enc");

    let plaintext = serde_json::to_vec(state).context("Failed to serialize Signal store")?;
    let encrypted = encrypt_data(&key, &plaintext)?;

    let content =
        serde_json::to_string_pretty(&encrypted).context("Failed to serialize encrypted store")?;

    fs::write(&store_path, content).context("Failed to write Signal store file")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(&store_path, perms)
            .context("Failed to set Signal store file permissions")?;
    }

    log::debug!("Saved encrypted Signal store to {:?}", store_path);
    Ok(())
}

/// Delete all Signal state for a hub.
pub fn delete_signal_store(hub_id: &str) -> Result<()> {
    let state_dir = hub_state_dir(hub_id)?;
    let store_path = state_dir.join("signal_store.enc");

    if store_path.exists() {
        fs::remove_file(&store_path).context("Failed to delete Signal store file")?;
        log::info!("Deleted Signal store file");
    }

    Ok(())
}

/// Check if a Signal store exists for a hub.
pub fn signal_store_exists(hub_id: &str) -> bool {
    hub_state_dir(hub_id)
        .map(|dir| dir.join("signal_store.enc").exists())
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
        let id1 = hub_id_for_repo(path);
        let id2 = hub_id_for_repo(path);
        assert_eq!(id1, id2);
        assert_eq!(id1.len(), 32); // 16 bytes as hex
    }

    #[test]
    fn test_hub_id_for_repo_differs_by_path() {
        let path1 = std::path::Path::new("/tmp/repo-a");
        let path2 = std::path::Path::new("/tmp/repo-b");
        let id1 = hub_id_for_repo(path1);
        let id2 = hub_id_for_repo(path2);
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = [0u8; 32]; // Test key
        let plaintext = b"Hello, Signal Protocol!";

        let encrypted = encrypt_data(&key, plaintext).unwrap();
        let decrypted = decrypt_data(&key, &encrypted).unwrap();

        assert_eq!(decrypted, plaintext);
        assert_eq!(encrypted.version, 4);
    }

    #[test]
    fn test_signal_store_persistence_roundtrip() {
        let hub_id = "test-hub-signal-store";

        // Create a test store state
        let state = SignalStoreState {
            identity_key_pair: vec![1, 2, 3, 4],
            registration_id: 12345,
            identities: HashMap::new(),
            sessions: HashMap::new(),
            pre_keys: HashMap::new(),
            signed_pre_keys: HashMap::new(),
            kyber_pre_keys: HashMap::new(),
            sender_keys: HashMap::new(),
            used_pre_keys: vec![],
        };

        // Save
        save_signal_store(hub_id, &state).unwrap();

        // Load
        let loaded = load_signal_store(hub_id).unwrap();
        assert_eq!(loaded.registration_id, 12345);
        assert_eq!(loaded.identity_key_pair, vec![1, 2, 3, 4]);

        // Cleanup
        let state_dir = hub_state_dir(hub_id).unwrap();
        let _ = fs::remove_dir_all(state_dir);
    }
}
