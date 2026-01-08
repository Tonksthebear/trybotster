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
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

/// Stored device identity (keypair + metadata)
#[derive(Debug, Serialize, Deserialize)]
pub struct StoredDevice {
    /// Base64-encoded public key
    pub public_key: String,
    /// Base64-encoded secret key (NEVER sent to server)
    secret_key: String,
    /// Human-readable fingerprint for visual verification
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
    fn config_path() -> Result<PathBuf> {
        let config_dir = dirs::config_dir()
            .context("Could not determine config directory")?
            .join("botster");

        fs::create_dir_all(&config_dir)
            .context("Failed to create config directory")?;

        Ok(config_dir.join("device.json"))
    }

    /// Load device from config file
    fn load_from_file(path: &PathBuf) -> Result<Self> {
        let content = fs::read_to_string(path)
            .context("Failed to read device config")?;

        let stored: StoredDevice = serde_json::from_str(&content)
            .context("Failed to parse device config")?;

        let public_key_bytes = BASE64.decode(&stored.public_key)
            .context("Invalid public key encoding")?;
        let secret_key_bytes = BASE64.decode(&stored.secret_key)
            .context("Invalid secret key encoding")?;

        let public_key = PublicKey::from_slice(&public_key_bytes)
            .map_err(|e| anyhow::anyhow!("Invalid public key: {}", e))?;
        let secret_key = SecretKey::from_slice(&secret_key_bytes)
            .map_err(|e| anyhow::anyhow!("Invalid secret key: {}", e))?;

        log::info!("Loaded device identity: fingerprint={}", stored.fingerprint);

        Ok(Self {
            public_key,
            secret_key,
            fingerprint: stored.fingerprint,
            name: stored.name,
            device_id: stored.device_id,
            config_path: path.clone(),
        })
    }

    /// Create a new device with fresh keypair
    fn create_new(path: &PathBuf) -> Result<Self> {
        let secret_key = SecretKey::generate(&mut OsRng);
        let public_key = secret_key.public_key();

        let fingerprint = Self::compute_fingerprint(&public_key);
        let name = Self::default_name();

        let stored = StoredDevice {
            public_key: BASE64.encode(public_key.as_bytes()),
            secret_key: BASE64.encode(secret_key.to_bytes()),
            fingerprint: fingerprint.clone(),
            name: name.clone(),
            device_id: None,
        };

        let content = serde_json::to_string_pretty(&stored)
            .context("Failed to serialize device config")?;

        fs::write(path, content)
            .context("Failed to write device config")?;

        log::info!("Created new device identity: fingerprint={}", fingerprint);

        Ok(Self {
            public_key,
            secret_key,
            fingerprint,
            name,
            device_id: None,
            config_path: path.clone(),
        })
    }

    /// Compute fingerprint from public key (first 8 bytes of SHA256 as hex)
    fn compute_fingerprint(public_key: &PublicKey) -> String {
        let hash = Sha256::digest(public_key.as_bytes());
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

    /// Save updated device info (e.g., after registration)
    pub fn save(&self) -> Result<()> {
        let stored = StoredDevice {
            public_key: BASE64.encode(self.public_key.as_bytes()),
            secret_key: BASE64.encode(self.secret_key.to_bytes()),
            fingerprint: self.fingerprint.clone(),
            name: self.name.clone(),
            device_id: self.device_id,
        };

        let content = serde_json::to_string_pretty(&stored)
            .context("Failed to serialize device config")?;

        fs::write(&self.config_path, content)
            .context("Failed to write device config")?;

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
        let secret_key = SecretKey::generate(&mut OsRng);
        let public_key = secret_key.public_key();
        let fingerprint = Device::compute_fingerprint(&public_key);

        // Should be 8 hex bytes separated by colons
        let parts: Vec<&str> = fingerprint.split(':').collect();
        assert_eq!(parts.len(), 8);
        for part in parts {
            assert_eq!(part.len(), 2);
            assert!(part.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }
}
