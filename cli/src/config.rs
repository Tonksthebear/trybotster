//! Configuration loading and persistence.
//!
//! Handles reading and writing the botster configuration file.
//! Sensitive tokens are stored in OS keyring via the keyring module.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::collections::HashMap;
use std::{fs, path::PathBuf};

use crate::keyring::Credentials;

/// Configuration for the botster CLI.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    /// URL of the botster server.
    pub server_url: String,
    /// API token - NOT serialized to disk (stored in keyring).
    #[serde(skip)]
    pub token: String,
    /// Interval in seconds between server polls.
    pub poll_interval: u64,
    /// Timeout in seconds before an idle agent is stopped.
    pub agent_timeout: u64,
    /// Maximum number of concurrent agent sessions.
    pub max_sessions: usize,
    /// Base directory for creating worktrees.
    pub worktree_base: PathBuf,
    /// Deprecated: hub names are now per-directory in `HubRegistry`.
    /// Kept for backwards-compatible deserialization of old config files.
    #[serde(default, skip_serializing)]
    pub hub_name: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        // Worktree base: in test mode use project tmp/, otherwise use home directory
        let worktree_base = if crate::env::is_any_test() {
            // Test mode: use project tmp/ to avoid leaking outside the project
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .map(|p| p.join("tmp/botster-sessions"))
                .unwrap_or_else(|| PathBuf::from("tmp/botster-sessions"))
        } else {
            dirs::home_dir()
                .map(|h| h.join("botster-sessions"))
                .unwrap_or_else(|| {
                    // Log warning - this will be caught when config is actually used
                    eprintln!("Warning: Could not determine home directory for worktree_base");
                    PathBuf::from("botster-sessions")
                })
        };

        Self {
            server_url: crate::env::DEFAULT_SERVER_URL.to_string(),
            token: String::new(),
            poll_interval: 5,
            agent_timeout: 3600,
            max_sessions: 20,
            worktree_base,
            hub_name: None,
        }
    }
}

impl Config {
    /// Returns the configuration directory path, creating it if necessary.
    ///
    /// Directory selection priority:
    /// 1. `#[cfg(test)]` (unit tests): `tmp/botster-test`
    /// 2. `BOTSTER_CONFIG_DIR` env var: explicit override
    /// 3. `BOTSTER_ENV=test`: `tmp/botster-test` (integration tests)
    /// 4. Default: platform config dir (macOS: ~/Library/Application Support/botster)
    pub fn config_dir() -> Result<PathBuf> {
        let dir = {
            #[cfg(test)]
            {
                // Unit tests: use repo's tmp/ directory (already gitignored)
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .context("cli/ has no parent directory")?
                    .join("tmp/botster-test")
            }

            #[cfg(not(test))]
            {
                if let Ok(test_dir) = std::env::var("BOTSTER_CONFIG_DIR") {
                    // Explicit override via env var
                    PathBuf::from(test_dir)
                } else if crate::env::should_skip_keyring() {
                    // Integration/system tests (BOTSTER_ENV=test or system_test): use repo's tmp/ directory
                    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .parent()
                        .context("cli/ has no parent directory")?
                        .join("tmp/botster-test")
                } else {
                    // Production: use platform-standard config directory
                    dirs::config_dir()
                        .context("Could not determine config directory")?
                        .join(crate::env::APP_NAME)
                }
            }
        };
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Loads configuration from file, with environment variable overrides.
    /// Token is loaded from consolidated keyring credentials (or env var).
    pub fn load() -> Result<Self> {
        let mut config = Self::load_from_file().unwrap_or_else(|_| Self::default());
        config.apply_env_overrides();

        // Load token from keyring if not set via env var
        if config.token.is_empty() {
            if let Ok(creds) = Credentials::load() {
                if let Some(token) = creds.api_token() {
                    config.token = token.to_string();
                }
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

    /// Get the API token for authentication.
    pub fn get_api_key(&self) -> &str {
        &self.token
    }

    /// Check if we have a valid authentication token.
    /// Only returns true if the token has the expected `btstr_` prefix.
    pub fn has_token(&self) -> bool {
        self.token.starts_with("btstr_")
    }

    /// Save a new device token to the consolidated keyring.
    pub fn save_token(&mut self, token: &str) -> Result<()> {
        self.token = token.to_string();

        // Load existing credentials, update token, save back
        let mut creds = Credentials::load().unwrap_or_default();
        creds.set_api_token(token.to_string());
        creds.save()?;

        Ok(())
    }

    /// Clear the token from keyring.
    pub fn clear_token(&mut self) -> Result<()> {
        self.token.clear();

        // Load existing credentials, clear token, save back
        let mut creds = Credentials::load().unwrap_or_default();
        creds.clear_api_token();
        creds.save()?;

        Ok(())
    }
}

/// Entry for a single hub in the registry.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HubEntry {
    /// User-chosen display name for this hub.
    pub name: String,
    /// Canonical repo/directory path (for debugging, not used for lookups).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_path: Option<String>,
}

/// Per-directory hub name registry.
///
/// Maps `hub_identifier` (SHA256 of repo/cwd path) to a `HubEntry` containing
/// the user-chosen display name. Stored at `{config_dir}/hub_registry.json`.
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct HubRegistry {
    hubs: HashMap<String, HubEntry>,
}

impl HubRegistry {
    /// Load the hub registry from disk, or return an empty registry.
    pub fn load() -> Self {
        Self::load_from_file().unwrap_or_default()
    }

    fn load_from_file() -> Result<Self> {
        let path = Self::registry_path()?;
        if path.exists() {
            let content = fs::read_to_string(&path)?;
            Ok(serde_json::from_str(&content)?)
        } else {
            Ok(Self::default())
        }
    }

    /// Persist the registry to disk.
    pub fn save(&self) -> Result<()> {
        let path = Self::registry_path()?;
        fs::write(&path, serde_json::to_string_pretty(self)?)?;

        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;

        Ok(())
    }

    /// Look up the hub name for a given identifier.
    pub fn get_hub_name(&self, hub_id: &str) -> Option<&str> {
        self.hubs.get(hub_id).map(|e| e.name.as_str())
    }

    /// Check if the registry has no entries.
    pub fn is_empty(&self) -> bool {
        self.hubs.is_empty()
    }

    /// Store a hub name for a given identifier.
    pub fn set_hub_name(&mut self, hub_id: &str, name: String, repo_path: Option<String>) {
        self.hubs.insert(
            hub_id.to_string(),
            HubEntry { name, repo_path },
        );
    }

    fn registry_path() -> Result<PathBuf> {
        Ok(Config::config_dir()?.join("hub_registry.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.server_url, crate::env::DEFAULT_SERVER_URL);
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
    fn test_get_api_key_returns_token() {
        let mut config = Config::default();
        config.token = "btstr_test123".to_string();
        assert_eq!(config.get_api_key(), "btstr_test123");
    }

    #[test]
    fn test_hub_registry_get_set() {
        let mut registry = HubRegistry::default();
        assert!(registry.get_hub_name("abc123").is_none());

        registry.set_hub_name("abc123", "my-api".to_string(), Some("/home/user/my-api".to_string()));
        assert_eq!(registry.get_hub_name("abc123"), Some("my-api"));

        registry.set_hub_name("def456", "frontend".to_string(), None);
        assert_eq!(registry.get_hub_name("def456"), Some("frontend"));
        assert_eq!(registry.get_hub_name("abc123"), Some("my-api"));
    }

    #[test]
    fn test_hub_registry_serialization() {
        let mut registry = HubRegistry::default();
        registry.set_hub_name("abc", "test-hub".to_string(), Some("/tmp/test".to_string()));

        let json = serde_json::to_string(&registry).unwrap();
        let loaded: HubRegistry = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.get_hub_name("abc"), Some("test-hub"));
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
        assert!(!config.has_token());
    }
}
