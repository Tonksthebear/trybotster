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
//! ~/.config/botster/
//!     vapid_keys.enc                         # Device-level VAPID keys (AES-GCM)
//!     push_subscriptions.enc                 # Device-level browser push subscriptions (AES-GCM)
//!     hubs/{hub_id}/
//!         vodozemac_store.enc                # AES-GCM encrypted Matrix crypto state
//!
//! OS Keyring (consolidated):
//!     botster/credentials  # Contains crypto_keys[key_id] = base64 AES key
//! ```
//!
//! Rust guideline compliant 2026-02

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::crypto::EncryptedData;
use crate::keyring::Credentials;

use std::sync::{OnceLock, RwLock};

/// Vodozemac crypto format version.
const CRYPTO_VERSION: u8 = 6;

/// Cache for encryption keys to avoid repeated keyring access.
/// Maps hub_id -> encryption key.
fn key_cache() -> &'static RwLock<HashMap<String, [u8; 32]>> {
    static CACHE: OnceLock<RwLock<HashMap<String, [u8; 32]>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
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

// ============================================================================
// Matrix Crypto Persistence
// ============================================================================

use super::olm_crypto::VodozemacCryptoState;

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

    let plaintext = crate::crypto::decrypt(&key, &encrypted)?;
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
    let encrypted = crate::crypto::encrypt(&key, &plaintext, CRYPTO_VERSION)?;

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

// ============================================================================
// VAPID Key Persistence
// ============================================================================

use crate::notifications::vapid::VapidKeys;
use crate::notifications::push::PushSubscriptionStore;

/// VAPID persistence format version.
const VAPID_VERSION: u8 = 1;

/// Push subscription persistence format version.
const PUSH_SUB_VERSION: u8 = 1;

/// Device-level config directory (same as `device.json` location).
///
/// VAPID keys are device-level — shared across all hubs on this CLI.
fn device_state_dir() -> Result<PathBuf> {
    crate::device::Device::config_dir()
}

/// Load VAPID keys from device-level encrypted storage.
///
/// Returns `None` if no keys have been generated yet.
/// Keys are stored in `vapid_keys.enc` alongside `device.json`.
pub fn load_vapid_keys() -> Result<Option<VapidKeys>> {
    let state_dir = device_state_dir()?;
    let keys_path = state_dir.join("vapid_keys.enc");

    if !keys_path.exists() {
        return Ok(None);
    }

    let key = get_or_create_encryption_key("_device_vapid")?;
    let content = fs::read_to_string(&keys_path).context("Failed to read VAPID keys file")?;
    let encrypted: EncryptedData =
        serde_json::from_str(&content).context("Failed to parse VAPID keys file")?;
    let plaintext = crate::crypto::decrypt(&key, &encrypted)?;
    let vapid: VapidKeys =
        serde_json::from_slice(&plaintext).context("Failed to deserialize VAPID keys")?;

    // Migrate legacy DER formats (SEC1/PKCS8) to raw 32-byte scalar.
    // Re-save so migration only happens once.
    let old_priv = vapid.private_key_base64url().to_string();
    let vapid = vapid.migrate_if_needed()?;
    if vapid.private_key_base64url() != old_priv {
        save_vapid_keys(&vapid)?;
    }

    log::info!("Loaded device-level VAPID keys (encrypted)");
    Ok(Some(vapid))
}

/// Save VAPID keys to device-level encrypted storage.
pub fn save_vapid_keys(vapid: &VapidKeys) -> Result<()> {
    let key = get_or_create_encryption_key("_device_vapid")?;
    let state_dir = device_state_dir()?;
    let keys_path = state_dir.join("vapid_keys.enc");

    let plaintext = serde_json::to_vec(vapid).context("Failed to serialize VAPID keys")?;
    let encrypted = crate::crypto::encrypt(&key, &plaintext, VAPID_VERSION)?;
    let content =
        serde_json::to_string_pretty(&encrypted).context("Failed to serialize encrypted VAPID")?;
    fs::write(&keys_path, content).context("Failed to write VAPID keys file")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(&keys_path, perms)
            .context("Failed to set VAPID keys file permissions")?;
    }

    log::debug!("Saved encrypted device-level VAPID keys to {:?}", keys_path);
    Ok(())
}

/// Load VAPID keys from legacy per-hub storage (migration helper).
///
/// Early versions stored VAPID keys per-hub. This loads from the old path
/// so `init_web_push` can migrate them to device-level on first run.
pub fn load_legacy_hub_vapid_keys(hub_id: &str) -> Result<Option<VapidKeys>> {
    let state_dir = hub_state_dir(hub_id)?;
    let keys_path = state_dir.join("vapid_keys.enc");

    if !keys_path.exists() {
        return Ok(None);
    }

    let key = get_or_create_encryption_key(hub_id)?;
    let content = fs::read_to_string(&keys_path).context("Failed to read legacy VAPID keys")?;
    let encrypted: EncryptedData =
        serde_json::from_str(&content).context("Failed to parse legacy VAPID keys")?;
    let plaintext = crate::crypto::decrypt(&key, &encrypted)?;
    let vapid: VapidKeys =
        serde_json::from_slice(&plaintext).context("Failed to deserialize legacy VAPID keys")?;
    let vapid = vapid.migrate_if_needed()?;
    log::info!(
        "Loaded legacy per-hub VAPID keys for hub {}",
        &hub_id[..hub_id.len().min(8)]
    );
    Ok(Some(vapid))
}

/// Load push subscriptions from device-level encrypted storage.
///
/// Push subscriptions are device-level — the same browser subscription is valid
/// for any hub on this CLI (since VAPID keys are device-level too).
/// Returns an empty store if the file doesn't exist yet.
pub fn load_push_subscriptions() -> Result<PushSubscriptionStore> {
    let state_dir = device_state_dir()?;
    let subs_path = state_dir.join("push_subscriptions.enc");

    if !subs_path.exists() {
        return Ok(PushSubscriptionStore::default());
    }

    let key = get_or_create_encryption_key("_device_push")?;
    let content =
        fs::read_to_string(&subs_path).context("Failed to read push subscriptions file")?;
    let encrypted: EncryptedData =
        serde_json::from_str(&content).context("Failed to parse push subscriptions file")?;
    let plaintext = crate::crypto::decrypt(&key, &encrypted)?;
    let store: PushSubscriptionStore =
        serde_json::from_slice(&plaintext).context("Failed to deserialize push subscriptions")?;

    log::info!("Loaded {} device-level push subscription(s)", store.len());
    Ok(store)
}

/// Save push subscriptions to device-level encrypted storage.
pub fn save_push_subscriptions(store: &PushSubscriptionStore) -> Result<()> {
    let key = get_or_create_encryption_key("_device_push")?;
    let state_dir = device_state_dir()?;
    let subs_path = state_dir.join("push_subscriptions.enc");

    let plaintext =
        serde_json::to_vec(store).context("Failed to serialize push subscriptions")?;
    let encrypted = crate::crypto::encrypt(&key, &plaintext, PUSH_SUB_VERSION)?;
    let content = serde_json::to_string_pretty(&encrypted)
        .context("Failed to serialize encrypted push subscriptions")?;
    fs::write(&subs_path, content).context("Failed to write push subscriptions file")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(&subs_path, perms)
            .context("Failed to set push subscriptions file permissions")?;
    }

    log::debug!("Saved encrypted device-level push subscriptions to {:?}", subs_path);
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
#[cfg(test)]
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
        let key = [0u8; 32];
        let plaintext = b"Hello, Matrix crypto!";

        let encrypted = crate::crypto::encrypt(&key, plaintext, CRYPTO_VERSION).unwrap();
        let decrypted = crate::crypto::decrypt(&key, &encrypted).unwrap();

        assert_eq!(decrypted, plaintext);
        assert_eq!(encrypted.version, CRYPTO_VERSION);
    }

    #[test]
    fn test_olm_crypto_store_persistence_roundtrip() {
        let hub_id = "test-hub-matrix-store";

        // Create a test store state with multiple sessions
        let mut state = VodozemacCryptoState::default();
        state.pickled_account = "test_pickled_account".to_string();
        state.hub_id = hub_id.to_string();
        state.pickled_sessions.insert("peer_key_1".to_string(), "pickled_session_1".to_string());
        state.pickled_sessions.insert("peer_key_2".to_string(), "pickled_session_2".to_string());

        // Save
        save_vodozemac_crypto_store(hub_id, &state).unwrap();

        // Load
        let loaded = load_vodozemac_crypto_store(hub_id).unwrap();
        assert_eq!(loaded.hub_id, hub_id);
        assert_eq!(loaded.pickled_account, "test_pickled_account");
        assert_eq!(loaded.pickled_sessions.len(), 2);
        assert_eq!(loaded.pickled_sessions["peer_key_1"], "pickled_session_1");
        assert_eq!(loaded.pickled_sessions["peer_key_2"], "pickled_session_2");

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
