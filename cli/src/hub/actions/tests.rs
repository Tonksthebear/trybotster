//! Tests for hub actions.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use super::*;
use crate::config::Config;

/// Single shared tokio runtime for all action tests.
fn shared_test_runtime() -> Arc<tokio::runtime::Runtime> {
    static RT: OnceLock<Arc<tokio::runtime::Runtime>> = OnceLock::new();
    Arc::clone(RT.get_or_init(|| Arc::new(tokio::runtime::Runtime::new().unwrap())))
}

fn test_config() -> Config {
    let mut config = Config::default();
    config.server_url = "http://localhost:3000".to_string();
    config.token = "btstr_test-key".to_string();
    config.poll_interval = 10;
    config.agent_timeout = 300;
    config.max_sessions = 10;
    config.worktree_base = PathBuf::from("/tmp/test-worktrees");
    config
}

/// Create a Hub with TUI registered via Lua and pairing initialized.
fn test_hub() -> Hub {
    let config = test_config();
    let mut hub = Hub::with_runtime(config, shared_test_runtime()).unwrap();

    // Register TUI via Lua (Hub-side processing)
    let (_request_tx, request_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::client::TuiRequest>();
    let _output_rx = hub.register_tui_via_lua(request_rx);

    let signing_key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let mut authority = crate::relay::PairingAuthority::in_memory(
        signing_key,
        "/hubs/test-hub/signaling".to_string(),
    );
    hub.browser.pairing_bundle = Some(authority.create_pairing_bundle());
    hub.browser.pairing_authority = Some(authority);

    hub
}

// === Connection URL Tests ===

/// TEST: Copy connection URL generates a URL from the pairing bundle.
#[test]
fn test_copy_connection_url_generates_fresh_url() {
    let mut hub = test_hub();

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

/// TEST: Copy connection URL fails gracefully when no pairing bundle is available.
#[test]
fn test_copy_connection_url_no_bundle_no_panic() {
    let mut hub = test_hub();

    hub.browser.pairing_bundle = None;
    hub.browser.pairing_authority = None;

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
        "test-branch".to_string(),
        temp_dir.path().to_path_buf(),
    );

    // Single PTY handle should be accessible
    let handle = agent.get_pty_handle();
    // Verify we can subscribe to events through the handle
    let _rx = handle.subscribe();
}

// === BrowserCommand Serialization Tests ===

/// Test that non-PTY commands (SelectAgent, CreateAgent, etc.) are ignored.
#[test]
fn test_pty_input_receiver_ignores_non_pty_commands() {
    use crate::relay::BrowserCommand;

    let non_pty_commands = [
        serde_json::to_string(&BrowserCommand::SelectAgent {
            id: "agent-123".to_string(),
        })
        .unwrap(),
        serde_json::to_string(&BrowserCommand::CreateAgent {
            issue_or_branch: Some("42".to_string()),
            prompt: None,
            agent_name: None,
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
            !matches!(
                cmd,
                BrowserCommand::Input { .. } | BrowserCommand::Resize { .. }
            ),
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
