//! Tests for hub actions.

use std::path::PathBuf;

use super::*;
use crate::config::Config;

fn test_config() -> Config {
    Config {
        server_url: "http://localhost:3000".to_string(),
        token: "btstr_test-key".to_string(),
        poll_interval: 10,
        agent_timeout: 300,
        max_sessions: 10,
        worktree_base: PathBuf::from("/tmp/test-worktrees"),
    }
}

/// Create a Hub with TUI registered via Lua and crypto service initialized.
fn test_hub() -> Hub {
    use crate::relay::crypto_service::CryptoService;

    let config = test_config();
    let mut hub = Hub::new(config).unwrap();

    // Register TUI via Lua (Hub-side processing)
    let (_request_tx, request_rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
    let _output_rx = hub.register_tui_via_lua(request_rx);

    // Initialize crypto service for browser client tests
    let crypto_service = CryptoService::start("test-hub").unwrap();
    hub.browser.crypto_service = Some(crypto_service);

    hub
}

// === Connection URL Tests ===

/// Create a test PreKeyBundleData for unit tests.
fn create_test_prekey_bundle() -> crate::relay::PreKeyBundleData {
    use base64::{engine::general_purpose::STANDARD, Engine};

    crate::relay::PreKeyBundleData {
        version: 4,
        hub_id: "test-hub-123".to_string(),
        registration_id: 12345,
        device_id: 1,
        identity_key: STANDARD.encode([1u8; 33]),
        signed_prekey_id: 1,
        signed_prekey: STANDARD.encode([2u8; 33]),
        signed_prekey_signature: STANDARD.encode([3u8; 64]),
        prekey_id: Some(1),
        prekey: Some(STANDARD.encode([4u8; 33])),
        kyber_prekey_id: 1,
        kyber_prekey: STANDARD.encode([5u8; 1569]),
        kyber_prekey_signature: STANDARD.encode([6u8; 64]),
    }
}

/// TEST: Copy connection URL generates URL fresh from Signal bundle.
#[test]
fn test_copy_connection_url_generates_fresh_url() {
    let mut hub = test_hub();

    // Set up a mock Signal bundle (required for URL generation)
    hub.browser.signal_bundle = Some(create_test_prekey_bundle());

    // Dispatch CopyConnectionUrl action - should not panic
    dispatch(&mut hub, HubAction::CopyConnectionUrl);

    // Verify URL generation works
    let url = hub.generate_connection_url();
    assert!(
        url.is_ok(),
        "generate_connection_url should succeed with valid bundle"
    );

    let url = url.unwrap();
    assert!(
        url.contains(&hub.config.server_url),
        "URL should contain server URL"
    );
    assert!(url.contains("#"), "URL should contain fragment with bundle");
    assert!(
        url.len() > 100,
        "URL should contain substantial encoded bundle data"
    );
}

/// TEST: Copy connection URL fails gracefully when no Signal bundle is available.
#[test]
fn test_copy_connection_url_no_bundle_no_panic() {
    let mut hub = test_hub();

    // Verify no Signal bundle
    assert!(
        hub.browser.signal_bundle.is_none(),
        "Should start with no Signal bundle"
    );

    // This should not panic - error is logged but not propagated
    dispatch(&mut hub, HubAction::CopyConnectionUrl);
}

// === PTY Handle Tests ===

/// TEST: Agent get_pty_handle returns correct handle.
#[test]
fn test_agent_get_pty_handle_returns_valid_handle() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let temp_dir = TempDir::new().unwrap();
    let agent = Agent::new(
        Uuid::new_v4(),
        "test/repo".to_string(),
        Some(1),
        "test-branch".to_string(),
        temp_dir.path().to_path_buf(),
    );

    // CLI PTY (index 0) should always exist
    let cli_handle = agent.get_pty_handle(0);
    assert!(cli_handle.is_some(), "CLI PTY handle should exist");

    // Server PTY (index 1) doesn't exist for un-spawned agent
    let server_handle = agent.get_pty_handle(1);
    assert!(server_handle.is_none(), "Server PTY handle should not exist");

    // Invalid index
    let invalid_handle = agent.get_pty_handle(99);
    assert!(
        invalid_handle.is_none(),
        "Invalid PTY index should return None"
    );

    // Verify we can subscribe to events through the handle
    let handle = cli_handle.unwrap();
    let _rx = handle.subscribe();
}

// === BrowserCommand Serialization Tests ===

/// Test that non-PTY commands (ListAgents, SelectAgent, etc.) are ignored.
#[test]
fn test_pty_input_receiver_ignores_non_pty_commands() {
    use crate::relay::BrowserCommand;

    let non_pty_commands = [
        serde_json::to_string(&BrowserCommand::ListAgents).unwrap(),
        serde_json::to_string(&BrowserCommand::SelectAgent {
            id: "agent-123".to_string(),
        })
        .unwrap(),
        serde_json::to_string(&BrowserCommand::CreateAgent {
            issue_or_branch: Some("42".to_string()),
            prompt: None,
        })
        .unwrap(),
        serde_json::to_string(&BrowserCommand::DeleteAgent {
            id: "agent-123".to_string(),
            delete_worktree: Some(false),
        })
        .unwrap(),
    ];

    for cmd_json in &non_pty_commands {
        let parsed: Result<BrowserCommand, _> = serde_json::from_str(cmd_json);
        assert!(parsed.is_ok(), "Failed to parse: {}", cmd_json);

        let cmd = parsed.unwrap();
        assert!(
            !matches!(cmd, BrowserCommand::Input { .. } | BrowserCommand::Resize { .. }),
            "Expected non-PTY command, got {:?}",
            cmd
        );
    }
}

/// Test BrowserCommand serialization format for Input.
#[test]
fn test_browser_command_input_serialization() {
    use crate::relay::BrowserCommand;

    let cmd = BrowserCommand::Input {
        data: "hello".to_string(),
    };
    let json = serde_json::to_string(&cmd).unwrap();

    assert!(json.contains(r#""type":"input""#));
    assert!(json.contains(r#""data":"hello""#));

    let parsed: BrowserCommand = serde_json::from_str(&json).unwrap();
    match parsed {
        BrowserCommand::Input { data } => assert_eq!(data, "hello"),
        _ => panic!("Wrong variant"),
    }
}

/// Test BrowserCommand serialization format for Resize.
#[test]
fn test_browser_command_resize_serialization() {
    use crate::relay::BrowserCommand;

    let cmd = BrowserCommand::Resize { cols: 80, rows: 24 };
    let json = serde_json::to_string(&cmd).unwrap();

    assert!(json.contains(r#""type":"resize""#));
    assert!(json.contains(r#""cols":80"#));
    assert!(json.contains(r#""rows":24"#));

    let parsed: BrowserCommand = serde_json::from_str(&json).unwrap();
    match parsed {
        BrowserCommand::Resize { cols, rows } => {
            assert_eq!(cols, 80);
            assert_eq!(rows, 24);
        }
        _ => panic!("Wrong variant"),
    }
}
