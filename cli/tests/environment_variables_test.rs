//! Comprehensive tests for environment variable handling.
//!
//! Tests use BOTSTER_CONFIG_DIR to isolate from real user config at ~/.botster_hub.
//! Tests are serialized via ENV_LOCK mutex to prevent env var contamination.

// Rust guideline compliant 2025-01

use botster_hub::Config;
use std::env;
use std::path::PathBuf;
use std::sync::Mutex;
use tempfile::TempDir;

/// Global lock to prevent env var pollution between tests (run serially).
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Helper to set environment variables for a test and clean them up after.
/// Also sets BOTSTER_CONFIG_DIR to a temp directory to isolate from real config.
struct EnvGuard {
    keys: Vec<String>,
    _temp_dir: TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn new() -> Self {
        // Acquire lock to serialize tests (prevents env var race conditions)
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // Create a temp directory for config isolation
        let temp_dir = TempDir::new().expect("Failed to create temp dir for test");

        // Set config dir to temp directory to avoid reading real config
        env::set_var("BOTSTER_CONFIG_DIR", temp_dir.path());

        // Clear all known botster env vars when creating a new guard
        env::remove_var("BOTSTER_SERVER_URL");
        env::remove_var("BOTSTER_TOKEN");
        env::remove_var("BOTSTER_WORKTREE_BASE");
        env::remove_var("BOTSTER_POLL_INTERVAL");
        env::remove_var("BOTSTER_MAX_SESSIONS");
        env::remove_var("BOTSTER_AGENT_TIMEOUT");

        // Clear any token from keyring to ensure test isolation
        if let Ok(mut config) = Config::load() {
            let _ = config.clear_token();
        }

        Self {
            keys: vec!["BOTSTER_CONFIG_DIR".to_string()],
            _temp_dir: temp_dir,
            _guard: guard,
        }
    }

    fn set(&mut self, key: &str, value: &str) {
        env::set_var(key, value);
        self.keys.push(key.to_string());
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // Clean up all environment variables we set
        for key in &self.keys {
            env::remove_var(key);
        }
        // Also ensure these common ones are cleaned
        env::remove_var("BOTSTER_SERVER_URL");
        env::remove_var("BOTSTER_TOKEN");
        env::remove_var("BOTSTER_WORKTREE_BASE");
        env::remove_var("BOTSTER_POLL_INTERVAL");
        env::remove_var("BOTSTER_MAX_SESSIONS");
        env::remove_var("BOTSTER_AGENT_TIMEOUT");
        env::remove_var("BOTSTER_CONFIG_DIR");
    }
}

#[test]
fn test_default_config_no_env_vars() {
    let _guard = EnvGuard::new();

    let config = Config::default();

    // Verify default values
    assert_eq!(config.server_url, "https://trybotster.com");
    assert_eq!(config.token, "");
    assert_eq!(config.poll_interval, 5);
    assert_eq!(config.agent_timeout, 3600);
    assert_eq!(config.max_sessions, 20);

    // Worktree base should be ~/botster-sessions
    assert!(config
        .worktree_base
        .to_string_lossy()
        .contains("botster-sessions"));
}

#[test]
fn test_env_override_server_url() {
    let mut guard = EnvGuard::new();
    guard.set("BOTSTER_SERVER_URL", "https://custom.example.com");

    let config = Config::load().unwrap();

    assert_eq!(config.server_url, "https://custom.example.com");
}

#[test]
fn test_env_override_token() {
    let mut guard = EnvGuard::new();
    guard.set("BOTSTER_TOKEN", "btstr_test_token_12345");

    let config = Config::load().unwrap();

    assert_eq!(config.token, "btstr_test_token_12345");
}

#[test]
fn test_env_override_worktree_base() {
    let temp_dir = TempDir::new().unwrap();
    let mut guard = EnvGuard::new();
    guard.set("BOTSTER_WORKTREE_BASE", temp_dir.path().to_str().unwrap());

    let config = Config::load().unwrap();

    assert_eq!(config.worktree_base, temp_dir.path());
}

#[test]
fn test_env_override_poll_interval() {
    let mut guard = EnvGuard::new();
    guard.set("BOTSTER_POLL_INTERVAL", "10");

    let config = Config::load().unwrap();

    assert_eq!(config.poll_interval, 10);
}

#[test]
fn test_env_override_poll_interval_invalid() {
    let mut guard = EnvGuard::new();
    guard.set("BOTSTER_POLL_INTERVAL", "invalid");

    let config = Config::load().unwrap();

    // Should fall back to default
    assert_eq!(config.poll_interval, 5);
}

#[test]
fn test_env_override_max_sessions() {
    let mut guard = EnvGuard::new();
    guard.set("BOTSTER_MAX_SESSIONS", "50");

    let config = Config::load().unwrap();

    assert_eq!(config.max_sessions, 50);
}

#[test]
fn test_env_override_max_sessions_invalid() {
    let mut guard = EnvGuard::new();
    guard.set("BOTSTER_MAX_SESSIONS", "not_a_number");

    let config = Config::load().unwrap();

    // Should fall back to default
    assert_eq!(config.max_sessions, 20);
}

#[test]
fn test_env_override_agent_timeout() {
    let mut guard = EnvGuard::new();
    guard.set("BOTSTER_AGENT_TIMEOUT", "7200");

    let config = Config::load().unwrap();

    assert_eq!(config.agent_timeout, 7200);
}

#[test]
fn test_env_override_agent_timeout_invalid() {
    let mut guard = EnvGuard::new();
    guard.set("BOTSTER_AGENT_TIMEOUT", "abc");

    let config = Config::load().unwrap();

    // Should fall back to default
    assert_eq!(config.agent_timeout, 3600);
}

#[test]
fn test_all_env_overrides_together() {
    let temp_dir = TempDir::new().unwrap();
    let mut guard = EnvGuard::new();

    guard.set("BOTSTER_SERVER_URL", "https://test.example.com");
    guard.set("BOTSTER_TOKEN", "btstr_test_key");
    guard.set("BOTSTER_WORKTREE_BASE", temp_dir.path().to_str().unwrap());
    guard.set("BOTSTER_POLL_INTERVAL", "15");
    guard.set("BOTSTER_MAX_SESSIONS", "100");
    guard.set("BOTSTER_AGENT_TIMEOUT", "9000");

    let config = Config::load().unwrap();

    assert_eq!(config.server_url, "https://test.example.com");
    assert_eq!(config.token, "btstr_test_key");
    assert_eq!(config.worktree_base, temp_dir.path());
    assert_eq!(config.poll_interval, 15);
    assert_eq!(config.max_sessions, 100);
    assert_eq!(config.agent_timeout, 9000);
}

#[test]
fn test_partial_env_overrides() {
    let mut guard = EnvGuard::new();

    // Only set some variables
    guard.set("BOTSTER_TOKEN", "btstr_partial_key");
    guard.set("BOTSTER_POLL_INTERVAL", "8");

    let config = Config::load().unwrap();

    // Overridden values
    assert_eq!(config.token, "btstr_partial_key");
    assert_eq!(config.poll_interval, 8);

    // Default values for non-overridden
    assert_eq!(
        config.server_url, "https://trybotster.com",
        "Server URL should be default"
    );
    assert_eq!(config.max_sessions, 20);
    assert_eq!(config.agent_timeout, 3600);
}

#[test]
fn test_env_priority_over_defaults() {
    let mut guard = EnvGuard::new();
    guard.set("BOTSTER_POLL_INTERVAL", "1");

    let config = Config::load().unwrap();

    // Environment variable should override default of 5
    assert_eq!(config.poll_interval, 1);
}

#[test]
fn test_env_zero_values() {
    let mut guard = EnvGuard::new();
    guard.set("BOTSTER_POLL_INTERVAL", "0");
    guard.set("BOTSTER_MAX_SESSIONS", "0");
    guard.set("BOTSTER_AGENT_TIMEOUT", "0");

    let config = Config::load().unwrap();

    // Zero values should be accepted
    assert_eq!(config.poll_interval, 0);
    assert_eq!(config.max_sessions, 0);
    assert_eq!(config.agent_timeout, 0);
}

#[test]
fn test_env_large_values() {
    let mut guard = EnvGuard::new();
    guard.set("BOTSTER_POLL_INTERVAL", "86400"); // 24 hours
    guard.set("BOTSTER_MAX_SESSIONS", "1000");
    guard.set("BOTSTER_AGENT_TIMEOUT", "604800"); // 1 week

    let config = Config::load().unwrap();

    assert_eq!(config.poll_interval, 86400);
    assert_eq!(config.max_sessions, 1000);
    assert_eq!(config.agent_timeout, 604800);
}

#[test]
fn test_env_negative_values_rejected() {
    let mut guard = EnvGuard::new();
    guard.set("BOTSTER_POLL_INTERVAL", "-5");
    guard.set("BOTSTER_MAX_SESSIONS", "-10");

    let config = Config::load().unwrap();

    // Negative values should be rejected, fall back to defaults
    assert_eq!(config.poll_interval, 5);
    assert_eq!(config.max_sessions, 20);
}

#[test]
fn test_env_empty_string_values() {
    let mut guard = EnvGuard::new();
    guard.set("BOTSTER_SERVER_URL", "");
    guard.set("BOTSTER_TOKEN", "");

    let config = Config::load().unwrap();

    // Empty strings should be accepted (they override defaults)
    assert_eq!(config.server_url, "");
    assert_eq!(config.token, "");
}

#[test]
fn test_env_whitespace_values() {
    let mut guard = EnvGuard::new();
    guard.set("BOTSTER_SERVER_URL", "  https://example.com  ");
    guard.set("BOTSTER_TOKEN", " key ");

    let config = Config::load().unwrap();

    // Whitespace is preserved (not trimmed)
    assert_eq!(config.server_url, "  https://example.com  ");
    assert_eq!(config.token, " key ");
}

#[test]
fn test_env_special_characters_in_token() {
    let mut guard = EnvGuard::new();
    guard.set(
        "BOTSTER_TOKEN",
        "btstr_key-with-special!@#$%^&*()_+=[]{}|;:,.<>?",
    );

    let config = Config::load().unwrap();

    assert_eq!(
        config.token,
        "btstr_key-with-special!@#$%^&*()_+=[]{}|;:,.<>?"
    );
}

#[test]
fn test_env_url_formats() {
    let test_cases = vec![
        "http://localhost:3000",
        "https://api.example.com",
        "https://api.example.com:8080",
        "https://api.example.com/v1",
        "http://127.0.0.1:8080",
    ];

    for url in test_cases {
        let mut guard = EnvGuard::new();
        guard.set("BOTSTER_SERVER_URL", url);

        let config = Config::load().unwrap();
        assert_eq!(config.server_url, url);

        // Explicitly drop to ensure cleanup before next iteration
        drop(guard);
    }
}

#[test]
fn test_env_worktree_base_relative_path() {
    let mut guard = EnvGuard::new();
    guard.set("BOTSTER_WORKTREE_BASE", "./relative/path");

    let config = Config::load().unwrap();

    assert_eq!(config.worktree_base, PathBuf::from("./relative/path"));
}

#[test]
fn test_env_worktree_base_absolute_path() {
    let mut guard = EnvGuard::new();
    guard.set("BOTSTER_WORKTREE_BASE", "/absolute/path/to/worktrees");

    let config = Config::load().unwrap();

    assert_eq!(
        config.worktree_base,
        PathBuf::from("/absolute/path/to/worktrees")
    );
}

#[test]
fn test_env_worktree_base_with_tilde() {
    let mut guard = EnvGuard::new();
    guard.set("BOTSTER_WORKTREE_BASE", "~/custom-worktrees");

    let config = Config::load().unwrap();

    // Tilde is NOT expanded by the config module (would need shellexpand)
    assert_eq!(config.worktree_base, PathBuf::from("~/custom-worktrees"));
}

#[test]
fn test_config_save_and_load_preserves_values() {
    let temp_dir = TempDir::new().unwrap();

    // Create a config with custom values
    let mut config = Config::default();
    config.server_url = "https://saved.example.com".to_string();
    // Note: token is #[serde(skip)] - stored in keyring, not file
    config.poll_interval = 42;
    config.max_sessions = 77;
    config.agent_timeout = 1234;
    config.worktree_base = temp_dir.path().to_path_buf();

    // Save to JSON
    let json = serde_json::to_string(&config).unwrap();

    // Load back
    let loaded: Config = serde_json::from_str(&json).unwrap();

    assert_eq!(loaded.server_url, "https://saved.example.com");
    // token is not serialized (uses keyring)
    assert_eq!(loaded.token, ""); // Skipped field defaults to empty
    assert_eq!(loaded.poll_interval, 42);
    assert_eq!(loaded.max_sessions, 77);
    assert_eq!(loaded.agent_timeout, 1234);
    assert_eq!(loaded.worktree_base, temp_dir.path());
}

#[test]
fn test_config_serialization_format() {
    let config = Config::default();
    let json = serde_json::to_string_pretty(&config).unwrap();

    // Verify JSON contains expected fields
    // Note: token is NOT serialized (it uses keyring via #[serde(skip)])
    assert!(json.contains("server_url"));
    assert!(!json.contains("token"), "token should not be serialized");
    assert!(json.contains("poll_interval"));
    assert!(json.contains("agent_timeout"));
    assert!(json.contains("max_sessions"));
    assert!(json.contains("worktree_base"));
}

/// Document all expected environment variables.
#[test]
fn test_documented_environment_variables() {
    // This test serves as documentation for all environment variables

    let expected_vars = vec![
        (
            "BOTSTER_SERVER_URL",
            "string",
            "https://trybotster.com",
            "URL of the botster server",
        ),
        (
            "BOTSTER_TOKEN",
            "string",
            "",
            "API token for authentication (btstr_ prefix)",
        ),
        (
            "BOTSTER_WORKTREE_BASE",
            "path",
            "~/botster-sessions",
            "Base directory for git worktrees",
        ),
        (
            "BOTSTER_POLL_INTERVAL",
            "u64",
            "5",
            "Seconds between server polls",
        ),
        (
            "BOTSTER_MAX_SESSIONS",
            "usize",
            "20",
            "Maximum concurrent agent sessions",
        ),
        (
            "BOTSTER_AGENT_TIMEOUT",
            "u64",
            "3600",
            "Agent timeout in seconds",
        ),
    ];

    // This test always passes, it's just documentation
    for (var_name, var_type, default, description) in expected_vars {
        println!(
            "{}: {} (default: {}) - {}",
            var_name, var_type, default, description
        );
    }

    assert!(true, "Environment variables documented");
}

#[test]
fn test_environment_variable_types_and_validation() {
    // Numeric types must be valid numbers
    let numeric_vars = vec![
        ("BOTSTER_POLL_INTERVAL", "5"),
        ("BOTSTER_MAX_SESSIONS", "20"),
        ("BOTSTER_AGENT_TIMEOUT", "3600"),
    ];

    for (var_name, valid_value) in numeric_vars {
        let mut guard = EnvGuard::new();
        guard.set(var_name, valid_value);

        // Should load successfully with valid values
        let config = Config::load();
        assert!(
            config.is_ok(),
            "{} should accept valid numeric value",
            var_name
        );
    }
}

#[test]
fn test_get_api_key_returns_token() {
    let mut guard = EnvGuard::new();
    guard.set("BOTSTER_TOKEN", "btstr_test_token");

    let config = Config::load().unwrap();

    // get_api_key() is a legacy accessor that returns &self.token
    assert_eq!(config.get_api_key(), "btstr_test_token");
}

#[test]
fn test_has_token_validates_prefix() {
    let _guard = EnvGuard::new();

    let mut config = Config::default();

    // No token
    assert!(!config.has_token());

    // Valid token with btstr_ prefix
    config.token = "btstr_valid_token".to_string();
    assert!(config.has_token());

    // Invalid token without prefix
    config.token = "invalid_no_prefix".to_string();
    assert!(!config.has_token());
}
