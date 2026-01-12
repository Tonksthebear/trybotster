//! Device identity management for CLI authentication.
//!
//! This module handles:
//! - Ed25519 signing keypair generation and persistence
//! - Device registration with the Rails server
//! - Fingerprint generation for visual verification
//!
//! Note: E2E encryption is handled by Olm (vodozemac) in the relay module.
//! This module only manages device identity for authentication.

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{SigningKey, VerifyingKey};
use keyring::Entry;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// Keyring service name for storing secrets
const KEYRING_SERVICE: &str = "botster";
/// Keyring entry suffix for signing key
const KEYRING_SIGNING_SUFFIX: &str = "signing";

/// Check if keyring should be skipped (for testing).
///
/// Keyring is skipped when:
/// - Compiled with `cfg(test)` (unit tests via `cargo test --lib`)
/// - `BOTSTER_ENV=test` is set (integration/system tests)
///
/// This avoids macOS keychain prompts during test runs.
fn should_skip_keyring() -> bool {
    // Compile-time check for unit tests
    #[cfg(test)]
    {
        return true;
    }

    #[cfg(not(test))]
    {
        crate::env::is_test_mode()
    }
}

/// Stored device identity (public keys + metadata)
///
/// Note: Secret keys are stored in OS keyring, not in this file.
#[derive(Debug, Serialize, Deserialize)]
pub struct StoredDevice {
    /// Base64-encoded Ed25519 verifying key (for signature verification)
    pub verifying_key: String,
    /// Human-readable fingerprint for visual verification
    pub fingerprint: String,
    /// Device name (e.g., "Botster CLI")
    pub name: String,
    /// Server-assigned device ID (set after registration)
    pub device_id: Option<i64>,
}

/// Runtime device identity with parsed keys
pub struct Device {
    /// Ed25519 signing key for authenticating.
    pub signing_key: SigningKey,
    /// Ed25519 verifying key (public part of signing key).
    pub verifying_key: VerifyingKey,
    /// Human-readable fingerprint for verification.
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
    ///
    /// In test mode (`cfg(test)`), uses `target/test-config/` to avoid touching real config.
    /// Also respects `BOTSTER_CONFIG_DIR` for integration test isolation.
    fn config_path() -> Result<PathBuf> {
        let config_dir = {
            // Test mode: use target/test-config/ within the project
            #[cfg(test)]
            {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-config")
            }

            #[cfg(not(test))]
            {
                if let Ok(custom_dir) = std::env::var("BOTSTER_CONFIG_DIR") {
                    PathBuf::from(custom_dir)
                } else {
                    dirs::config_dir()
                        .context("Could not determine config directory")?
                        .join("botster")
                }
            }
        };

        fs::create_dir_all(&config_dir).context("Failed to create config directory")?;

        Ok(config_dir.join("device.json"))
    }

    /// Get the path for file-based signing key storage (test mode).
    fn signing_key_file_path(config_path: &PathBuf) -> PathBuf {
        config_path.with_extension("signing_key")
    }

    /// Store signing secret key (keyring or file based on environment).
    fn store_signing_key(config_path: &PathBuf, fingerprint: &str, signing_key: &SigningKey) -> Result<()> {
        if should_skip_keyring() {
            // Test mode: store in file
            let key_path = Self::signing_key_file_path(config_path);
            let secret_b64 = BASE64.encode(signing_key.to_bytes());
            fs::write(&key_path, &secret_b64).context("Failed to write signing key file")?;
            #[cfg(unix)]
            {
                let perms = fs::Permissions::from_mode(0o600);
                fs::set_permissions(&key_path, perms).context("Failed to set signing key permissions")?;
            }
            log::info!("Stored signing key in file (test mode, keyring skipped)");
            Ok(())
        } else {
            // Production: use OS keyring
            let entry_name = format!("{}-{}", fingerprint, KEYRING_SIGNING_SUFFIX);
            log::debug!("Creating keyring entry: service={} user={}", KEYRING_SERVICE, entry_name);

            let entry = Entry::new(KEYRING_SERVICE, &entry_name)
                .map_err(|e| anyhow::anyhow!("Failed to create keyring entry: {:?}", e))?;

            let secret_b64 = BASE64.encode(signing_key.to_bytes());
            entry
                .set_password(&secret_b64)
                .map_err(|e| anyhow::anyhow!("Failed to store in keyring: {:?}", e))?;

            log::info!("Stored signing key in OS keyring");
            Ok(())
        }
    }

    /// Load signing secret key (keyring or file based on environment).
    fn load_signing_key(config_path: &PathBuf, fingerprint: &str) -> Result<SigningKey> {
        if should_skip_keyring() {
            // Test mode: load from file
            let key_path = Self::signing_key_file_path(config_path);
            let secret_b64 = fs::read_to_string(&key_path)
                .context("Signing key file not found (test mode)")?;
            let secret_bytes = BASE64
                .decode(secret_b64.trim())
                .context("Invalid signing key encoding in file")?;
            let key_bytes: [u8; 32] = secret_bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("Invalid signing key length in file"))?;
            log::debug!("Loaded signing key from file (test mode, keyring skipped)");
            Ok(SigningKey::from_bytes(&key_bytes))
        } else {
            // Production: use OS keyring
            let entry_name = format!("{}-{}", fingerprint, KEYRING_SIGNING_SUFFIX);
            let entry = Entry::new(KEYRING_SERVICE, &entry_name)
                .context("Failed to create keyring entry for signing key")?;
            let secret_b64 = entry
                .get_password()
                .context("Signing key not found in keyring")?;
            let secret_bytes = BASE64
                .decode(&secret_b64)
                .context("Invalid signing key encoding in keyring")?;
            let key_bytes: [u8; 32] = secret_bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("Invalid signing key length in keyring"))?;
            Ok(SigningKey::from_bytes(&key_bytes))
        }
    }

    /// Load device from config file
    fn load_from_file(path: &PathBuf) -> Result<Self> {
        let content = fs::read_to_string(path).context("Failed to read device config")?;

        let stored: StoredDevice =
            serde_json::from_str(&content).context("Failed to parse device config")?;

        // Load signing key (from keyring or file depending on environment)
        let signing_key = match Self::load_signing_key(path, &stored.fingerprint) {
            Ok(sk) => sk,
            Err(e) => {
                anyhow::bail!(
                    "Signing key not found. Device may need to be recreated: {}",
                    e
                );
            }
        };
        let verifying_key = signing_key.verifying_key();

        log::info!("Loaded device identity: fingerprint={}", stored.fingerprint);

        Ok(Self {
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
        // Generate Ed25519 keypair for signing/identity
        let mut signing_secret = [0u8; 32];
        rand::rng().fill_bytes(&mut signing_secret);
        let signing_key = SigningKey::from_bytes(&signing_secret);
        let verifying_key = signing_key.verifying_key();

        // Fingerprint is based on signing identity (verifying key)
        let fingerprint = Self::compute_fingerprint(&verifying_key);
        let name = Self::default_name();

        // Store signing key (in keyring or file depending on environment)
        Self::store_signing_key(path, &fingerprint, &signing_key)?;

        // Store only public info in file
        let stored = StoredDevice {
            verifying_key: BASE64.encode(verifying_key.as_bytes()),
            fingerprint: fingerprint.clone(),
            name: name.clone(),
            device_id: None,
        };

        let content =
            serde_json::to_string_pretty(&stored).context("Failed to serialize device config")?;

        fs::write(path, content).context("Failed to write device config")?;

        #[cfg(unix)]
        {
            let perms = fs::Permissions::from_mode(0o600);
            fs::set_permissions(path, perms).context("Failed to set device config permissions")?;
        }

        let storage_location = if should_skip_keyring() { "file (test mode)" } else { "OS keyring" };
        log::info!(
            "Created new device identity: fingerprint={} (signing key in {})",
            fingerprint,
            storage_location
        );

        Ok(Self {
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
            .and_then(|h| h.into_string().ok())
            .map_or_else(
                || "Botster CLI".to_string(),
                |h| format!("Botster CLI ({})", h),
            )
    }

    /// Get verifying key (signing public key) as base64 string
    pub fn verifying_key_base64(&self) -> String {
        BASE64.encode(self.verifying_key.as_bytes())
    }

    /// Save updated device info (e.g., after registration)
    pub fn save(&self) -> Result<()> {
        let stored = StoredDevice {
            verifying_key: BASE64.encode(self.verifying_key.as_bytes()),
            fingerprint: self.fingerprint.clone(),
            name: self.name.clone(),
            device_id: self.device_id,
        };

        let content =
            serde_json::to_string_pretty(&stored).context("Failed to serialize device config")?;

        fs::write(&self.config_path, content).context("Failed to write device config")?;

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
    pub fn clear_device_id(&mut self) -> Result<()> {
        if self.device_id.is_some() {
            log::info!("Clearing stale device_id={:?}", self.device_id);
            self.device_id = None;
            self.save()?;
        }
        Ok(())
    }

    /// Register device with server (POST /api/devices)
    pub fn register(
        &mut self,
        client: &reqwest::blocking::Client,
        server_url: &str,
        api_key: &str,
    ) -> Result<i64> {
        #[derive(Serialize)]
        struct RegisterRequest {
            device_type: String,
            name: String,
            fingerprint: String,
        }

        #[derive(Deserialize)]
        struct RegisterResponse {
            device_id: i64,
            fingerprint: String,
            created: bool,
        }

        let request = RegisterRequest {
            device_type: "cli".to_string(),
            name: self.name.clone(),
            fingerprint: self.fingerprint.clone(),
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

        let data: RegisterResponse = response
            .json()
            .context("Failed to parse device registration response")?;

        log::info!(
            "Device registered: id={} fingerprint={} created={}",
            data.device_id,
            data.fingerprint,
            data.created
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
        let mut secret_bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut secret_bytes);
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
