//! Integration tests for device authorization flow.
//!
//! These tests verify the CLI correctly handles various authentication scenarios.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use tempfile::TempDir;

// Global lock to prevent env var pollution between tests
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Helper to set up a temporary config directory for tests
fn setup_test_env() -> (TempDir, std::sync::MutexGuard<'static, ()>) {
    let guard = ENV_LOCK.lock().unwrap();
    let temp_dir = TempDir::new().unwrap();

    // Clear any existing env vars
    env::remove_var("BOTSTER_TOKEN");
    env::remove_var("BOTSTER_API_KEY");
    env::remove_var("BOTSTER_SERVER_URL");

    // Set test config dir
    env::set_var("BOTSTER_CONFIG_DIR", temp_dir.path());
    // Disable browser opening in tests
    env::set_var("BOTSTER_NO_BROWSER", "1");

    (temp_dir, guard)
}

/// Helper to create a config file with a token
fn create_config_with_token(config_dir: &PathBuf, server_url: &str, token: &str) {
    let config = serde_json::json!({
        "server_url": server_url,
        "token": token,
        "api_key": "",
        "poll_interval": 5,
        "agent_timeout": 3600,
        "max_sessions": 20,
        "worktree_base": "/tmp/botster-sessions",
        "server_assisted_pairing": false
    });
    fs::create_dir_all(config_dir).unwrap();
    fs::write(
        config_dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
}

/// Helper to create a config file with legacy api_key
fn create_config_with_api_key(config_dir: &PathBuf, server_url: &str, api_key: &str) {
    let config = serde_json::json!({
        "server_url": server_url,
        "token": "",
        "api_key": api_key,
        "poll_interval": 5,
        "agent_timeout": 3600,
        "max_sessions": 20,
        "worktree_base": "/tmp/botster-sessions",
        "server_assisted_pairing": false
    });
    fs::create_dir_all(config_dir).unwrap();
    fs::write(
        config_dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
}

/// Helper to create an empty config file
fn create_empty_config(config_dir: &PathBuf, server_url: &str) {
    let config = serde_json::json!({
        "server_url": server_url,
        "token": "",
        "api_key": "",
        "poll_interval": 5,
        "agent_timeout": 3600,
        "max_sessions": 20,
        "worktree_base": "/tmp/botster-sessions",
        "server_assisted_pairing": false
    });
    fs::create_dir_all(config_dir).unwrap();
    fs::write(
        config_dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
}

mod validate_token_tests {
    use super::*;
    use botster_hub::auth;

    #[test]
    fn returns_false_for_empty_token() {
        let result = auth::validate_token("http://localhost:9999", "");
        assert!(!result, "Expected empty token to return false");
    }

    #[test]
    fn returns_false_for_unreachable_server() {
        // Use a port that's unlikely to have anything listening
        let result = auth::validate_token("http://127.0.0.1:59999", "btstr_some_token");
        assert!(!result, "Expected unreachable server to return false");
    }
}

mod config_tests {
    use super::*;
    use botster_hub::Config;

    #[test]
    fn loads_token_from_config_file() {
        let (temp_dir, _guard) = setup_test_env();
        create_config_with_token(
            &temp_dir.path().to_path_buf(),
            "https://test.example.com",
            "btstr_test_token",
        );

        let config = Config::load().unwrap();
        assert_eq!(config.get_api_key(), "btstr_test_token");
    }

    #[test]
    fn loads_legacy_api_key_from_config_file() {
        let (temp_dir, _guard) = setup_test_env();
        create_config_with_api_key(
            &temp_dir.path().to_path_buf(),
            "https://test.example.com",
            "legacy_api_key",
        );

        let config = Config::load().unwrap();
        assert_eq!(config.get_api_key(), "legacy_api_key");
    }

    #[test]
    fn token_takes_precedence_over_api_key() {
        let (temp_dir, _guard) = setup_test_env();
        let config_dir = temp_dir.path().to_path_buf();

        let config = serde_json::json!({
            "server_url": "https://test.example.com",
            "token": "btstr_new_token",
            "api_key": "legacy_api_key",
            "poll_interval": 5,
            "agent_timeout": 3600,
            "max_sessions": 20,
            "worktree_base": "/tmp/botster-sessions",
            "server_assisted_pairing": false
        });
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("config.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        )
        .unwrap();

        let loaded_config = Config::load().unwrap();
        assert_eq!(
            loaded_config.get_api_key(),
            "btstr_new_token",
            "token should take precedence over api_key"
        );
    }

    #[test]
    fn env_var_token_takes_precedence_over_config() {
        let (temp_dir, _guard) = setup_test_env();
        create_config_with_token(
            &temp_dir.path().to_path_buf(),
            "https://test.example.com",
            "btstr_config_token",
        );

        env::set_var("BOTSTER_TOKEN", "btstr_env_token");

        let config = Config::load().unwrap();
        assert_eq!(
            config.get_api_key(),
            "btstr_env_token",
            "env var should take precedence over config file"
        );
    }

    #[test]
    fn has_token_returns_false_when_empty() {
        let (temp_dir, _guard) = setup_test_env();
        create_empty_config(&temp_dir.path().to_path_buf(), "https://test.example.com");

        let config = Config::load().unwrap();
        assert!(
            !config.has_token(),
            "has_token should return false when both token and api_key are empty"
        );
    }

    #[test]
    fn has_token_returns_true_with_token() {
        let (temp_dir, _guard) = setup_test_env();
        create_config_with_token(
            &temp_dir.path().to_path_buf(),
            "https://test.example.com",
            "btstr_test_token",
        );

        let config = Config::load().unwrap();
        assert!(config.has_token(), "has_token should return true when token is set");
    }

    #[test]
    fn has_token_returns_false_with_legacy_api_key() {
        let (temp_dir, _guard) = setup_test_env();
        create_config_with_api_key(
            &temp_dir.path().to_path_buf(),
            "https://test.example.com",
            "legacy_key",
        );

        let config = Config::load().unwrap();
        // Legacy api_key without btstr_ prefix should not be considered valid
        // This triggers the device auth flow to get a proper token
        assert!(
            !config.has_token(),
            "has_token should return false for legacy api_key without btstr_ prefix"
        );
        // But get_api_key still returns the legacy value (for display/logging)
        assert_eq!(config.get_api_key(), "legacy_key");
    }

    #[test]
    fn save_token_persists_to_file() {
        let (temp_dir, _guard) = setup_test_env();
        create_config_with_token(
            &temp_dir.path().to_path_buf(),
            "https://test.example.com",
            "btstr_old_token",
        );

        let mut config = Config::load().unwrap();
        config.save_token("btstr_new_saved_token").unwrap();

        // Reload config from file (need to clear env vars again to read fresh)
        env::remove_var("BOTSTER_TOKEN");
        let reloaded = Config::load().unwrap();
        assert_eq!(
            reloaded.get_api_key(),
            "btstr_new_saved_token",
            "saved token should be persisted to file"
        );
    }

    #[test]
    fn clear_token_removes_from_file() {
        let (temp_dir, _guard) = setup_test_env();
        create_config_with_token(
            &temp_dir.path().to_path_buf(),
            "https://test.example.com",
            "btstr_old_token",
        );

        let mut config = Config::load().unwrap();
        assert!(config.has_token());

        config.clear_token().unwrap();

        let reloaded = Config::load().unwrap();
        assert!(
            reloaded.token.is_empty(),
            "token should be cleared from file"
        );
    }

    #[test]
    fn get_api_key_returns_empty_string_when_no_token() {
        let (temp_dir, _guard) = setup_test_env();
        create_empty_config(&temp_dir.path().to_path_buf(), "https://test.example.com");

        let config = Config::load().unwrap();
        assert_eq!(config.get_api_key(), "", "should return empty string when no token");
    }
}

/// Tests that verify the auth module response parsing
mod auth_response_parsing {
    use botster_hub::auth::{DeviceCodeResponse, ErrorResponse, TokenResponse};

    #[test]
    fn parses_device_code_response() {
        let json = r#"{
            "device_code": "abc123",
            "user_code": "WDJB-MJHT",
            "verification_uri": "https://example.com/device",
            "expires_in": 900,
            "interval": 5
        }"#;
        let resp: DeviceCodeResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.device_code, "abc123");
        assert_eq!(resp.user_code, "WDJB-MJHT");
        assert_eq!(resp.verification_uri, "https://example.com/device");
        assert_eq!(resp.expires_in, 900);
        assert_eq!(resp.interval, 5);
    }

    #[test]
    fn parses_token_response() {
        let json = r#"{
            "access_token": "btstr_xyz789abc",
            "token_type": "bearer"
        }"#;
        let resp: TokenResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.access_token, "btstr_xyz789abc");
        assert_eq!(resp.token_type, "bearer");
    }

    #[test]
    fn parses_error_response() {
        let json = r#"{"error": "authorization_pending"}"#;
        let resp: ErrorResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.error, "authorization_pending");
    }

    #[test]
    fn parses_access_denied_error() {
        let json = r#"{"error": "access_denied"}"#;
        let resp: ErrorResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.error, "access_denied");
    }

    #[test]
    fn parses_expired_token_error() {
        let json = r#"{"error": "expired_token"}"#;
        let resp: ErrorResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.error, "expired_token");
    }
}
