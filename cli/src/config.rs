//! Configuration loading and persistence.
//!
//! Handles reading and writing the botster-hub configuration file.
//! Sensitive tokens are stored in OS keyring, not in the config file.

use crate::env::is_test_mode;
use anyhow::{Context, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Keyring service name (shared with device.rs)
const KEYRING_SERVICE: &str = "botster";
/// Keyring entry name for API token
const KEYRING_TOKEN_ENTRY: &str = "api-token";

/// Configuration for the botster-hub CLI.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    /// URL of the botster server.
    pub server_url: String,
    /// API token - NOT serialized to disk (stored in keyring)
    #[serde(skip)]
    pub token: String,
    /// Legacy API key - NOT serialized to disk
    #[serde(skip)]
    pub api_key: String,
    /// Interval in seconds between server polls.
    pub poll_interval: u64,
    /// Timeout in seconds before an idle agent is stopped.
    pub agent_timeout: u64,
    /// Maximum number of concurrent agent sessions.
    pub max_sessions: usize,
    /// Base directory for creating worktrees.
    pub worktree_base: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        // Worktree base: prefer home directory, but don't silently fall back
        // If home directory is unavailable, we'll catch this when loading config
        let worktree_base = dirs::home_dir()
            .map(|h| h.join("botster-sessions"))
            .unwrap_or_else(|| {
                // Log warning - this will be caught when config is actually used
                eprintln!("Warning: Could not determine home directory for worktree_base");
                PathBuf::from("botster-sessions")
            });

        Self {
            server_url: "https://trybotster.com".to_string(),
            token: String::new(),
            api_key: String::new(),
            poll_interval: 5,
            agent_timeout: 3600,
            max_sessions: 20,
            worktree_base,
        }
    }
}

impl Config {
    /// Returns the configuration directory path, creating it if necessary.
    ///
    /// Directory selection priority:
    /// 1. `BOTSTER_CONFIG_DIR` env var: explicit override
    /// 2. `BOTSTER_ENV=test`: `tmp/botster-test` (integration tests)
    /// 3. Default: `~/.botster_hub`
    pub fn config_dir() -> Result<PathBuf> {
        let dir = if let Ok(test_dir) = std::env::var("BOTSTER_CONFIG_DIR") {
            // Explicit override via env var
            PathBuf::from(test_dir)
        } else if is_test_mode() {
            // Integration tests (BOTSTER_ENV=test): use repo's tmp/ directory
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .context("cli/ has no parent directory")?
                .join("tmp/botster-test")
        } else {
            // Production: use home directory
            dirs::home_dir()
                .context("No home directory")?
                .join(".botster_hub")
        };
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Loads configuration from file, with environment variable overrides.
    /// Token is loaded from keyring (or env var).
    pub fn load() -> Result<Self> {
        let mut config = Self::load_from_file().unwrap_or_else(|_| Self::default());
        config.apply_env_overrides();

        // Load token from keyring if not set via env var
        if config.token.is_empty() {
            if let Ok(token) = Self::load_token_from_keyring() {
                config.token = token;
            }
        }

        Ok(config)
    }

    fn load_from_file() -> Result<Self> {
        let config_path = Self::config_dir()?.join("config.json");
        if config_path.exists() {
            let content = fs::read_to_string(&config_path)?;
            Ok(serde_json::from_str(&content)?)
        } else {
            anyhow::bail!("Config file not found")
        }
    }

    fn apply_env_overrides(&mut self) {
        if let Ok(server_url) = std::env::var("BOTSTER_SERVER_URL") {
            self.server_url = server_url;
        }

        // Token from env var (for CI/CD)
        if let Ok(token) = std::env::var("BOTSTER_TOKEN") {
            self.token = token;
        }

        // Legacy env var support
        if let Ok(api_key) = std::env::var("BOTSTER_API_KEY") {
            self.api_key = api_key;
        }

        if let Ok(worktree_base) = std::env::var("BOTSTER_WORKTREE_BASE") {
            self.worktree_base = PathBuf::from(worktree_base);
        }

        if let Ok(poll_interval) = std::env::var("BOTSTER_POLL_INTERVAL") {
            if let Ok(interval) = poll_interval.parse::<u64>() {
                self.poll_interval = interval;
            }
        }

        if let Ok(max_sessions) = std::env::var("BOTSTER_MAX_SESSIONS") {
            if let Ok(max) = max_sessions.parse::<usize>() {
                self.max_sessions = max;
            }
        }

        if let Ok(agent_timeout) = std::env::var("BOTSTER_AGENT_TIMEOUT") {
            if let Ok(timeout) = agent_timeout.parse::<u64>() {
                self.agent_timeout = timeout;
            }
        }
    }

    /// Persists the current configuration to disk.
    /// Note: Token is NOT saved here (use save_token for that).
    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_dir()?.join("config.json");
        fs::write(&config_path, serde_json::to_string_pretty(self)?)?;

        // Set restrictive permissions (owner read/write only)
        #[cfg(unix)]
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))?;

        Ok(())
    }

    /// Get the API key to use for authentication.
    /// Returns the new device token if set, otherwise falls back to legacy api_key.
    pub fn get_api_key(&self) -> &str {
        if self.token.is_empty() {
            &self.api_key
        } else {
            &self.token
        }
    }

    /// Check if we have a valid authentication token.
    /// Only returns true if the token has the expected `btstr_` prefix.
    pub fn has_token(&self) -> bool {
        const TOKEN_PREFIX: &str = "btstr_";

        if !self.token.is_empty() {
            return self.token.starts_with(TOKEN_PREFIX);
        }

        if !self.api_key.is_empty() {
            return self.api_key.starts_with(TOKEN_PREFIX);
        }

        false
    }

    /// Save a new device token to the keyring.
    pub fn save_token(&mut self, token: &str) -> Result<()> {
        self.token = token.to_string();
        Self::save_token_to_keyring(token)?;
        Ok(())
    }

    /// Clear the token from keyring.
    pub fn clear_token(&mut self) -> Result<()> {
        self.token.clear();
        Self::delete_token_from_keyring()?;
        Ok(())
    }

    // ========== Keyring Operations ==========

    /// Load token from OS keyring (or file in test mode).
    fn load_token_from_keyring() -> Result<String> {
        if is_test_mode() || cfg!(test) {
            // Test mode: use file storage
            let token_path = Self::config_dir()?.join("token");
            if token_path.exists() {
                return Ok(fs::read_to_string(&token_path)?.trim().to_string());
            }
            anyhow::bail!("Token file not found");
        }

        // Production: use OS keyring
        let entry = Entry::new(KEYRING_SERVICE, KEYRING_TOKEN_ENTRY)
            .map_err(|e| anyhow::anyhow!("Failed to create keyring entry: {:?}", e))?;

        entry
            .get_password()
            .map_err(|e| anyhow::anyhow!("Token not found in keyring: {:?}", e))
    }

    /// Save token to OS keyring (or file in test mode).
    fn save_token_to_keyring(token: &str) -> Result<()> {
        if is_test_mode() || cfg!(test) {
            // Test mode: use file storage
            let token_path = Self::config_dir()?.join("token");
            fs::write(&token_path, token)?;
            #[cfg(unix)]
            fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600))?;
            log::debug!("Saved token to file (test mode)");
            return Ok(());
        }

        // Production: use OS keyring
        let entry = Entry::new(KEYRING_SERVICE, KEYRING_TOKEN_ENTRY)
            .map_err(|e| anyhow::anyhow!("Failed to create keyring entry: {:?}", e))?;

        entry
            .set_password(token)
            .map_err(|e| anyhow::anyhow!("Failed to store token in keyring: {:?}", e))?;

        log::info!("Stored API token in OS keyring");
        Ok(())
    }

    /// Delete token from OS keyring (or file in test mode).
    fn delete_token_from_keyring() -> Result<()> {
        if is_test_mode() || cfg!(test) {
            let token_path = Self::config_dir()?.join("token");
            if token_path.exists() {
                fs::remove_file(&token_path)?;
            }
            return Ok(());
        }

        let entry = Entry::new(KEYRING_SERVICE, KEYRING_TOKEN_ENTRY)
            .map_err(|e| anyhow::anyhow!("Failed to create keyring entry: {:?}", e))?;

        // Ignore errors if entry doesn't exist
        let _ = entry.delete_credential();
        log::info!("Deleted API token from OS keyring");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.server_url, "https://trybotster.com");
        assert_eq!(config.poll_interval, 5);
        assert_eq!(config.max_sessions, 20);
        assert_eq!(config.agent_timeout, 3600);
    }

    #[test]
    fn test_config_serialization_excludes_token() {
        let mut config = Config::default();
        config.token = "secret_token".to_string();
        let json = serde_json::to_string(&config).unwrap();

        // Token should NOT be in the JSON
        assert!(!json.contains("secret_token"));
        assert!(!json.contains("token"));
    }

    #[test]
    fn test_get_api_key_prefers_token() {
        let mut config = Config::default();
        config.api_key = "legacy_key".to_string();
        config.token = "new_token".to_string();
        assert_eq!(config.get_api_key(), "new_token");
    }

    #[test]
    fn test_get_api_key_falls_back_to_api_key() {
        let mut config = Config::default();
        config.api_key = "legacy_key".to_string();
        assert_eq!(config.get_api_key(), "legacy_key");
    }

    #[test]
    fn test_has_token() {
        let mut config = Config::default();
        assert!(!config.has_token());

        config.token = "btstr_token123".to_string();
        assert!(config.has_token());

        config.token = "invalid_token".to_string();
        assert!(!config.has_token());

        config.token.clear();
        config.api_key = "legacy_key".to_string();
        assert!(!config.has_token());

        config.api_key = "btstr_legacy_key".to_string();
        assert!(config.has_token());
    }
}
