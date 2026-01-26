//! TUI client implementation for the local terminal interface.
//!
//! `TuiClient` handles I/O routing for the local terminal, analogous to how
//! `BrowserClient` routes I/O to the web browser. The actual terminal emulation
//! (vt100 parsing) happens in `TuiRunner`, just as the web browser handles
//! its own terminal rendering.
//!
//! # Architecture
//!
//! ```text
//! TuiClient (I/O routing)          BrowserClient (I/O routing)
//!   ├── hub_handle                   ├── hub_handle
//!   ├── dims                         ├── dims
//!   ├── output_sink                  └── terminal_channels
//!   └── output_task                        │
//!          │                               ▼
//!          ▼                         Web Browser (rendering)
//! TuiRunner (rendering)               └── xterm.js
//!   └── vt100_parser
//! ```
//!
//! # Event Flow
//!
//! 1. PTY emits `PtyEvent::Output` → forwarder task receives via broadcast
//!    → sends `TuiOutput::Output` through channel → TuiRunner receives and
//!    feeds to its vt100_parser
//! 2. User types → TuiRunner routes input via hub_handle
//! 3. Agent selected → `connect_to_pty()` fetches scrollback, sends via channel,
//!    spawns forwarder task
//!
//! # Why No Parser Here
//!
//! TuiClient is like BrowserClient - it routes bytes, it doesn't parse them.
//! TuiRunner owns the vt100 parser, just as the web browser owns xterm.js.

// Rust guideline compliant 2026-01

use tokio::runtime::Handle;
use tokio::sync::broadcast;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::mpsc::error::TryRecvError;
use tokio::task::JoinHandle;

use crate::agent::pty::PtyEvent;
use crate::hub::hub_handle::HubHandle;
use crate::hub::AgentHandle;

use super::{Client, ClientId};

/// Requests from TuiRunner to TuiClient.
///
/// Symmetric with how Browser sends requests to BrowserClient via WebSocket.
/// Every variant maps to exactly one Client trait method call in `handle_request()`.
///
/// # Design Principle
///
/// TuiRunner should NOT know about Hub. All communication goes through TuiRequest.
/// TuiClient is pure transport - every `handle_request()` variant is a one-liner
/// delegating to a Client trait method. No business logic lives in TuiClient.
#[derive(Debug)]
pub enum TuiRequest {
    // === PTY I/O Operations ===
    /// Send keyboard input to the connected PTY.
    SendInput {
        /// Raw bytes to send (keyboard input).
        data: Vec<u8>,
    },

    /// Update terminal dimensions and resize connected PTY.
    SetDims {
        /// Terminal width in columns.
        cols: u16,
        /// Terminal height in rows.
        rows: u16,
    },

    /// Select an agent by index and connect to its CLI PTY.
    ///
    /// TuiClient handles the agent selection internally, including:
    /// - Looking up the agent by index
    /// - Connecting to its CLI PTY
    /// - Notifying Hub of the selection (SelectAgentForClient)
    /// - Returning metadata via the response channel
    SelectAgent {
        /// Zero-based agent index in Hub's agent list.
        index: usize,
        /// Channel to receive the selected agent's metadata (or None if not found).
        response_tx: tokio::sync::oneshot::Sender<Option<TuiAgentMetadata>>,
    },

    /// Connect to a specific PTY on the current agent.
    ConnectToPty {
        /// Index of the agent in Hub's agent list.
        agent_index: usize,
        /// Index of the PTY within the agent (0=CLI, 1=server).
        pty_index: usize,
    },

    /// Disconnect from the current PTY.
    DisconnectFromPty,

    // === Hub Operations ===
    /// Request Hub shutdown.
    Quit,

    /// List available worktrees for agent creation.
    ListWorktrees {
        /// Channel to receive list of (name, path) worktree pairs.
        response_tx: tokio::sync::oneshot::Sender<Vec<(String, String)>>,
    },

    /// Get the current connection code URL for QR display.
    GetConnectionCode {
        /// Channel to receive the connection URL or error message.
        response_tx: tokio::sync::oneshot::Sender<Result<String, String>>,
    },

    /// Create a new agent.
    CreateAgent {
        /// Agent creation parameters (worktree, issue, etc.).
        request: super::types::CreateAgentRequest,
    },

    /// Delete an existing agent.
    DeleteAgent {
        /// Agent deletion parameters (identifies which agent to remove).
        request: super::types::DeleteAgentRequest,
    },

    /// Regenerate the connection code (Signal bundle).
    RegenerateConnectionCode,

    /// Copy connection URL to clipboard.
    CopyConnectionUrl,
}

/// Metadata returned when selecting an agent for TUI.
///
/// Contains the essential information TuiRunner needs after selecting an agent,
/// without exposing Hub internals. This maintains the principle that TuiRunner
/// only interfaces through TuiRequest, not Hub types directly.
#[derive(Debug, Clone)]
pub struct TuiAgentMetadata {
    /// The agent's unique identifier (session key).
    pub agent_id: String,
    /// The agent's index in the Hub's ordered list.
    pub agent_index: usize,
    /// Whether this agent has a server PTY (index 1).
    pub has_server_pty: bool,
}

/// Output messages sent from TuiClient to TuiRunner.
///
/// Mirrors `TerminalMessage` used by BrowserClient. TuiRunner receives these
/// through the channel and processes them (feeding to vt100 parser, handling
/// process exit, etc.).
#[derive(Debug, Clone)]
pub enum TuiOutput {
    /// Historical output from before connection.
    ///
    /// Sent once when connecting to a PTY, contains the scrollback buffer.
    Scrollback(Vec<u8>),

    /// Ongoing PTY output.
    ///
    /// Sent whenever the PTY produces new output.
    Output(Vec<u8>),

    /// PTY process exited.
    ///
    /// Sent when the PTY process terminates. TuiRunner should handle this
    /// appropriately (e.g., show exit status, disable input).
    ProcessExited {
        /// Exit code from the PTY process, if available.
        exit_code: Option<i32>,
    },
}

/// TUI client - I/O routing for the local terminal.
///
/// Routes PTY events to TuiRunner via a channel, analogous to how BrowserClient
/// routes to the browser via WebSocket. Does NOT parse terminal output - that's
/// TuiRunner's job.
///
/// # What's Here (I/O Routing)
///
/// - Hub access via `hub_handle`
/// - Client identity (`id`)
/// - Terminal dimensions (`dims`)
/// - Output sink channel (`output_sink`)
/// - Output forwarder task (`output_task`)
///
/// # What's NOT Here (Rendering - in TuiRunner)
///
/// - vt100 parser (TuiRunner owns this)
/// - Scroll position (TuiRunner manages)
/// - Selection state (TuiRunner manages)
pub struct TuiClient {
    /// Thread-safe access to Hub state and operations.
    hub_handle: HubHandle,

    /// Tokio runtime handle for spawning async tasks.
    ///
    /// Stored directly to avoid blocking cross-thread calls when spawning
    /// forwarder tasks. Hub passes this at construction time.
    runtime: Handle,

    /// Unique identifier (always `ClientId::Tui`).
    id: ClientId,

    /// Terminal dimensions (cols, rows).
    dims: (u16, u16),

    /// Channel sender for PTY output to TuiRunner.
    ///
    /// When connected to a PTY, output events are forwarded through this channel.
    /// TuiRunner owns the receiver end.
    output_sink: UnboundedSender<TuiOutput>,

    /// Handle to the output forwarder task.
    ///
    /// Spawned by `connect_to_pty()`, aborted by `disconnect_from_pty()`.
    /// Unlike BrowserClient's `terminal_channels` (which supports multiple
    /// simultaneous connections), TUI only connects to one PTY at a time.
    output_task: Option<JoinHandle<()>>,

    /// Currently connected PTY indices, if any.
    ///
    /// Stores (agent_index, pty_index) when connected to a PTY.
    /// Used by `set_dims()` to propagate resize to the connected PTY.
    connected_pty: Option<(usize, usize)>,

    /// Channel for receiving requests from TuiRunner.
    ///
    /// TuiRunner sends `TuiRequest` messages through this channel, which are
    /// processed by `poll_requests()` in Hub's event loop. This mirrors how
    /// Browser sends requests to BrowserClient via WebSocket.
    request_rx: Option<UnboundedReceiver<TuiRequest>>,
}

impl TuiClient {
    /// Create a new TUI client with default dimensions.
    ///
    /// # Arguments
    ///
    /// * `hub_handle` - Handle for Hub communication and agent queries.
    /// * `output_sink` - Channel sender for PTY output to TuiRunner.
    /// * `runtime` - Tokio runtime handle for spawning async tasks.
    #[must_use]
    pub fn new(
        hub_handle: HubHandle,
        output_sink: UnboundedSender<TuiOutput>,
        runtime: Handle,
    ) -> Self {
        Self::with_dims(hub_handle, output_sink, runtime, 80, 24)
    }

    /// Create a new TUI client with specific dimensions.
    ///
    /// # Arguments
    ///
    /// * `hub_handle` - Handle for Hub communication and agent queries.
    /// * `output_sink` - Channel sender for PTY output to TuiRunner.
    /// * `runtime` - Tokio runtime handle for spawning async tasks.
    /// * `cols` - Terminal width in columns.
    /// * `rows` - Terminal height in rows.
    #[must_use]
    pub fn with_dims(
        hub_handle: HubHandle,
        output_sink: UnboundedSender<TuiOutput>,
        runtime: Handle,
        cols: u16,
        rows: u16,
    ) -> Self {
        Self {
            hub_handle,
            runtime,
            id: ClientId::Tui,
            dims: (cols, rows),
            output_sink,
            output_task: None,
            connected_pty: None,
            request_rx: None,
        }
    }

    /// Get the hub handle for Hub communication.
    #[must_use]
    pub fn hub_handle(&self) -> &HubHandle {
        &self.hub_handle
    }

    /// Update terminal dimensions without propagating to PTY.
    ///
    /// Updates local dims only. For full resize that propagates to the connected
    /// PTY, use `set_dims()` (the Client trait method).
    pub fn update_dims(&mut self, cols: u16, rows: u16) {
        self.dims = (cols, rows);
    }

    /// Get the currently connected PTY indices, if any.
    ///
    /// Returns `Some((agent_index, pty_index))` when connected to a PTY,
    /// `None` when not connected.
    #[must_use]
    pub fn connected_pty(&self) -> Option<(usize, usize)> {
        self.connected_pty
    }

    /// Clear connection state without notifying PTY.
    ///
    /// Used when PTY no longer exists (agent deleted) but client state needs cleanup.
    /// Aborts the output forwarder task and clears `connected_pty` tracking.
    pub fn clear_connection(&mut self) {
        if let Some(task) = self.output_task.take() {
            task.abort();
        }
        self.connected_pty = None;
    }

    /// Set connected PTY indices for testing.
    ///
    /// Allows tests to simulate a connected state without spawning async tasks.
    #[cfg(test)]
    pub fn set_connected_pty_for_test(&mut self, agent_index: usize, pty_index: usize) {
        self.connected_pty = Some((agent_index, pty_index));
    }

    /// Set the request receiver for TuiRunner → TuiClient communication.
    ///
    /// Called during Hub initialization to wire up the channel. TuiRunner holds
    /// the sender end and sends `TuiRequest` messages through it.
    pub fn set_request_receiver(&mut self, rx: UnboundedReceiver<TuiRequest>) {
        self.request_rx = Some(rx);
    }

    /// Poll for requests from TuiRunner and process them.
    ///
    /// Called from Hub's event loop. Processes up to 100 requests per tick
    /// to prevent blocking on high-volume input.
    pub fn poll_requests(&mut self) {
        let Some(rx) = &mut self.request_rx else { return };

        // Collect requests first to avoid borrow checker issues
        // (can't call handle_request while borrowing request_rx)
        let mut requests = Vec::with_capacity(100);
        for _ in 0..100 {
            match rx.try_recv() {
                Ok(request) => requests.push(request),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    log::warn!("TuiRunner disconnected from request channel");
                    break;
                }
            }
        }

        // Now process all collected requests
        for request in requests {
            self.handle_request(request);
        }
    }

    /// Handle a single request from TuiRunner.
    ///
    /// Every variant is a one-liner delegating to a Client trait method.
    /// TuiClient is pure transport - no business logic lives here.
    fn handle_request(&mut self, request: TuiRequest) {
        match request {
            // === PTY I/O Operations (Client trait methods) ===
            TuiRequest::SendInput { data } => {
                if let Some((agent_idx, pty_idx)) = self.connected_pty {
                    if let Err(e) = self.send_input(agent_idx, pty_idx, &data) {
                        log::error!("Failed to send input: {}", e);
                    }
                }
            }
            TuiRequest::SetDims { cols, rows } => {
                self.set_dims(cols, rows);
            }
            TuiRequest::SelectAgent { index, response_tx } => {
                let result = self.select_agent(index);
                let _ = response_tx.send(result.ok().map(|m| TuiAgentMetadata {
                    agent_id: m.agent_id,
                    agent_index: m.agent_index,
                    has_server_pty: m.has_server_pty,
                }));
            }
            TuiRequest::ConnectToPty { agent_index, pty_index } => {
                if let Err(e) = self.connect_to_pty(agent_index, pty_index) {
                    log::error!("Failed to connect to PTY: {}", e);
                }
            }
            TuiRequest::DisconnectFromPty => {
                if let Some((agent_idx, pty_idx)) = self.connected_pty {
                    self.disconnect_from_pty(agent_idx, pty_idx);
                }
            }

            // === Hub Management Operations (Client trait methods) ===
            TuiRequest::Quit => {
                if let Err(e) = self.quit() {
                    log::error!("Failed to send quit command: {}", e);
                }
            }
            TuiRequest::ListWorktrees { response_tx } => {
                let _ = response_tx.send(self.list_worktrees());
            }
            TuiRequest::GetConnectionCode { response_tx } => {
                let _ = response_tx.send(self.get_connection_code());
            }
            TuiRequest::CreateAgent { request } => {
                if let Err(e) = Client::create_agent(self, request) {
                    log::error!("Failed to create agent: {}", e);
                }
            }
            TuiRequest::DeleteAgent { request } => {
                if let Err(e) = Client::delete_agent(self, request) {
                    log::error!("Failed to delete agent: {}", e);
                }
            }
            TuiRequest::RegenerateConnectionCode => {
                if let Err(e) = self.regenerate_connection_code() {
                    log::error!("Failed to regenerate connection code: {}", e);
                }
            }
            TuiRequest::CopyConnectionUrl => {
                if let Err(e) = self.copy_connection_url() {
                    log::error!("Failed to copy connection URL: {}", e);
                }
            }
        }
    }
}

impl std::fmt::Debug for TuiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TuiClient")
            .field("id", &self.id)
            .field("dims", &self.dims)
            .field("connected_pty", &self.connected_pty)
            .finish_non_exhaustive()
    }
}

impl Client for TuiClient {
    fn id(&self) -> &ClientId {
        &self.id
    }

    fn dims(&self) -> (u16, u16) {
        self.dims
    }

    fn hub_handle(&self) -> &HubHandle {
        &self.hub_handle
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn set_dims(&mut self, cols: u16, rows: u16) {
        self.dims = (cols, rows);
        // Propagate resize to connected PTY via resize_pty()
        if let Some((agent_index, pty_index)) = self.connected_pty {
            if let Err(e) = self.resize_pty(agent_index, pty_index, rows, cols) {
                log::error!("Failed to resize PTY: {}", e);
            }
        }
    }

    /// Connect to a PTY and start forwarding output (using pre-resolved handle).
    ///
    /// Primary implementation of `Client::connect_to_pty_with_handle`. Uses the
    /// pre-resolved AgentHandle to look up the PTY without re-acquiring locks.
    ///
    /// Steps:
    /// 1. Aborts previous output task if any
    /// 2. Gets PTY handle from agent
    /// 3. Calls `pty.connect_blocking()` to get scrollback
    /// 4. Sends scrollback through output channel
    /// 5. Subscribes to PTY events
    /// 6. Spawns forwarder task to route events to TuiRunner
    ///
    /// # Returns
    ///
    /// - `Ok(())` on success
    /// - `Err(message)` if PTY not found or connection fails
    fn connect_to_pty_with_handle(
        &mut self,
        agent_handle: &AgentHandle,
        agent_index: usize,
        pty_index: usize,
    ) -> Result<(), String> {
        // Abort previous output task if any (like BrowserClient removes old channel).
        if let Some(task) = self.output_task.take() {
            task.abort();
        }
        self.connected_pty = None;

        // Get PTY handle from agent.
        let pty_handle = agent_handle
            .get_pty(pty_index)
            .ok_or_else(|| format!("PTY at index {} not found for agent", pty_index))?
            .clone();

        // Connect to PTY and get scrollback BEFORE spawning forwarder.
        // This ensures TuiRunner receives historical output first.
        let scrollback = pty_handle.connect_blocking(self.id.clone(), self.dims)?;

        // Send scrollback to TuiRunner if available.
        if !scrollback.is_empty() {
            // If receiver is dropped, this will fail - that's fine, we'll discover
            // it when the forwarder task tries to send.
            let _ = self.output_sink.send(TuiOutput::Scrollback(scrollback));
        }

        // Subscribe to PTY events for output forwarding.
        let pty_rx = pty_handle.subscribe();

        // Spawn output forwarder: PTY -> TuiRunner.
        // Uses stored runtime handle - no blocking cross-thread call needed.
        let sink = self.output_sink.clone();
        self.output_task = Some(self.runtime.spawn(spawn_tui_output_forwarder(pty_rx, sink)));

        // Track connected PTY indices for resize propagation.
        self.connected_pty = Some((agent_index, pty_index));

        log::info!(
            "TUI connected to PTY ({}, {})",
            agent_index,
            pty_index
        );

        Ok(())
    }

    /// Disconnect from a PTY using an already-resolved handle.
    ///
    /// Overrides the default to also clear `connected_pty` tracking and abort
    /// the output forwarder task.
    fn disconnect_from_pty_with_handle(
        &mut self,
        pty: &crate::hub::agent_handle::PtyHandle,
        agent_index: usize,
        pty_index: usize,
    ) {
        // Abort output forwarder task if running.
        if let Some(task) = self.output_task.take() {
            task.abort();
        }
        self.connected_pty = None;

        // Notify PTY of disconnection.
        let _ = pty.disconnect_blocking(self.id.clone());

        log::info!(
            "TUI disconnected from PTY ({}, {})",
            agent_index,
            pty_index
        );
    }

    fn disconnect_from_pty(&mut self, agent_index: usize, pty_index: usize) {
        // Abort output forwarder task if running.
        if let Some(task) = self.output_task.take() {
            task.abort();
        }
        self.connected_pty = None;

        // Notify PTY of disconnection.
        // NOTE: hub_handle.get_agent() reads from HandleCache (non-blocking).
        // However, disconnect_blocking() is blocking and must not be called from Hub's event loop.
        if let Some(agent) = self.hub_handle.get_agent(agent_index) {
            if let Some(pty) = agent.get_pty(pty_index) {
                let _ = pty.disconnect_blocking(self.id.clone());
            }
        }

        log::info!(
            "TUI disconnected from PTY ({}, {})",
            agent_index,
            pty_index
        );
    }

    // NOTE: get_agents, get_agent, send_input, resize_pty, agent_count
    // all use DEFAULT IMPLEMENTATIONS from the trait - not implemented here
}

/// Background task that forwards PTY output to TuiRunner via channel.
///
/// Mirrors `spawn_pty_output_forwarder` from browser.rs. Subscribes to PTY
/// events and sends `TuiOutput` messages through the channel.
/// Exits when the PTY closes or channel receiver is dropped.
async fn spawn_tui_output_forwarder(
    mut pty_rx: broadcast::Receiver<PtyEvent>,
    sink: UnboundedSender<TuiOutput>,
) {
    log::debug!("Started TUI output forwarder task");

    loop {
        match pty_rx.recv().await {
            Ok(PtyEvent::Output(data)) => {
                if sink.send(TuiOutput::Output(data)).is_err() {
                    // Receiver dropped, stop forwarding.
                    log::debug!("TUI output sink closed, stopping forwarder");
                    break;
                }
            }
            Ok(PtyEvent::ProcessExited { exit_code }) => {
                log::info!("TUI PTY process exited (code={:?})", exit_code);
                // Send exit notification, then continue - may have final output.
                let _ = sink.send(TuiOutput::ProcessExited { exit_code });
            }
            Ok(_other_event) => {
                // Ignore other events (Resized, OwnerChanged).
                // TuiRunner handles these through other mechanisms.
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                log::warn!("TUI output forwarder lagged by {} events", n);
                // Continue - we'll catch up with future events.
            }
            Err(broadcast::error::RecvError::Closed) => {
                log::debug!("PTY channel closed, stopping TUI forwarder");
                break;
            }
        }
    }

    log::debug!("Stopped TUI output forwarder task");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    /// Get a mock runtime handle for testing.
    ///
    /// Creates a runtime handle that can be used in tests. Since tests
    /// don't actually spawn async tasks, we just need a valid handle.
    fn mock_runtime_handle() -> Handle {
        // Use the current runtime if we're in a tokio context, otherwise create one
        Handle::try_current().unwrap_or_else(|_| {
            // Create a minimal runtime just for the handle
            let rt = tokio::runtime::Runtime::new().expect("Failed to create test runtime");
            rt.handle().clone()
        })
    }

    /// Helper to create a TuiClient with a mock HubHandle for testing.
    /// Returns both the client and the receiver for TuiOutput messages.
    fn test_client() -> (TuiClient, mpsc::UnboundedReceiver<TuiOutput>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let client = TuiClient::new(HubHandle::mock(), tx, mock_runtime_handle());
        (client, rx)
    }

    /// Helper to create a TuiClient with specific dimensions.
    /// Returns both the client and the receiver for TuiOutput messages.
    fn test_client_with_dims(cols: u16, rows: u16) -> (TuiClient, mpsc::UnboundedReceiver<TuiOutput>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let client = TuiClient::with_dims(HubHandle::mock(), tx, mock_runtime_handle(), cols, rows);
        (client, rx)
    }

    #[test]
    fn test_construction_default() {
        let (client, _rx) = test_client();
        assert_eq!(client.id(), &ClientId::Tui);
        assert_eq!(client.dims(), (80, 24));
    }

    #[test]
    fn test_construction_with_dims() {
        let (client, _rx) = test_client_with_dims(120, 40);
        assert_eq!(client.dims(), (120, 40));
    }

    #[test]
    fn test_update_dims() {
        let (mut client, _rx) = test_client();
        client.update_dims(100, 50);
        assert_eq!(client.dims(), (100, 50));
    }

    #[test]
    fn test_client_id_is_tui() {
        let (client, _rx) = test_client();
        assert!(client.id().is_tui());
    }

    #[test]
    fn test_hub_handle_accessible() {
        let (client, _rx) = test_client();
        // Should be able to access hub_handle
        let _handle = client.hub_handle();
    }

    #[test]
    fn test_debug_format() {
        let (client, _rx) = test_client();
        let debug_str = format!("{:?}", client);
        assert!(debug_str.contains("TuiClient"));
        assert!(debug_str.contains("id"));
        assert!(debug_str.contains("dims"));
    }

    #[test]
    fn test_connected_pty_initially_none() {
        let (client, _rx) = test_client();
        assert!(client.connected_pty.is_none());
    }

    #[test]
    fn test_output_task_initially_none() {
        let (client, _rx) = test_client();
        assert!(client.output_task.is_none());
    }

    #[test]
    fn test_tui_output_debug() {
        // Verify TuiOutput variants can be debugged
        let scrollback = TuiOutput::Scrollback(vec![1, 2, 3]);
        let output = TuiOutput::Output(vec![4, 5, 6]);
        let exited = TuiOutput::ProcessExited { exit_code: Some(0) };

        assert!(format!("{:?}", scrollback).contains("Scrollback"));
        assert!(format!("{:?}", output).contains("Output"));
        assert!(format!("{:?}", exited).contains("ProcessExited"));
    }

    #[test]
    fn test_request_rx_initially_none() {
        let (client, _rx) = test_client();
        assert!(client.request_rx.is_none());
    }

    #[test]
    fn test_set_request_receiver() {
        let (mut client, _output_rx) = test_client();
        let (_tx, rx) = mpsc::unbounded_channel::<TuiRequest>();

        client.set_request_receiver(rx);
        assert!(client.request_rx.is_some());
    }

    #[test]
    fn test_poll_requests_without_receiver() {
        let (mut client, _rx) = test_client();
        // Should not panic when no receiver is set
        client.poll_requests();
    }

    #[test]
    fn test_poll_requests_empty_channel() {
        let (mut client, _output_rx) = test_client();
        let (_tx, rx) = mpsc::unbounded_channel::<TuiRequest>();
        client.set_request_receiver(rx);

        // Should not panic with empty channel
        client.poll_requests();
    }

    #[test]
    fn test_poll_requests_set_dims() {
        let (mut client, _output_rx) = test_client();
        let (tx, rx) = mpsc::unbounded_channel::<TuiRequest>();
        client.set_request_receiver(rx);

        // Send SetDims request
        tx.send(TuiRequest::SetDims { cols: 120, rows: 40 }).unwrap();

        // Process it
        client.poll_requests();

        // Dims should be updated
        assert_eq!(client.dims(), (120, 40));
    }

    #[test]
    fn test_poll_requests_multiple() {
        let (mut client, _output_rx) = test_client();
        let (tx, rx) = mpsc::unbounded_channel::<TuiRequest>();
        client.set_request_receiver(rx);

        // Send multiple SetDims requests
        tx.send(TuiRequest::SetDims { cols: 100, rows: 30 }).unwrap();
        tx.send(TuiRequest::SetDims { cols: 120, rows: 40 }).unwrap();
        tx.send(TuiRequest::SetDims { cols: 80, rows: 24 }).unwrap();

        // Process all
        client.poll_requests();

        // Final dims should be the last one
        assert_eq!(client.dims(), (80, 24));
    }

    #[test]
    fn test_poll_requests_disconnected_channel() {
        let (mut client, _output_rx) = test_client();
        let (tx, rx) = mpsc::unbounded_channel::<TuiRequest>();
        client.set_request_receiver(rx);

        // Drop the sender
        drop(tx);

        // Should not panic, just log warning
        client.poll_requests();
    }

    #[test]
    fn test_tui_request_debug() {
        // Verify TuiRequest variants can be debugged
        let send_input = TuiRequest::SendInput { data: vec![1, 2, 3] };
        let set_dims = TuiRequest::SetDims { cols: 80, rows: 24 };
        let (response_tx, _rx) = tokio::sync::oneshot::channel();
        let select_agent = TuiRequest::SelectAgent { index: 0, response_tx };
        let connect = TuiRequest::ConnectToPty { agent_index: 0, pty_index: 0 };
        let disconnect = TuiRequest::DisconnectFromPty;

        assert!(format!("{:?}", send_input).contains("SendInput"));
        assert!(format!("{:?}", set_dims).contains("SetDims"));
        assert!(format!("{:?}", select_agent).contains("SelectAgent"));
        assert!(format!("{:?}", connect).contains("ConnectToPty"));
        assert!(format!("{:?}", disconnect).contains("DisconnectFromPty"));
    }

    // =========================================================================
    // Integration Tests: TuiRequest full flow with real Hub
    // =========================================================================
    //
    // These tests exercise the complete TuiRequest pipeline:
    //   TuiRunner sends TuiRequest -> TuiClient.poll_requests() -> Client trait method -> PTY
    //
    // Unlike the unit tests above (which use mock HubHandle), these create a
    // real Hub with real agents and PTY sessions to verify end-to-end behavior.

    mod integration {
        use super::*;
        use crate::agent::Agent;
        use crate::config::Config;
        use crate::hub::Hub;
        use std::path::PathBuf;
        use uuid::Uuid;

        /// Test configuration matching hub::tests::test_config().
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

        const TEST_DIMS: (u16, u16) = (24, 80);

        /// Create a test agent with given issue number.
        fn create_test_agent(issue: u32) -> (String, Agent) {
            let agent = Agent::new(
                Uuid::new_v4(),
                "test/repo".to_string(),
                Some(issue),
                format!("botster-issue-{}", issue),
                PathBuf::from("/tmp/test"),
            );
            let key = format!("test-repo-{}", issue);
            (key, agent)
        }

        /// Set up a Hub with a TuiClient wired through the request channel.
        ///
        /// Returns:
        /// - The Hub (owns all state)
        /// - The request sender (simulates TuiRunner sending requests)
        /// - The output receiver (receives TuiOutput from TuiClient)
        fn setup_tui_integration() -> (
            Hub,
            mpsc::UnboundedSender<TuiRequest>,
            mpsc::UnboundedReceiver<TuiOutput>,
        ) {
            let config = test_config();
            let mut hub = Hub::new(config, TEST_DIMS).unwrap();

            // Create the request channel (TuiRunner -> TuiClient)
            let (request_tx, request_rx) = mpsc::unbounded_channel();

            // Register TuiClient with both output and request channels
            let output_rx = hub.register_tui_client_with_request_channel(request_rx);

            (hub, request_tx, output_rx)
        }

        /// Add an agent to the hub and sync the handle cache.
        ///
        /// Returns the agent key for reference.
        fn add_agent_to_hub(hub: &mut Hub, issue: u32) -> String {
            let (key, agent) = create_test_agent(issue);
            hub.state.write().unwrap().add_agent(key.clone(), agent);
            hub.sync_handle_cache();
            key
        }

        /// Add an agent and spawn its PTY command processor so that
        /// `PtyHandle::connect_blocking()` (used by `select_agent()`) won't hang.
        ///
        /// The command processor runs as a tokio task on Hub's runtime, processing
        /// Connect, Input, Resize, and Disconnect commands from PtyHandle callers.
        fn add_agent_with_command_processor(hub: &mut Hub, issue: u32) -> String {
            let (key, agent) = create_test_agent(issue);
            hub.state.write().unwrap().add_agent(key.clone(), agent);

            // Spawn the command processor on the Hub's tokio runtime.
            // This is what Hub::lifecycle normally does after spawning an agent.
            let _guard = hub.tokio_runtime.enter();
            hub.state
                .write()
                .unwrap()
                .agents
                .get_mut(&key)
                .unwrap()
                .cli_pty
                .spawn_command_processor();

            hub.sync_handle_cache();
            key
        }

        /// Get a mutable reference to the TuiClient from the Hub's client registry.
        fn get_tui_client_mut(hub: &mut Hub) -> &mut TuiClient {
            hub.clients
                .get_tui_mut()
                .expect("TuiClient should be registered")
        }

        // =====================================================================
        // TEST 1: SendInput reaches PTY via TuiRequest pipeline
        // =====================================================================

        /// Verify that TuiRequest::SendInput routes keyboard input to the PTY.
        ///
        /// Full flow:
        /// 1. Setup Hub with agent and PTY
        /// 2. Connect TuiClient to agent's PTY
        /// 3. Send TuiRequest::SendInput through request channel
        /// 4. Call poll_requests() to process the request
        /// 5. Verify input command arrived at PTY's command channel
        #[test]
        fn test_tui_send_input_reaches_pty() {
            let (mut hub, request_tx, _output_rx) = setup_tui_integration();
            let agent_key = add_agent_to_hub(&mut hub, 42);

            // Connect TuiClient to agent's PTY (register as client for size ownership)
            {
                let state = hub.state.read().unwrap();
                let _ = state
                    .agents
                    .get(&agent_key)
                    .unwrap()
                    .cli_pty
                    .connect(ClientId::Tui, (80, 24));
            }
            get_tui_client_mut(&mut hub).set_connected_pty_for_test(0, 0);

            // Send input through the TuiRequest channel
            let input_data = b"echo hello\n".to_vec();
            request_tx
                .send(TuiRequest::SendInput {
                    data: input_data.clone(),
                })
                .unwrap();

            // Process the request through TuiClient
            get_tui_client_mut(&mut hub).poll_requests();

            // Verify the input command arrived at the PTY.
            // process_commands() drains the PTY's command channel and handles them.
            // Since we have no actual writer, the command will be processed but
            // the write will be a no-op. We verify the command was received by
            // checking process_commands returns > 0.
            let commands_processed = hub
                .state
                .write()
                .unwrap()
                .agents
                .get_mut(&agent_key)
                .unwrap()
                .cli_pty
                .process_commands();

            assert!(
                commands_processed > 0,
                "PTY should have received at least one command (Input) from SendInput request"
            );
        }

        // =====================================================================
        // TEST 2: SetDims resizes PTY through TuiRequest pipeline
        // =====================================================================

        /// Verify that TuiRequest::SetDims updates client dims and resizes PTY.
        ///
        /// Full flow:
        /// 1. Setup Hub with agent and PTY at default (24, 80)
        /// 2. Connect TuiClient to PTY (becomes size owner)
        /// 3. Send TuiRequest::SetDims { cols: 120, rows: 40 }
        /// 4. Call poll_requests()
        /// 5. Process PTY commands
        /// 6. Verify PTY dimensions are (40, 120)
        #[test]
        fn test_tui_set_dims_resizes_pty() {
            let (mut hub, request_tx, _output_rx) = setup_tui_integration();
            let agent_key = add_agent_to_hub(&mut hub, 42);

            // Connect TuiClient to PTY (becomes size owner)
            {
                let state = hub.state.read().unwrap();
                let _ = state
                    .agents
                    .get(&agent_key)
                    .unwrap()
                    .cli_pty
                    .connect(ClientId::Tui, (80, 24));
            }
            get_tui_client_mut(&mut hub).set_connected_pty_for_test(0, 0);

            // Verify initial PTY dimensions
            let initial_dims = hub
                .state
                .read()
                .unwrap()
                .agents
                .get(&agent_key)
                .unwrap()
                .cli_pty
                .dimensions();
            assert_eq!(initial_dims, (24, 80), "Initial PTY dims should be (24, 80)");

            // Send SetDims through the request channel
            request_tx
                .send(TuiRequest::SetDims {
                    cols: 120,
                    rows: 40,
                })
                .unwrap();

            // Process the request (TuiClient updates dims and sends resize command)
            get_tui_client_mut(&mut hub).poll_requests();

            // Verify TuiClient dims were updated
            let client_dims = hub.clients.get(&ClientId::Tui).unwrap().dims();
            assert_eq!(client_dims, (120, 40), "TuiClient dims should be (120, 40)");

            // Process PTY commands to apply the resize
            hub.state
                .write()
                .unwrap()
                .agents
                .get_mut(&agent_key)
                .unwrap()
                .cli_pty
                .process_commands();

            // Verify PTY dimensions were updated
            let pty_dims = hub
                .state
                .read()
                .unwrap()
                .agents
                .get(&agent_key)
                .unwrap()
                .cli_pty
                .dimensions();
            assert_eq!(
                pty_dims,
                (40, 120),
                "PTY dimensions should be (rows=40, cols=120) after SetDims"
            );
        }

        // =====================================================================
        // TEST 3: SelectAgent connects to PTY and returns metadata
        // =====================================================================

        /// Verify that TuiRequest::SelectAgent connects to an agent's PTY
        /// and returns correct metadata.
        ///
        /// This test requires the PTY command processor to be running because
        /// `select_agent()` calls `connect_to_pty_with_handle()` which calls
        /// `PtyHandle::connect_blocking()`. That method sends a Connect command
        /// through the PTY channel and waits for a response - without a command
        /// processor, it would hang forever.
        ///
        /// Full flow:
        /// 1. Setup Hub with 2 agents (command processors running)
        /// 2. Send TuiRequest::SelectAgent { index: 0 }
        /// 3. Call poll_requests()
        /// 4. Verify response contains correct agent metadata
        /// 5. Verify TuiClient is connected to agent 0's PTY
        #[test]
        fn test_tui_select_agent_connects_to_pty() {
            let (mut hub, request_tx, _output_rx) = setup_tui_integration();
            let _key_0 = add_agent_with_command_processor(&mut hub, 42);
            let _key_1 = add_agent_with_command_processor(&mut hub, 43);

            // Send SelectAgent request for agent at index 0
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            request_tx
                .send(TuiRequest::SelectAgent {
                    index: 0,
                    response_tx,
                })
                .unwrap();

            // Process the request.
            // poll_requests() -> handle_request(SelectAgent) -> select_agent(0) ->
            //   connect_to_pty(0, 0) -> connect_to_pty_with_handle() ->
            //     PtyHandle::connect_blocking() -> PtyCommand::Connect -> response
            get_tui_client_mut(&mut hub).poll_requests();

            // Verify response contains metadata
            let metadata = response_rx
                .blocking_recv()
                .expect("Should receive response")
                .expect("Should have metadata (agent exists at index 0)");

            assert_eq!(metadata.agent_index, 0, "Agent index should be 0");
            assert!(
                !metadata.agent_id.is_empty(),
                "Agent ID should not be empty"
            );
            assert!(
                !metadata.has_server_pty,
                "Test agent should not have server PTY"
            );

            // Verify TuiClient is now connected to agent 0's PTY
            let connected = get_tui_client_mut(&mut hub).connected_pty();
            assert_eq!(
                connected,
                Some((0, 0)),
                "TuiClient should be connected to (agent_index=0, pty_index=0)"
            );
        }

        // =====================================================================
        // TEST 4: SelectAgent returns correct metadata fields
        // =====================================================================

        /// Verify that SelectAgent metadata has correct agent_id, agent_index,
        /// and has_server_pty fields.
        #[test]
        fn test_tui_select_agent_returns_metadata() {
            let (mut hub, request_tx, _output_rx) = setup_tui_integration();
            let key_0 = add_agent_with_command_processor(&mut hub, 42);

            // Add a server PTY to the agent to test has_server_pty
            {
                let mut state = hub.state.write().unwrap();
                let agent = state.agents.get_mut(&key_0).unwrap();
                agent.server_pty = Some(crate::agent::PtySession::new(24, 80));
            }
            // Re-sync handle cache since we changed agent state
            hub.sync_handle_cache();

            // Send SelectAgent
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            request_tx
                .send(TuiRequest::SelectAgent {
                    index: 0,
                    response_tx,
                })
                .unwrap();

            // Process the request
            get_tui_client_mut(&mut hub).poll_requests();

            // Verify metadata
            let metadata = response_rx
                .blocking_recv()
                .expect("Should receive response")
                .expect("Should have metadata");

            // agent_id comes from AgentHandle which derives from agent_key
            assert!(
                !metadata.agent_id.is_empty(),
                "agent_id should be populated"
            );
            assert_eq!(metadata.agent_index, 0, "agent_index should be 0");
            assert!(
                metadata.has_server_pty,
                "has_server_pty should be true (we added a server PTY)"
            );
        }

        // =====================================================================
        // TEST 5: DisconnectFromPty clears connection state
        // =====================================================================

        /// Verify that TuiRequest::DisconnectFromPty clears the client's
        /// connection state.
        ///
        /// Full flow:
        /// 1. Setup Hub with agent
        /// 2. Connect TuiClient to PTY
        /// 3. Send TuiRequest::DisconnectFromPty
        /// 4. Call poll_requests()
        /// 5. Verify TuiClient is disconnected (connected_pty is None)
        #[test]
        fn test_tui_disconnect_from_pty() {
            let (mut hub, request_tx, _output_rx) = setup_tui_integration();
            let agent_key = add_agent_to_hub(&mut hub, 42);

            // First connect TuiClient to PTY
            {
                let state = hub.state.read().unwrap();
                let _ = state
                    .agents
                    .get(&agent_key)
                    .unwrap()
                    .cli_pty
                    .connect(ClientId::Tui, (80, 24));
            }
            get_tui_client_mut(&mut hub).set_connected_pty_for_test(0, 0);

            // Verify we're connected
            assert_eq!(
                get_tui_client_mut(&mut hub).connected_pty(),
                Some((0, 0)),
                "Should be connected before disconnect"
            );

            // Send DisconnectFromPty
            request_tx
                .send(TuiRequest::DisconnectFromPty)
                .unwrap();

            // Process the request
            get_tui_client_mut(&mut hub).poll_requests();

            // Verify TuiClient is disconnected
            let connected = get_tui_client_mut(&mut hub).connected_pty();
            assert_eq!(
                connected, None,
                "TuiClient should be disconnected after DisconnectFromPty"
            );
        }

        // =====================================================================
        // TEST 6: SelectAgent for non-existent index returns None
        // =====================================================================

        /// Verify that SelectAgent returns None when no agent exists at index.
        #[test]
        fn test_tui_select_agent_invalid_index_returns_none() {
            let (mut hub, request_tx, _output_rx) = setup_tui_integration();
            // No agents added - index 0 should not exist

            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            request_tx
                .send(TuiRequest::SelectAgent {
                    index: 0,
                    response_tx,
                })
                .unwrap();

            get_tui_client_mut(&mut hub).poll_requests();

            let result = response_rx
                .blocking_recv()
                .expect("Should receive response");
            assert!(
                result.is_none(),
                "SelectAgent with no agents should return None"
            );
        }

        // =====================================================================
        // TEST 7: SendInput without connection is a no-op
        // =====================================================================

        /// Verify that SendInput is silently ignored when not connected to any PTY.
        #[test]
        fn test_tui_send_input_without_connection_is_noop() {
            let (mut hub, request_tx, _output_rx) = setup_tui_integration();
            let _agent_key = add_agent_to_hub(&mut hub, 42);

            // Do NOT connect - TuiClient.connected_pty is None

            // Send input (should be silently ignored)
            request_tx
                .send(TuiRequest::SendInput {
                    data: b"echo hello\n".to_vec(),
                })
                .unwrap();

            // Process the request - should not panic
            get_tui_client_mut(&mut hub).poll_requests();

            // Verify no commands were sent to PTY
            let commands_processed = hub
                .state
                .write()
                .unwrap()
                .agents
                .get_mut("test-repo-42")
                .unwrap()
                .cli_pty
                .process_commands();

            assert_eq!(
                commands_processed, 0,
                "No commands should reach PTY when TuiClient is not connected"
            );
        }

        // =====================================================================
        // TEST 8: Full lifecycle: Select -> Input -> Resize -> Disconnect
        // =====================================================================

        /// End-to-end test exercising the full TuiRequest lifecycle.
        ///
        /// Simulates a typical user session:
        /// 1. Select an agent (connect to PTY via command processor)
        /// 2. Send keyboard input
        /// 3. Resize terminal
        /// 4. Disconnect
        #[test]
        fn test_tui_full_lifecycle() {
            let (mut hub, request_tx, _output_rx) = setup_tui_integration();
            let agent_key = add_agent_with_command_processor(&mut hub, 42);

            // Step 1: Select agent (connects to PTY via command processor)
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            request_tx
                .send(TuiRequest::SelectAgent {
                    index: 0,
                    response_tx,
                })
                .unwrap();
            get_tui_client_mut(&mut hub).poll_requests();

            let metadata = response_rx
                .blocking_recv()
                .unwrap()
                .expect("Agent should exist");
            assert_eq!(metadata.agent_index, 0);
            assert!(get_tui_client_mut(&mut hub).connected_pty().is_some());

            // Step 2: Send input
            request_tx
                .send(TuiRequest::SendInput {
                    data: b"ls -la\n".to_vec(),
                })
                .unwrap();
            get_tui_client_mut(&mut hub).poll_requests();

            // Give the command processor a moment to handle the Input command
            std::thread::sleep(std::time::Duration::from_millis(50));

            // Step 3: Resize
            request_tx
                .send(TuiRequest::SetDims {
                    cols: 200,
                    rows: 50,
                })
                .unwrap();
            get_tui_client_mut(&mut hub).poll_requests();

            // Give the command processor a moment to handle the Resize command
            std::thread::sleep(std::time::Duration::from_millis(50));

            let dims = hub
                .state
                .read()
                .unwrap()
                .agents
                .get(&agent_key)
                .unwrap()
                .cli_pty
                .dimensions();
            assert_eq!(dims, (50, 200), "PTY should be resized to (50, 200)");

            // Step 4: Disconnect
            request_tx
                .send(TuiRequest::DisconnectFromPty)
                .unwrap();
            get_tui_client_mut(&mut hub).poll_requests();

            assert!(
                get_tui_client_mut(&mut hub).connected_pty().is_none(),
                "Should be disconnected"
            );
        }

        // =====================================================================
        // TEST: poll_all_requests() processes TuiRequests (regression test)
        // =====================================================================

        /// Regression test: verify `ClientRegistry::poll_all_requests()` processes
        /// TuiRequest messages sent through the request channel.
        ///
        /// This test reproduces the critical bug where `poll_requests()` was never
        /// called from the Hub's main event loop, causing every `blocking_recv()`
        /// in TuiRunner to deadlock. The fix was adding
        /// `hub.clients.poll_all_requests()` to the Hub loop in `run_with_hub()`.
        ///
        /// # What this tests
        ///
        /// The multi-threaded pattern used in production:
        /// 1. TuiRunner thread sends TuiRequest with oneshot response channel
        /// 2. TuiRunner thread calls `blocking_recv()` on the oneshot
        /// 3. Hub thread calls `hub.clients.poll_all_requests()`
        /// 4. TuiClient processes the request and sends response
        /// 5. TuiRunner thread unblocks
        ///
        /// Without `poll_all_requests()`, step 3 never happens and step 5 deadlocks.
        #[test]
        fn test_poll_all_requests_processes_tui_requests() {
            let (mut hub, request_tx, _output_rx) = setup_tui_integration();
            add_agent_with_command_processor(&mut hub, 42);

            // Simulate TuiRunner: send SelectAgent from a separate thread
            let tx = request_tx.clone();
            let handle = std::thread::spawn(move || {
                let (response_tx, response_rx) = tokio::sync::oneshot::channel();
                tx.send(TuiRequest::SelectAgent {
                    index: 0,
                    response_tx,
                })
                .expect("channel open");

                // This would deadlock without poll_all_requests()
                response_rx.blocking_recv().expect("response channel open")
            });

            // Simulate Hub loop: poll client requests (the fix)
            // Small delay to let the thread send its request
            std::thread::sleep(std::time::Duration::from_millis(50));
            hub.clients.poll_all_requests();

            // Verify the thread completed without deadlock
            let result = handle.join().expect("thread should not panic");
            assert!(result.is_some(), "SelectAgent should return metadata");

            let metadata = result.unwrap();
            assert_eq!(metadata.agent_index, 0);
            assert!(!metadata.agent_id.is_empty());
        }

        /// Regression test: verify `poll_all_requests()` handles ListWorktrees.
        ///
        /// ListWorktrees is used when opening the New Agent modal. Now reads
        /// directly from HandleCache (no Hub command channel round-trip),
        /// which eliminates the re-entrant deadlock entirely.
        #[test]
        fn test_poll_all_requests_processes_list_worktrees() {
            let (mut hub, request_tx, _output_rx) = setup_tui_integration();

            // Pre-populate worktrees in HandleCache (Hub maintains this)
            hub.handle_cache.set_worktrees(vec![
                ("/tmp/wt1".to_string(), "feature-1".to_string()),
                ("/tmp/wt2".to_string(), "feature-2".to_string()),
            ]);

            let tx = request_tx.clone();
            let handle = std::thread::spawn(move || {
                let (response_tx, response_rx) = tokio::sync::oneshot::channel();
                tx.send(TuiRequest::ListWorktrees { response_tx })
                    .expect("channel open");

                // Reads from HandleCache - no deadlock possible
                response_rx.blocking_recv().expect("response channel open")
            });

            // Simulate Hub loop: poll client requests
            std::thread::sleep(std::time::Duration::from_millis(50));
            hub.clients.poll_all_requests();

            let worktrees = handle.join().expect("thread should not panic");
            assert_eq!(worktrees.len(), 2);
            assert_eq!(worktrees[0].1, "feature-1");
        }

        /// Regression test: verify `poll_all_requests()` handles GetConnectionCode.
        ///
        /// GetConnectionCode is called on EVERY RENDER FRAME while the connection
        /// code modal is open. Now reads directly from HandleCache (no Hub
        /// command channel round-trip), which eliminates the deadlock entirely.
        #[test]
        fn test_poll_all_requests_processes_get_connection_code() {
            let (mut hub, request_tx, _output_rx) = setup_tui_integration();

            // Pre-populate cached connection URL in HandleCache (Hub maintains this)
            hub.handle_cache.set_connection_url(Ok(
                "https://botster.dev/hubs/123#TESTBUNDLE".to_string(),
            ));

            let tx = request_tx.clone();
            let handle = std::thread::spawn(move || {
                let (response_tx, response_rx) = tokio::sync::oneshot::channel();
                tx.send(TuiRequest::GetConnectionCode { response_tx })
                    .expect("channel open");

                // Reads from HandleCache - no deadlock possible
                response_rx.blocking_recv().expect("response channel open")
            });

            std::thread::sleep(std::time::Duration::from_millis(50));
            hub.clients.poll_all_requests();

            let result = handle.join().expect("thread should not panic");
            assert!(result.is_ok());
            assert!(result.unwrap().contains("TESTBUNDLE"));
        }
    }
}
