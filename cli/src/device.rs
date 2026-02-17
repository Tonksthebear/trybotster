//! Device identity management for CLI authentication.
//!
//! This module handles:
//! - Ed25519 signing keypair generation and persistence
//! - Device registration with the Rails server
//! - Fingerprint generation for visual verification
//!
//! Note: E2E encryption is handled by Olm (vodozemac) in the relay module.
//! This module only manages device identity for authentication.
//!
//! Signing keys are stored in the consolidated keyring entry via the keyring module.

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use crate::keyring::Credentials;

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

/// Global mutex to prevent race conditions when multiple threads
/// try to load/create the device simultaneously.
/// This is especially important in tests where multiple threads
/// share the same config directory.
static DEVICE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Device {
    /// Load existing device or create new one.
    ///
    /// Keypair is stored in ~/.config/botster/device.json.
    /// Uses a process-wide mutex to prevent race conditions when
    /// multiple threads (e.g., parallel tests) call this simultaneously.
    pub fn load_or_create() -> Result<Self> {
        // Hold lock for entire load/create operation to prevent races
        let _guard = DEVICE_LOCK.lock().expect("device lock poisoned");

        let config_path = Self::config_path()?;

        if config_path.exists() {
            Self::load_from_file(&config_path)
        } else {
            Self::create_new(&config_path)
        }
    }

    /// Get the device config file path
    ///
    /// Directory selection priority:
    /// 1. `#[cfg(test)]` (unit tests): `tmp/botster-test`
    /// 2. `BOTSTER_CONFIG_DIR` env var: explicit override
    /// 3. `BOTSTER_ENV=test`: `tmp/botster-test` (integration tests)
    /// 4. Default: system config directory (e.g., `~/Library/Application Support/botster`)
    fn config_path() -> Result<PathBuf> {
        let config_dir = {
            #[cfg(test)]
            {
                // Use repo's tmp/ directory (already gitignored)
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .expect("cli/ has parent directory")
                    .join("tmp/botster-test")
            }

            #[cfg(not(test))]
            {
                if let Ok(custom_dir) = std::env::var("BOTSTER_CONFIG_DIR") {
                    // Explicit override via env var
                    PathBuf::from(custom_dir)
                } else if crate::env::should_skip_keyring() {
                    // Integration/system tests (BOTSTER_ENV=test or system_test): use repo's tmp/ directory
                    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .parent()
                        .expect("cli/ has parent directory")
                        .join("tmp/botster-test")
                } else {
                    // Production: use system config directory
                    dirs::config_dir()
                        .context("Could not determine config directory")?
                        .join("botster")
                }
            }
        };

        fs::create_dir_all(&config_dir).context("Failed to create config directory")?;

        Ok(config_dir.join("device.json"))
    }

    /// Store signing secret key in consolidated credentials.
    fn store_signing_key(fingerprint: &str, signing_key: &SigningKey) -> Result<()> {
        let secret_b64 = BASE64.encode(signing_key.to_bytes());

        // Load existing credentials, update signing key, save back
        let mut creds = Credentials::load().unwrap_or_default();
        creds.set_signing_key(secret_b64, fingerprint.to_string());
        creds.save()?;

        log::info!("Stored signing key in consolidated credentials");
        Ok(())
    }

    /// Load signing secret key from consolidated credentials.
    ///
    /// Implements graceful degradation for fingerprint mismatches:
    /// - If fingerprint in keyring is stale but key is valid, update fingerprint
    /// - This handles macOS keychain issues when binary signature changes
    fn load_signing_key(expected_fingerprint: &str) -> Result<SigningKey> {
        let mut creds = Credentials::load().context("Failed to load credentials")?;

        let secret_b64 = creds
            .signing_key()
            .context("Signing key not found in credentials")?;

        // First, try to decode and validate the key itself
        let secret_bytes = BASE64
            .decode(secret_b64)
            .context("Invalid signing key encoding")?;

        let key_bytes: [u8; 32] = secret_bytes
            .try_into()
            .map_err(|_vec| anyhow::anyhow!("Invalid signing key length"))?;

        let signing_key = SigningKey::from_bytes(&key_bytes);

        // Derive the actual fingerprint from the key
        let actual_fingerprint = Self::compute_fingerprint(&signing_key.verifying_key());

        // Check if fingerprints match
        if actual_fingerprint == expected_fingerprint {
            // Key is valid and matches expected fingerprint
            // Check if keyring's stored fingerprint needs updating
            if !creds.signing_key_matches_fingerprint(expected_fingerprint) {
                log::warn!(
                    "Keyring fingerprint stale (was {:?}, updating to {}). \
                     This can happen after rebuilding the binary.",
                    creds.fingerprint,
                    expected_fingerprint
                );
                // Update the stale fingerprint in keyring
                creds.set_signing_key(secret_b64.to_string(), expected_fingerprint.to_string());
                if let Err(e) = creds.save() {
                    // Log but don't fail - we have a valid key
                    log::warn!("Failed to update stale fingerprint in keyring: {}", e);
                } else {
                    log::info!("Updated keyring fingerprint successfully");
                }
            }
            log::debug!("Loaded signing key from consolidated credentials");
            return Ok(signing_key);
        }

        // Key exists but derives to wrong fingerprint - this is a real mismatch
        // The key in keyring belongs to a different device identity
        log::error!(
            "Signing key fingerprint mismatch: key derives to {}, expected {}. \
             The stored key does not belong to this device identity.",
            actual_fingerprint,
            expected_fingerprint
        );
        anyhow::bail!(
            "Signing key fingerprint mismatch: expected {}, key derives to {}. \
             Device identity may need to be recreated.",
            expected_fingerprint,
            actual_fingerprint
        );
    }

    /// Load device from config file.
    ///
    /// Handles keyring access failures gracefully:
    /// - Stale fingerprints are automatically updated
    /// - Clear error messages guide users to resolution
    fn load_from_file(path: &PathBuf) -> Result<Self> {
        let content = fs::read_to_string(path).context("Failed to read device config")?;

        let stored: StoredDevice =
            serde_json::from_str(&content).context("Failed to parse device config")?;

        // Load signing key from consolidated credentials
        let signing_key = match Self::load_signing_key(&stored.fingerprint) {
            Ok(sk) => sk,
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("not found") {
                    log::error!(
                        "Signing key not found in credential storage. \
                         This may happen if:\n  \
                         - Credentials were stored in a keyring that is no longer accessible\n  \
                         - The credential file was deleted\n  \
                         - On macOS: the keychain is locked or blocked the binary"
                    );
                    anyhow::bail!(
                        "Signing key not found. Your credentials may have been lost.\n\
                         Re-authenticate with 'botster auth'."
                    );
                }
                anyhow::bail!(
                    "Failed to load signing key: {}.\n\
                     Re-authenticate with 'botster auth'.",
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

        // Store signing key in consolidated credentials
        Self::store_signing_key(&fingerprint, &signing_key)?;

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

        log::info!(
            "Created new device identity: fingerprint={} (signing key in consolidated credentials)",
            fingerprint
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
