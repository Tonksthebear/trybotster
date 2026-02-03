//! Tests for hub actions.

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

/// Create a Hub with TuiClient task registered and crypto service initialized.
///
/// Most tests need TuiClient registered and some need crypto service for
/// browser client tests, so this helper initializes both.
fn test_hub() -> Hub {
    use crate::relay::crypto_service::CryptoService;

    let config = test_config();
    let mut hub = Hub::new(config).unwrap();

    // Register TuiClient as async task
    let (request_tx, request_rx) = tokio::sync::mpsc::unbounded_channel::<crate::client::TuiRequest>();
    let _output_rx = hub.register_tui_client_with_request_channel(request_rx);
    drop(request_tx); // Not needed for most tests

    // Initialize crypto service for browser client tests
    let crypto_service = CryptoService::start("test-hub").unwrap();
    hub.browser.crypto_service = Some(crypto_service);

    hub
}

// === Client lifecycle tests ===
//
// NOTE: ClientConnected and ClientDisconnected are now no-ops.
// Browser communication happens directly via WebRTC in server_comms.rs,
// bypassing the Client trait and ClientRegistry entirely.
// These tests verify the no-op behavior.

/// Test that ClientConnected is a no-op (does not register anything).
#[test]
fn test_client_connected_is_noop() {
    let mut hub = test_hub();

    let browser_id = ClientId::Browser("test-browser-identity".to_string());

    // Initially no browser client
    assert!(!hub.clients.contains(&browser_id));

    // Dispatch ClientConnected - should be a no-op
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser_id.clone(),
        },
    );

    // Browser client should still NOT be registered (no-op behavior)
    assert!(!hub.clients.contains(&browser_id));
}

/// Test that ClientDisconnected is a no-op.
#[test]
fn test_client_disconnected_is_noop() {
    let mut hub = test_hub();

    let browser_id = ClientId::Browser("test-browser-identity".to_string());

    // Dispatch ClientDisconnected - should be a no-op (nothing to disconnect)
    dispatch(
        &mut hub,
        HubAction::ClientDisconnected {
            client_id: browser_id.clone(),
        },
    );

    // Hub should still be functional
    assert!(!hub.clients.contains(&browser_id));
}

/// Test: Selection dispatch works with browser client ID.
///
/// Note: ClientConnected is a no-op, but SelectAgentForClient still works
/// because it doesn't require a registered client.
#[test]
fn test_selection_dispatch_works() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let mut hub = test_hub();

    // Create an agent
    let temp_dir = TempDir::new().unwrap();
    let agent = Agent::new(
        Uuid::new_v4(),
        "test/repo".to_string(),
        Some(1),
        "test-branch".to_string(),
        temp_dir.path().to_path_buf(),
    );
    hub.state
        .write()
        .unwrap()
        .add_agent("agent-1".to_string(), agent);

    // Browser selects the agent directly - should not panic
    // (Browser connections are now handled via WebRTC, not ClientRegistry)
    let browser_id = ClientId::Browser("browser-1".to_string());
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        },
    );

    // Agent still exists
    assert!(hub.state.read().unwrap().agents.contains_key("agent-1"));
}

/// Test: Switching between agents does not crash.
#[test]
fn test_switching_agents_does_not_crash() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let mut hub = test_hub();

    // Create two agents
    let temp_dir1 = TempDir::new().unwrap();
    let agent1 = Agent::new(
        Uuid::new_v4(),
        "test/repo".to_string(),
        Some(1),
        "branch-1".to_string(),
        temp_dir1.path().to_path_buf(),
    );
    hub.state
        .write()
        .unwrap()
        .add_agent("agent-1".to_string(), agent1);

    let temp_dir2 = TempDir::new().unwrap();
    let agent2 = Agent::new(
        Uuid::new_v4(),
        "test/repo".to_string(),
        Some(2),
        "branch-2".to_string(),
        temp_dir2.path().to_path_buf(),
    );
    hub.state
        .write()
        .unwrap()
        .add_agent("agent-2".to_string(), agent2);

    // Select agent-1 then agent-2 with browser client ID -- should not crash
    // (Browser connections are now handled via WebRTC, not ClientRegistry)
    let browser_id = ClientId::Browser("browser-1".to_string());
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        },
    );
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-2".to_string(),
        },
    );

    // Both agents still exist
    let state = hub.state.read().unwrap();
    assert!(state.agents.contains_key("agent-1"));
    assert!(state.agents.contains_key("agent-2"));
}

// === DeleteAgentForClient tests ===

/// Test that deleting non-existent agent is handled gracefully.
#[test]
fn test_delete_nonexistent_agent_is_graceful() {
    let mut hub = test_hub();

    // Attempt to delete an agent that doesn't exist - should not panic
    // (Browser connections are now handled via WebRTC, not ClientRegistry)
    let browser_id = ClientId::Browser("browser-1".to_string());
    dispatch(
        &mut hub,
        HubAction::DeleteAgentForClient {
            client_id: browser_id.clone(),
            request: crate::client::DeleteAgentRequest {
                agent_id: "nonexistent-agent".to_string(),
                delete_worktree: false,
            },
        },
    );

    // Hub should still be functional - no panic occurred
    assert!(hub.state.read().unwrap().agents.is_empty());
}

// === Request Routing Tests ===

/// Helper: Create hub with two agents.
fn setup_hub_with_two_agents() -> (Hub, tempfile::TempDir, tempfile::TempDir) {
    use crate::agent::Agent;
    use uuid::Uuid;

    let hub = test_hub();

    let temp_dir1 = tempfile::TempDir::new().unwrap();
    let agent1 = Agent::new(
        Uuid::new_v4(),
        "test/repo".to_string(),
        Some(1),
        "branch-1".to_string(),
        temp_dir1.path().to_path_buf(),
    );
    hub.state
        .write()
        .unwrap()
        .add_agent("agent-1".to_string(), agent1);

    let temp_dir2 = tempfile::TempDir::new().unwrap();
    let agent2 = Agent::new(
        Uuid::new_v4(),
        "test/repo".to_string(),
        Some(2),
        "branch-2".to_string(),
        temp_dir2.path().to_path_buf(),
    );
    hub.state
        .write()
        .unwrap()
        .add_agent("agent-2".to_string(), agent2);

    (hub, temp_dir1, temp_dir2)
}

/// TEST: RequestAgentList should not panic.
#[test]
fn test_request_agent_list_targets_requesting_browser() {
    let (mut hub, _td1, _td2) = setup_hub_with_two_agents();

    // Browser A requests agent list - should not panic
    // (Browser connections are now handled via WebRTC, not ClientRegistry)
    let browser_a = ClientId::Browser("browser-a".to_string());
    dispatch(
        &mut hub,
        HubAction::RequestAgentList {
            client_id: browser_a.clone(),
        },
    );
}

/// TEST: RequestWorktreeList should not panic.
#[test]
fn test_request_worktree_list_targets_requesting_browser() {
    let (mut hub, _td1, _td2) = setup_hub_with_two_agents();

    // Browser A requests worktree list - should not panic
    // (Browser connections are now handled via WebRTC, not ClientRegistry)
    let browser_a = ClientId::Browser("browser-a".to_string());
    dispatch(
        &mut hub,
        HubAction::RequestWorktreeList {
            client_id: browser_a.clone(),
        },
    );
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
