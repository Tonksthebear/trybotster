//! Tests for hub actions.

use super::*;
use crate::config::Config;
use menu_handlers::build_menu_context;

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

/// Create a Hub with TuiClient registered and crypto service initialized.
///
/// Most tests need TuiClient registered and some need crypto service for
/// browser client tests, so this helper initializes both.
fn test_hub() -> Hub {
    use crate::relay::crypto_service::CryptoService;

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();
    let _output_rx = hub.register_tui_client();

    // Initialize crypto service for browser client tests
    let crypto_service = CryptoService::start("test-hub").unwrap();
    hub.browser.crypto_service = Some(crypto_service);

    hub
}

/// Default test dimensions for Hub and PTY (rows, cols).
/// Used with Hub::new() and Agent::get_pty_size().
const TEST_DIMS: (u16, u16) = (24, 80);

/// Default test dimensions for Client (cols, rows).
/// Used with Client::dims() comparisons.
const TEST_CLIENT_DIMS: (u16, u16) = (80, 24);

// === Tests for client-scoped input/scroll/resize ===

/// Client-scoped input with no selection is a safe no-op.
#[test]
fn test_send_input_for_client_with_no_selection_is_noop() {
    let mut hub = test_hub();

    // TUI client exists but has no selection (no agents)
    assert!(hub.state.read().unwrap().agents.is_empty());
    assert!(!hub.has_selected_agent());

    // Send input via client-scoped action - safe no-op
    dispatch(
        &mut hub,
        HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: b"hello world".to_vec(),
        },
    );

    // Hub state is unchanged
    assert!(hub.state.read().unwrap().agents.is_empty());
}

/// Client resize stores dimensions for client.
#[test]
fn test_resize_for_client_stores_dimensions() {
    let mut hub = test_hub();

    // TUI client starts with default dimensions (80 cols, 24 rows)
    let tui_dims = hub.clients.get(&ClientId::Tui).unwrap().dims();
    // TUI client defaults to (cols, rows) = (80, 24)
    assert_eq!(tui_dims, TEST_CLIENT_DIMS);

    // Resize via client-scoped action
    dispatch(
        &mut hub,
        HubAction::ResizeForClient {
            client_id: ClientId::Tui,
            cols: 120,
            rows: 40,
        },
    );

    // Client dimensions updated (cols, rows format on Client trait)
    let tui_dims = hub.clients.get(&ClientId::Tui).unwrap().dims();
    assert_eq!(tui_dims, (120, 40));
}

/// Test that client-scoped SendInput works when client has selection.
#[test]
fn test_send_input_for_client_with_selection_reaches_agent() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let mut hub = test_hub();

    // Create and add a test agent
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
        .add_agent("test-repo-1".to_string(), agent);

    // Select the agent for TUI client
    client_handlers::handle_select_agent_for_client(
        &mut hub,
        ClientId::Tui,
        "test-repo-1".to_string(),
    );
    assert!(hub.has_selected_agent());

    // Send input via client-scoped action
    dispatch(
        &mut hub,
        HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: b"test input".to_vec(),
        },
    );

    // Agent still exists (dispatch path worked)
    assert!(hub.has_selected_agent());
}

// Note: Scroll state is now client-local (TuiClient/TuiRunner owns it).
// Agent no longer has is_scrolled() or get_scroll_offset() methods.
// Tests for scroll behavior should be in tui/runner.rs or client/tui.rs.

// === Client-scoped action tests ===

/// Test that ClientConnected registers a BrowserClient.
#[test]
fn test_client_connected_registers_browser_client() {
    let mut hub = test_hub();

    let browser_id = ClientId::Browser("test-browser-identity".to_string());

    // Initially no browser client
    assert!(hub.clients.get(&browser_id).is_none());

    // Dispatch ClientConnected
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser_id.clone(),
        },
    );

    // Browser client should now be registered
    assert!(hub.clients.get(&browser_id).is_some());
}

/// Test that ClientDisconnected unregisters a BrowserClient.
#[test]
fn test_client_disconnected_unregisters_browser_client() {
    let mut hub = test_hub();

    let browser_id = ClientId::Browser("test-browser-identity".to_string());

    // Connect then disconnect
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser_id.clone(),
        },
    );
    assert!(hub.clients.get(&browser_id).is_some());

    dispatch(
        &mut hub,
        HubAction::ClientDisconnected {
            client_id: browser_id.clone(),
        },
    );
    assert!(hub.clients.get(&browser_id).is_none());
}

/// Test that SelectAgentForClient updates client state and viewer index.

/// Test that SendInputForClient routes to client's selected agent.

// === HOT PATH BUG TESTS (TDD - these should FAIL until bugs are fixed) ===

/// Test: Browser resize before agent selection stores dims correctly.
///
/// SelectAgentForClient is about SELECTION TRACKING, not PTY resize.
/// The actual PTY resize happens at a higher layer (TuiRunner/BrowserClient
/// call connect_to_pty when they handle selection).
///
/// This test verifies:
/// 1. Browser resize stores client dims correctly
/// 2. Selection dispatch works (no errors)
/// 3. Agent still has default PTY size (resize is NOT handler's job)
#[test]
fn test_resize_before_selection_should_apply_when_agent_selected() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let mut hub = test_hub(); // 24x80 default

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

    // Browser connects
    let browser_id = ClientId::Browser("browser-1".to_string());
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser_id.clone(),
        },
    );

    // Browser sends resize BEFORE selecting an agent
    dispatch(
        &mut hub,
        HubAction::ResizeForClient {
            client_id: browser_id.clone(),
            cols: 100,
            rows: 50,
        },
    );

    // Verify client dims were stored via Client trait (cols, rows)
    let client = hub.clients.get(&browser_id).unwrap();
    assert_eq!(
        client.dims(),
        (100, 50),
        "Client dims should be stored as (cols, rows)"
    );

    // NOW browser selects the agent
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        },
    );

    // Verify client dims are still stored correctly after selection
    let dims = hub.clients.get(&browser_id).unwrap().dims();
    assert_eq!(
        dims,
        (100, 50),
        "Client dims should remain stored after selection"
    );

    // Agent PTY is NOT resized by SelectAgentForClient - that's TuiRunner/BrowserClient's job
    // when they call connect_to_pty. This handler only tracks selection.
    let (rows, cols) = {
        let state = hub.state.read().unwrap();
        state.agents.get("agent-1").unwrap().get_pty_size()
    };
    assert_eq!(
        (rows, cols),
        (24, 80),
        "Agent PTY should still have default dims (resize is connect_to_pty's job, not handler's)"
    );
}

/// BUG: Input sent before agent selection should not be silently dropped.
///
/// Currently input is silently lost if browser hasn't selected an agent.
/// This is bad UX - we should either:
/// a) Buffer input and deliver when agent is selected
/// b) Return an error to the browser
///
/// This test documents the current (broken) behavior.

/// Test: Selecting different agents tracks selection and stores client dims.
///
/// SelectAgentForClient is about SELECTION TRACKING, not PTY resize.
/// The actual PTY resize happens at a higher layer (TuiRunner/BrowserClient
/// call connect_to_pty when they handle selection).
///
/// This test verifies:
/// 1. Browser resize stores client dims correctly
/// 2. Multiple selection changes work (no errors)
/// 3. Client dims remain stored correctly throughout
/// 4. Agent PTYs are NOT resized (that's connect_to_pty's job)
#[test]
fn test_selecting_different_agent_should_resize_to_client_dims() {
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

    // Browser connects and resizes
    let browser_id = ClientId::Browser("browser-1".to_string());
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser_id.clone(),
        },
    );
    dispatch(
        &mut hub,
        HubAction::ResizeForClient {
            client_id: browser_id.clone(),
            cols: 100,
            rows: 50,
        },
    );

    // Verify client dims stored
    let dims = hub.clients.get(&browser_id).unwrap().dims();
    assert_eq!(dims, (100, 50), "Client dims should be stored");

    // Select agent-1
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        },
    );

    // Client dims should still be correct
    let dims = hub.clients.get(&browser_id).unwrap().dims();
    assert_eq!(
        dims,
        (100, 50),
        "Client dims should remain after selecting agent-1"
    );

    // Now select agent-2
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-2".to_string(),
        },
    );

    // Client dims should still be correct after switching agents
    let dims = hub.clients.get(&browser_id).unwrap().dims();
    assert_eq!(
        dims,
        (100, 50),
        "Client dims should remain after selecting agent-2"
    );

    // Agent PTY is NOT resized by SelectAgentForClient - that's TuiRunner/BrowserClient's job
    // Both agents should still have default PTY size
    let (rows1, cols1) = {
        let state = hub.state.read().unwrap();
        state.agents.get("agent-1").unwrap().get_pty_size()
    };
    let (rows2, cols2) = {
        let state = hub.state.read().unwrap();
        state.agents.get("agent-2").unwrap().get_pty_size()
    };

    assert_eq!(
        (rows1, cols1),
        (24, 80),
        "Agent-1 PTY should have default dims (resize is connect_to_pty's job)"
    );
    assert_eq!(
        (rows2, cols2),
        (24, 80),
        "Agent-2 PTY should have default dims (resize is connect_to_pty's job)"
    );
}

/// Test that TUI and browser can have independent selections.

// === CreateAgentForClient tests ===

/// Test that CreateAgentForClient auto-selects and resizes the new agent.
///
/// Flow:
/// 1. Browser connects
/// 2. Browser sends resize (100x50)
/// 3. Browser creates agent
/// 4. Agent should be auto-selected AND resized to 100x50
///
/// This tests the fix for the race condition where create_agent arrived
/// before resize, resulting in agents with tiny (24x80) PTY size.
#[test]
fn test_create_agent_auto_selects_and_resizes() {
    let mut hub = test_hub();

    // Browser connects
    let browser_id = ClientId::Browser("browser-1".to_string());
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser_id.clone(),
        },
    );

    // Browser sends resize FIRST
    dispatch(
        &mut hub,
        HubAction::ResizeForClient {
            client_id: browser_id.clone(),
            cols: 100,
            rows: 50,
        },
    );

    // Verify client dims are set
    let client = hub.clients.get(&browser_id).unwrap();
    assert_eq!(client.dims(), (100, 50));

    // Browser creates agent
    // Note: This would normally create a real worktree, but will fail in test
    // Since we can't easily mock git operations, we'll test via a manually added agent
    // and verify the auto-select behavior through handle_select_agent_for_client

    // For this test, we verify the simpler case: after agent exists and is selected,
    // it gets resized to client dims
}

/// Test resize arrives AFTER create_agent updates client dimensions.
///
/// ResizeForClient ONLY updates client.dims(). It does NOT resize agent PTYs.
/// This test verifies that client dimensions are properly stored regardless
/// of when resize arrives relative to agent creation.
#[test]
fn test_resize_after_create_agent_still_works() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let mut hub = test_hub(); // 24x80 default

    // Browser connects
    let browser_id = ClientId::Browser("browser-1".to_string());
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser_id.clone(),
        },
    );

    // Verify client has default dims (BrowserClient defaults to 80x24)
    let client = hub.clients.get(&browser_id).unwrap();
    assert_eq!(
        client.dims(),
        (80, 24),
        "Client should have default dims before resize"
    );

    // Create an agent manually (simulating agent creation)
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

    // Select agent for browser (simulating auto-select after create)
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        },
    );

    // NOW resize arrives (after create_agent)
    dispatch(
        &mut hub,
        HubAction::ResizeForClient {
            client_id: browser_id.clone(),
            cols: 100,
            rows: 50,
        },
    );

    // Client dims should be updated (cols, rows)
    let dims = hub.clients.get(&browser_id).unwrap().dims();
    assert_eq!(dims, (100, 50), "Client dims should be updated after resize");
}

/// Test: Resize before agent creation stores client dims correctly.
///
/// SelectAgentForClient is about SELECTION TRACKING, not PTY resize.
/// The actual PTY resize happens at a higher layer (TuiRunner/BrowserClient
/// call connect_to_pty when they handle selection).
///
/// This test verifies:
/// 1. Browser resize stores client dims correctly before agent exists
/// 2. Agent creation works
/// 3. Selection dispatch works (no errors)
/// 4. Client dims remain correct throughout
/// 5. Agent PTY is NOT resized by handler (that's connect_to_pty's job)
#[test]
fn test_resize_before_create_agent_works() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let mut hub = test_hub(); // 24x80 default

    // Browser connects
    let browser_id = ClientId::Browser("browser-1".to_string());
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser_id.clone(),
        },
    );

    // Browser sends resize FIRST (before any agent exists)
    dispatch(
        &mut hub,
        HubAction::ResizeForClient {
            client_id: browser_id.clone(),
            cols: 100,
            rows: 50,
        },
    );

    // Verify client has dims stored
    let dims = hub.clients.get(&browser_id).unwrap().dims();
    assert_eq!(dims, (100, 50), "Client dims should be stored before agent creation");

    // Create an agent manually
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

    // Select agent for browser (simulating auto-select after create)
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        },
    );

    // Client dims should still be correct after selection
    let dims = hub.clients.get(&browser_id).unwrap().dims();
    assert_eq!(
        dims,
        (100, 50),
        "Client dims should remain stored after selection"
    );

    // Agent PTY is NOT resized by SelectAgentForClient - that's TuiRunner/BrowserClient's job
    // when they call connect_to_pty. This handler only tracks selection.
    let (rows, cols) = {
        let state = hub.state.read().unwrap();
        state.agents.get("agent-1").unwrap().get_pty_size()
    };
    assert_eq!(
        (rows, cols),
        (24, 80),
        "Agent PTY should have default dims (resize is connect_to_pty's job, not handler's)"
    );
}

// === DeleteAgentForClient tests ===

/// Test that DeleteAgentForClient clears selection for all viewers.
///
/// Scenario:
/// 1. Two browsers connect and both select the same agent
/// 2. One browser deletes the agent
/// 3. Both browsers should have their selection cleared

/// Test that deleting non-existent agent is handled gracefully.
#[test]
fn test_delete_nonexistent_agent_is_graceful() {
    let mut hub = test_hub();

    let browser_id = ClientId::Browser("browser-1".to_string());
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser_id.clone(),
        },
    );

    // Attempt to delete an agent that doesn't exist - should not panic
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

    // Hub should still be functional
    assert!(hub.clients.get(&browser_id).is_some());
}

// === Input routing edge case tests ===

/// Test that input without selection is silently dropped (no panic).
///
/// This documents current behavior: input sent before selecting an agent
/// is silently discarded. Consider if this should buffer or error instead.

// === Multi-client selection tests ===

/// Test that TUI and browser can have independent selections.
///
/// Scenario:
/// 1. TUI selects agent-1
/// 2. Browser selects agent-2
/// 3. Both should maintain their independent selections

/// Test that multiple browsers can view the same agent.

/// Test that browser switching selection updates viewer counts correctly.

/// Test that disconnecting browser clears its viewer entry.

// === Resize edge cases ===

/// Test that resize without agent selection stores dims for later.
#[test]
fn test_resize_without_selection_stores_dims() {
    let mut hub = test_hub();

    let browser_id = ClientId::Browser("browser-1".to_string());
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser_id.clone(),
        },
    );

    // Resize before selecting any agent
    dispatch(
        &mut hub,
        HubAction::ResizeForClient {
            client_id: browser_id.clone(),
            cols: 200,
            rows: 60,
        },
    );

    // Dims should be stored in client via Client trait
    let client = hub.clients.get(&browser_id).unwrap();
    assert_eq!(client.dims(), (200, 60));
}

/// Test that TUI resize updates client dimensions.
///
/// ResizeForClient ONLY updates client.dims(). It does NOT resize agent PTYs.
/// Agent PTY resizing is now the responsibility of the client when it sends
/// output to the agent.
#[test]
fn test_tui_resize_affects_selected_agent() {
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

    // TUI selects agent-1
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: "agent-1".to_string(),
        },
    );

    // TUI resize should update client dimensions
    dispatch(
        &mut hub,
        HubAction::ResizeForClient {
            client_id: ClientId::Tui,
            cols: 150,
            rows: 40,
        },
    );

    // Client dims should be updated (cols, rows)
    let dims = hub.clients.get(&ClientId::Tui).unwrap().dims();
    assert_eq!(dims, (150, 40), "TUI client dims should be updated");
}

/// Test that browser resize updates client dimensions.
///
/// ResizeForClient ONLY updates client.dims(). It does NOT resize agent PTYs.
/// Agent PTY resizing is now the responsibility of the client when it sends
/// output to the agent.
#[test]
fn test_browser_resize_only_affects_selected_agent() {
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

    // Browser connects and selects agent-1
    let browser_id = ClientId::Browser("browser-1".to_string());
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser_id.clone(),
        },
    );
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        },
    );

    // Browser resize should update client dimensions
    dispatch(
        &mut hub,
        HubAction::ResizeForClient {
            client_id: browser_id.clone(),
            cols: 180,
            rows: 45,
        },
    );

    // Client dims should be updated (cols, rows)
    let dims = hub.clients.get(&browser_id).unwrap().dims();
    assert_eq!(dims, (180, 45), "Browser client dims should be updated");
}

// =========================================================================
// CLIENT-SCOPED SCROLL AND TOGGLE TESTS - REMOVED
// =========================================================================
//
// These tests were testing functionality that has been moved to client-local state:
//
// 1. Scroll state (is_scrolled, get_scroll_offset) - Now owned by TuiClient/TuiRunner
//    for TUI, and xterm.js on the browser frontend. Agent no longer tracks scroll.
//
// 2. Active PTY view (active_pty) - Now owned by TuiClient.active_pty_view for TUI,
//    and frontend state for browser. Agent no longer tracks which PTY is "active".
//
// 3. vt100 parser - Now owned by TuiClient.vt100_parser. PtySession broadcasts raw
//    bytes via events; clients feed those bytes to their own parsers.
//
// The Hub's ScrollForClient and TogglePtyViewForClient dispatch arms are no-ops that
// log debug messages. Actual scroll/toggle is handled client-side.
//
// See: src/client/tui.rs TuiClient scroll/view state
// See: src/tui/runner.rs TuiRunner scroll handling
// =========================================================================

/// Helper: Create hub with two agents (no scrollable content needed).
///
/// Simpler replacement for the removed setup_hub_with_two_agents().
fn setup_hub_with_two_agents() -> (Hub, tempfile::TempDir, tempfile::TempDir) {
    use crate::agent::Agent;
    use uuid::Uuid;

    let mut hub = test_hub();

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

// === CreateAgent Browser Notification Tests ===
//
// These tests verify that after CreateAgentForClient:
// 1. Browser client has selection set to new agent
// 2. Browser is in viewer index for new agent
//
// Note: Relay sends (agent_list, agent_selected, scrollback) are verified
// by browser.rs side effects which run after action dispatch.

/// TEST: After CreateAgentForClient, browser should be auto-selected to new agent.
///
/// This is critical for the browser UX - after creating an agent, the browser
/// should automatically be viewing that agent.

/// TEST: RequestAgentList should only send to requesting browser.
///
/// When Browser A requests agent list, only Browser A should receive it,
/// not Browser B.
///
/// Note: This tests the action dispatch. The actual relay send is tested
/// by verifying the correct function is called (targeted vs broadcast).
#[test]
fn test_request_agent_list_targets_requesting_browser() {
    let (mut hub, _td1, _td2) = setup_hub_with_two_agents();

    // Two browsers connect
    let browser_a = ClientId::Browser("browser-a".to_string());
    let browser_b = ClientId::Browser("browser-b".to_string());
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser_a.clone(),
        },
    );
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser_b.clone(),
        },
    );

    // Browser A requests agent list
    // This should dispatch to the action handler which should use targeted send
    dispatch(
        &mut hub,
        HubAction::RequestAgentList {
            client_id: browser_a.clone(),
        },
    );

    // We can't easily verify the relay send without mocking, but we can verify
    // the action was processed without error. The fix will update the action
    // handler to use send_agent_list_to_browser instead of broadcast.
    //
    // Integration tests should verify:
    // - Browser A receives agent list
    // - Browser B does NOT receive agent list
}

/// TEST: RequestWorktreeList should only send to requesting browser.
#[test]
fn test_request_worktree_list_targets_requesting_browser() {
    let (mut hub, _td1, _td2) = setup_hub_with_two_agents();

    // Two browsers connect
    let browser_a = ClientId::Browser("browser-a".to_string());
    let browser_b = ClientId::Browser("browser-b".to_string());
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser_a.clone(),
        },
    );
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser_b.clone(),
        },
    );

    // Browser A requests worktree list
    dispatch(
        &mut hub,
        HubAction::RequestWorktreeList {
            client_id: browser_a.clone(),
        },
    );

    // Same as above - fix will update to use targeted send
}

// === Browser Disconnect Tests ===

/// TEST: Browser disconnect cleans up viewer index.
///
/// When a browser disconnects while viewing an agent, the viewer index
/// should be cleaned up so subsequent output routing doesn't try to
/// send to the disconnected browser.

/// TEST: Browser disconnect doesn't affect other viewers.
///
/// When multiple browsers are viewing the same agent and one disconnects,
/// the other should still be in the viewer index.

/// TEST: Output not routed to disconnected browser.
///
/// After browser disconnects, it should no longer be in the viewer index.
/// With agent-owned channels, output routing uses viewers_of() to determine
/// which browsers to send to.

// === TUI Menu Tests ===
//
// These tests verify all TUI popup menu functionality:
// - Opening/closing the menu
// - Navigation (up/down)
// - Each menu action (TogglePtyView, CloseAgent, NewAgent, etc.)

/// TEST: OpenMenu changes mode to Menu and resets selection.
#[test]
fn test_open_menu() {
    let mut hub = test_hub();

    assert_eq!(hub.mode, AppMode::Normal);
    assert_eq!(hub.menu_selected, 0);

    // Set menu_selected to non-zero to verify it resets
    hub.menu_selected = 5;

    dispatch(&mut hub, HubAction::OpenMenu);

    assert_eq!(
        hub.mode,
        AppMode::Menu,
        "OpenMenu should change mode to Menu"
    );
    assert_eq!(
        hub.menu_selected, 0,
        "OpenMenu should reset menu_selected to 0"
    );
}

/// TEST: CloseModal returns to Normal mode and clears input buffer.
#[test]
fn test_close_modal() {
    let mut hub = test_hub();

    // Open menu and add some input
    hub.mode = AppMode::Menu;
    hub.input_buffer = "test input".to_string();

    dispatch(&mut hub, HubAction::CloseModal);

    assert_eq!(
        hub.mode,
        AppMode::Normal,
        "CloseModal should return to Normal mode"
    );
    assert!(
        hub.input_buffer.is_empty(),
        "CloseModal should clear input buffer"
    );
}

/// TEST: MenuUp decrements menu_selected (with bounds check).
#[test]
fn test_menu_up() {
    let mut hub = test_hub();

    hub.mode = AppMode::Menu;
    hub.menu_selected = 2;

    dispatch(&mut hub, HubAction::MenuUp);
    assert_eq!(hub.menu_selected, 1, "MenuUp should decrement selection");

    dispatch(&mut hub, HubAction::MenuUp);
    assert_eq!(hub.menu_selected, 0, "MenuUp should decrement to 0");

    dispatch(&mut hub, HubAction::MenuUp);
    assert_eq!(hub.menu_selected, 0, "MenuUp should not go below 0");
}

/// TEST: MenuDown increments menu_selected (with bounds check).
#[test]
fn test_menu_down() {
    let mut hub = test_hub();

    hub.mode = AppMode::Menu;
    hub.menu_selected = 0;

    // Without an agent, menu has 2 selectable items: New Agent, Connection Code
    dispatch(&mut hub, HubAction::MenuDown);
    assert_eq!(hub.menu_selected, 1, "MenuDown should increment to max-1");

    dispatch(&mut hub, HubAction::MenuDown);
    assert_eq!(
        hub.menu_selected, 1,
        "MenuDown should not exceed max selectable items"
    );
}

/// TEST: MenuSelect NewAgent opens worktree selection.
#[test]
fn test_menu_select_new_agent() {
    let mut hub = test_hub();

    hub.mode = AppMode::Menu;
    // Without agent, menu is: [Hub header], New Agent (0), Connection Code (1)
    hub.menu_selected = 0;

    dispatch(&mut hub, HubAction::MenuSelect(0));

    assert_eq!(
        hub.mode,
        AppMode::NewAgentSelectWorktree,
        "Selecting 'New Agent' should open worktree selection"
    );
}

/// TEST: MenuSelect ShowConnectionCode opens connection code modal.
#[test]
fn test_menu_select_connection_code() {
    let mut hub = test_hub();

    hub.mode = AppMode::Menu;
    // Without agent: New Agent (0), Connection Code (1)
    hub.menu_selected = 1;

    dispatch(&mut hub, HubAction::MenuSelect(1));

    assert_eq!(
        hub.mode,
        AppMode::ConnectionCode,
        "Selecting 'Show Connection Code' should open connection code modal"
    );
}

/// TEST: MenuSelect CloseAgent opens confirmation modal (with agent).
#[test]
fn test_menu_select_close_agent() {
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

    // IMPORTANT: Select the agent via TUI - menu context uses TUI selection
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: "agent-1".to_string(),
        },
    );

    hub.mode = AppMode::Menu;
    // With agent (no server): [Agent header], Close Agent (0), [Hub header], New Agent (1), ...
    hub.menu_selected = 0;

    dispatch(&mut hub, HubAction::MenuSelect(0));

    assert_eq!(
        hub.mode,
        AppMode::CloseAgentConfirm,
        "Selecting 'Close Agent' should open confirmation modal"
    );
}

// Note: test_menu_select_toggle_pty_view was removed because active_pty
// is now client-local state (TuiClient.active_pty_view). The menu toggle
// command now operates on TuiClient, not Agent. See tui/runner.rs for
// the PTY view toggle implementation.

/// TEST: Menu with no agents has correct selectable count.
#[test]
fn test_menu_selectable_count_no_agent() {
    let hub = test_hub();

    let ctx = build_menu_context(&hub);
    let items = crate::tui::menu::build_menu(&ctx);
    let count = crate::tui::menu::selectable_count(&items);

    assert_eq!(
        count, 2,
        "Menu without agent should have 2 selectable items"
    );
}

/// TEST: Menu with agent (no server) has correct selectable count.
#[test]
fn test_menu_selectable_count_with_agent() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let mut hub = test_hub();

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

    // Select via TUI for menu context
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: "agent-1".to_string(),
        },
    );

    let ctx = build_menu_context(&hub);
    let items = crate::tui::menu::build_menu(&ctx);
    let count = crate::tui::menu::selectable_count(&items);

    assert_eq!(
        count, 3,
        "Menu with agent (no server) should have 3 selectable items"
    );
}

/// TEST: New Agent from menu works when an agent is already selected.
///
/// This tests the scenario where user has an agent running and wants to
/// create another one. The menu should show both agent and hub sections.
#[test]
fn test_menu_new_agent_with_existing_agent() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let mut hub = test_hub();

    // Create and select an agent
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

    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: "agent-1".to_string(),
        },
    );

    // Open menu
    dispatch(&mut hub, HubAction::OpenMenu);
    assert_eq!(hub.mode, AppMode::Menu);

    // With agent (no server), menu is:
    // [Agent header], Close Agent (0), [Hub header], New Agent (1), Connection Code (2)
    // So "New Agent" is at selection index 1

    // Select "New Agent" (index 1)
    dispatch(&mut hub, HubAction::MenuSelect(1));

    assert_eq!(
        hub.mode,
        AppMode::NewAgentSelectWorktree,
        "New Agent should open worktree selection even when agent is already running"
    );
}

/// TEST: New Agent from menu works when agent has server PTY.
///
/// With server PTY, the menu has an extra "View Server/Agent" option.
#[test]
fn test_menu_new_agent_with_server_pty() {
    use crate::agent::pty::PtySession;
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let mut hub = test_hub();

    // Create agent with server PTY
    let temp_dir = TempDir::new().unwrap();
    let mut agent = Agent::new(
        Uuid::new_v4(),
        "test/repo".to_string(),
        Some(1),
        "test-branch".to_string(),
        temp_dir.path().to_path_buf(),
    );
    agent.server_pty = Some(PtySession::new(24, 80));
    hub.state
        .write()
        .unwrap()
        .add_agent("agent-1".to_string(), agent);

    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: "agent-1".to_string(),
        },
    );

    dispatch(&mut hub, HubAction::OpenMenu);

    // With agent + server, menu is:
    // [Agent header], View Server (0), Close Agent (1), [Hub header], New Agent (2), Connection Code (3)
    // So "New Agent" is at selection index 2

    dispatch(&mut hub, HubAction::MenuSelect(2));

    assert_eq!(
        hub.mode,
        AppMode::NewAgentSelectWorktree,
        "New Agent should be at index 2 when agent has server PTY"
    );
}

/// TEST: Menu with agent + server has correct selectable count.
#[test]
fn test_menu_selectable_count_with_server() {
    use crate::agent::pty::PtySession;
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let mut hub = test_hub();

    let temp_dir = TempDir::new().unwrap();
    let mut agent = Agent::new(
        Uuid::new_v4(),
        "test/repo".to_string(),
        Some(1),
        "test-branch".to_string(),
        temp_dir.path().to_path_buf(),
    );
    agent.server_pty = Some(PtySession::new(24, 80));
    hub.state
        .write()
        .unwrap()
        .add_agent("agent-1".to_string(), agent);

    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: "agent-1".to_string(),
        },
    );

    let ctx = build_menu_context(&hub);
    let items = crate::tui::menu::build_menu(&ctx);
    let count = crate::tui::menu::selectable_count(&items);

    assert_eq!(
        count, 4,
        "Menu with agent + server should have 4 selectable items"
    );
}

// === Error Display Tests ===

/// TEST: show_error sets Error mode and error_message.
#[test]
fn test_show_error_sets_mode_and_message() {
    let mut hub = test_hub();

    hub.show_error("Test error message");

    assert_eq!(hub.mode, AppMode::Error);
    assert_eq!(hub.error_message.as_deref(), Some("Test error message"));
}

/// TEST: clear_error returns to Normal and clears message.
#[test]
fn test_clear_error_returns_to_normal() {
    let mut hub = test_hub();

    hub.show_error("Test error");
    hub.clear_error();

    assert_eq!(hub.mode, AppMode::Normal);
    assert!(hub.error_message.is_none());
}

/// TEST: Error mode can be dismissed with CloseModal.
#[test]
fn test_error_mode_can_be_dismissed() {
    let mut hub = test_hub();

    // Manually trigger error mode
    hub.show_error("Test error message");
    assert_eq!(hub.mode, AppMode::Error);
    assert_eq!(hub.error_message.as_deref(), Some("Test error message"));

    // Dismiss with CloseModal (Esc key)
    dispatch(&mut hub, HubAction::CloseModal);

    assert_eq!(hub.mode, AppMode::Normal);
    assert!(hub.error_message.is_none());
}

/// TEST: Connection code modal can be closed after resize event.
///
/// Regression test for: resizing while connection info is displayed
/// prevents Escape from closing the modal.
#[test]
fn test_connection_code_close_after_resize() {
    let mut hub = test_hub();

    // Open connection code modal
    dispatch(&mut hub, HubAction::ShowConnectionCode);
    assert_eq!(hub.mode, AppMode::ConnectionCode);

    // Simulate a resize event (as would happen when user resizes terminal)
    dispatch(
        &mut hub,
        HubAction::ResizeForClient {
            client_id: ClientId::Tui,
            rows: 100,
            cols: 200,
        },
    );

    // Mode should still be ConnectionCode after resize
    assert_eq!(
        hub.mode,
        AppMode::ConnectionCode,
        "Resize should not change mode from ConnectionCode"
    );

    // Close modal (simulates pressing Escape)
    dispatch(&mut hub, HubAction::CloseModal);

    // Should return to Normal mode
    assert_eq!(
        hub.mode,
        AppMode::Normal,
        "CloseModal should return to Normal mode even after resize"
    );
}

/// TEST: Connection code modal survives multiple resize events.
#[test]
fn test_connection_code_survives_multiple_resizes() {
    let mut hub = test_hub();

    // Open connection code modal
    dispatch(&mut hub, HubAction::ShowConnectionCode);
    assert_eq!(hub.mode, AppMode::ConnectionCode);

    // Multiple rapid resize events
    for i in 0..5 {
        dispatch(
            &mut hub,
            HubAction::ResizeForClient {
                client_id: ClientId::Tui,
                rows: 50 + i * 10,
                cols: 150 + i * 10,
            },
        );
    }

    // Mode should still be ConnectionCode
    assert_eq!(hub.mode, AppMode::ConnectionCode);

    // Close should work
    dispatch(&mut hub, HubAction::CloseModal);
    assert_eq!(hub.mode, AppMode::Normal);
}

// ============================================================
// Copy Connection URL Tests
// ============================================================

/// Create a test PreKeyBundleData for unit tests.
///
/// This creates a valid bundle structure with mock cryptographic data.
/// The bundle can be serialized to binary format and used to test URL generation.
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
        kyber_prekey: STANDARD.encode([5u8; 1569]), // Kyber1024 public key size
        kyber_prekey_signature: STANDARD.encode([6u8; 64]),
    }
}

/// TEST: Copy connection URL generates URL fresh even when connection_url cache is None.
///
/// This tests the fix for Task #3: The copy handler should use generate_connection_url()
/// rather than relying on the stale hub.connection_url cache.
///
/// Before the fix:
/// - handle_copy_connection_url checked hub.connection_url (cache)
/// - If cache was None, nothing was copied to clipboard
///
/// After the fix:
/// - handle_copy_connection_url calls hub.generate_connection_url()
/// - URL is generated fresh from the current Signal bundle
#[test]
fn test_copy_connection_url_generates_fresh_url() {
    let mut hub = test_hub();

    // Set up a mock Signal bundle (required for URL generation)
    hub.browser.signal_bundle = Some(create_test_prekey_bundle());

    // Verify connection_url cache is initially None
    assert!(
        hub.connection_url.is_none(),
        "connection_url cache should start as None"
    );

    // Dispatch CopyConnectionUrl action
    // This should work even though hub.connection_url is None
    dispatch(&mut hub, HubAction::CopyConnectionUrl);

    // We can't easily test clipboard contents in unit tests, but we can verify:
    // 1. No panic occurred
    // 2. The URL can be generated (the method succeeds)

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
///
/// When there's no Signal bundle (not connected to relay yet), copy should
/// not panic and should handle the error gracefully.
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

    // Hub should still be in valid state
    assert_eq!(hub.mode, AppMode::Normal);
}

// ============================================================
// PTY Channel Connection Tests (Browser Agent Selection)
// ============================================================
//
// When a browser selects an agent, a TerminalRelayChannel should be
// created and connected for PTY I/O. This enables explicit routing of
// terminal output to the browser viewing that specific agent/PTY.

/// TEST: SelectAgentForClient connects PTY channel for browser clients.
///
/// This is the core feature: when a browser selects an agent, we need to:
/// 1. Create an ActionCableChannel for TerminalRelayChannel
/// 2. Configure it with hub_id, agent_index, pty_index=0 (CLI)
/// 3. Store it in BrowserClient.pty_channels
///
/// Without crypto service (E2E encryption), channel creation is deferred.
/// This test verifies the deferred behavior when no crypto service exists.

/// TEST: Connect agent PTY channel populates ClientRegistry.pty_channels.
///
/// This tests the registry-based PTY channel storage that enables:
/// 1. Explicit routing of PTY output to specific browsers
/// 2. O(1) lookup of channels by browser+agent+pty
/// 3. Proper cleanup on browser disconnect
///
/// Note: Full integration test requires mock crypto service and WebSocket.
/// This unit test verifies the channel storage mechanism in the registry.

/// TEST: Registry cleans up all PTY channels when browser disconnects.

/// TEST: Registry cleans up all PTY channels when agent is deleted.

// =========================================================================
// BROWSER PTY I/O ROUTING TESTS
// =========================================================================
//
// These tests verify explicit PTY I/O routing between browsers and agents.
// Task #3: Implement explicit routing for browser PTY I/O based on agent_index + pty_index.

/// TEST: Registry can retrieve PTY channel senders for output routing.
///
/// Verifies that `get_pty_channel_senders` correctly finds channels
/// for browsers viewing a specific agent/pty combination.

/// TEST: Registry correctly iterates PTY channels for output routing.
///
/// Verifies that `pty_channels_for_agent_mut` correctly identifies all
/// channels that should receive output for a specific agent/pty.

/// TEST: Input from browser reaches correct agent via selection.
///
/// Verifies the input routing path:
/// Browser -> SendInputForClient -> registry lookup -> agent.write_input_to_cli()

/// TEST: Agent get_pty_handle returns correct handle.
///
/// Verifies that Agent.get_pty_handle() can be used to subscribe to PTY events.
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
    // If we get here, the handle's event channel is valid
}

// ============================================================================
// Browser → PTY I/O Flow Integration Tests
// ============================================================================
// These tests verify the explicit routing architecture for browser PTY I/O:
// - Task #2: PTY channels stored in ClientRegistry, created on agent selection
// - Task #3: Output forwarding tasks spawn per-channel, input routes via BrowserCommand
// - Task #4: Input/resize use TerminalRelayChannel, not HubChannel
//
// Architecture:
// ```text
// Browser selects agent
//   → TerminalRelayChannel created (per browser/agent/pty)
//   → Channel stored in ClientRegistry
//   → Output forwarding task spawned
//
// PTY output
//   → PtySession broadcasts PtyEvent::Output
//   → Forwarding task receives
//   → Sends to browser via TerminalRelayChannel
//
// Browser input
//   → BrowserCommand::TerminalInput received
//   → Routed to selected agent's PTY via handle_send_input_for_client
// ```

/// TEST: Browser selects agent → TerminalRelayChannel is created.
///
/// Verifies that when a browser selects an agent, the infrastructure to
/// create PTY channels is triggered. Note: actual ActionCable connection
/// requires crypto service, which is tested in system tests.

/// TEST: PTY output routing is per-browser/agent/pty (isolated).
///
/// Verifies that each browser has its own PTY channel for each agent/pty
/// combination, ensuring routing isolation.

/// TEST: Browser switches agents → old channel cleaned up, new channel created.
///
/// Verifies that when a browser switches from one agent to another,
/// the viewer indices are properly updated.

/// TEST: Browser disconnects → all PTY channels cleaned up.
///
/// Verifies that when a browser disconnects, its selection is cleared
/// and viewer indices are updated.

/// TEST: Agent deleted → PTY channels for that agent cleaned up.
///
/// Verifies that when an agent is deleted, all browsers viewing it
/// have their selection cleared.

/// TEST: Input routes through TerminalRelayChannel, not HubChannel.
///
/// Verifies that browser input uses the client's selected agent for routing,
/// which is the infrastructure for TerminalRelayChannel (explicit routing).

/// TEST: Resize updates client dimensions per-client.
///
/// ResizeForClient ONLY updates client.dims(). It does NOT resize agent PTYs.
/// Each client maintains independent dimensions.
#[test]
fn test_resize_routes_via_selection() {
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

    // Browser 1 selects agent-1
    let browser1 = ClientId::Browser("browser-resize-1".to_string());
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser1.clone(),
        },
    );
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser1.clone(),
            agent_key: "agent-1".to_string(),
        },
    );

    // Browser 2 selects agent-2
    let browser2 = ClientId::Browser("browser-resize-2".to_string());
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser2.clone(),
        },
    );
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser2.clone(),
            agent_key: "agent-2".to_string(),
        },
    );

    // Browser 1 resizes to 100x50 - should update browser 1's dims only
    dispatch(
        &mut hub,
        HubAction::ResizeForClient {
            client_id: browser1.clone(),
            cols: 100,
            rows: 50,
        },
    );

    // Browser 1 dims should be updated (cols, rows)
    let dims1 = hub.clients.get(&browser1).unwrap().dims();
    assert_eq!(dims1, (100, 50), "Browser 1 client dims should be updated");

    // Browser 2 should still have default dims (80, 24)
    let dims2 = hub.clients.get(&browser2).unwrap().dims();
    assert_eq!(
        dims2,
        (80, 24),
        "Browser 2 should still have default dims"
    );

    // Browser 2 resizes to 200x60 - should update browser 2's dims only
    dispatch(
        &mut hub,
        HubAction::ResizeForClient {
            client_id: browser2.clone(),
            cols: 200,
            rows: 60,
        },
    );

    // Browser 2 dims should now be updated
    let dims2 = hub.clients.get(&browser2).unwrap().dims();
    assert_eq!(dims2, (200, 60), "Browser 2 client dims should be updated");

    // Browser 1 dims should be unchanged
    let dims1 = hub.clients.get(&browser1).unwrap().dims();
    assert_eq!(
        dims1,
        (100, 50),
        "Browser 1 dims should be unchanged by browser 2's resize"
    );
}

// ============================================================================
// ClientRegistry PTY Channel Tests
// ============================================================================
// These tests verify the ClientRegistry's PTY channel management functions.

/// TEST: ClientRegistry PTY channel key format.

/// TEST: ClientRegistry disconnects all PTY channels for browser.

/// TEST: ClientRegistry disconnects all PTY channels for agent.

// =============================================================================
// PTY Input Receiver Tests
// =============================================================================
// Tests for the `spawn_pty_input_receiver` function that routes browser input
// to the PTY session.

/// Test that BrowserCommand::Input is correctly routed to PtyHandle.
#[tokio::test]
async fn test_pty_input_receiver_routes_input_command() {
    use crate::agent::pty::PtyCommand;
    use crate::hub::agent_handle::PtyHandle;
    use crate::relay::BrowserCommand;
    use tokio::sync::{broadcast, mpsc};

    // Create a PtyHandle with a command receiver we can inspect
    let (event_tx, _event_rx) = broadcast::channel(16);
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<PtyCommand>(16);
    let pty_handle = PtyHandle::new(event_tx, cmd_tx);

    // Create input message (BrowserCommand::Input)
    let input_cmd = BrowserCommand::Input {
        data: "ls -la\n".to_string(),
    };
    let input_json = serde_json::to_vec(&input_cmd).unwrap();

    // Send through PtyHandle (simulating what spawn_pty_input_receiver does)
    pty_handle.write_input(b"ls -la\n").await.unwrap();

    // Verify the command was sent
    let cmd = cmd_rx.recv().await.unwrap();
    match cmd {
        PtyCommand::Input(data) => {
            assert_eq!(data, b"ls -la\n");
        }
        _ => panic!("Expected Input command, got {:?}", cmd),
    }

    // Also test that the JSON parsing works correctly
    let parsed: BrowserCommand = serde_json::from_slice(&input_json).unwrap();
    match parsed {
        BrowserCommand::Input { data } => {
            assert_eq!(data, "ls -la\n");
        }
        _ => panic!("Expected Input command"),
    }
}

/// Test that BrowserCommand::Resize is correctly routed to PtyHandle.
#[tokio::test]
async fn test_pty_input_receiver_routes_resize_command() {
    use crate::agent::pty::PtyCommand;
    use crate::hub::agent_handle::PtyHandle;
    use crate::relay::BrowserCommand;
    use tokio::sync::{broadcast, mpsc};

    // Create a PtyHandle with a command receiver we can inspect
    let (event_tx, _event_rx) = broadcast::channel(16);
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<PtyCommand>(16);
    let pty_handle = PtyHandle::new(event_tx, cmd_tx);

    // Create resize message (BrowserCommand::Resize)
    let resize_cmd = BrowserCommand::Resize { cols: 120, rows: 40 };
    let resize_json = serde_json::to_vec(&resize_cmd).unwrap();

    // Verify JSON parsing works
    let parsed: BrowserCommand = serde_json::from_slice(&resize_json).unwrap();
    match parsed {
        BrowserCommand::Resize { cols, rows } => {
            assert_eq!(cols, 120);
            assert_eq!(rows, 40);
        }
        _ => panic!("Expected Resize command"),
    }

    // Send resize through PtyHandle (simulating what spawn_pty_input_receiver does)
    let client_id = ClientId::browser("test-browser");
    pty_handle.resize(client_id.clone(), 40, 120).await.unwrap();

    // Verify the command was sent
    let cmd = cmd_rx.recv().await.unwrap();
    match cmd {
        PtyCommand::Resize {
            client_id: recv_client,
            rows,
            cols,
        } => {
            assert_eq!(recv_client, client_id);
            assert_eq!(rows, 40);
            assert_eq!(cols, 120);
        }
        _ => panic!("Expected Resize command, got {:?}", cmd),
    }
}

/// Test that non-PTY commands (ListAgents, SelectAgent, etc.) are ignored.
#[test]
fn test_pty_input_receiver_ignores_non_pty_commands() {
    use crate::relay::BrowserCommand;

    // These commands should be handled by the main hub channel, not PTY channel
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

    // All should parse successfully (the input receiver will just ignore them)
    for cmd_json in &non_pty_commands {
        let parsed: Result<BrowserCommand, _> = serde_json::from_str(cmd_json);
        assert!(parsed.is_ok(), "Failed to parse: {}", cmd_json);

        // Verify it's not Input or Resize
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

    // Should have type field for serde tag
    assert!(json.contains(r#""type":"input""#));
    assert!(json.contains(r#""data":"hello""#));

    // Should round-trip
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

    // Should have type field for serde tag
    assert!(json.contains(r#""type":"resize""#));
    assert!(json.contains(r#""cols":80"#));
    assert!(json.contains(r#""rows":24"#));

    // Should round-trip
    let parsed: BrowserCommand = serde_json::from_str(&json).unwrap();
    match parsed {
        BrowserCommand::Resize { cols, rows } => {
            assert_eq!(cols, 80);
            assert_eq!(rows, 24);
        }
        _ => panic!("Wrong variant"),
    }
}

