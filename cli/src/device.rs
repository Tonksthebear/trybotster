//! Device identity management for E2E encrypted terminal access.
//!
//! This module handles:
//! - Keypair generation and persistence
//! - Device registration with the Rails server
//! - Fingerprint generation for visual verification
//!
//! The private key NEVER leaves this device - only the public key is sent to the server.
//! This enables zero-knowledge E2E encryption where the server cannot read terminal data.
//!
//! # Security Model
//!
//! - Uses X25519 + XSalsa20-Poly1305 (crypto_box) - compatible with TweetNaCl
//! - Keypair stored in ~/.config/botster/device.json
//! - Fingerprint: first 8 bytes of SHA256(public_key) as hex
//!
//! Rust guideline compliant 2025-01-05

use anyhow::{Context, Result};
use base64::{
    engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD as BASE64_URL},
    Engine,
};
use crypto_box::{PublicKey, SecretKey};
use ed25519_dalek::{SigningKey, VerifyingKey};
use keyring::Entry;
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// Keyring service name for storing secrets
const KEYRING_SERVICE: &str = "botster";
/// Keyring entry suffix for encryption key
const KEYRING_ENCRYPTION_SUFFIX: &str = "encryption";
/// Keyring entry suffix for signing key
const KEYRING_SIGNING_SUFFIX: &str = "signing";

/// Stored device identity (keypair + metadata)
///
/// Note: Secret keys are stored in OS keyring, not in this file.
/// The `secret_key` field is kept for migration from old format only.
#[derive(Debug, Serialize, Deserialize)]
pub struct StoredDevice {
    /// Base64-encoded X25519 public key (for encryption)
    pub public_key: String,
    /// Base64-encoded Ed25519 verifying key (for signature verification)
    #[serde(default)]
    pub verifying_key: Option<String>,
    /// Base64-encoded secret key (DEPRECATED: now stored in keyring)
    /// Kept optional for migration from old format
    #[serde(skip_serializing_if = "Option::is_none")]
    secret_key: Option<String>,
    /// Human-readable fingerprint for visual verification
    /// Based on verifying key (signing identity)
    pub fingerprint: String,
    /// Device name (e.g., "Botster CLI")
    pub name: String,
    /// Server-assigned device ID (set after registration)
    pub device_id: Option<i64>,
}

/// Runtime device identity with parsed keys
pub struct Device {
    /// X25519 public key for encryption.
    pub public_key: PublicKey,
    /// X25519 secret key (never leaves device).
    pub secret_key: SecretKey,
    /// Ed25519 signing key for authenticating key exchanges.
    pub signing_key: SigningKey,
    /// Ed25519 verifying key (public part of signing key).
    pub verifying_key: VerifyingKey,
    /// Human-readable fingerprint for verification.
    /// Based on verifying key (signing identity).
    pub fingerprint: String,
    /// Device name (e.g., hostname).
    pub name: String,
    /// Server-assigned device ID after registration.
    pub device_id: Option<i64>,
    /// Path to the device config file.
    config_path: PathBuf,
}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device")
            .field("fingerprint", &self.fingerprint)
            .field("name", &self.name)
            .field("device_id", &self.device_id)
            .field("config_path", &self.config_path)
            .finish_non_exhaustive()
    }
}

impl Device {
    /// Load existing device or create new one.
    ///
    /// Keypair is stored in ~/.config/botster/device.json
    pub fn load_or_create() -> Result<Self> {
        let config_path = Self::config_path()?;

        if config_path.exists() {
            Self::load_from_file(&config_path)
        } else {
            Self::create_new(&config_path)
        }
    }

    /// Get the device config file path
    fn config_path() -> Result<PathBuf> {
        let config_dir = dirs::config_dir()
            .context("Could not determine config directory")?
            .join("botster");

        fs::create_dir_all(&config_dir)
            .context("Failed to create config directory")?;

        Ok(config_dir.join("device.json"))
    }

    /// Store encryption secret key in OS keyring
    fn store_encryption_key_in_keyring(fingerprint: &str, secret_key: &SecretKey) -> Result<()> {
        let entry_name = format!("{}-{}", fingerprint, KEYRING_ENCRYPTION_SUFFIX);
        let entry = Entry::new(KEYRING_SERVICE, &entry_name)
            .context("Failed to create keyring entry for encryption key")?;
        let secret_b64 = BASE64.encode(secret_key.to_bytes());
        entry.set_password(&secret_b64)
            .context("Failed to store encryption key in keyring")?;
        log::info!("Stored encryption key in OS keyring");
        Ok(())
    }

    /// Load encryption secret key from OS keyring
    fn load_encryption_key_from_keyring(fingerprint: &str) -> Result<SecretKey> {
        // Try new format first, then legacy format
        let entry_name = format!("{}-{}", fingerprint, KEYRING_ENCRYPTION_SUFFIX);
        let entry = Entry::new(KEYRING_SERVICE, &entry_name)
            .or_else(|_| Entry::new(KEYRING_SERVICE, fingerprint))  // Legacy fallback
            .context("Failed to create keyring entry for encryption key")?;
        let secret_b64 = entry.get_password()
            .context("Encryption key not found in keyring")?;
        let secret_bytes = BASE64.decode(&secret_b64)
            .context("Invalid encryption key encoding in keyring")?;
        let secret_key = SecretKey::from_slice(&secret_bytes)
            .map_err(|e| anyhow::anyhow!("Invalid encryption key in keyring: {}", e))?;
        Ok(secret_key)
    }

    /// Store signing secret key in OS keyring
    fn store_signing_key_in_keyring(fingerprint: &str, signing_key: &SigningKey) -> Result<()> {
        let entry_name = format!("{}-{}", fingerprint, KEYRING_SIGNING_SUFFIX);
        let entry = Entry::new(KEYRING_SERVICE, &entry_name)
            .context("Failed to create keyring entry for signing key")?;
        let secret_b64 = BASE64.encode(signing_key.to_bytes());
        entry.set_password(&secret_b64)
            .context("Failed to store signing key in keyring")?;
        log::info!("Stored signing key in OS keyring");
        Ok(())
    }

    /// Load signing secret key from OS keyring
    fn load_signing_key_from_keyring(fingerprint: &str) -> Result<SigningKey> {
        let entry_name = format!("{}-{}", fingerprint, KEYRING_SIGNING_SUFFIX);
        let entry = Entry::new(KEYRING_SERVICE, &entry_name)
            .context("Failed to create keyring entry for signing key")?;
        let secret_b64 = entry.get_password()
            .context("Signing key not found in keyring")?;
        let secret_bytes = BASE64.decode(&secret_b64)
            .context("Invalid signing key encoding in keyring")?;
        let key_bytes: [u8; 32] = secret_bytes.try_into()
            .map_err(|_| anyhow::anyhow!("Invalid signing key length in keyring"))?;
        Ok(SigningKey::from_bytes(&key_bytes))
    }

    /// Load device from config file
    fn load_from_file(path: &PathBuf) -> Result<Self> {
        let content = fs::read_to_string(path)
            .context("Failed to read device config")?;

        let mut stored: StoredDevice = serde_json::from_str(&content)
            .context("Failed to parse device config")?;

        let public_key_bytes = BASE64.decode(&stored.public_key)
            .context("Invalid public key encoding")?;
        let public_key = PublicKey::from_slice(&public_key_bytes)
            .map_err(|e| anyhow::anyhow!("Invalid public key: {}", e))?;

        // Try to load encryption key from keyring first, fall back to file for migration
        let secret_key = match Self::load_encryption_key_from_keyring(&stored.fingerprint) {
            Ok(key) => {
                log::debug!("Loaded encryption key from OS keyring");
                key
            }
            Err(_) => {
                // Fallback: migrate from file if present
                if let Some(ref secret_b64) = stored.secret_key {
                    log::info!("Migrating encryption key from file to OS keyring...");
                    let secret_bytes = BASE64.decode(secret_b64)
                        .context("Invalid secret key encoding in file")?;
                    let key = SecretKey::from_slice(&secret_bytes)
                        .map_err(|e| anyhow::anyhow!("Invalid secret key in file: {}", e))?;

                    // Store in keyring
                    Self::store_encryption_key_in_keyring(&stored.fingerprint, &key)?;

                    // Remove secret from file and save
                    stored.secret_key = None;
                    let content = serde_json::to_string_pretty(&stored)
                        .context("Failed to serialize device config")?;
                    fs::write(path, &content)
                        .context("Failed to update device config")?;
                    #[cfg(unix)]
                    {
                        let perms = fs::Permissions::from_mode(0o600);
                        fs::set_permissions(path, perms)
                            .context("Failed to set device config permissions")?;
                    }
                    log::info!("Migrated encryption key to OS keyring, removed from file");

                    key
                } else {
                    anyhow::bail!("Encryption key not found in keyring or file. Device may need to be recreated.");
                }
            }
        };

        // Load or generate signing keypair
        // Old devices won't have signing keys, so we generate them on first load
        let (signing_key, verifying_key, needs_save) = match Self::load_signing_key_from_keyring(&stored.fingerprint) {
            Ok(sk) => {
                log::debug!("Loaded signing key from OS keyring");
                let vk = sk.verifying_key();
                (sk, vk, false)
            }
            Err(_) => {
                // Generate new signing keypair for legacy devices
                log::info!("Generating signing keypair for legacy device...");
                let mut secret_bytes = [0u8; 32];
                OsRng.fill_bytes(&mut secret_bytes);
                let sk = SigningKey::from_bytes(&secret_bytes);
                let vk = sk.verifying_key();
                Self::store_signing_key_in_keyring(&stored.fingerprint, &sk)?;
                log::info!("Created and stored new signing keypair in keyring");
                (sk, vk, true)
            }
        };

        // Update stored config if we generated a new signing key
        if needs_save || stored.verifying_key.is_none() {
            stored.verifying_key = Some(BASE64.encode(verifying_key.as_bytes()));
            let content = serde_json::to_string_pretty(&stored)
                .context("Failed to serialize device config")?;
            fs::write(path, &content)
                .context("Failed to update device config")?;
            #[cfg(unix)]
            {
                let perms = fs::Permissions::from_mode(0o600);
                fs::set_permissions(path, perms)
                    .context("Failed to set device config permissions")?;
            }
        }

        log::info!("Loaded device identity: fingerprint={}", stored.fingerprint);

        Ok(Self {
            public_key,
            secret_key,
            signing_key,
            verifying_key,
            fingerprint: stored.fingerprint,
            name: stored.name,
            device_id: stored.device_id,
            config_path: path.clone(),
        })
    }

    /// Create a new device with fresh keypair
    fn create_new(path: &PathBuf) -> Result<Self> {
        // Generate X25519 keypair for encryption
        let secret_key = SecretKey::generate(&mut OsRng);
        let public_key = secret_key.public_key();

        // Generate Ed25519 keypair for signing
        let mut signing_secret = [0u8; 32];
        OsRng.fill_bytes(&mut signing_secret);
        let signing_key = SigningKey::from_bytes(&signing_secret);
        let verifying_key = signing_key.verifying_key();

        // Fingerprint is based on signing identity (verifying key)
        let fingerprint = Self::compute_fingerprint(&verifying_key);
        let name = Self::default_name();

        // Store both keys in OS keyring (not in file!)
        Self::store_encryption_key_in_keyring(&fingerprint, &secret_key)?;
        Self::store_signing_key_in_keyring(&fingerprint, &signing_key)?;

        // Store only public info in file (secret keys in keyring)
        let stored = StoredDevice {
            public_key: BASE64.encode(public_key.as_bytes()),
            verifying_key: Some(BASE64.encode(verifying_key.as_bytes())),
            secret_key: None, // Stored in keyring, not file
            fingerprint: fingerprint.clone(),
            name: name.clone(),
            device_id: None,
        };

        let content = serde_json::to_string_pretty(&stored)
            .context("Failed to serialize device config")?;

        fs::write(path, content)
            .context("Failed to write device config")?;

        // Set restrictive permissions (0600) - good practice even without secret
        #[cfg(unix)]
        {
            let perms = fs::Permissions::from_mode(0o600);
            fs::set_permissions(path, perms)
                .context("Failed to set device config permissions")?;
        }

        log::info!("Created new device identity: fingerprint={} (secrets in OS keyring)", fingerprint);

        Ok(Self {
            public_key,
            secret_key,
            signing_key,
            verifying_key,
            fingerprint,
            name,
            device_id: None,
            config_path: path.clone(),
        })
    }

    /// Compute fingerprint from verifying key (signing identity)
    ///
    /// The fingerprint is first 8 bytes of SHA256(verifying_key) as hex.
    /// This ties the device identity to the signing keypair, not encryption.
    fn compute_fingerprint(verifying_key: &VerifyingKey) -> String {
        let hash = Sha256::digest(verifying_key.as_bytes());
        hash[..8]
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<_>>()
            .join(":")
    }

    /// Generate default device name based on hostname
    fn default_name() -> String {
        hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok()).map_or_else(|| "Botster CLI".to_string(), |h| format!("Botster CLI ({})", h))
    }

    /// Get public key as base64 string (for sending to server)
    pub fn public_key_base64(&self) -> String {
        BASE64.encode(self.public_key.as_bytes())
    }

    /// Get public key as URL-safe base64 string (for QR codes and URLs)
    /// Uses base64url encoding without padding for safe URL fragment use
    pub fn public_key_base64url(&self) -> String {
        BASE64_URL.encode(self.public_key.as_bytes())
    }

    /// Get verifying key (signing public key) as base64 string
    pub fn verifying_key_base64(&self) -> String {
        BASE64.encode(self.verifying_key.as_bytes())
    }

    /// Save updated device info (e.g., after registration)
    ///
    /// Note: Secret keys are stored in OS keyring, not in the config file.
    pub fn save(&self) -> Result<()> {
        let stored = StoredDevice {
            public_key: BASE64.encode(self.public_key.as_bytes()),
            verifying_key: Some(BASE64.encode(self.verifying_key.as_bytes())),
            secret_key: None, // Secret keys are in OS keyring, not file
            fingerprint: self.fingerprint.clone(),
            name: self.name.clone(),
            device_id: self.device_id,
        };

        let content = serde_json::to_string_pretty(&stored)
            .context("Failed to serialize device config")?;

        fs::write(&self.config_path, content)
            .context("Failed to write device config")?;

        // Set restrictive permissions (0600) - good practice
        #[cfg(unix)]
        {
            let perms = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&self.config_path, perms)
                .context("Failed to set device config permissions")?;
        }

        Ok(())
    }

    /// Update device ID after server registration
    pub fn set_device_id(&mut self, id: i64) -> Result<()> {
        self.device_id = Some(id);
        self.save()
    }

    /// Clear stale device ID (e.g., after database reset)
    /// Call this when the server returns "device not found" errors.
    pub fn clear_device_id(&mut self) -> Result<()> {
        if self.device_id.is_some() {
            log::info!("Clearing stale device_id={:?}", self.device_id);
            self.device_id = None;
            self.save()?;
        }
        Ok(())
    }

    /// Register device with server (POST /api/devices)
    ///
    /// If `share_public_key` is true (convenience mode), the public key is sent to the server.
    /// This enables server-assisted pairing but allows potential MITM attacks.
    ///
    /// If `share_public_key` is false (secure mode - default), only the fingerprint and name
    /// are sent. Key exchange must happen via QR code URL fragment (MITM-proof).
    pub fn register(
        &mut self,
        client: &reqwest::blocking::Client,
        server_url: &str,
        api_key: &str,
        share_public_key: bool,
    ) -> Result<i64> {
        #[derive(Serialize)]
        struct RegisterRequest {
            #[serde(skip_serializing_if = "Option::is_none")]
            public_key: Option<String>,
            device_type: String,
            name: String,
            fingerprint: String,
            /// Flag to indicate if this device uses server-assisted pairing
            server_assisted_pairing: bool,
        }

        #[derive(Deserialize)]
        struct RegisterResponse {
            device_id: i64,
            fingerprint: String,
            created: bool,
        }

        let request = RegisterRequest {
            // Only include public_key if server-assisted pairing is enabled
            public_key: if share_public_key {
                log::warn!("Server-assisted pairing enabled - sharing public key with server");
                Some(self.public_key_base64())
            } else {
                log::info!("Secure mode - public key NOT shared with server (MITM-proof)");
                None
            },
            device_type: "cli".to_string(),
            name: self.name.clone(),
            fingerprint: self.fingerprint.clone(),
            server_assisted_pairing: share_public_key,
        };

        let url = format!("{}/devices", server_url);
        let response = client
            .post(&url)
            .bearer_auth(api_key)
            .json(&request)
            .send()
            .context("Failed to send device registration request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            anyhow::bail!("Device registration failed: {} - {}", status, body);
        }

        let data: RegisterResponse = response.json()
            .context("Failed to parse device registration response")?;

        log::info!(
            "Device registered: id={} fingerprint={} created={} server_assisted={}",
            data.device_id,
            data.fingerprint,
            data.created,
            share_public_key
        );

        self.set_device_id(data.device_id)?;

        Ok(data.device_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fingerprint_format() {
        // Fingerprint is now based on Ed25519 verifying key (signing identity)
        let mut secret_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut secret_bytes);
        let signing_key = SigningKey::from_bytes(&secret_bytes);
        let verifying_key = signing_key.verifying_key();
        let fingerprint = Device::compute_fingerprint(&verifying_key);

        // Should be 8 hex bytes separated by colons
        let parts: Vec<&str> = fingerprint.split(':').collect();
        assert_eq!(parts.len(), 8);
        for part in parts {
            assert_eq!(part.len(), 2);
            assert!(part.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }
}
