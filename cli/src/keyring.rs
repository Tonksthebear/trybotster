//! Consolidated keyring storage for all CLI credentials.
//!
//! Stores all secrets in a single keyring entry to avoid multiple
//! macOS keychain prompts when the binary changes (new builds).
//!
//! # Storage
//!
//! Production: Single OS keyring entry `botster/credentials` containing JSON.
//! Test mode: File at `{config_dir}/credentials.json`.

use anyhow::{Context, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// Keyring service name.
const KEYRING_SERVICE: &str = "botster";
/// Consolidated keyring entry name.
const KEYRING_CREDENTIALS: &str = "credentials";

/// Check if keyring should be skipped (any test mode).
///
/// Uses multiple checks to ensure keyring is never accessed during tests:
/// 1. `#[cfg(test)]` - Always skip during Rust unit tests
/// 2. Direct env var check - Fallback if env module detection fails
/// 3. `crate::env::should_skip_keyring()` - Standard environment detection
fn should_skip_keyring() -> bool {
    #[cfg(test)]
    {
        return true;
    }

    #[cfg(not(test))]
    {
        // Direct env var check as a safety fallback.
        // This catches cases where BOTSTER_ENV is set but env detection
        // might fail or be called before module initialization.
        if let Ok(env_val) = std::env::var("BOTSTER_ENV") {
            if env_val == "test" || env_val == "system_test" {
                return true;
            }
        }

        // Standard environment detection
        crate::env::should_skip_keyring()
    }
}

/// Get the credentials file path for test mode.
fn credentials_file_path() -> Result<PathBuf> {
    crate::config::Config::config_dir().map(|d| d.join("credentials.json"))
}

/// Consolidated credentials stored in a single keyring entry.
///
/// This reduces macOS keychain prompts to 1 on new binary builds.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Credentials {
    /// API token for hub-server authentication (btstr_...).
    /// Used by the hub process for full server access.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_token: Option<String>,

    /// MCP token for agent authentication (btmcp_...).
    /// Scoped to MCP operations only, passed to spawned agents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_token: Option<String>,

    /// Base64-encoded Ed25519 signing key for device identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signing_key: Option<String>,

    /// Device fingerprint (used to identify which signing key this is).
    /// Stored alongside signing_key for verification.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,

    /// Per-hub Signal encryption keys (hub_id -> base64 AES key).
    /// Used to encrypt Signal Protocol session state at rest.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub signal_keys: HashMap<String, String>,

    /// Schema version for future migrations.
    #[serde(default = "default_version")]
    pub version: u8,
}

fn default_version() -> u8 {
    1
}

impl Credentials {
    /// Load credentials from keyring (or file in test mode).
    pub fn load() -> Result<Self> {
        if should_skip_keyring() {
            return Self::load_from_file();
        }

        // Load consolidated credentials from keyring
        let entry = Entry::new(KEYRING_SERVICE, KEYRING_CREDENTIALS)
            .map_err(|e| anyhow::anyhow!("Failed to create keyring entry: {e:?}"))?;

        match entry.get_password() {
            Ok(json) => {
                let creds: Credentials = serde_json::from_str(&json)
                    .context("Failed to parse credentials from keyring")?;
                log::debug!("Loaded consolidated credentials from keyring");
                Ok(creds)
            }
            Err(_) => {
                // No credentials yet - return empty
                log::debug!("No credentials found in keyring, returning empty");
                Ok(Credentials::default())
            }
        }
    }

    /// Load credentials from file (test mode).
    fn load_from_file() -> Result<Self> {
        let path = credentials_file_path()?;
        if path.exists() {
            let content = fs::read_to_string(&path)?;
            let creds: Credentials = serde_json::from_str(&content)?;
            log::debug!("Loaded credentials from file (test mode)");
            Ok(creds)
        } else {
            // No credentials yet - return empty
            log::debug!("No credentials file found, returning empty");
            Ok(Credentials::default())
        }
    }

    /// Save credentials to keyring (or file in test mode).
    pub fn save(&self) -> Result<()> {
        if should_skip_keyring() {
            return self.save_to_file();
        }

        let entry = Entry::new(KEYRING_SERVICE, KEYRING_CREDENTIALS)
            .map_err(|e| anyhow::anyhow!("Failed to create keyring entry: {e:?}"))?;

        let json = serde_json::to_string(self)?;
        entry
            .set_password(&json)
            .map_err(|e| anyhow::anyhow!("Failed to store credentials in keyring: {e:?}"))?;

        log::info!("Saved consolidated credentials to OS keyring");
        Ok(())
    }

    /// Save credentials to file (test mode).
    fn save_to_file(&self) -> Result<()> {
        let path = credentials_file_path()?;
        let json = serde_json::to_string_pretty(self)?;
        fs::write(&path, json)?;

        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;

        log::debug!("Saved credentials to file (test mode)");
        Ok(())
    }

    /// Delete all credentials from keyring.
    pub fn delete() -> Result<()> {
        if should_skip_keyring() {
            let path = credentials_file_path()?;
            if path.exists() {
                fs::remove_file(&path)?;
            }
            return Ok(());
        }

        let entry = Entry::new(KEYRING_SERVICE, KEYRING_CREDENTIALS)
            .map_err(|e| anyhow::anyhow!("Failed to create keyring entry: {e:?}"))?;

        let _ = entry.delete_credential();
        log::info!("Deleted credentials from OS keyring");
        Ok(())
    }

    // === Convenience accessors ===

    /// Get API token if set.
    pub fn api_token(&self) -> Option<&str> {
        self.api_token.as_deref()
    }

    /// Set API token.
    pub fn set_api_token(&mut self, token: String) {
        self.api_token = Some(token);
    }

    /// Clear API token.
    pub fn clear_api_token(&mut self) {
        self.api_token = None;
    }

    // === MCP token accessors ===

    /// Get MCP token if set.
    pub fn mcp_token(&self) -> Option<&str> {
        self.mcp_token.as_deref()
    }

    /// Set MCP token.
    pub fn set_mcp_token(&mut self, token: String) {
        self.mcp_token = Some(token);
    }

    /// Clear MCP token.
    pub fn clear_mcp_token(&mut self) {
        self.mcp_token = None;
    }

    /// Get signing key if set.
    pub fn signing_key(&self) -> Option<&str> {
        self.signing_key.as_deref()
    }

    /// Set signing key with fingerprint.
    pub fn set_signing_key(&mut self, key: String, fingerprint: String) {
        self.signing_key = Some(key);
        self.fingerprint = Some(fingerprint);
    }

    /// Check if signing key matches expected fingerprint.
    pub fn signing_key_matches_fingerprint(&self, expected: &str) -> bool {
        self.fingerprint.as_deref() == Some(expected)
    }

    // === Signal key accessors ===

    /// Get Signal encryption key for a hub.
    pub fn signal_key(&self, hub_id: &str) -> Option<&str> {
        self.signal_keys.get(hub_id).map(|s| s.as_str())
    }

    /// Set Signal encryption key for a hub.
    pub fn set_signal_key(&mut self, hub_id: String, key: String) {
        self.signal_keys.insert(hub_id, key);
    }

    /// Remove Signal encryption key for a hub.
    pub fn remove_signal_key(&mut self, hub_id: &str) {
        self.signal_keys.remove(hub_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_credentials_roundtrip() {
        let mut creds = Credentials::default();
        creds.api_token = Some("btstr_test123".to_string());
        creds.signing_key = Some("base64key".to_string());
        creds.fingerprint = Some("aa:bb:cc:dd".to_string());

        let json = serde_json::to_string(&creds).unwrap();
        let loaded: Credentials = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.api_token, creds.api_token);
        assert_eq!(loaded.signing_key, creds.signing_key);
        assert_eq!(loaded.fingerprint, creds.fingerprint);
    }

    #[test]
    fn test_credentials_skips_none_fields() {
        let creds = Credentials {
            api_token: Some("token".to_string()),
            mcp_token: None,
            signing_key: None,
            fingerprint: None,
            signal_keys: HashMap::new(),
            version: 1,
        };

        let json = serde_json::to_string(&creds).unwrap();
        assert!(!json.contains("mcp_token"));
        assert!(!json.contains("signing_key"));
        assert!(!json.contains("fingerprint"));
        assert!(!json.contains("signal_keys"));
    }

    #[test]
    fn test_signal_keys_stored_and_retrieved() {
        let mut creds = Credentials::default();
        creds.set_signal_key("hub123".to_string(), "base64key".to_string());

        assert_eq!(creds.signal_key("hub123"), Some("base64key"));
        assert_eq!(creds.signal_key("other"), None);

        creds.remove_signal_key("hub123");
        assert_eq!(creds.signal_key("hub123"), None);
    }

    // === MCP Token Tests ===

    #[test]
    fn test_mcp_token_stored_and_retrieved() {
        let mut creds = Credentials::default();
        assert_eq!(creds.mcp_token(), None);

        creds.set_mcp_token("btmcp_test123".to_string());
        assert_eq!(creds.mcp_token(), Some("btmcp_test123"));
    }

    #[test]
    fn test_mcp_token_cleared() {
        let mut creds = Credentials::default();
        creds.set_mcp_token("btmcp_test123".to_string());
        assert!(creds.mcp_token().is_some());

        creds.clear_mcp_token();
        assert_eq!(creds.mcp_token(), None);
    }

    #[test]
    fn test_mcp_token_serialized_in_json() {
        let mut creds = Credentials::default();
        creds.set_mcp_token("btmcp_test123".to_string());

        let json = serde_json::to_string(&creds).unwrap();
        assert!(json.contains("mcp_token"));
        assert!(json.contains("btmcp_test123"));

        let loaded: Credentials = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.mcp_token(), Some("btmcp_test123"));
    }

    #[test]
    fn test_mcp_token_skipped_when_none() {
        let creds = Credentials::default();
        let json = serde_json::to_string(&creds).unwrap();
        assert!(!json.contains("mcp_token"));
    }

    #[test]
    fn test_credentials_roundtrip_with_mcp_token() {
        let mut creds = Credentials::default();
        creds.api_token = Some("btstr_hub".to_string());
        creds.set_mcp_token("btmcp_agent".to_string());

        let json = serde_json::to_string(&creds).unwrap();
        let loaded: Credentials = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.api_token(), Some("btstr_hub"));
        assert_eq!(loaded.mcp_token(), Some("btmcp_agent"));
    }
}
