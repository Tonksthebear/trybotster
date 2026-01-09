//! Olm session persistence for surviving CLI restarts.
//!
//! This module handles saving and loading Olm account and session state
//! so that browser connections can survive CLI restarts.
//!
//! # Security
//!
//! Pickles are encrypted at rest using AES-256-GCM with a key stored in the
//! OS keyring (Keychain on macOS, Secret Service on Linux). This follows
//! industry best practice (Signal, Matrix/Element) for protecting E2E
//! encryption session state.
//!
//! # Storage structure
//!
//! ```text
//! ~/.config/botster/hubs/{hub_id}/
//!     olm_account.enc    # AES-GCM encrypted pickle
//!     olm_session.enc    # AES-GCM encrypted pickle
//!
//! OS Keyring:
//!     botster/{hub_id}-pickle-key   # 256-bit AES key
//! ```

// Rust guideline compliant 2025-01

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use keyring::Entry;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

use super::olm::{OlmAccount, OlmSession};

/// Keyring service name (matches device.rs).
const KEYRING_SERVICE: &str = "botster";

/// Nonce size for AES-GCM (96 bits = 12 bytes).
const NONCE_SIZE: usize = 12;

/// Encrypted data format stored on disk.
#[derive(Debug, Serialize, Deserialize)]
struct EncryptedData {
    /// Base64-encoded nonce (12 bytes).
    nonce: String,
    /// Base64-encoded ciphertext.
    ciphertext: String,
    /// Identity key for verification (not secret).
    identity_key: String,
}

/// Check if keyring should be skipped (for testing).
/// Mirrors the logic in device.rs.
fn should_skip_keyring() -> bool {
    #[cfg(test)]
    {
        return true;
    }

    #[cfg(not(test))]
    {
        if std::env::var("BOTSTER_SKIP_KEYRING")
            .map(|v| v == "1" || v.to_lowercase() == "true")
            .unwrap_or(false)
        {
            return true;
        }
        std::env::var("BOTSTER_CONFIG_DIR").is_ok()
    }
}

/// Get the hub state directory for a given hub_identifier.
fn hub_state_dir(hub_id: &str) -> Result<PathBuf> {
    let base_dir = {
        #[cfg(test)]
        {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-config/hubs")
        }

        #[cfg(not(test))]
        {
            if let Ok(custom_dir) = std::env::var("BOTSTER_CONFIG_DIR") {
                PathBuf::from(custom_dir).join("hubs")
            } else {
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

/// Get or create the pickle encryption key for a hub.
fn get_or_create_pickle_key(hub_id: &str) -> Result<[u8; 32]> {
    if should_skip_keyring() {
        // Test mode: use deterministic key derived from hub_id
        let hash = Sha256::digest(format!("test-pickle-key-{hub_id}").as_bytes());
        let mut key = [0u8; 32];
        key.copy_from_slice(&hash[..32]);
        return Ok(key);
    }

    let entry_name = format!("{hub_id}-pickle-key");
    let entry = Entry::new(KEYRING_SERVICE, &entry_name)
        .map_err(|e| anyhow::anyhow!("Failed to create keyring entry: {e:?}"))?;

    // Try to load existing key
    if let Ok(key_b64) = entry.get_password() {
        let key_bytes = BASE64
            .decode(&key_b64)
            .context("Invalid pickle key encoding in keyring")?;
        let key: [u8; 32] = key_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid pickle key length in keyring"))?;
        log::debug!("Loaded pickle key from OS keyring");
        return Ok(key);
    }

    // Generate new key
    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);

    // Store in keyring
    let key_b64 = BASE64.encode(key);
    entry
        .set_password(&key_b64)
        .map_err(|e| anyhow::anyhow!("Failed to store pickle key in keyring: {e:?}"))?;

    log::info!("Generated and stored new pickle key in OS keyring");
    Ok(key)
}

/// Encrypt data using AES-256-GCM.
fn encrypt_data(key: &[u8; 32], plaintext: &[u8], identity_key: &str) -> Result<EncryptedData> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("valid key length");

    // Generate random nonce
    let mut nonce_bytes = [0u8; NONCE_SIZE];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    // Encrypt
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("Encryption failed: {e}"))?;

    Ok(EncryptedData {
        nonce: BASE64.encode(nonce_bytes),
        ciphertext: BASE64.encode(ciphertext),
        identity_key: identity_key.to_string(),
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

/// Load or create an Olm account for a hub.
///
/// If an account exists on disk, loads and decrypts it. Otherwise creates
/// a new one and saves it immediately (encrypted).
pub fn load_or_create_account(hub_id: &str) -> Result<OlmAccount> {
    let state_dir = hub_state_dir(hub_id)?;
    let account_path = state_dir.join("olm_account.enc");

    if account_path.exists() {
        load_account(hub_id, &account_path)
    } else {
        let account = OlmAccount::new();
        save_account(hub_id, &account)?;
        log::info!("Created new Olm account for hub {}", &hub_id[..8]);
        Ok(account)
    }
}

/// Load an Olm account from encrypted storage.
fn load_account(hub_id: &str, path: &PathBuf) -> Result<OlmAccount> {
    let key = get_or_create_pickle_key(hub_id)?;

    let content = fs::read_to_string(path).context("Failed to read Olm account file")?;
    let encrypted: EncryptedData =
        serde_json::from_str(&content).context("Failed to parse Olm account file")?;

    let pickle_bytes = decrypt_data(&key, &encrypted)?;
    let pickle =
        String::from_utf8(pickle_bytes).context("Invalid UTF-8 in decrypted account pickle")?;

    let account = OlmAccount::from_pickle(&pickle)?;

    // Verify the loaded account matches stored identity
    if account.curve25519_key() != encrypted.identity_key {
        anyhow::bail!("Olm account identity mismatch - file may be corrupted");
    }

    log::info!(
        "Loaded Olm account (encrypted): curve25519={}...",
        &encrypted.identity_key[..8]
    );
    Ok(account)
}

/// Save an Olm account to encrypted storage.
pub fn save_account(hub_id: &str, account: &OlmAccount) -> Result<()> {
    let key = get_or_create_pickle_key(hub_id)?;
    let state_dir = hub_state_dir(hub_id)?;
    let account_path = state_dir.join("olm_account.enc");

    let pickle = account.pickle();
    let encrypted = encrypt_data(&key, pickle.as_bytes(), &account.curve25519_key())?;

    let content =
        serde_json::to_string_pretty(&encrypted).context("Failed to serialize encrypted account")?;

    fs::write(&account_path, content).context("Failed to write Olm account file")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(&account_path, perms)
            .context("Failed to set Olm account file permissions")?;
    }

    log::debug!("Saved encrypted Olm account to {:?}", account_path);
    Ok(())
}

/// Load an Olm session from encrypted storage, if it exists.
pub fn load_session(hub_id: &str) -> Result<Option<OlmSession>> {
    let state_dir = hub_state_dir(hub_id)?;
    let session_path = state_dir.join("olm_session.enc");

    if !session_path.exists() {
        return Ok(None);
    }

    let key = get_or_create_pickle_key(hub_id)?;

    let content = fs::read_to_string(&session_path).context("Failed to read Olm session file")?;
    let encrypted: EncryptedData =
        serde_json::from_str(&content).context("Failed to parse Olm session file")?;

    let pickle_bytes = decrypt_data(&key, &encrypted)?;
    let pickle =
        String::from_utf8(pickle_bytes).context("Invalid UTF-8 in decrypted session pickle")?;

    // The identity_key field stores the peer's curve25519 key for sessions
    let session = OlmSession::from_pickle(&pickle, encrypted.identity_key.clone())?;

    log::info!(
        "Loaded Olm session (encrypted): peer={}...",
        &encrypted.identity_key[..8]
    );
    Ok(Some(session))
}

/// Save an Olm session to encrypted storage.
pub fn save_session(hub_id: &str, session: &OlmSession) -> Result<()> {
    let key = get_or_create_pickle_key(hub_id)?;
    let state_dir = hub_state_dir(hub_id)?;
    let session_path = state_dir.join("olm_session.enc");

    let pickle = session.pickle();
    let encrypted = encrypt_data(&key, pickle.as_bytes(), session.peer_curve25519())?;

    let content =
        serde_json::to_string_pretty(&encrypted).context("Failed to serialize encrypted session")?;

    fs::write(&session_path, content).context("Failed to write Olm session file")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(&session_path, perms)
            .context("Failed to set Olm session file permissions")?;
    }

    log::debug!("Saved encrypted Olm session to {:?}", session_path);
    Ok(())
}

/// Delete the Olm session file (e.g., when session becomes invalid).
pub fn delete_session(hub_id: &str) -> Result<()> {
    let state_dir = hub_state_dir(hub_id)?;
    let session_path = state_dir.join("olm_session.enc");

    if session_path.exists() {
        fs::remove_file(&session_path).context("Failed to delete Olm session file")?;
        log::info!("Deleted Olm session file");
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
        let plaintext = b"Hello, World!";
        let identity = "test-identity";

        let encrypted = encrypt_data(&key, plaintext, identity).unwrap();
        let decrypted = decrypt_data(&key, &encrypted).unwrap();

        assert_eq!(decrypted, plaintext);
        assert_eq!(encrypted.identity_key, identity);
    }

    #[test]
    fn test_account_persistence_roundtrip() {
        let hub_id = "test-hub-account";

        // Create and save account
        let account = OlmAccount::new();
        let original_key = account.curve25519_key();
        save_account(hub_id, &account).unwrap();

        // Load account
        let loaded = load_or_create_account(hub_id).unwrap();
        assert_eq!(loaded.curve25519_key(), original_key);

        // Cleanup
        let state_dir = hub_state_dir(hub_id).unwrap();
        let _ = fs::remove_dir_all(state_dir);
    }

    #[test]
    fn test_session_persistence_roundtrip() {
        use vodozemac::olm::OlmMessage;

        let hub_id = "test-hub-session";

        // Create CLI account and generate keys
        let mut cli_account = OlmAccount::new();
        cli_account.generate_one_time_keys(1);
        let cli_identity = cli_account.curve25519_key();
        let cli_otk = cli_account.get_one_time_key().unwrap();

        // Create browser account
        let browser_account = OlmAccount::new();
        let browser_identity = browser_account.curve25519_key();

        // Browser creates outbound session (using test-only method)
        let mut browser_session = browser_account
            .create_outbound_session(&cli_identity, &cli_otk)
            .unwrap();

        // Browser encrypts to create PreKey message
        let prekey_msg = browser_session.encrypt(b"hello");
        let (message_type, ciphertext) = match prekey_msg {
            OlmMessage::PreKey(m) => (0u8, BASE64.encode(m.to_bytes())),
            OlmMessage::Normal(m) => (1u8, BASE64.encode(m.to_bytes())),
        };

        let envelope = super::super::olm::OlmEnvelope {
            version: 3,
            message_type,
            ciphertext,
            sender_key: browser_identity.clone(),
        };

        // CLI creates inbound session
        let (cli_session, _plaintext) = cli_account
            .create_inbound_session(&browser_identity, &envelope)
            .unwrap();

        // Save CLI's session
        save_session(hub_id, &cli_session).unwrap();

        // Load session
        let loaded = load_session(hub_id).unwrap();
        assert!(loaded.is_some());
        let loaded_session = loaded.unwrap();
        assert_eq!(loaded_session.peer_curve25519(), browser_identity);

        // Cleanup
        let state_dir = hub_state_dir(hub_id).unwrap();
        let _ = fs::remove_dir_all(state_dir);
    }
}
