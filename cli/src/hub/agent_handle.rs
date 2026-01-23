//! Agent handle for client-to-agent communication.
//!
//! `AgentHandle` provides a clean interface for clients to interact with agents.
//! Clients obtain handles via `HubCommand::GetAgent`, then use the handle to:
//! - Get agent info
//! - Access PTY sessions (subscribe, send input, resize)
//!
//! # Hierarchy
//!
//! ```text
//! Hub
//!   └── GetAgent(id) → AgentHandle
//!                         ├── info() → AgentInfo
//!                         ├── cli_pty() → PtyHandle
//!                         └── server_pty() → Option<PtyHandle>
//!
//! PtyHandle
//!   ├── subscribe() → broadcast::Receiver<PtyEvent>
//!   ├── write_input(data) → sends input to PTY
//!   └── resize(rows, cols) → resizes PTY
//! ```
//!
//! This matches the CLIENT_REFACTOR_DESIGN.md architecture where clients
//! call `pty.write_input()` directly rather than through Hub.

// Rust guideline compliant 2026-01

use tokio::sync::{broadcast, mpsc};

use crate::agent::pty::PtyEvent;
use crate::client::ClientId;
use crate::relay::types::AgentInfo;

/// Command sent through PtyHandle to the PtySession.
#[derive(Debug, Clone)]
pub enum PtyCommand {
    /// Send input data to the PTY.
    Input(Vec<u8>),
    /// Resize the PTY (from a specific client).
    Resize {
        /// The client requesting the resize.
        client_id: ClientId,
        /// Number of rows.
        rows: u16,
        /// Number of columns.
        cols: u16,
    },
    /// Client connected to this PTY.
    Connect {
        /// The connecting client.
        client_id: ClientId,
        /// Terminal dimensions (rows, cols).
        dims: (u16, u16),
    },
    /// Client disconnected from this PTY.
    Disconnect {
        /// The disconnecting client.
        client_id: ClientId,
    },
}

/// Handle for interacting with an agent.
///
/// Clients obtain this via `HubCommand::GetAgent`. The handle provides
/// access to agent info and PTY sessions without exposing internal state.
///
/// # Thread Safety
///
/// `AgentHandle` is `Clone` + `Send` + `Sync`, allowing it to be passed
/// across threads and shared between async tasks.
#[derive(Debug, Clone)]
pub struct AgentHandle {
    /// Agent identifier.
    agent_id: String,

    /// Agent info snapshot at time of handle creation.
    info: AgentInfo,

    /// CLI PTY handle.
    cli_pty: PtyHandle,

    /// Server PTY handle (if server is running).
    server_pty: Option<PtyHandle>,
}

impl AgentHandle {
    /// Create a new agent handle.
    ///
    /// Called internally by Hub when processing `GetAgent` command.
    ///
    /// # Arguments
    ///
    /// * `agent_id` - Unique agent identifier
    /// * `info` - Agent info snapshot
    /// * `cli_pty_event_tx` - Broadcast sender for CLI PTY events
    /// * `cli_pty_cmd_tx` - Command channel for CLI PTY operations
    /// * `server_pty_event_tx` - Optional broadcast sender for server PTY events
    /// * `server_pty_cmd_tx` - Optional command channel for server PTY operations
    #[must_use]
    pub fn new(
        agent_id: impl Into<String>,
        info: AgentInfo,
        cli_pty_event_tx: broadcast::Sender<PtyEvent>,
        cli_pty_cmd_tx: mpsc::Sender<PtyCommand>,
        server_pty_event_tx: Option<broadcast::Sender<PtyEvent>>,
        server_pty_cmd_tx: Option<mpsc::Sender<PtyCommand>>,
    ) -> Self {
        let server_pty = match (server_pty_event_tx, server_pty_cmd_tx) {
            (Some(event_tx), Some(cmd_tx)) => Some(PtyHandle::new(event_tx, cmd_tx)),
            _ => None,
        };

        Self {
            agent_id: agent_id.into(),
            info,
            cli_pty: PtyHandle::new(cli_pty_event_tx, cli_pty_cmd_tx),
            server_pty,
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

    /// Get PTY handle by index.
    ///
    /// - Index 0: CLI PTY (always present)
    /// - Index 1: Server PTY (if server is running)
    ///
    /// Returns `None` if index is out of bounds or PTY doesn't exist.
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
        match pty_index {
            0 => Some(&self.cli_pty),
            1 => self.server_pty.as_ref(),
            _ => None,
        }
    }

    /// Get the CLI PTY handle.
    ///
    /// All agents have a CLI PTY for Claude Code interaction.
    /// Equivalent to `get_pty(0).unwrap()`.
    #[must_use]
    pub fn cli_pty(&self) -> &PtyHandle {
        &self.cli_pty
    }

    /// Get the server PTY handle (if available).
    ///
    /// Only present when the agent has spawned a dev server.
    /// Equivalent to `get_pty(1)`.
    #[must_use]
    pub fn server_pty(&self) -> Option<&PtyHandle> {
        self.server_pty.as_ref()
    }

    /// Check if this agent has a server PTY.
    #[must_use]
    pub fn has_server_pty(&self) -> bool {
        self.server_pty.is_some()
    }

    /// Get the number of PTYs available.
    ///
    /// Returns 1 if only CLI PTY, 2 if server PTY also exists.
    #[must_use]
    pub fn pty_count(&self) -> usize {
        if self.server_pty.is_some() {
            2
        } else {
            1
        }
    }
}

/// Handle for interacting with a PTY session.
///
/// Provides both event subscription and direct PTY interaction:
/// - `subscribe()` to receive PTY events (output, resize, exit)
/// - `write_input()` to send input to the PTY
/// - `resize()` to notify PTY of client resize
/// - `connect()` / `disconnect()` for client lifecycle
///
/// # Example
///
/// ```ignore
/// let handle = hub.get_agent("agent-123").await?;
/// let pty = handle.cli_pty();
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
/// ```
#[derive(Debug, Clone)]
pub struct PtyHandle {
    /// Broadcast sender for PTY events.
    ///
    /// Clients subscribe via `subscribe()` to receive events.
    event_tx: broadcast::Sender<PtyEvent>,

    /// Command channel for PTY operations.
    ///
    /// Sends input, resize, connect/disconnect commands to PtySession.
    command_tx: mpsc::Sender<PtyCommand>,
}

impl PtyHandle {
    /// Create a new PTY handle.
    #[must_use]
    pub fn new(
        event_tx: broadcast::Sender<PtyEvent>,
        command_tx: mpsc::Sender<PtyCommand>,
    ) -> Self {
        Self {
            event_tx,
            command_tx,
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

    /// Write input to the PTY.
    ///
    /// This is the primary method for sending user input to the terminal.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed (PTY terminated).
    pub async fn write_input(&self, data: &[u8]) -> Result<(), String> {
        self.command_tx
            .send(PtyCommand::Input(data.to_vec()))
            .await
            .map_err(|_| "PTY command channel closed".to_string())
    }

    /// Write input to the PTY (blocking version).
    ///
    /// Use this from synchronous code (e.g., TUI thread).
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub fn write_input_blocking(&self, data: &[u8]) -> Result<(), String> {
        self.command_tx
            .blocking_send(PtyCommand::Input(data.to_vec()))
            .map_err(|_| "PTY command channel closed".to_string())
    }

    /// Notify PTY of client resize.
    ///
    /// If this client is the size owner, the PTY will be resized.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub async fn resize(&self, client_id: ClientId, rows: u16, cols: u16) -> Result<(), String> {
        self.command_tx
            .send(PtyCommand::Resize {
                client_id,
                rows,
                cols,
            })
            .await
            .map_err(|_| "PTY command channel closed".to_string())
    }

    /// Notify PTY of client resize (blocking version).
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub fn resize_blocking(&self, client_id: ClientId, rows: u16, cols: u16) -> Result<(), String> {
        self.command_tx
            .blocking_send(PtyCommand::Resize {
                client_id,
                rows,
                cols,
            })
            .map_err(|_| "PTY command channel closed".to_string())
    }

    /// Connect a client to this PTY.
    ///
    /// The PTY will track the client and may become the size owner.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub async fn connect(&self, client_id: ClientId, dims: (u16, u16)) -> Result<(), String> {
        self.command_tx
            .send(PtyCommand::Connect { client_id, dims })
            .await
            .map_err(|_| "PTY command channel closed".to_string())
    }

    /// Connect a client to this PTY (blocking version).
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub fn connect_blocking(&self, client_id: ClientId, dims: (u16, u16)) -> Result<(), String> {
        self.command_tx
            .blocking_send(PtyCommand::Connect { client_id, dims })
            .map_err(|_| "PTY command channel closed".to_string())
    }

    /// Disconnect a client from this PTY.
    ///
    /// The PTY will update its client list and may change size owner.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub async fn disconnect(&self, client_id: ClientId) -> Result<(), String> {
        self.command_tx
            .send(PtyCommand::Disconnect { client_id })
            .await
            .map_err(|_| "PTY command channel closed".to_string())
    }

    /// Disconnect a client from this PTY (blocking version).
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub fn disconnect_blocking(&self, client_id: ClientId) -> Result<(), String> {
        self.command_tx
            .blocking_send(PtyCommand::Disconnect { client_id })
            .map_err(|_| "PTY command channel closed".to_string())
    }

    /// Get the number of active event subscribers.
    ///
    /// Useful for debugging and monitoring.
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.event_tx.receiver_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_info() -> AgentInfo {
        AgentInfo {
            id: "test-agent".to_string(),
            repo: Some("owner/repo".to_string()),
            issue_number: Some(42),
            branch_name: Some("botster-issue-42".to_string()),
            name: None,
            status: Some("Running".to_string()),
            tunnel_port: None,
            server_running: None,
            has_server_pty: None,
            active_pty_view: None,
            scroll_offset: None,
            hub_identifier: None,
        }
    }

    #[test]
    fn test_agent_handle_creation() {
        let (event_tx, _) = broadcast::channel(16);
        let (cmd_tx, _) = mpsc::channel(16);
        let handle = AgentHandle::new("agent-123", test_info(), event_tx, cmd_tx, None, None);

        assert_eq!(handle.agent_id(), "agent-123");
        assert_eq!(handle.info().id, "test-agent");
        assert!(!handle.has_server_pty());
    }

    #[test]
    fn test_agent_handle_with_server_pty() {
        let (cli_event_tx, _) = broadcast::channel(16);
        let (cli_cmd_tx, _) = mpsc::channel(16);
        let (server_event_tx, _) = broadcast::channel(16);
        let (server_cmd_tx, _) = mpsc::channel(16);
        let handle = AgentHandle::new(
            "agent-123",
            test_info(),
            cli_event_tx,
            cli_cmd_tx,
            Some(server_event_tx),
            Some(server_cmd_tx),
        );

        assert!(handle.has_server_pty());
        assert!(handle.server_pty().is_some());
    }

    #[test]
    fn test_get_pty_index_based_access() {
        let (cli_event_tx, _) = broadcast::channel(16);
        let (cli_cmd_tx, _) = mpsc::channel(16);
        let handle = AgentHandle::new(
            "agent-123",
            test_info(),
            cli_event_tx,
            cli_cmd_tx,
            None,
            None,
        );

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
        let (cli_event_tx, _) = broadcast::channel(16);
        let (cli_cmd_tx, _) = mpsc::channel(16);
        let (server_event_tx, _) = broadcast::channel(16);
        let (server_cmd_tx, _) = mpsc::channel(16);
        let handle = AgentHandle::new(
            "agent-123",
            test_info(),
            cli_event_tx,
            cli_cmd_tx,
            Some(server_event_tx),
            Some(server_cmd_tx),
        );

        // Both PTYs present
        assert!(handle.get_pty(0).is_some());
        assert!(handle.get_pty(1).is_some());
        assert!(handle.get_pty(2).is_none());
    }

    #[test]
    fn test_pty_count() {
        // Without server PTY
        let (cli_event_tx, _) = broadcast::channel(16);
        let (cli_cmd_tx, _) = mpsc::channel(16);
        let handle = AgentHandle::new(
            "agent-123",
            test_info(),
            cli_event_tx,
            cli_cmd_tx,
            None,
            None,
        );
        assert_eq!(handle.pty_count(), 1);

        // With server PTY
        let (cli_event_tx, _) = broadcast::channel(16);
        let (cli_cmd_tx, _) = mpsc::channel(16);
        let (server_event_tx, _) = broadcast::channel(16);
        let (server_cmd_tx, _) = mpsc::channel(16);
        let handle = AgentHandle::new(
            "agent-456",
            test_info(),
            cli_event_tx,
            cli_cmd_tx,
            Some(server_event_tx),
            Some(server_cmd_tx),
        );
        assert_eq!(handle.pty_count(), 2);
    }

    #[test]
    fn test_pty_handle_subscribe() {
        let (event_tx, _) = broadcast::channel(16);
        let (cmd_tx, _) = mpsc::channel(16);
        let handle = PtyHandle::new(event_tx.clone(), cmd_tx);

        // Subscribe creates a new receiver
        let _rx = handle.subscribe();
        assert_eq!(handle.subscriber_count(), 1);

        // Multiple subscriptions
        let _rx2 = handle.subscribe();
        assert_eq!(handle.subscriber_count(), 2);
    }

    #[tokio::test]
    async fn test_pty_handle_receives_events() {
        let (event_tx, _) = broadcast::channel(16);
        let (cmd_tx, _) = mpsc::channel(16);
        let handle = PtyHandle::new(event_tx.clone(), cmd_tx);
        let mut rx = handle.subscribe();

        // Send an event
        event_tx.send(PtyEvent::Output(b"hello".to_vec())).unwrap();

        // Receiver gets it
        let event = rx.recv().await.unwrap();
        match event {
            PtyEvent::Output(data) => assert_eq!(data, b"hello"),
            _ => panic!("Expected Output event"),
        }
    }

    #[tokio::test]
    async fn test_pty_handle_write_input() {
        let (event_tx, _) = broadcast::channel(16);
        let (cmd_tx, mut cmd_rx) = mpsc::channel(16);
        let handle = PtyHandle::new(event_tx, cmd_tx);

        // Write input
        handle.write_input(b"hello").await.unwrap();

        // Verify command received
        let cmd = cmd_rx.recv().await.unwrap();
        match cmd {
            PtyCommand::Input(data) => assert_eq!(data, b"hello"),
            _ => panic!("Expected Input command"),
        }
    }

    #[tokio::test]
    async fn test_pty_handle_resize() {
        let (event_tx, _) = broadcast::channel(16);
        let (cmd_tx, mut cmd_rx) = mpsc::channel(16);
        let handle = PtyHandle::new(event_tx, cmd_tx);

        // Resize
        handle.resize(ClientId::Tui, 24, 80).await.unwrap();

        // Verify command received
        let cmd = cmd_rx.recv().await.unwrap();
        match cmd {
            PtyCommand::Resize {
                client_id,
                rows,
                cols,
            } => {
                assert_eq!(client_id, ClientId::Tui);
                assert_eq!(rows, 24);
                assert_eq!(cols, 80);
            }
            _ => panic!("Expected Resize command"),
        }
    }

    #[tokio::test]
    async fn test_pty_handle_connect_disconnect() {
        let (event_tx, _) = broadcast::channel(16);
        let (cmd_tx, mut cmd_rx) = mpsc::channel(16);
        let handle = PtyHandle::new(event_tx, cmd_tx);

        // Connect
        handle.connect(ClientId::Tui, (24, 80)).await.unwrap();
        let cmd = cmd_rx.recv().await.unwrap();
        match cmd {
            PtyCommand::Connect { client_id, dims } => {
                assert_eq!(client_id, ClientId::Tui);
                assert_eq!(dims, (24, 80));
            }
            _ => panic!("Expected Connect command"),
        }

        // Disconnect
        handle.disconnect(ClientId::Tui).await.unwrap();
        let cmd = cmd_rx.recv().await.unwrap();
        match cmd {
            PtyCommand::Disconnect { client_id } => {
                assert_eq!(client_id, ClientId::Tui);
            }
            _ => panic!("Expected Disconnect command"),
        }
    }
}
