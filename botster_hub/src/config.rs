use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    pub server_url: String,
    pub api_key: String,
    pub agent_command: String,
    pub poll_interval: u64,
    pub agent_timeout: u64,
    pub max_sessions: usize,
    pub worktree_base: PathBuf,
    pub claude_permission_mode: String,
    pub claude_allowed_tools: String,
    pub preserve_agent_ansi: bool,
    pub spawn_mode: String, // "embedded" or "external"
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server_url: "http://localhost:3000".to_string(),
            api_key: String::new(),
            agent_command: "claude".to_string(),
            poll_interval: 5,
            agent_timeout: 3600,
            max_sessions: 20,
            worktree_base: dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("botster-sessions"),
            claude_permission_mode: "acceptEdits".to_string(),
            claude_allowed_tools: "mcp__*".to_string(),
            preserve_agent_ansi: false,
            spawn_mode: "external".to_string(), // external = better for TUI apps!
        }
    }
}

impl Config {
    pub fn config_dir() -> Result<PathBuf> {
        let dir = dirs::home_dir()
            .context("No home directory")?
            .join(".botster_hub");
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    pub fn load() -> Result<Self> {
        let config_path = Self::config_dir()?.join("config.json");
        if config_path.exists() {
            let content = fs::read_to_string(&config_path)?;
            Ok(serde_json::from_str(&content)?)
        } else {
            let default = Self::default();
            default.save()?;
            Ok(default)
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
        assert_eq!(config.server_url, "http://localhost:3000");
        assert_eq!(config.agent_command, "claude");
        assert_eq!(config.poll_interval, 5);
        assert_eq!(config.max_sessions, 20);
    }

    #[test]
    fn test_config_serialization() {
        let config = Config::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(config.server_url, deserialized.server_url);
    }
}
