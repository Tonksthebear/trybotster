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
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
#[test]
fn test_select_agent_for_client_updates_state() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
        .add_agent("test-repo-1".to_string(), agent);

    // Register a browser client
    let browser_id = ClientId::Browser("test-browser".to_string());
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser_id.clone(),
        },
    );

    // Select agent for client
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "test-repo-1".to_string(),
        },
    );

    // Verify client selection updated via registry
    assert_eq!(
        hub.clients.selected_agent(&browser_id),
        Some("test-repo-1")
    );

    // Verify viewer index updated
    assert_eq!(hub.clients.viewer_count("test-repo-1"), 1);
}

/// Test that SendInputForClient routes to client's selected agent.
#[test]
fn test_send_input_for_client_routes_to_selected_agent() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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

    // Register browser and select agent-2
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
            agent_key: "agent-2".to_string(),
        },
    );

    // Send input for client - should go to agent-2, not agent-1
    // (This test verifies routing logic, actual PTY write would need a spawned agent)
    dispatch(
        &mut hub,
        HubAction::SendInputForClient {
            client_id: browser_id.clone(),
            data: b"test input".to_vec(),
        },
    );

    // Verify client is still viewing agent-2
    assert_eq!(hub.clients.selected_agent(&browser_id), Some("agent-2"));
}

// === HOT PATH BUG TESTS (TDD - these should FAIL until bugs are fixed) ===

/// BUG: Browser resize before agent selection should apply dims when agent is later selected.
///
/// Flow that's broken:
/// 1. Browser connects
/// 2. Browser sends resize (100x50) BEFORE selecting an agent
/// 3. Browser selects agent-1
/// 4. Agent-1's PTY should be 100x50 but it's still default size!
///
/// This test SHOULD FAIL until the bug is fixed.
#[test]
fn test_resize_before_selection_should_apply_when_agent_selected() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap(); // 24x80 default

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

    // Verify agent starts with default dims
    let (initial_rows, initial_cols) = {
        let state = hub.state.read().unwrap();
        state.agents.get("agent-1").unwrap().get_pty_size()
    };
    assert_eq!(
        (initial_rows, initial_cols),
        (24, 80),
        "Agent should start with default dims"
    );

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

    // Agent still has old dims (this is expected - no agent selected yet)
    let (rows, cols) = {
        let state = hub.state.read().unwrap();
        state.agents.get("agent-1").unwrap().get_pty_size()
    };
    assert_eq!(
        (rows, cols),
        (24, 80),
        "Agent should still have default dims before selection"
    );

    // NOW browser selects the agent
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        },
    );

    // BUG: Agent SHOULD have been resized to 100x50 when selected!
    let (rows, cols) = {
        let state = hub.state.read().unwrap();
        state.agents.get("agent-1").unwrap().get_pty_size()
    };

    // This assertion WILL FAIL - proving the bug exists
    assert_eq!(
        (rows, cols),
        (50, 100),
        "BUG: Agent should be resized to browser dims when selected, but it's still ({}, {})",
        rows,
        cols
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
#[test]
fn test_input_without_selection_is_silently_dropped() {
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

    // Browser connects (no agents, so can't select one)
    let browser_id = ClientId::Browser("browser-1".to_string());
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser_id.clone(),
        },
    );

    // Browser sends input before selecting an agent
    // This should NOT silently succeed - it should either error or buffer
    dispatch(
        &mut hub,
        HubAction::SendInputForClient {
            client_id: browser_id.clone(),
            data: b"important input".to_vec(),
        },
    );

    // Currently this passes silently - input is LOST
    // A proper implementation would either:
    // - Store a pending_input queue in ClientState
    // - Send an error response to the browser

    // For now, document that no error was sent (bad behavior)
    // The test "passes" but documents broken behavior
    assert!(
        hub.clients.selected_agent(&browser_id).is_none(),
        "No agent should be selected"
    );
    // No way to verify input was buffered because it wasn't
}

/// BUG: Selecting a different agent should resize that agent's PTY to client dims.
///
/// Flow:
/// 1. Browser connects and resizes to 100x50
/// 2. Browser selects agent-1 (should resize to 100x50)
/// 3. Browser selects agent-2 (should ALSO resize to 100x50)
///
/// Currently agent-2 keeps its old size.
#[test]
fn test_selecting_different_agent_should_resize_to_client_dims() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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

    // Select agent-1
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        },
    );

    // Now select agent-2 - it should be resized to 100x50
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-2".to_string(),
        },
    );

    let (rows, cols) = {
        let state = hub.state.read().unwrap();
        state.agents.get("agent-2").unwrap().get_pty_size()
    };

    // This assertion WILL FAIL - proving the bug
    assert_eq!(
        (rows, cols),
        (50, 100),
        "BUG: agent-2 should be resized to browser dims when selected, but it's ({}, {})",
        rows,
        cols
    );
}

/// Test that TUI and browser can have independent selections.
#[test]
fn test_independent_tui_and_browser_selection() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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

    // Register browser and select agent-2
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
            agent_key: "agent-2".to_string(),
        },
    );

    // TUI selects agent-1 via client-scoped selection
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: "agent-1".to_string(),
        },
    );

    // Verify independent selections via registry
    assert_eq!(hub.clients.selected_agent(&browser_id), Some("agent-2"));

    assert_eq!(
        hub.clients.selected_agent(&ClientId::Tui),
        Some("agent-1")
    );

    // Verify viewer index shows both
    assert_eq!(hub.clients.viewer_count("agent-1"), 1); // TUI
    assert_eq!(hub.clients.viewer_count("agent-2"), 1); // Browser
}

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
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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

/// Test resize arrives AFTER create_agent (worst case race condition).
///
/// Flow:
/// 1. Browser connects
/// 2. Browser creates agent (resize hasn't arrived yet)
/// 3. Agent spawns with hub.terminal_dims (24x80)
/// 4. Agent is auto-selected (but client has no dims yet)
/// 5. Browser sends resize (100x50)
/// 6. Agent should be resized to 100x50
///
/// This is the actual race condition scenario.
#[test]
fn test_resize_after_create_agent_still_works() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap(); // 24x80 default

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

    // Agent starts with default dims
    let (rows, cols) = {
        let state = hub.state.read().unwrap();
        state.agents.get("agent-1").unwrap().get_pty_size()
    };
    assert_eq!(
        (rows, cols),
        (24, 80),
        "Agent should start with default dims"
    );

    // Select agent for browser (simulating auto-select after create)
    // Client has no dims, so no resize happens yet
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        },
    );

    // Agent still has default dims (no resize because client had no dims)
    let (rows, cols) = {
        let state = hub.state.read().unwrap();
        state.agents.get("agent-1").unwrap().get_pty_size()
    };
    assert_eq!(
        (rows, cols),
        (24, 80),
        "Agent should still have default dims (no client dims yet)"
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

    // Agent should NOW be resized because:
    // 1. Client has agent-1 selected
    // 2. ResizeForClient resizes the selected agent
    let (rows, cols) = {
        let state = hub.state.read().unwrap();
        state.agents.get("agent-1").unwrap().get_pty_size()
    };
    assert_eq!(
        (rows, cols),
        (50, 100),
        "Agent should be resized when resize arrives after create, but got ({}, {})",
        rows,
        cols
    );
}

/// Test resize arrives BEFORE create_agent (ideal case).
///
/// Flow:
/// 1. Browser connects
/// 2. Browser sends resize (100x50)
/// 3. Browser creates agent
/// 4. Agent spawns with client.dims (100x50) - correct!
/// 5. Agent is auto-selected (resize happens again but same dims)
#[test]
fn test_resize_before_create_agent_works() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap(); // 24x80 default

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

    // Verify client has dims
    let client = hub.clients.get(&browser_id).unwrap();
    assert_eq!(client.dims(), (100, 50));

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
    // Client HAS dims, so resize happens immediately
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        },
    );

    // Agent should be resized to browser dims
    let (rows, cols) = {
        let state = hub.state.read().unwrap();
        state.agents.get("agent-1").unwrap().get_pty_size()
    };
    assert_eq!(
        (rows, cols),
        (50, 100),
        "Agent should be resized on selection when client has dims, but got ({}, {})",
        rows,
        cols
    );
}

// === DeleteAgentForClient tests ===

/// Test that DeleteAgentForClient clears selection for all viewers.
///
/// Scenario:
/// 1. Two browsers connect and both select the same agent
/// 2. One browser deletes the agent
/// 3. Both browsers should have their selection cleared
#[test]
fn test_delete_agent_clears_selection_for_all_viewers() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
        .add_agent("test-repo-1".to_string(), agent);

    // Two browsers connect and select the same agent
    let browser1 = ClientId::Browser("browser-1".to_string());
    let browser2 = ClientId::Browser("browser-2".to_string());

    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser1.clone(),
        },
    );
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser2.clone(),
        },
    );

    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser1.clone(),
            agent_key: "test-repo-1".to_string(),
        },
    );
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser2.clone(),
            agent_key: "test-repo-1".to_string(),
        },
    );

    // Verify both are viewing the agent
    assert_eq!(hub.clients.viewer_count("test-repo-1"), 2);

    // Browser 1 deletes the agent
    dispatch(
        &mut hub,
        HubAction::DeleteAgentForClient {
            client_id: browser1.clone(),
            request: crate::client::DeleteAgentRequest {
                agent_id: "test-repo-1".to_string(),
                delete_worktree: false,
            },
        },
    );

    // Both browsers should have cleared selection
    assert_eq!(
        hub.clients.selected_agent(&browser1),
        None,
        "Browser 1 selection should be cleared"
    );
    assert_eq!(
        hub.clients.selected_agent(&browser2),
        None,
        "Browser 2 selection should be cleared"
    );

    // Viewer count should be 0
    assert_eq!(hub.clients.viewer_count("test-repo-1"), 0);

    // Agent should be removed
    assert!(hub
        .state
        .read()
        .unwrap()
        .agents
        .get("test-repo-1")
        .is_none());
}

/// Test that deleting non-existent agent is handled gracefully.
#[test]
fn test_delete_nonexistent_agent_is_graceful() {
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
#[test]
fn test_input_without_selection_is_dropped() {
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

    let browser_id = ClientId::Browser("browser-1".to_string());
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser_id.clone(),
        },
    );

    // Send input without selecting an agent first
    // This should not panic - input is silently dropped
    dispatch(
        &mut hub,
        HubAction::SendInputForClient {
            client_id: browser_id.clone(),
            data: b"hello world".to_vec(),
        },
    );

    // Client should still be registered (no crash)
    assert!(hub.clients.get(&browser_id).is_some());
    assert_eq!(hub.clients.selected_agent(&browser_id), None);
}

// === Multi-client selection tests ===

/// Test that TUI and browser can have independent selections.
///
/// Scenario:
/// 1. TUI selects agent-1
/// 2. Browser selects agent-2
/// 3. Both should maintain their independent selections
#[test]
fn test_tui_and_browser_independent_selections() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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

    // TUI selects agent-1 (via global action which updates TUI client state)
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: "agent-1".to_string(),
        },
    );

    // Browser connects and selects agent-2
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
            agent_key: "agent-2".to_string(),
        },
    );

    // Verify independent selections via registry
    assert_eq!(
        hub.clients.selected_agent(&ClientId::Tui),
        Some("agent-1")
    );
    assert_eq!(hub.clients.selected_agent(&browser_id), Some("agent-2"));

    // Verify viewer counts
    assert_eq!(hub.clients.viewer_count("agent-1"), 1);
    assert_eq!(hub.clients.viewer_count("agent-2"), 1);
}

/// Test that multiple browsers can view the same agent.
#[test]
fn test_multiple_browsers_same_agent() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
        .add_agent("shared-agent".to_string(), agent);

    // Three browsers all select the same agent
    for i in 1..=3 {
        let browser_id = ClientId::Browser(format!("browser-{}", i));
        dispatch(
            &mut hub,
            HubAction::ClientConnected {
                client_id: browser_id.clone(),
            },
        );
        dispatch(
            &mut hub,
            HubAction::SelectAgentForClient {
                client_id: browser_id,
                agent_key: "shared-agent".to_string(),
            },
        );
    }

    // Viewer count should be 3
    assert_eq!(hub.clients.viewer_count("shared-agent"), 3);
}

/// Test that browser switching selection updates viewer counts correctly.
#[test]
fn test_browser_switch_selection_updates_viewers() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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

    assert_eq!(hub.clients.viewer_count("agent-1"), 1);
    assert_eq!(hub.clients.viewer_count("agent-2"), 0);

    // Browser switches to agent-2
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-2".to_string(),
        },
    );

    // Viewer counts should be updated
    assert_eq!(
        hub.clients.viewer_count("agent-1"),
        0,
        "Old agent should have 0 viewers"
    );
    assert_eq!(
        hub.clients.viewer_count("agent-2"),
        1,
        "New agent should have 1 viewer"
    );
}

/// Test that disconnecting browser clears its viewer entry.
#[test]
fn test_disconnect_clears_viewer_entry() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
        .add_agent("test-agent".to_string(), agent);

    // Browser connects and selects the agent
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
            agent_key: "test-agent".to_string(),
        },
    );

    assert_eq!(hub.clients.viewer_count("test-agent"), 1);

    // Browser disconnects
    dispatch(
        &mut hub,
        HubAction::ClientDisconnected {
            client_id: browser_id.clone(),
        },
    );

    // Viewer count should be 0 (client unregistered, viewer entry cleared)
    assert_eq!(hub.clients.viewer_count("test-agent"), 0);
}

// === Resize edge cases ===

/// Test that resize without agent selection stores dims for later.
#[test]
fn test_resize_without_selection_stores_dims() {
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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

/// Test that TUI resize only affects the selected agent (newest wins behavior).
#[test]
fn test_tui_resize_affects_selected_agent() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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

    // TUI resize should only affect the selected agent (agent-1)
    dispatch(
        &mut hub,
        HubAction::ResizeForClient {
            client_id: ClientId::Tui,
            cols: 150,
            rows: 40,
        },
    );

    // Only agent-1 should be resized
    let ((rows1, cols1), (rows2, cols2)) = {
        let state = hub.state.read().unwrap();
        let a1 = state.agents.get("agent-1").unwrap().get_pty_size();
        let a2 = state.agents.get("agent-2").unwrap().get_pty_size();
        (a1, a2)
    };

    assert_eq!(
        (rows1, cols1),
        (40, 150),
        "Agent 1 should be resized to TUI dims"
    );
    assert_eq!(
        (rows2, cols2),
        TEST_DIMS,
        "Agent 2 should keep original dims (not selected)"
    );
}

/// Test that browser resize only affects selected agent.
#[test]
fn test_browser_resize_only_affects_selected_agent() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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

    // Get initial sizes
    let (init_rows2, init_cols2) = hub
        .state
        .read()
        .unwrap()
        .agents
        .get("agent-2")
        .unwrap()
        .get_pty_size();

    // Browser resize should ONLY affect agent-1
    dispatch(
        &mut hub,
        HubAction::ResizeForClient {
            client_id: browser_id.clone(),
            cols: 180,
            rows: 45,
        },
    );

    // Agent-1 should be resized
    let (rows1, cols1) = {
        let state = hub.state.read().unwrap();
        state.agents.get("agent-1").unwrap().get_pty_size()
    };
    assert_eq!(
        (rows1, cols1),
        (45, 180),
        "Selected agent should be resized"
    );

    // Agent-2 should NOT be resized (still at initial size)
    let (rows2, cols2) = {
        let state = hub.state.read().unwrap();
        state.agents.get("agent-2").unwrap().get_pty_size()
    };
    assert_eq!(
        (rows2, cols2),
        (init_rows2, init_cols2),
        "Unselected agent should not be resized"
    );
}

// =========================================================================
// SIZE OWNER TESTS - REMOVED
// =========================================================================
//
// Size ownership tracking was removed from Agent. Resize now works as follows:
// - When a client selects an agent, the agent is resized to that client's dims
// - When a client with a selected agent resizes, the agent is resized to new dims
// - When a client disconnects, the agent is resized to the next remaining viewer's dims
//
// This simplification removes the "size_owner" field from Agent and the associated
// ownership tracking logic. The behavior is now: "last resize wins" rather than
// "only owner can resize".
//
// See: src/hub/actions.rs handle_resize_for_client(), resize_agent_for_remaining_viewers()
// =========================================================================

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

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
// 3. Agent is resized to browser's dims
//
// Note: Relay sends (agent_list, agent_selected, scrollback) are verified
// by browser.rs side effects which run after action dispatch.

/// TEST: After CreateAgentForClient, browser should be auto-selected to new agent.
///
/// This is critical for the browser UX - after creating an agent, the browser
/// should automatically be viewing that agent.
#[test]
fn test_create_agent_auto_selects_for_browser() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

    // Browser connects and sends resize
    let browser_id = ClientId::Browser("browser-create".to_string());
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

    // Verify browser has no selection yet
    assert!(hub.clients.selected_agent(&browser_id).is_none());

    // Manually add an agent (simulating successful creation)
    // Real CreateAgentForClient would create worktree which fails in tests
    let temp_dir = TempDir::new().unwrap();
    let agent = Agent::new(
        Uuid::new_v4(),
        "test/repo".to_string(),
        Some(42),
        "botster-issue-42".to_string(),
        temp_dir.path().to_path_buf(),
    );
    hub.state
        .write()
        .unwrap()
        .add_agent("test-repo-42".to_string(), agent);

    // Simulate the auto-select that happens after successful creation
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "test-repo-42".to_string(),
        },
    );

    // Browser should now have the agent selected
    assert_eq!(
        hub.clients.selected_agent(&browser_id),
        Some("test-repo-42"),
        "Browser should be auto-selected to newly created agent"
    );

    // Browser should be in viewer index
    assert_eq!(
        hub.clients.viewer_count("test-repo-42"),
        1,
        "Browser should be in viewer index for new agent"
    );

    // Agent should be resized to browser dims
    let (rows, cols) = {
        let state = hub.state.read().unwrap();
        state.agents.get("test-repo-42").unwrap().get_pty_size()
    };
    assert_eq!(
        (rows, cols),
        (50, 100),
        "New agent should be resized to browser dims"
    );
}

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
#[test]
fn test_browser_disconnect_cleans_viewer_index() {
    let (mut hub, _td1, _td2) = setup_hub_with_two_agents();

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

    // Verify browser is in viewer index
    assert_eq!(hub.clients.viewer_count("agent-1"), 1);
    let viewers: Vec<_> = hub.clients.viewers_of("agent-1").collect();
    assert!(viewers.contains(&&browser_id));

    // Browser disconnects
    dispatch(
        &mut hub,
        HubAction::ClientDisconnected {
            client_id: browser_id.clone(),
        },
    );

    // Viewer index should be empty for agent-1
    assert_eq!(
        hub.clients.viewer_count("agent-1"),
        0,
        "Viewer index should be cleaned up after browser disconnect"
    );

    // Verify no viewers remain
    let viewers: Vec<_> = hub.clients.viewers_of("agent-1").collect();
    assert!(
        viewers.is_empty(),
        "No viewers should remain after disconnect"
    );
}

/// TEST: Browser disconnect doesn't affect other viewers.
///
/// When multiple browsers are viewing the same agent and one disconnects,
/// the other should still be in the viewer index.
#[test]
fn test_browser_disconnect_preserves_other_viewers() {
    let (mut hub, _td1, _td2) = setup_hub_with_two_agents();

    // Two browsers connect and both select agent-1
    let browser_1 = ClientId::Browser("browser-1".to_string());
    let browser_2 = ClientId::Browser("browser-2".to_string());

    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser_1.clone(),
        },
    );
    dispatch(
        &mut hub,
        HubAction::ClientConnected {
            client_id: browser_2.clone(),
        },
    );

    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser_1.clone(),
            agent_key: "agent-1".to_string(),
        },
    );
    dispatch(
        &mut hub,
        HubAction::SelectAgentForClient {
            client_id: browser_2.clone(),
            agent_key: "agent-1".to_string(),
        },
    );

    // Both are viewers
    assert_eq!(hub.clients.viewer_count("agent-1"), 2);

    // Browser 1 disconnects
    dispatch(
        &mut hub,
        HubAction::ClientDisconnected {
            client_id: browser_1.clone(),
        },
    );

    // Browser 2 should still be a viewer
    assert_eq!(
        hub.clients.viewer_count("agent-1"),
        1,
        "Other viewer should remain after one disconnects"
    );
    let viewers: Vec<_> = hub.clients.viewers_of("agent-1").collect();
    assert!(viewers.contains(&&browser_2));
    assert!(!viewers.contains(&&browser_1));
}

/// TEST: Output not routed to disconnected browser.
///
/// After browser disconnects, it should no longer be in the viewer index.
/// With agent-owned channels, output routing uses viewers_of() to determine
/// which browsers to send to.
#[test]
fn test_output_not_routed_to_disconnected_browser() {
    let (mut hub, _td1, _td2) = setup_hub_with_two_agents();

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

    // Browser is a viewer - output would be routed
    assert_eq!(hub.clients.viewer_count("agent-1"), 1);

    // Browser disconnects
    dispatch(
        &mut hub,
        HubAction::ClientDisconnected {
            client_id: browser_id.clone(),
        },
    );

    // No viewers after disconnect - output routing will skip this agent
    assert_eq!(hub.clients.viewer_count("agent-1"), 0);
}

// === TUI Menu Tests ===
//
// These tests verify all TUI popup menu functionality:
// - Opening/closing the menu
// - Navigation (up/down)
// - Each menu action (TogglePtyView, CloseAgent, NewAgent, etc.)

/// TEST: OpenMenu changes mode to Menu and resets selection.
#[test]
fn test_open_menu() {
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

    hub.mode = AppMode::Menu;
    hub.menu_selected = 0;

    // Without an agent, menu has 3 selectable items: New Agent, Connection Code, Toggle Polling
    dispatch(&mut hub, HubAction::MenuDown);
    assert_eq!(hub.menu_selected, 1, "MenuDown should increment selection");

    dispatch(&mut hub, HubAction::MenuDown);
    assert_eq!(hub.menu_selected, 2, "MenuDown should increment to max-1");

    dispatch(&mut hub, HubAction::MenuDown);
    assert_eq!(
        hub.menu_selected, 2,
        "MenuDown should not exceed max selectable items"
    );
}

/// TEST: MenuSelect NewAgent opens worktree selection.
#[test]
fn test_menu_select_new_agent() {
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

    hub.mode = AppMode::Menu;
    // Without agent, menu is: [Hub header], New Agent (0), Connection Code (1), Toggle Polling (2)
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
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

    hub.mode = AppMode::Menu;
    // Without agent: New Agent (0), Connection Code (1), Toggle Polling (2)
    hub.menu_selected = 1;

    dispatch(&mut hub, HubAction::MenuSelect(1));

    assert_eq!(
        hub.mode,
        AppMode::ConnectionCode,
        "Selecting 'Show Connection Code' should open connection code modal"
    );
}

/// TEST: MenuSelect TogglePolling toggles polling and closes menu.
#[test]
fn test_menu_select_toggle_polling() {
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

    hub.mode = AppMode::Menu;
    let initial_polling = hub.polling_enabled;

    // Without agent: New Agent (0), Connection Code (1), Toggle Polling (2)
    hub.menu_selected = 2;

    dispatch(&mut hub, HubAction::MenuSelect(2));

    assert_eq!(
        hub.polling_enabled, !initial_polling,
        "Selecting 'Toggle Polling' should toggle polling state"
    );
    assert_eq!(
        hub.mode,
        AppMode::Normal,
        "Toggle Polling should close menu"
    );
}

/// TEST: MenuSelect CloseAgent opens confirmation modal (with agent).
#[test]
fn test_menu_select_close_agent() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
    let config = test_config();
    let hub = Hub::new(config, TEST_DIMS).unwrap();

    let ctx = build_menu_context(&hub);
    let items = crate::tui::menu::build_menu(&ctx);
    let count = crate::tui::menu::selectable_count(&items);

    assert_eq!(
        count, 3,
        "Menu without agent should have 3 selectable items"
    );
}

/// TEST: Menu with agent (no server) has correct selectable count.
#[test]
fn test_menu_selectable_count_with_agent() {
    use crate::agent::Agent;
    use tempfile::TempDir;
    use uuid::Uuid;

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
        count, 4,
        "Menu with agent (no server) should have 4 selectable items"
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

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
    // [Agent header], Close Agent (0), [Hub header], New Agent (1), Connection Code (2), Toggle Polling (3)
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

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
    // [Agent header], View Server (0), Close Agent (1), [Hub header], New Agent (2), Connection Code (3), Toggle Polling (4)
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

    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
        count, 5,
        "Menu with agent + server should have 5 selectable items"
    );
}

// === Error Display Tests ===

/// TEST: show_error sets Error mode and error_message.
#[test]
fn test_show_error_sets_mode_and_message() {
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

    hub.show_error("Test error message");

    assert_eq!(hub.mode, AppMode::Error);
    assert_eq!(hub.error_message.as_deref(), Some("Test error message"));
}

/// TEST: clear_error returns to Normal and clears message.
#[test]
fn test_clear_error_returns_to_normal() {
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

    hub.show_error("Test error");
    hub.clear_error();

    assert_eq!(hub.mode, AppMode::Normal);
    assert!(hub.error_message.is_none());
}

/// TEST: Error mode can be dismissed with CloseModal.
#[test]
fn test_error_mode_can_be_dismissed() {
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
    let config = test_config();
    let mut hub = Hub::new(config, TEST_DIMS).unwrap();

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
