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
//! # Async Task Model
//!
//! TuiClient runs as an independent async task via `run_task()`. It processes:
//! - `TuiRequest` from TuiRunner (user actions like input, resize, agent selection)
//! - `HubEvent` from Hub broadcast (agent lifecycle, shutdown)
//!
//! # Event Flow
//!
//! 1. PTY emits `PtyEvent::Output` -> forwarder task receives via broadcast
//!    -> sends `TuiOutput::Output` through channel -> TuiRunner receives and
//!    feeds to its vt100_parser
//! 2. User types -> TuiRunner sends `TuiRequest::SendInput` -> `run_task()` processes
//!    -> `Client::send_input().await`
//! 3. Agent selected -> `connect_to_pty()` fetches scrollback, sends via channel,
//!    spawns forwarder task
//!
//! # Why No Parser Here
//!
//! TuiClient is like BrowserClient - it routes bytes, it doesn't parse them.
//! TuiRunner owns the vt100 parser, just as the web browser owns xterm.js.

// Rust guideline compliant 2026-01

use tokio::sync::broadcast;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
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
    /// Send keyboard input to a specific PTY.
    ///
    /// Carries explicit agent and PTY indices from TuiRunner,
    /// which tracks the current selection via `current_agent_index`
    /// and `current_pty_index`.
    SendInput {
        /// Index of the agent in Hub's agent list.
        agent_index: usize,
        /// Index of the PTY within the agent (0=CLI, 1=server).
        pty_index: usize,
        /// Raw bytes to send (keyboard input).
        data: Vec<u8>,
    },

    /// Update terminal dimensions and resize a specific PTY.
    ///
    /// Carries explicit agent and PTY indices from TuiRunner,
    /// which tracks the current selection via `current_agent_index`
    /// and `current_pty_index`.
    SetDims {
        /// Index of the agent in Hub's agent list.
        agent_index: usize,
        /// Index of the PTY within the agent (0=CLI, 1=server).
        pty_index: usize,
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

    /// Disconnect from a specific PTY.
    ///
    /// Carries explicit agent and PTY indices from TuiRunner,
    /// which tracks the current selection via `current_agent_index`
    /// and `current_pty_index`.
    DisconnectFromPty {
        /// Index of the agent in Hub's agent list.
        agent_index: usize,
        /// Index of the PTY within the agent (0=CLI, 1=server).
        pty_index: usize,
    },

    // === Hub Operations ===
    /// Request Hub shutdown.
    Quit,

    /// List available worktrees for agent creation.
    ListWorktrees {
        /// Channel to receive list of (name, path) worktree pairs.
        response_tx: tokio::sync::oneshot::Sender<Vec<(String, String)>>,
    },

    /// Get the current connection code with QR PNG for display.
    GetConnectionCodeWithQr {
        /// Channel to receive the connection data (URL + QR PNG) or error message.
        response_tx: tokio::sync::oneshot::Sender<Result<crate::tui::ConnectionCodeData, String>>,
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

    /// Hub event forwarded from TuiClient to TuiRunner.
    ///
    /// TuiClient receives hub events via broadcast channel and forwards them
    /// to TuiRunner through the output channel. This keeps TuiRunner decoupled
    /// from the Hub's broadcast mechanism -- all communication flows through
    /// the TuiOutput channel.
    HubEvent(crate::hub::HubEvent),
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

    /// Channel for receiving requests from TuiRunner.
    ///
    /// TuiRunner sends `TuiRequest` messages through this channel, which are
    /// processed by `run_task()` in a `tokio::select!` loop. This mirrors how
    /// Browser sends requests to BrowserClient via WebSocket.
    request_rx: Option<UnboundedReceiver<TuiRequest>>,

    /// Broadcast receiver for Hub events (agent created/deleted/status, shutdown).
    ///
    /// Taken once by `run_task()` via `take_hub_event_rx()` and consumed in the
    /// async event loop. `None` after first take or if not provided at construction.
    hub_event_rx: Option<tokio::sync::broadcast::Receiver<crate::hub::HubEvent>>,
}

impl TuiClient {
    /// Create a new TUI client with default dimensions.
    ///
    /// # Arguments
    ///
    /// * `hub_handle` - Handle for Hub communication and agent queries.
    /// * `output_sink` - Channel sender for PTY output to TuiRunner.
    /// * `hub_event_rx` - Optional broadcast receiver for Hub events.
    #[must_use]
    pub fn new(
        hub_handle: HubHandle,
        output_sink: UnboundedSender<TuiOutput>,
        hub_event_rx: Option<tokio::sync::broadcast::Receiver<crate::hub::HubEvent>>,
    ) -> Self {
        Self::with_dims(hub_handle, output_sink, 80, 24, hub_event_rx)
    }

    /// Create a new TUI client with specific dimensions.
    ///
    /// # Arguments
    ///
    /// * `hub_handle` - Handle for Hub communication and agent queries.
    /// * `output_sink` - Channel sender for PTY output to TuiRunner.
    /// * `cols` - Terminal width in columns.
    /// * `rows` - Terminal height in rows.
    /// * `hub_event_rx` - Optional broadcast receiver for Hub events.
    #[must_use]
    pub fn with_dims(
        hub_handle: HubHandle,
        output_sink: UnboundedSender<TuiOutput>,
        cols: u16,
        rows: u16,
        hub_event_rx: Option<tokio::sync::broadcast::Receiver<crate::hub::HubEvent>>,
    ) -> Self {
        Self {
            hub_handle,
            id: ClientId::Tui,
            dims: (cols, rows),
            output_sink,
            output_task: None,
            request_rx: None,
            hub_event_rx,
        }
    }

    /// Get the hub handle for Hub communication.
    #[must_use]
    pub fn hub_handle(&self) -> &HubHandle {
        &self.hub_handle
    }

    /// Update terminal dimensions without propagating to PTY.
    ///
    /// Updates local dims only. PTY resize propagation happens through
    /// `TuiRequest::SetDims` which carries explicit agent and PTY indices.
    pub fn update_dims(&mut self, cols: u16, rows: u16) {
        self.dims = (cols, rows);
    }

    /// Set the request receiver for TuiRunner -> TuiClient communication.
    ///
    /// Called during Hub initialization to wire up the channel. TuiRunner holds
    /// the sender end and sends `TuiRequest` messages through it.
    pub fn set_request_receiver(&mut self, rx: UnboundedReceiver<TuiRequest>) {
        self.request_rx = Some(rx);
    }

    /// Handle a single request from TuiRunner.
    ///
    /// Every variant delegates to a Client trait method. TuiClient is pure
    /// transport - no business logic lives here.
    async fn handle_request(&mut self, request: TuiRequest) {
        match request {
            // === PTY I/O Operations (Client trait methods) ===
            TuiRequest::SendInput { agent_index, pty_index, data } => {
                if let Err(e) = self.send_input(agent_index, pty_index, &data).await {
                    log::error!("Failed to send input: {}", e);
                }
            }
            TuiRequest::SetDims { agent_index, pty_index, cols, rows } => {
                self.dims = (cols, rows);
                if let Err(e) = self.resize_pty(agent_index, pty_index, rows, cols).await {
                    log::error!("Failed to resize PTY: {}", e);
                }
            }
            TuiRequest::SelectAgent { index, response_tx } => {
                let result = self.select_agent(index).await;
                let _ = response_tx.send(result.ok().map(|m| TuiAgentMetadata {
                    agent_id: m.agent_id,
                    agent_index: m.agent_index,
                    has_server_pty: m.has_server_pty,
                }));
            }
            TuiRequest::ConnectToPty { agent_index, pty_index } => {
                if let Err(e) = self.connect_to_pty(agent_index, pty_index).await {
                    log::error!("Failed to connect to PTY: {}", e);
                }
            }
            TuiRequest::DisconnectFromPty { agent_index, pty_index } => {
                self.disconnect_from_pty(agent_index, pty_index).await;
            }

            // === Hub Management Operations (Client trait methods) ===
            TuiRequest::Quit => {
                if let Err(e) = self.quit().await {
                    log::error!("Failed to send quit command: {}", e);
                }
            }
            TuiRequest::CreateAgent { request } => {
                if let Err(e) = Client::create_agent(self, request).await {
                    log::error!("Failed to create agent: {}", e);
                }
            }
            TuiRequest::DeleteAgent { request } => {
                if let Err(e) = Client::delete_agent(self, request).await {
                    log::error!("Failed to delete agent: {}", e);
                }
            }
            TuiRequest::RegenerateConnectionCode => {
                if let Err(e) = self.regenerate_connection_code().await {
                    log::error!("Failed to regenerate connection code: {}", e);
                }
            }
            TuiRequest::CopyConnectionUrl => {
                if let Err(e) = self.copy_connection_url().await {
                    log::error!("Failed to copy connection URL: {}", e);
                }
            }
            TuiRequest::GetConnectionCodeWithQr { response_tx } => {
                // Use async path to ensure bundle is generated (like BrowserClient).
                // The sync trait method only reads from cache, which may be stale.
                let result = match self.hub_handle().get_connection_code_or_generate().await {
                    Ok(url) => {
                        crate::tui::generate_qr_png(&url, 4)
                            .map(|qr_png| crate::tui::ConnectionCodeData { url, qr_png })
                    }
                    Err(e) => Err(e),
                };
                let _ = response_tx.send(result);
            }

            // === Sync operations (HandleCache reads) ===
            TuiRequest::ListWorktrees { response_tx } => {
                let _ = response_tx.send(self.list_worktrees());
            }
        }
    }

    /// Run TuiClient as an independent async task.
    ///
    /// Processes requests from TuiRunner via `request_rx` and hub events via
    /// broadcast in a `tokio::select!` loop.
    pub async fn run_task(mut self) {
        let Some(mut request_rx) = self.request_rx.take() else {
            log::error!("TuiClient has no request receiver");
            return;
        };

        // If hub_event_rx is None (tests), create a dummy channel that never sends.
        let (_dummy_tx, mut hub_event_rx) = if let Some(rx) = self.take_hub_event_rx() {
            (None, rx)
        } else {
            let (tx, rx) = tokio::sync::broadcast::channel::<crate::hub::HubEvent>(1);
            (Some(tx), rx)
        };

        loop {
            tokio::select! {
                request = request_rx.recv() => {
                    match request {
                        Some(req) => self.handle_request(req).await,
                        None => {
                            log::info!("TuiRunner disconnected, stopping TuiClient task");
                            break;
                        }
                    }
                }
                event = hub_event_rx.recv() => {
                    match event {
                        Ok(hub_event) => self.handle_hub_event(hub_event).await,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            log::warn!("TuiClient lagged {} hub events", n);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            log::info!("Hub event channel closed, stopping TuiClient");
                            break;
                        }
                    }
                }
            }
        }

        log::info!("TuiClient task stopped");
    }

    /// Handle a hub event broadcast.
    ///
    /// Most events are forwarded directly to TuiRunner via `TuiOutput::HubEvent`.
    /// Special handling:
    /// - `AgentDeleted`: Disconnects from the deleted agent's PTYs before forwarding,
    ///   since the PTY handles will become invalid after Hub removes the agent.
    /// - `Shutdown`: Not forwarded. The `run_task` loop will break when the broadcast
    ///   channel closes, and TuiRunner detects shutdown via its own mechanisms.
    async fn handle_hub_event(&mut self, event: crate::hub::HubEvent) {
        use crate::hub::HubEvent;

        match &event {
            HubEvent::AgentDeleted { agent_id } => {
                // Find the agent's index in the handle cache so we can disconnect
                // from its PTYs before forwarding the event to TuiRunner.
                let agent_index = self
                    .hub_handle
                    .get_all_agent_handles()
                    .iter()
                    .position(|handle| handle.agent_id() == agent_id);

                if let Some(idx) = agent_index {
                    self.disconnect_from_pty(idx, 0).await;
                    self.disconnect_from_pty(idx, 1).await;
                }
                let _ = self.output_sink.send(TuiOutput::HubEvent(event));
            }
            HubEvent::Shutdown => {
                log::info!("TuiClient received shutdown event");
                // Don't forward -- run_task loop will break on Closed channel.
            }
            HubEvent::PtyConnectionRequested { .. }
            | HubEvent::PtyDisconnectionRequested { .. }
            | HubEvent::HttpConnectionRequested { .. } => {
                // Browser-specific, TuiClient ignores.
            }
            _ => {
                let _ = self.output_sink.send(TuiOutput::HubEvent(event));
            }
        }
    }
}

impl std::fmt::Debug for TuiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TuiClient")
            .field("id", &self.id)
            .field("dims", &self.dims)
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

    fn take_hub_event_rx(&mut self) -> Option<tokio::sync::broadcast::Receiver<crate::hub::HubEvent>> {
        self.hub_event_rx.take()
    }

    /// Connect to a PTY and start forwarding output (using pre-resolved handle).
    ///
    /// Steps:
    /// 1. Aborts previous output task if any
    /// 2. Gets PTY handle from agent
    /// 3. Calls `pty.connect()` to get scrollback
    /// 4. Sends scrollback through output channel
    /// 5. Subscribes to PTY events
    /// 6. Spawns forwarder task to route events to TuiRunner
    ///
    /// # Returns
    ///
    /// - `Ok(())` on success
    /// - `Err(message)` if PTY not found or connection fails
    async fn connect_to_pty_with_handle(
        &mut self,
        agent_handle: &AgentHandle,
        agent_index: usize,
        pty_index: usize,
    ) -> Result<(), String> {
        // Abort previous output task if any (like BrowserClient removes old channel).
        if let Some(task) = self.output_task.take() {
            task.abort();
        }

        // Get PTY handle from agent.
        let pty_handle = agent_handle
            .get_pty(pty_index)
            .ok_or_else(|| format!("PTY at index {} not found for agent", pty_index))?
            .clone();

        // Connect to PTY and get scrollback BEFORE spawning forwarder.
        // This ensures TuiRunner receives historical output first.
        // Direct sync connect - immediate, no async channel delay.
        let scrollback = pty_handle.connect_direct(self.id.clone(), self.dims)?;

        // Send scrollback to TuiRunner if available.
        if !scrollback.is_empty() {
            let _ = self.output_sink.send(TuiOutput::Scrollback(scrollback));
        }

        // Subscribe to PTY events for output forwarding.
        let pty_rx = pty_handle.subscribe();

        // Spawn output forwarder: PTY -> TuiRunner.
        let sink = self.output_sink.clone();
        self.output_task = Some(tokio::spawn(spawn_tui_output_forwarder(pty_rx, sink)));

        log::info!(
            "TUI connected to PTY ({}, {})",
            agent_index,
            pty_index
        );

        Ok(())
    }

    /// Disconnect from a PTY using an already-resolved handle.
    ///
    /// Overrides the default to also abort the output forwarder task.
    async fn disconnect_from_pty_with_handle(
        &mut self,
        pty: &crate::hub::agent_handle::PtyHandle,
        agent_index: usize,
        pty_index: usize,
    ) {
        // Abort output forwarder task if running.
        if let Some(task) = self.output_task.take() {
            task.abort();
        }

        // Notify PTY of disconnection - direct sync, immediate.
        let client_id = self.id.clone();
        pty.disconnect_direct(client_id);

        log::info!(
            "TUI disconnected from PTY ({}, {})",
            agent_index,
            pty_index
        );
    }

    async fn disconnect_from_pty(&mut self, agent_index: usize, pty_index: usize) {
        // Abort output forwarder task if running.
        if let Some(task) = self.output_task.take() {
            task.abort();
        }

        // Notify PTY of disconnection - direct sync, immediate.
        // hub_handle.get_agent() reads from HandleCache (non-blocking).
        if let Some(agent) = self.hub_handle.get_agent(agent_index) {
            if let Some(pty) = agent.get_pty(pty_index) {
                let client_id = self.id.clone();
                pty.disconnect_direct(client_id);
            }
        }

        log::info!(
            "TUI disconnected from PTY ({}, {})",
            agent_index,
            pty_index
        );
    }

    // NOTE: get_agent, send_input, resize_pty, select_agent, quit, create_agent,
    // delete_agent, regenerate_connection_code, copy_connection_url, list_worktrees,
    // get_connection_code all use DEFAULT IMPLEMENTATIONS from the trait

    async fn regenerate_prekey_bundle(&self) -> Result<crate::relay::signal::PreKeyBundleData, String> {
        let crypto_service = self.hub_handle
            .crypto_service()
            .ok_or_else(|| "Crypto service not available".to_string())?;
        let next_id = crypto_service.next_prekey_id().await.unwrap_or(1);
        crypto_service
            .get_prekey_bundle(next_id)
            .await
            .map_err(|e| format!("Failed to regenerate bundle: {}", e))
    }
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

    /// Helper to create a TuiClient with a mock HubHandle for testing.
    /// Returns both the client and the receiver for TuiOutput messages.
    fn test_client() -> (TuiClient, mpsc::UnboundedReceiver<TuiOutput>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let client = TuiClient::new(HubHandle::mock(), tx, None);
        (client, rx)
    }

    /// Helper to create a TuiClient with specific dimensions.
    /// Returns both the client and the receiver for TuiOutput messages.
    fn test_client_with_dims(cols: u16, rows: u16) -> (TuiClient, mpsc::UnboundedReceiver<TuiOutput>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let client = TuiClient::with_dims(HubHandle::mock(), tx, cols, rows, None);
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

    #[tokio::test]
    async fn test_handle_request_set_dims() {
        let (mut client, _output_rx) = test_client();

        // Send SetDims request directly via handle_request
        client.handle_request(TuiRequest::SetDims {
            agent_index: 0,
            pty_index: 0,
            cols: 120,
            rows: 40,
        }).await;

        // Dims should be updated (resize will fail silently with mock hub)
        assert_eq!(client.dims(), (120, 40));
    }

    #[tokio::test]
    async fn test_handle_request_multiple_set_dims() {
        let (mut client, _output_rx) = test_client();

        // Send multiple SetDims requests
        client.handle_request(TuiRequest::SetDims {
            agent_index: 0, pty_index: 0, cols: 100, rows: 30,
        }).await;
        client.handle_request(TuiRequest::SetDims {
            agent_index: 0, pty_index: 0, cols: 120, rows: 40,
        }).await;
        client.handle_request(TuiRequest::SetDims {
            agent_index: 0, pty_index: 0, cols: 80, rows: 24,
        }).await;

        // Final dims should be the last one
        assert_eq!(client.dims(), (80, 24));
    }

    #[test]
    fn test_tui_request_debug() {
        // Verify TuiRequest variants can be debugged
        let send_input = TuiRequest::SendInput { agent_index: 0, pty_index: 0, data: vec![1, 2, 3] };
        let set_dims = TuiRequest::SetDims { agent_index: 0, pty_index: 0, cols: 80, rows: 24 };
        let (response_tx, _rx) = tokio::sync::oneshot::channel();
        let select_agent = TuiRequest::SelectAgent { index: 0, response_tx };
        let connect = TuiRequest::ConnectToPty { agent_index: 0, pty_index: 0 };
        let disconnect = TuiRequest::DisconnectFromPty { agent_index: 0, pty_index: 0 };

        assert!(format!("{:?}", send_input).contains("SendInput"));
        assert!(format!("{:?}", set_dims).contains("SetDims"));
        assert!(format!("{:?}", select_agent).contains("SelectAgent"));
        assert!(format!("{:?}", connect).contains("ConnectToPty"));
        assert!(format!("{:?}", disconnect).contains("DisconnectFromPty"));
    }

    #[tokio::test]
    async fn test_run_task_shutdown() {
        let (mut client, _output_rx) = test_client();
        let (_tx, rx) = mpsc::unbounded_channel::<TuiRequest>();
        client.set_request_receiver(rx);

        // Give client a hub event receiver, then drop the sender to close the
        // broadcast channel. run_task exits when it sees Closed on the hub
        // event channel.
        let (hub_event_tx, hub_event_rx) = tokio::sync::broadcast::channel::<crate::hub::HubEvent>(16);
        client.hub_event_rx = Some(hub_event_rx);
        drop(hub_event_tx);

        // run_task should detect the closed broadcast channel and exit
        client.run_task().await;
        // If we reach here, the task completed successfully
    }

    #[tokio::test]
    async fn test_run_task_request_channel_closed() {
        let (mut client, _output_rx) = test_client();
        let (tx, rx) = mpsc::unbounded_channel::<TuiRequest>();
        client.set_request_receiver(rx);

        // Drop the request sender to close the channel
        drop(tx);

        // run_task should detect the closed channel and exit
        client.run_task().await;
        // If we reach here, the task completed successfully
    }

    #[tokio::test]
    async fn test_run_task_no_receiver() {
        let (client, _output_rx) = test_client();
        // Don't set request_rx - should log error and return immediately

        client.run_task().await;
        // If we reach here, the task handled missing receiver gracefully
    }

    // =========================================================================
    // Integration Tests: TuiRequest full flow with real Hub
    // =========================================================================
    //
    // These tests exercise the complete TuiRequest pipeline:
    //   TuiRunner sends TuiRequest -> TuiClient.handle_request() -> Client trait method -> PTY
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
        /// `PtyHandle::connect()` (used by `select_agent()`) won't hang.
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

        /// Create a TuiClient wired to the Hub's handle.
        fn create_tui_client(hub: &Hub) -> (TuiClient, mpsc::UnboundedReceiver<TuiOutput>) {
            let (tx, rx) = mpsc::unbounded_channel();
            let client = TuiClient::new(hub.handle(), tx, None);
            (client, rx)
        }

        // =====================================================================
        // TEST 1: SendInput reaches PTY via handle_request pipeline
        // =====================================================================

        /// Verify that TuiRequest::SendInput routes keyboard input to the PTY.
        #[test]
        fn test_tui_send_input_reaches_pty() {
            let mut hub = Hub::new(test_config()).unwrap();
            let agent_key = add_agent_to_hub(&mut hub, 42);

            let (mut client, _output_rx) = create_tui_client(&hub);

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

            // Process SendInput request (async, must run on Hub's runtime)
            hub.tokio_runtime.block_on(async {
                let input_data = b"echo hello\n".to_vec();
                client.handle_request(TuiRequest::SendInput {
                    agent_index: 0,
                    pty_index: 0,
                    data: input_data,
                }).await;
            });

            // With direct access, input goes straight to PTY writer (bypassing channel).
            // Verify that commands are NOT queued in the channel (proves direct write worked).
            let commands_processed = hub
                .state
                .write()
                .unwrap()
                .agents
                .get_mut(&agent_key)
                .unwrap()
                .cli_pty
                .process_commands();

            // Direct access bypasses the command channel - input is written directly to PTY.
            // If this assertion fails, it means direct access isn't working and input
            // is still going through the async channel (which has latency).
            assert_eq!(
                commands_processed, 0,
                "With direct access, input should bypass the command channel (was written directly to PTY)"
            );
        }

        // =====================================================================
        // TEST 2: SetDims resizes PTY through handle_request pipeline
        // =====================================================================

        /// Verify that TuiRequest::SetDims updates client dims and resizes PTY.
        #[test]
        fn test_tui_set_dims_resizes_pty() {
            let mut hub = Hub::new(test_config()).unwrap();
            let agent_key = add_agent_to_hub(&mut hub, 42);

            let (mut client, _output_rx) = create_tui_client(&hub);

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

            // Process SetDims request (async, must run on Hub's runtime)
            hub.tokio_runtime.block_on(async {
                client.handle_request(TuiRequest::SetDims {
                    agent_index: 0,
                    pty_index: 0,
                    cols: 120,
                    rows: 40,
                }).await;
            });

            // Verify TuiClient dims were updated
            assert_eq!(client.dims(), (120, 40), "TuiClient dims should be (120, 40)");

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
        #[test]
        fn test_tui_select_agent_connects_to_pty() {
            let mut hub = Hub::new(test_config()).unwrap();
            let _key_0 = add_agent_with_command_processor(&mut hub, 42);
            let _key_1 = add_agent_with_command_processor(&mut hub, 43);

            let (mut client, _output_rx) = create_tui_client(&hub);

            // Process SelectAgent request (async, must run on Hub's runtime)
            let metadata = hub.tokio_runtime.block_on(async {
                let (response_tx, response_rx) = tokio::sync::oneshot::channel();
                client.handle_request(TuiRequest::SelectAgent {
                    index: 0,
                    response_tx,
                }).await;

                // Verify response contains metadata
                response_rx
                    .await
                    .expect("Should receive response")
                    .expect("Should have metadata (agent exists at index 0)")
            });

            assert_eq!(metadata.agent_index, 0, "Agent index should be 0");
            assert!(
                !metadata.agent_id.is_empty(),
                "Agent ID should not be empty"
            );
            assert!(
                !metadata.has_server_pty,
                "Test agent should not have server PTY"
            );
        }

        // =====================================================================
        // TEST 4: SelectAgent returns correct metadata fields
        // =====================================================================

        /// Verify that SelectAgent metadata has correct agent_id, agent_index,
        /// and has_server_pty fields.
        #[test]
        fn test_tui_select_agent_returns_metadata() {
            let mut hub = Hub::new(test_config()).unwrap();
            let key_0 = add_agent_with_command_processor(&mut hub, 42);

            // Add a server PTY to the agent to test has_server_pty
            {
                let mut state = hub.state.write().unwrap();
                let agent = state.agents.get_mut(&key_0).unwrap();
                agent.server_pty = Some(crate::agent::PtySession::new(24, 80));
            }
            // Re-sync handle cache since we changed agent state
            hub.sync_handle_cache();

            let (mut client, _output_rx) = create_tui_client(&hub);

            // Process SelectAgent request (async, must run on Hub's runtime)
            let metadata = hub.tokio_runtime.block_on(async {
                let (response_tx, response_rx) = tokio::sync::oneshot::channel();
                client.handle_request(TuiRequest::SelectAgent {
                    index: 0,
                    response_tx,
                }).await;

                response_rx
                    .await
                    .expect("Should receive response")
                    .expect("Should have metadata")
            });

            // Verify metadata
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

        /// Verify that TuiRequest::DisconnectFromPty does not panic.
        #[test]
        fn test_tui_disconnect_from_pty() {
            let mut hub = Hub::new(test_config()).unwrap();
            let agent_key = add_agent_to_hub(&mut hub, 42);

            let (mut client, _output_rx) = create_tui_client(&hub);

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

            // Process DisconnectFromPty request - should not panic
            // (async, must run on Hub's runtime)
            hub.tokio_runtime.block_on(async {
                client.handle_request(TuiRequest::DisconnectFromPty {
                    agent_index: 0,
                    pty_index: 0,
                }).await;
            });
        }

        // =====================================================================
        // TEST 6: SelectAgent for non-existent index returns None
        // =====================================================================

        /// Verify that SelectAgent returns None when no agent exists at index.
        #[test]
        fn test_tui_select_agent_invalid_index_returns_none() {
            let hub = Hub::new(test_config()).unwrap();
            // No agents added - index 0 should not exist

            let (mut client, _output_rx) = create_tui_client(&hub);

            // Process SelectAgent request (async, must run on Hub's runtime)
            let result = hub.tokio_runtime.block_on(async {
                let (response_tx, response_rx) = tokio::sync::oneshot::channel();
                client.handle_request(TuiRequest::SelectAgent {
                    index: 0,
                    response_tx,
                }).await;

                response_rx
                    .await
                    .expect("Should receive response")
            });

            assert!(
                result.is_none(),
                "SelectAgent with no agents should return None"
            );
        }

        // =====================================================================
        // TEST 7: Full lifecycle: Select -> Input -> Resize -> Disconnect
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
            let mut hub = Hub::new(test_config()).unwrap();
            let agent_key = add_agent_with_command_processor(&mut hub, 42);

            let (mut client, _output_rx) = create_tui_client(&hub);

            // Clone state handle for use inside block_on (Arc<RwLock> clone is cheap)
            let state = hub.state.clone();

            // Run all async operations on Hub's runtime to avoid nested runtime panic
            hub.tokio_runtime.block_on(async {
                // Step 1: Select agent (connects to PTY via command processor)
                let (response_tx, response_rx) = tokio::sync::oneshot::channel();
                client.handle_request(TuiRequest::SelectAgent {
                    index: 0,
                    response_tx,
                }).await;

                let metadata = response_rx
                    .await
                    .unwrap()
                    .expect("Agent should exist");
                assert_eq!(metadata.agent_index, 0);

                // Step 2: Send input
                client.handle_request(TuiRequest::SendInput {
                    agent_index: 0,
                    pty_index: 0,
                    data: b"ls -la\n".to_vec(),
                }).await;

                // Give the command processor a moment to handle the Input command
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;

                // Step 3: Resize
                client.handle_request(TuiRequest::SetDims {
                    agent_index: 0,
                    pty_index: 0,
                    cols: 200,
                    rows: 50,
                }).await;

                // Give the command processor a moment to handle the Resize command
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;

                let dims = state
                    .read()
                    .unwrap()
                    .agents
                    .get(&agent_key)
                    .unwrap()
                    .cli_pty
                    .dimensions();
                assert_eq!(dims, (50, 200), "PTY should be resized to (50, 200)");

                // Step 4: Disconnect
                client.handle_request(TuiRequest::DisconnectFromPty {
                    agent_index: 0,
                    pty_index: 0,
                }).await;
            });
        }
    }
}
