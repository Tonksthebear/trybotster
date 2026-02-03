//! Agent handle for client-to-agent communication.
//!
//! `AgentHandle` provides a clean interface for clients to interact with agents.
//! Clients obtain handles and use them to:
//! - Get agent info
//! - Access PTY sessions (subscribe, send input, resize)
//!
//! # How to Get Agent Handles
//!
//! There are two ways to obtain handles, depending on thread context:
//!
//! ## 1. Via HandleCache (preferred for clients)
//!
//! Clients using `HubHandle` read directly from the cache:
//!
//! ```text
//! HubHandle::get_agent(idx) → HandleCache → Option<AgentHandle>
//! ```
//!
//! Use this from TuiClient, BrowserClient, or any code on Hub's thread.
//!
//! ## 2. Via GetAgentByIndex command (TuiRunner only)
//!
//! TuiRunner runs on a separate thread and uses blocking commands:
//!
//! ```text
//! TuiRunner → HubCommand::GetAgentByIndex(idx) → Hub → AgentHandle
//! ```
//!
//! # Hierarchy
//!
//! ```text
//! AgentHandle
//!   ├── info() → AgentInfo
//!   ├── get_pty(0) → Option<&PtyHandle>  (CLI PTY)
//!   └── get_pty(1) → Option<&PtyHandle>  (Server PTY)
//!
//! PtyHandle
//!   ├── subscribe() → broadcast::Receiver<PtyEvent>
//!   ├── write_input(data) → sends input to PTY
//!   └── resize(rows, cols) → resizes PTY
//! ```
//!
//! # PTY Indexing
//!
//! PTYs are accessed by index:
//! - Index 0: CLI PTY (always present)
//! - Index 1: Server PTY (present when server is running)
//!
//! This matches the CLIENT_REFACTOR_DESIGN.md architecture where clients
//! call `pty.write_input()` directly rather than through Hub.

// Rust guideline compliant 2026-01

use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use crate::agent::pty::{
    process_connect_command, process_disconnect_command, process_resize_command, PtyEvent,
    SharedPtyState,
};
use crate::client::ClientId;
use crate::relay::types::AgentInfo;

/// Handle for interacting with an agent.
///
/// Clients obtain this via:
/// - `HubHandle::get_agent()` - reads from HandleCache (preferred)
/// - `HubCommand::GetAgentByIndex` - cross-thread only (TuiRunner)
///
/// The handle provides access to agent info and PTY sessions without
/// exposing internal state.
///
/// # Thread Safety
///
/// `AgentHandle` is `Clone` + `Send` + `Sync`, allowing it to be passed
/// across threads and shared between async tasks.
///
/// # PTY Access
///
/// PTYs are accessed by index via `get_pty()`:
/// - Index 0: CLI PTY (always present)
/// - Index 1: Server PTY (present when server is running)
#[derive(Debug, Clone)]
pub struct AgentHandle {
    /// Agent identifier.
    agent_id: String,

    /// Agent info snapshot at time of handle creation.
    info: AgentInfo,

    /// PTY handles for this agent.
    ///
    /// - ptys[0]: CLI PTY (always present)
    /// - ptys[1]: Server PTY (if server is running)
    ptys: Vec<PtyHandle>,

    /// Index of this agent in the Hub's agent list.
    ///
    /// Used for index-based navigation in clients.
    agent_index: usize,
}

impl AgentHandle {
    /// Create a new agent handle.
    ///
    /// Called internally by Hub when processing `GetAgentByIndex` command.
    ///
    /// # Arguments
    ///
    /// * `agent_id` - Unique agent identifier
    /// * `info` - Agent info snapshot
    /// * `ptys` - Vector of PTY handles (index 0 = CLI, index 1 = Server if present)
    /// * `agent_index` - Index of this agent in the Hub's ordered agent list
    ///
    /// # Panics
    ///
    /// Panics if `ptys` is empty. At minimum, the CLI PTY must be present.
    #[must_use]
    pub fn new(
        agent_id: impl Into<String>,
        info: AgentInfo,
        ptys: Vec<PtyHandle>,
        agent_index: usize,
    ) -> Self {
        assert!(!ptys.is_empty(), "AgentHandle requires at least one PTY (CLI PTY)");

        Self {
            agent_id: agent_id.into(),
            info,
            ptys,
            agent_index,
        }
    }

    /// Get the agent ID.
    #[must_use]
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Get agent info snapshot.
    ///
    /// Note: This is a snapshot from when the handle was created.
    /// For live status, re-fetch via `GetAgent` or subscribe to `HubEvent`.
    #[must_use]
    pub fn info(&self) -> &AgentInfo {
        &self.info
    }

    /// Get the agent's index in the Hub's ordered agent list.
    ///
    /// Used for index-based navigation in clients.
    #[must_use]
    pub fn agent_index(&self) -> usize {
        self.agent_index
    }

    /// Get PTY handle by index.
    ///
    /// - Index 0: CLI PTY (always present)
    /// - Index 1: Server PTY (if server is running)
    ///
    /// Returns `None` if index is out of bounds.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Get CLI PTY
    /// let cli_pty = handle.get_pty(0).unwrap();
    ///
    /// // Get server PTY (may be None)
    /// if let Some(server_pty) = handle.get_pty(1) {
    ///     // Server is running
    /// }
    /// ```
    #[must_use]
    pub fn get_pty(&self, pty_index: usize) -> Option<&PtyHandle> {
        self.ptys.get(pty_index)
    }

    /// Get the number of PTYs available.
    ///
    /// Returns 1 if only CLI PTY, 2 if server PTY also exists.
    #[must_use]
    pub fn pty_count(&self) -> usize {
        self.ptys.len()
    }
}

/// Handle for interacting with a PTY session.
///
/// Provides both event subscription and direct PTY interaction:
/// - `subscribe()` to receive PTY events (output, resize, exit)
/// - `write_input()` to send input to the PTY
/// - `resize()` to notify PTY of client resize
/// - `connect()` / `disconnect()` for client lifecycle
/// - `port()` to get the HTTP forwarding port (if assigned)
///
/// # Example
///
/// ```ignore
/// let handle = hub.get_agent_by_index(0).await?.unwrap();
/// let pty = handle.get_pty(0).unwrap();
///
/// // Subscribe to output events
/// let mut rx = pty.subscribe();
///
/// // Send input directly to PTY
/// pty.write_input(b"ls -la\n")?;
///
/// while let Ok(event) = rx.recv().await {
///     match event {
///         PtyEvent::Output(data) => process_output(&data),
///         PtyEvent::Resized { rows, cols } => update_size(rows, cols),
///         // ...
///     }
/// }
///
/// // Get the HTTP forwarding port (for server PTY)
/// if let Some(port) = pty.port() {
///     println!("Dev server on port {}", port);
/// }
/// ```
#[derive(Clone)]
pub struct PtyHandle {
    /// Broadcast sender for PTY events.
    ///
    /// Clients subscribe via `subscribe()` to receive events.
    event_tx: broadcast::Sender<PtyEvent>,

    /// Direct access to shared PTY state for sync I/O.
    ///
    /// Enables immediate input/connect/resize without async channel hop.
    shared_state: Arc<Mutex<SharedPtyState>>,

    /// Direct access to scrollback buffer for sync connect.
    scrollback_buffer: Arc<Mutex<VecDeque<u8>>>,

    /// HTTP forwarding port for preview proxying.
    ///
    /// Primarily used by server PTYs (pty_index=1) to expose the dev server
    /// port for HTTP preview. `None` for CLI PTYs or if no port assigned.
    port: Option<u16>,
}

impl std::fmt::Debug for PtyHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyHandle")
            .field("port", &self.port)
            .finish()
    }
}

impl PtyHandle {
    /// Create a new PTY handle with direct sync access.
    ///
    /// Direct access enables immediate I/O operations without async channel delays:
    /// - `write_input_direct()` - sync input, no channel hop
    /// - `connect_direct()` - sync connect, immediate scrollback
    /// - `resize_direct()` - sync resize
    ///
    /// # Arguments
    ///
    /// * `event_tx` - Broadcast sender for PTY events
    /// * `shared_state` - Direct access to PTY writer and state
    /// * `scrollback_buffer` - Direct access to scrollback
    /// * `port` - HTTP forwarding port (for server PTYs), or `None`
    #[must_use]
    pub fn new(
        event_tx: broadcast::Sender<PtyEvent>,
        shared_state: Arc<Mutex<SharedPtyState>>,
        scrollback_buffer: Arc<Mutex<VecDeque<u8>>>,
        port: Option<u16>,
    ) -> Self {
        Self {
            event_tx,
            shared_state,
            scrollback_buffer,
            port,
        }
    }

    /// Subscribe to PTY events.
    ///
    /// Returns a receiver that will receive all PTY events:
    /// - `Output(Vec<u8>)` - Terminal output data
    /// - `Resized { rows, cols }` - PTY was resized
    /// - `ProcessExited { exit_code }` - PTY process exited
    /// - `OwnerChanged { new_owner }` - Size ownership changed
    ///
    /// # Lagging
    ///
    /// If the receiver falls behind, it will receive
    /// `broadcast::error::RecvError::Lagged(n)` indicating how many
    /// events were missed. Handle this gracefully (e.g., request redraw).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<PtyEvent> {
        self.event_tx.subscribe()
    }

    /// Get the number of active event subscribers.
    ///
    /// Useful for debugging and monitoring.
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.event_tx.receiver_count()
    }

    /// Get the HTTP forwarding port for this PTY.
    ///
    /// Returns the port allocated for HTTP preview proxying, or `None` if
    /// no port has been assigned. This is primarily used for server PTYs
    /// (pty_index=1) running dev servers.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let server_pty = agent_handle.get_pty(1).unwrap();
    /// if let Some(port) = server_pty.port() {
    ///     // Proxy HTTP requests to localhost:port
    /// }
    /// ```
    #[must_use]
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// Get a copy of the current scrollback buffer.
    ///
    /// Returns the accumulated terminal output for replay on connect.
    #[must_use]
    pub fn get_scrollback(&self) -> Vec<u8> {
        self.scrollback_buffer
            .lock()
            .map(|buf| buf.iter().copied().collect())
            .unwrap_or_default()
    }

    // =========================================================================
    // Direct Sync Methods - Immediate I/O without async channel
    // =========================================================================

    /// Write input directly to the PTY.
    ///
    /// Locks the shared state mutex and writes directly to the PTY writer.
    /// This is the fastest path for sending input - no async channel hop.
    ///
    /// # Errors
    ///
    /// Returns an error if write fails.
    pub fn write_input_direct(&self, data: &[u8]) -> Result<(), String> {
        let mut state = self.shared_state
            .lock()
            .map_err(|_| "shared_state lock poisoned")?;

        if let Some(writer) = &mut state.writer {
            writer
                .write_all(data)
                .map_err(|e| format!("Failed to write PTY input: {e}"))?;
            writer
                .flush()
                .map_err(|e| format!("Failed to flush PTY writer: {e}"))?;
            Ok(())
        } else {
            Err("PTY writer not available".to_string())
        }
    }

    /// Connect a client directly.
    ///
    /// Registers the client, resizes the PTY to their dimensions, and
    /// returns the scrollback buffer immediately.
    pub fn connect_direct(&self, client_id: ClientId, dims: (u16, u16)) -> Result<Vec<u8>, String> {
        Ok(process_connect_command(
            &client_id,
            dims,
            &self.shared_state,
            &self.event_tx,
            &self.scrollback_buffer,
        ))
    }

    /// Resize the PTY directly.
    ///
    /// Checks if the client is the size owner and resizes if so.
    pub fn resize_direct(&self, client_id: ClientId, rows: u16, cols: u16) {
        process_resize_command(&client_id, rows, cols, &self.shared_state, &self.event_tx);
    }

    /// Disconnect a client directly.
    pub fn disconnect_direct(&self, client_id: ClientId) {
        process_disconnect_command(&client_id, &self.shared_state, &self.event_tx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::pty::PtySession;

    fn test_info() -> AgentInfo {
        AgentInfo {
            id: "test-agent".to_string(),
            repo: Some("owner/repo".to_string()),
            issue_number: Some(42),
            branch_name: Some("botster-issue-42".to_string()),
            name: None,
            status: Some("Running".to_string()),
            port: None,
            server_running: None,
            has_server_pty: None,
            active_pty_view: None,
            scroll_offset: None,
            hub_identifier: None,
        }
    }

    /// Helper to create a PTY handle for testing (no port).
    fn create_test_pty() -> PtyHandle {
        create_test_pty_with_port(None)
    }

    /// Helper to create a PTY handle for testing with a specific port.
    fn create_test_pty_with_port(port: Option<u16>) -> PtyHandle {
        let pty_session = PtySession::new(24, 80);
        let (shared_state, scrollback, event_tx) = pty_session.get_direct_access();
        // Leak the session to keep the state alive for tests
        std::mem::forget(pty_session);
        PtyHandle::new(event_tx, shared_state, scrollback, port)
    }

    #[test]
    fn test_agent_handle_creation() {
        let ptys = vec![create_test_pty()];
        let handle = AgentHandle::new("agent-123", test_info(), ptys, 0);

        assert_eq!(handle.agent_id(), "agent-123");
        assert_eq!(handle.info().id, "test-agent");
        assert_eq!(handle.agent_index(), 0);
        assert_eq!(handle.pty_count(), 1);
    }

    #[test]
    fn test_agent_handle_with_server_pty() {
        let ptys = vec![create_test_pty(), create_test_pty()];
        let handle = AgentHandle::new("agent-123", test_info(), ptys, 0);

        assert_eq!(handle.pty_count(), 2);
        assert!(handle.get_pty(0).is_some());
        assert!(handle.get_pty(1).is_some());
    }

    #[test]
    fn test_get_pty_index_based_access() {
        let ptys = vec![create_test_pty()];
        let handle = AgentHandle::new("agent-123", test_info(), ptys, 0);

        // Index 0 is CLI PTY (always present)
        assert!(handle.get_pty(0).is_some());

        // Index 1 is Server PTY (not present in this case)
        assert!(handle.get_pty(1).is_none());

        // Index 2+ always None
        assert!(handle.get_pty(2).is_none());
        assert!(handle.get_pty(99).is_none());
    }

    #[test]
    fn test_get_pty_with_server() {
        let ptys = vec![create_test_pty(), create_test_pty()];
        let handle = AgentHandle::new("agent-123", test_info(), ptys, 0);

        // Both PTYs present
        assert!(handle.get_pty(0).is_some());
        assert!(handle.get_pty(1).is_some());
        assert!(handle.get_pty(2).is_none());
    }

    #[test]
    fn test_pty_count() {
        // Without server PTY
        let ptys = vec![create_test_pty()];
        let handle = AgentHandle::new("agent-123", test_info(), ptys, 0);
        assert_eq!(handle.pty_count(), 1);

        // With server PTY
        let ptys = vec![create_test_pty(), create_test_pty()];
        let handle = AgentHandle::new("agent-456", test_info(), ptys, 1);
        assert_eq!(handle.pty_count(), 2);
        assert_eq!(handle.agent_index(), 1);
    }

    #[test]
    #[should_panic(expected = "AgentHandle requires at least one PTY")]
    fn test_agent_handle_panics_on_empty_ptys() {
        let ptys: Vec<PtyHandle> = vec![];
        let _ = AgentHandle::new("agent-123", test_info(), ptys, 0);
    }

    #[test]
    fn test_pty_handle_subscribe() {
        let handle = create_test_pty();

        // Subscribe creates a new receiver
        let _rx = handle.subscribe();
        assert_eq!(handle.subscriber_count(), 1);

        // Multiple subscriptions
        let _rx2 = handle.subscribe();
        assert_eq!(handle.subscriber_count(), 2);
    }

    #[test]
    fn test_pty_handle_port() {
        // Without port
        let handle = create_test_pty();
        assert!(handle.port().is_none());

        // With port
        let handle_with_port = create_test_pty_with_port(Some(8080));
        assert_eq!(handle_with_port.port(), Some(8080));
    }

    #[test]
    fn test_pty_handle_connect_direct() {
        let handle = create_test_pty();

        // Connect returns empty scrollback for new session
        let scrollback = handle.connect_direct(ClientId::Tui, (80, 24)).unwrap();
        assert!(scrollback.is_empty());
    }

    #[test]
    fn test_pty_handle_resize_direct() {
        let handle = create_test_pty();

        // Connect first to become size owner
        let _ = handle.connect_direct(ClientId::Tui, (80, 24));

        // Resize should work without panic
        handle.resize_direct(ClientId::Tui, 100, 50);
    }

    #[test]
    fn test_pty_handle_disconnect_direct() {
        let handle = create_test_pty();

        // Connect first
        let _ = handle.connect_direct(ClientId::Tui, (80, 24));

        // Disconnect should work without panic
        handle.disconnect_direct(ClientId::Tui);
    }
}
