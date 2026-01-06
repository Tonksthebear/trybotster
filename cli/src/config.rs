use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    pub server_url: String,
    pub api_key: String,
    pub poll_interval: u64,
    pub agent_timeout: u64,
    pub max_sessions: usize,
    pub worktree_base: PathBuf,
    /// If true, CLI shares its public key with the server for convenience.
    /// If false (default), key exchange only happens via QR code (MITM-proof).
    #[serde(default)]
    pub server_assisted_pairing: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server_url: "https://trybotster.com".to_string(),
            api_key: String::new(),
            poll_interval: 5,
            agent_timeout: 3600,
            max_sessions: 20,
            worktree_base: dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("botster-sessions"),
            // Default to secure mode - public key only shared via QR code
            server_assisted_pairing: false,
        }
    }
}

impl Config {
    pub fn config_dir() -> Result<PathBuf> {
        // Allow tests to override the config directory
        let dir = if let Ok(test_dir) = std::env::var("BOTSTER_CONFIG_DIR") {
            PathBuf::from(test_dir)
        } else {
            dirs::home_dir()
                .context("No home directory")?
                .join(".botster_hub")
        };
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    pub fn load() -> Result<Self> {
        // Priority: Environment variables > config file > defaults
        let mut config = Self::load_from_file().unwrap_or_else(|_| Self::default());

        // Override with environment variables if present
        config.apply_env_overrides();

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
        // Essential config
        if let Ok(server_url) = std::env::var("BOTSTER_SERVER_URL") {
            self.server_url = server_url;
        }

        if let Ok(api_key) = std::env::var("BOTSTER_API_KEY") {
            self.api_key = api_key;
        }

        if let Ok(worktree_base) = std::env::var("BOTSTER_WORKTREE_BASE") {
            self.worktree_base = PathBuf::from(worktree_base);
        }

        // Optional config
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

        // Server-assisted pairing (convenience mode)
        // Set BOTSTER_SERVER_ASSISTED_PAIRING=true to enable
        // WARNING: This shares your public key with the server, enabling potential MITM
        if let Ok(val) = std::env::var("BOTSTER_SERVER_ASSISTED_PAIRING") {
            self.server_assisted_pairing = val.eq_ignore_ascii_case("true") || val == "1";
        }
    }

    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_dir()?.join("config.json");
        fs::write(&config_path, serde_json::to_string_pretty(self)?)?;
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
    fn test_config_serialization() {
        let config = Config::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(config.server_url, deserialized.server_url);
    }
}
