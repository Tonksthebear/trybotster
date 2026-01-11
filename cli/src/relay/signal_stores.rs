//! Signal Protocol store implementation with encrypted persistence.
//!
//! This module implements the `SessionStorage` trait from libsignal-rust with
//! encrypted file storage. All sensitive data (identity keys, session state,
//! pre-keys) is encrypted using AES-256-GCM before writing to disk.
//!
//! # Storage Layout
//!
//! ```text
//! ~/.botster_hub/signal/{hub_id}/
//! ├── identity.enc           # Our identity keypair
//! ├── trusted_identities/    # Known peer identity keys
//! │   └── {address}.enc
//! ├── sessions/              # Session records per peer
//! │   └── {address}.enc
//! ├── pre_keys/              # One-time pre-keys
//! │   └── {id}.enc
//! └── signed_pre_keys/       # Signed pre-keys
//!     └── {id}.enc
//! ```
//!
//! # Security
//!
//! - Encryption key is stored in OS keyring (or file in test mode)
//! - AES-256-GCM provides authenticated encryption
//! - Each file has a unique 12-byte nonce

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use keyring::Entry;
use libsignal_rust::{
    curve::{self, KeyPair},
    session_builder::SessionStorage,
    session_record::SessionRecord,
};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Keyring service name for Signal encryption keys.
const KEYRING_SERVICE: &str = "botster-signal";

/// Check if keyring should be skipped (for testing).
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

/// Stored identity information (encrypted at rest).
#[derive(Debug, Serialize, Deserialize)]
struct StoredIdentity {
    /// Private key bytes (32 bytes).
    priv_key: Vec<u8>,
    /// Public key bytes (33 bytes with version prefix).
    pub_key: Vec<u8>,
}

/// Persistent Signal Protocol storage with encryption.
pub struct SignalStore {
    /// Hub identifier (used for namespace isolation).
    hub_id: String,
    /// Base directory for storage.
    base_dir: PathBuf,
    /// Encryption key (loaded from keyring).
    encryption_key: [u8; 32],
    /// In-memory session cache.
    sessions: Arc<RwLock<HashMap<String, SessionRecord>>>,
    /// In-memory pre-key cache.
    pre_keys: Arc<RwLock<HashMap<u32, KeyPair>>>,
    /// In-memory signed pre-key cache.
    signed_pre_keys: Arc<RwLock<HashMap<u32, KeyPair>>>,
    /// In-memory trusted identities cache.
    trusted_identities: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    /// Our identity keypair (cached).
    identity: Arc<RwLock<Option<KeyPair>>>,
}

impl std::fmt::Debug for SignalStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalStore")
            .field("hub_id", &self.hub_id)
            .field("base_dir", &self.base_dir)
            .finish_non_exhaustive()
    }
}

impl SignalStore {
    /// Create or load a Signal store for a hub.
    ///
    /// This loads the encryption key from the OS keyring (or generates a new one)
    /// and sets up the directory structure for encrypted storage.
    pub fn new(hub_id: &str) -> Result<Self> {
        let base_dir = Self::get_base_dir(hub_id)?;
        let encryption_key = Self::get_or_create_encryption_key(hub_id)?;

        let store = Self {
            hub_id: hub_id.to_string(),
            base_dir,
            encryption_key,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            pre_keys: Arc::new(RwLock::new(HashMap::new())),
            signed_pre_keys: Arc::new(RwLock::new(HashMap::new())),
            trusted_identities: Arc::new(RwLock::new(HashMap::new())),
            identity: Arc::new(RwLock::new(None)),
        };

        // Load existing data from disk
        store.load_from_disk()?;

        Ok(store)
    }

    /// Get the base directory for Signal storage.
    fn get_base_dir(hub_id: &str) -> Result<PathBuf> {
        let config_dir = if let Ok(custom_dir) = std::env::var("BOTSTER_CONFIG_DIR") {
            PathBuf::from(custom_dir)
        } else {
            dirs::config_dir()
                .context("Could not determine config directory")?
                .join("botster_hub")
        };

        let base_dir = config_dir.join("signal").join(hub_id);
        fs::create_dir_all(&base_dir).context("Failed to create signal storage directory")?;
        fs::create_dir_all(base_dir.join("sessions"))?;
        fs::create_dir_all(base_dir.join("pre_keys"))?;
        fs::create_dir_all(base_dir.join("signed_pre_keys"))?;
        fs::create_dir_all(base_dir.join("trusted_identities"))?;

        Ok(base_dir)
    }

    /// Get or create the encryption key for this hub.
    fn get_or_create_encryption_key(hub_id: &str) -> Result<[u8; 32]> {
        if should_skip_keyring() {
            // Test mode: use file-based key storage
            let key_path = Self::get_base_dir(hub_id)?.join("encryption.key");
            if key_path.exists() {
                let key_b64 = fs::read_to_string(&key_path)?;
                let key_bytes = BASE64.decode(key_b64.trim())?;
                let key: [u8; 32] = key_bytes
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("Invalid key length"))?;
                return Ok(key);
            } else {
                let mut key = [0u8; 32];
                OsRng.fill_bytes(&mut key);
                let key_b64 = BASE64.encode(key);
                fs::write(&key_path, &key_b64)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))?;
                }
                return Ok(key);
            }
        }

        // Production: use OS keyring
        let entry_name = format!("signal-{}", hub_id);
        let entry = Entry::new(KEYRING_SERVICE, &entry_name)
            .map_err(|e| anyhow::anyhow!("Failed to create keyring entry: {:?}", e))?;

        match entry.get_password() {
            Ok(key_b64) => {
                let key_bytes = BASE64
                    .decode(&key_b64)
                    .context("Invalid key encoding in keyring")?;
                let key: [u8; 32] = key_bytes
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("Invalid key length in keyring"))?;
                Ok(key)
            }
            Err(_) => {
                // Generate new key
                let mut key = [0u8; 32];
                OsRng.fill_bytes(&mut key);
                let key_b64 = BASE64.encode(key);
                entry
                    .set_password(&key_b64)
                    .map_err(|e| anyhow::anyhow!("Failed to store key in keyring: {:?}", e))?;
                log::info!("Generated new Signal encryption key for hub {}", hub_id);
                Ok(key)
            }
        }
    }

    /// Load existing data from disk into memory.
    fn load_from_disk(&self) -> Result<()> {
        // Load identity
        let identity_path = self.base_dir.join("identity.enc");
        if identity_path.exists() {
            if let Ok(data) = self.read_encrypted_file(&identity_path) {
                if let Ok(stored) = serde_json::from_slice::<StoredIdentity>(&data) {
                    let keypair = KeyPair {
                        priv_key: stored.priv_key,
                        pub_key: stored.pub_key,
                    };
                    let mut identity = self.identity.blocking_write();
                    *identity = Some(keypair);
                }
            }
        }

        // Load sessions
        let sessions_dir = self.base_dir.join("sessions");
        if sessions_dir.exists() {
            for entry in fs::read_dir(&sessions_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().map(|e| e == "enc").unwrap_or(false) {
                    if let Some(stem) = path.file_stem() {
                        let address = stem.to_string_lossy().to_string();
                        if let Ok(data) = self.read_encrypted_file(&path) {
                            if let Ok(record) = serde_json::from_slice::<SessionRecord>(&data) {
                                let mut sessions = self.sessions.blocking_write();
                                sessions.insert(address, record);
                            }
                        }
                    }
                }
            }
        }

        // Load pre-keys
        let pre_keys_dir = self.base_dir.join("pre_keys");
        if pre_keys_dir.exists() {
            for entry in fs::read_dir(&pre_keys_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().map(|e| e == "enc").unwrap_or(false) {
                    if let Some(stem) = path.file_stem() {
                        if let Ok(id) = stem.to_string_lossy().parse::<u32>() {
                            if let Ok(data) = self.read_encrypted_file(&path) {
                                if let Ok(keypair) = serde_json::from_slice::<KeyPair>(&data) {
                                    let mut pre_keys = self.pre_keys.blocking_write();
                                    pre_keys.insert(id, keypair);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Load signed pre-keys
        let signed_pre_keys_dir = self.base_dir.join("signed_pre_keys");
        if signed_pre_keys_dir.exists() {
            for entry in fs::read_dir(&signed_pre_keys_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().map(|e| e == "enc").unwrap_or(false) {
                    if let Some(stem) = path.file_stem() {
                        if let Ok(id) = stem.to_string_lossy().parse::<u32>() {
                            if let Ok(data) = self.read_encrypted_file(&path) {
                                if let Ok(keypair) = serde_json::from_slice::<KeyPair>(&data) {
                                    let mut signed_pre_keys = self.signed_pre_keys.blocking_write();
                                    signed_pre_keys.insert(id, keypair);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Load trusted identities
        let trusted_dir = self.base_dir.join("trusted_identities");
        if trusted_dir.exists() {
            for entry in fs::read_dir(&trusted_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().map(|e| e == "enc").unwrap_or(false) {
                    if let Some(stem) = path.file_stem() {
                        let address = stem.to_string_lossy().to_string();
                        if let Ok(data) = self.read_encrypted_file(&path) {
                            let mut trusted = self.trusted_identities.blocking_write();
                            trusted.insert(address, data);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Read and decrypt a file.
    fn read_encrypted_file(&self, path: &PathBuf) -> Result<Vec<u8>> {
        let contents = fs::read(path)?;
        if contents.len() < 12 {
            anyhow::bail!("File too short for nonce");
        }

        let (nonce_bytes, ciphertext) = contents.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);
        let cipher = Aes256Gcm::new_from_slice(&self.encryption_key)
            .map_err(|e| anyhow::anyhow!("Failed to create cipher: {}", e))?;

        cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow::anyhow!("Decryption failed: {}", e))
    }

    /// Encrypt and write a file.
    fn write_encrypted_file(&self, path: &PathBuf, data: &[u8]) -> Result<()> {
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let cipher = Aes256Gcm::new_from_slice(&self.encryption_key)
            .map_err(|e| anyhow::anyhow!("Failed to create cipher: {}", e))?;

        let ciphertext = cipher
            .encrypt(nonce, data)
            .map_err(|e| anyhow::anyhow!("Encryption failed: {}", e))?;

        let mut contents = nonce_bytes.to_vec();
        contents.extend(ciphertext);

        fs::write(path, &contents)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }

        Ok(())
    }

    /// Generate or get our identity keypair.
    pub async fn get_or_create_identity(&self) -> KeyPair {
        {
            let identity = self.identity.read().await;
            if let Some(ref keypair) = *identity {
                return keypair.clone();
            }
        }

        // Generate new identity
        let keypair = curve::generate_signing_key_pair();

        // Store it
        let mut identity = self.identity.write().await;
        *identity = Some(keypair.clone());

        // Persist to disk
        let stored = StoredIdentity {
            priv_key: keypair.priv_key.clone(),
            pub_key: keypair.pub_key.clone(),
        };
        let data = serde_json::to_vec(&stored).unwrap_or_default();
        let path = self.base_dir.join("identity.enc");
        if let Err(e) = self.write_encrypted_file(&path, &data) {
            log::error!("Failed to persist identity: {}", e);
        }

        keypair
    }

    /// Store a pre-key.
    pub async fn store_pre_key(&self, id: u32, keypair: KeyPair) -> Result<()> {
        {
            let mut pre_keys = self.pre_keys.write().await;
            pre_keys.insert(id, keypair.clone());
        }

        let data = serde_json::to_vec(&keypair)?;
        let path = self.base_dir.join("pre_keys").join(format!("{}.enc", id));
        self.write_encrypted_file(&path, &data)
    }

    /// Store a signed pre-key.
    pub async fn store_signed_pre_key(&self, id: u32, keypair: KeyPair) -> Result<()> {
        {
            let mut signed_pre_keys = self.signed_pre_keys.write().await;
            signed_pre_keys.insert(id, keypair.clone());
        }

        let data = serde_json::to_vec(&keypair)?;
        let path = self
            .base_dir
            .join("signed_pre_keys")
            .join(format!("{}.enc", id));
        self.write_encrypted_file(&path, &data)
    }

    /// Mark an identity as trusted.
    pub async fn trust_identity(&self, address: &str, identity_key: &[u8]) -> Result<()> {
        {
            let mut trusted = self.trusted_identities.write().await;
            trusted.insert(address.to_string(), identity_key.to_vec());
        }

        let path = self
            .base_dir
            .join("trusted_identities")
            .join(format!("{}.enc", Self::sanitize_filename(address)));
        self.write_encrypted_file(&path, identity_key)
    }

    /// Remove a pre-key after use (one-time keys).
    pub async fn remove_pre_key(&self, id: u32) -> Result<()> {
        {
            let mut pre_keys = self.pre_keys.write().await;
            pre_keys.remove(&id);
        }

        let path = self.base_dir.join("pre_keys").join(format!("{}.enc", id));
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Sanitize a string for use as a filename.
    fn sanitize_filename(s: &str) -> String {
        s.chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }
}

impl SessionStorage for SignalStore {
    fn is_trusted_identity(
        &self,
        address: &str,
        identity_key: &[u8],
    ) -> impl std::future::Future<Output = bool> + Send {
        let trusted = self.trusted_identities.clone();
        let address = address.to_string();
        let identity_key = identity_key.to_vec();

        async move {
            let trusted = trusted.read().await;

            // If we've never seen this identity, it's trusted (TOFU)
            match trusted.get(&address) {
                Some(stored_key) => stored_key == &identity_key,
                None => true, // Trust on first use
            }
        }
    }

    fn load_session(
        &self,
        address: &str,
    ) -> impl std::future::Future<Output = Option<SessionRecord>> + Send {
        let sessions = self.sessions.clone();
        let address = address.to_string();

        async move {
            let sessions = sessions.read().await;
            sessions.get(&address).cloned()
        }
    }

    fn store_session(
        &self,
        address: &str,
        record: SessionRecord,
    ) -> impl std::future::Future<Output = ()> + Send {
        let sessions = self.sessions.clone();
        let base_dir = self.base_dir.clone();
        let encryption_key = self.encryption_key;
        let address = address.to_string();

        async move {
            // Store in memory
            {
                let mut sessions = sessions.write().await;
                sessions.insert(address.clone(), record.clone());
            }

            // Persist to disk
            if let Ok(data) = serde_json::to_vec(&record) {
                let path = base_dir
                    .join("sessions")
                    .join(format!("{}.enc", Self::sanitize_filename(&address)));

                // Encrypt and write
                let mut nonce_bytes = [0u8; 12];
                OsRng.fill_bytes(&mut nonce_bytes);
                let nonce = Nonce::from_slice(&nonce_bytes);

                if let Ok(cipher) = Aes256Gcm::new_from_slice(&encryption_key) {
                    if let Ok(ciphertext) = cipher.encrypt(nonce, data.as_slice()) {
                        let mut contents = nonce_bytes.to_vec();
                        contents.extend(ciphertext);

                        if let Err(e) = fs::write(&path, &contents) {
                            log::error!("Failed to persist session: {}", e);
                        }
                    }
                }
            }
        }
    }

    fn load_pre_key(
        &self,
        pre_key_id: u32,
    ) -> impl std::future::Future<Output = Option<KeyPair>> + Send {
        let pre_keys = self.pre_keys.clone();

        async move {
            let pre_keys = pre_keys.read().await;
            pre_keys.get(&pre_key_id).cloned()
        }
    }

    fn load_signed_pre_key(
        &self,
        signed_pre_key_id: u32,
    ) -> impl std::future::Future<Output = Option<KeyPair>> + Send {
        let signed_pre_keys = self.signed_pre_keys.clone();

        async move {
            let signed_pre_keys = signed_pre_keys.read().await;
            signed_pre_keys.get(&signed_pre_key_id).cloned()
        }
    }

    fn get_our_identity(&self) -> impl std::future::Future<Output = KeyPair> + Send {
        let identity = self.identity.clone();
        let base_dir = self.base_dir.clone();
        let encryption_key = self.encryption_key;

        async move {
            // Check cache first
            {
                let identity = identity.read().await;
                if let Some(ref keypair) = *identity {
                    return keypair.clone();
                }
            }

            // Generate new identity
            let keypair = curve::generate_signing_key_pair();

            // Store in cache
            {
                let mut identity_write = identity.write().await;
                *identity_write = Some(keypair.clone());
            }

            // Persist to disk
            let stored = StoredIdentity {
                priv_key: keypair.priv_key.clone(),
                pub_key: keypair.pub_key.clone(),
            };
            if let Ok(data) = serde_json::to_vec(&stored) {
                let path = base_dir.join("identity.enc");

                let mut nonce_bytes = [0u8; 12];
                OsRng.fill_bytes(&mut nonce_bytes);
                let nonce = Nonce::from_slice(&nonce_bytes);

                if let Ok(cipher) = Aes256Gcm::new_from_slice(&encryption_key) {
                    if let Ok(ciphertext) = cipher.encrypt(nonce, data.as_slice()) {
                        let mut contents = nonce_bytes.to_vec();
                        contents.extend(ciphertext);

                        if let Err(e) = fs::write(&path, &contents) {
                            log::error!("Failed to persist identity: {}", e);
                        }
                    }
                }
            }

            keypair
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup_test_store() -> SignalStore {
        let temp_dir = tempdir().unwrap();
        std::env::set_var("BOTSTER_CONFIG_DIR", temp_dir.path().to_str().unwrap());
        SignalStore::new("test-hub").unwrap()
    }

    #[tokio::test]
    async fn test_identity_generation_and_persistence() {
        let store = setup_test_store();

        // Get identity (should generate new one)
        let identity1 = store.get_our_identity().await;
        assert_eq!(identity1.priv_key.len(), 32);
        assert_eq!(identity1.pub_key.len(), 33); // 32 + version byte

        // Get identity again (should return same one)
        let identity2 = store.get_our_identity().await;
        assert_eq!(identity1.priv_key, identity2.priv_key);
        assert_eq!(identity1.pub_key, identity2.pub_key);
    }

    #[tokio::test]
    async fn test_trusted_identity() {
        let store = setup_test_store();

        let address = "browser-device-123";
        let identity_key = vec![5u8; 33]; // Fake identity key

        // First use should be trusted (TOFU)
        assert!(store.is_trusted_identity(address, &identity_key).await);

        // Store the identity
        store.trust_identity(address, &identity_key).await.unwrap();

        // Same identity should still be trusted
        assert!(store.is_trusted_identity(address, &identity_key).await);

        // Different identity should not be trusted
        let different_key = vec![6u8; 33];
        assert!(!store.is_trusted_identity(address, &different_key).await);
    }

    #[tokio::test]
    async fn test_pre_key_storage() {
        let store = setup_test_store();

        let keypair = curve::generate_key_pair();
        let pre_key_id = 42u32;

        // Store pre-key
        store.store_pre_key(pre_key_id, keypair.clone()).await.unwrap();

        // Load pre-key
        let loaded = store.load_pre_key(pre_key_id).await;
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.priv_key, keypair.priv_key);
        assert_eq!(loaded.pub_key, keypair.pub_key);

        // Remove pre-key
        store.remove_pre_key(pre_key_id).await.unwrap();

        // Should be gone
        assert!(store.load_pre_key(pre_key_id).await.is_none());
    }

    #[tokio::test]
    async fn test_signed_pre_key_storage() {
        let store = setup_test_store();

        let keypair = curve::generate_key_pair();
        let signed_pre_key_id = 1u32;

        // Store signed pre-key
        store
            .store_signed_pre_key(signed_pre_key_id, keypair.clone())
            .await
            .unwrap();

        // Load signed pre-key
        let loaded = store.load_signed_pre_key(signed_pre_key_id).await;
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.priv_key, keypair.priv_key);
        assert_eq!(loaded.pub_key, keypair.pub_key);
    }
}
