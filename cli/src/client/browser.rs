//! Browser client implementation for WebSocket connections.
//!
//! `BrowserClient` represents a browser connection via WebSocket/ActionCable.
//! Unlike TUI (which displays one PTY at a time), BrowserClient supports
//! multiple simultaneous PTY connections - one per browser tab.
//!
//! # Architecture
//!
//! ```text
//! BrowserClient
//!   ├── hub_handle (required - thread-safe access to Hub state)
//!   ├── id (ClientId::Browser(identity))
//!   ├── dims (cols, rows)
//!   ├── identity (Signal identity key)
//!   ├── request_tx (cloned to each input receiver task)
//!   ├── request_rx (consumed by run_task())
//!   └── terminal_channels (HashMap keyed by (agent_index, pty_index))
//!         └── TerminalChannel
//!               ├── channel (ActionCableChannel for WebSocket)
//!               └── task handles for cleanup
//! ```
//!
//! # Async Task Model
//!
//! BrowserClient runs as an independent async task via `run_task()`. It processes:
//! - `BrowserRequest` from background input receiver tasks (keyboard input, resize)
//! - `HubEvent` from Hub broadcast (agent lifecycle, shutdown)
//!
//! # PTY I/O Routing
//!
//! When a browser connects to a PTY via `connect_to_pty()`:
//!
//! 1. Creates a TerminalRelayChannel (ActionCable with E2E encryption)
//! 2. Subscribes to PTY events via PtyHandle
//! 3. Spawns output forwarder: PTY events -> channel -> browser
//! 4. Spawns input receiver: channel -> BrowserRequest -> run_task() -> Client trait
//!
//! # Minimal Design
//!
//! BrowserClient implements only the required Client trait methods:
//! - `hub_handle()`, `id()`, `dims()`, `connect_to_pty_with_handle()`,
//!   `disconnect_from_pty()`, `disconnect_from_pty_with_handle()`
//!
//! Default trait implementations handle:
//! - `get_agent`, `send_input`, `resize_pty`, `select_agent`, `quit`,
//!   `create_agent`, `delete_agent`, etc.

// Rust guideline compliant 2026-01

use std::collections::HashMap;

use tokio::sync::broadcast;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::agent::pty::PtyEvent;
use crate::channel::{ActionCableChannel, Channel, ChannelConfig, ChannelSenderHandle, PeerId};
use crate::hub::HubHandle;
use crate::relay::crypto_service::CryptoServiceHandle;
use crate::relay::{
    build_scrollback_message, build_worktree_info, AgentCreationStage, BrowserCommand,
    TerminalMessage, WorktreeInfo,
};

use super::http_channel::HttpChannel;
use super::{Client, ClientId, CreateAgentRequest, DeleteAgentRequest};

/// Requests from Browser to BrowserClient.
///
/// Symmetric with `TuiRequest`. Browser background tasks send these via channel,
/// BrowserClient routes them through Client trait methods in its async task loop.
///
/// Unlike `TuiRequest`, these include PTY target indices on every variant because
/// Browser supports multiple simultaneous PTY connections (one per tab).
#[derive(Debug)]
pub enum BrowserRequest {
    /// Send keyboard input to a specific PTY.
    SendInput {
        /// Index of the target agent in Hub's ordered list.
        agent_index: usize,
        /// Index of the PTY within the agent (0 = CLI, 1 = Server).
        pty_index: usize,
        /// Raw input bytes to send to the PTY.
        data: Vec<u8>,
    },

    /// Resize a specific PTY.
    Resize {
        /// Index of the target agent in Hub's ordered list.
        agent_index: usize,
        /// Index of the PTY within the agent (0 = CLI, 1 = Server).
        pty_index: usize,
        /// New terminal height in rows.
        rows: u16,
        /// New terminal width in columns.
        cols: u16,
    },
}

/// Terminal channel for a single PTY connection.
///
/// Bundles the ActionCable channel with its associated task handles.
/// When dropped, the channel disconnects and tasks are aborted.
struct TerminalChannel {
    /// The ActionCable channel for WebSocket communication.
    ///
    /// Provides E2E encrypted communication with the browser.
    #[expect(dead_code, reason = "Channel held for lifetime, tasks use handles")]
    channel: ActionCableChannel,

    /// Output forwarder task handle (PTY -> Browser).
    ///
    /// Aborted on drop.
    output_task: JoinHandle<()>,

    /// Input receiver task handle (Browser -> PTY).
    ///
    /// Aborted on drop.
    input_task: JoinHandle<()>,
}

impl Drop for TerminalChannel {
    fn drop(&mut self) {
        self.output_task.abort();
        self.input_task.abort();
    }
}

impl std::fmt::Debug for TerminalChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalChannel")
            .field("output_task_finished", &self.output_task.is_finished())
            .field("input_task_finished", &self.input_task.is_finished())
            .finish()
    }
}

/// Configuration needed for ActionCable channel connections.
///
/// Stored at construction time to avoid blocking Hub command calls
/// during `connect_to_pty`. These values don't change after Hub initialization.
#[derive(Debug, Clone)]
pub struct BrowserClientConfig {
    /// Crypto service handle for E2E encryption.
    pub crypto_service: CryptoServiceHandle,
    /// Server URL for ActionCable WebSocket connections.
    pub server_url: String,
    /// API key for authentication.
    pub api_key: String,
    /// Server-assigned hub ID for channel routing.
    pub server_hub_id: String,
}

/// Browser client - WebSocket connection from browser.
///
/// Owns terminal channels for PTY I/O routing. Each channel corresponds
/// to a browser tab viewing a specific PTY.
///
/// # Example Flow
///
/// 1. Browser connects, BrowserClient created with `new()`
/// 2. Browser selects agent, `connect_to_pty(agent_idx, pty_idx)` called
/// 3. TerminalChannel created with output forwarder and input receiver
/// 4. Browser can now see PTY output and send input
/// 5. Browser switches tabs or disconnects, `disconnect_from_pty()` called
#[derive(Debug)]
pub struct BrowserClient {
    /// Thread-safe access to Hub state and operations.
    hub_handle: HubHandle,

    /// Unique identifier (ClientId::Browser(identity)).
    id: ClientId,

    /// Terminal dimensions from browser (cols, rows).
    dims: (u16, u16),

    /// Signal identity key for encryption routing.
    identity: String,

    /// Connection config for ActionCable channels.
    ///
    /// Stored at construction to avoid blocking hub_handle calls in connect_to_pty.
    config: BrowserClientConfig,

    /// Terminal channels keyed by (agent_index, pty_index).
    ///
    /// Browser can have multiple simultaneous connections (one per tab).
    terminal_channels: HashMap<(usize, usize), TerminalChannel>,

    /// HTTP channels for preview proxying, keyed by (agent_index, pty_index).
    ///
    /// Created on-demand when the browser requests a preview connection.
    /// Dropped when the agent is deleted or browser disconnects.
    http_channels: HashMap<(usize, usize), HttpChannel>,

    /// Per-browser ActionCableChannel for hub-level control plane communication.
    ///
    /// Created and connected in `run_task()` via `connect_hub_channel()` before
    /// the event loop starts. Separate from per-PTY terminal channels -- this
    /// carries commands like ListAgents, SelectAgent, CreateAgent, etc.
    /// `None` until `connect_hub_channel()` succeeds, or in tests.
    hub_channel: Option<ActionCableChannel>,

    /// Sender handle for outgoing hub-level control messages to the browser.
    ///
    /// Extracted from `hub_channel` after connection. Used by Phase 2.2 send
    /// methods to push agent lists, status updates, and error messages.
    /// `None` if `hub_channel` is not connected.
    hub_sender: Option<ChannelSenderHandle>,

    /// Sender for browser background tasks to route operations through Client trait.
    ///
    /// Cloned and passed to each `spawn_pty_input_receiver` task. Each task sends
    /// `BrowserRequest` with its specific (agent_index, pty_index) so the request
    /// handler can route to the correct PTY via Client trait methods.
    request_tx: UnboundedSender<BrowserRequest>,

    /// Receiver for requests from browser background tasks.
    ///
    /// Consumed by `run_task()` which processes requests in a `tokio::select!` loop.
    /// All input receiver tasks (one per PTY connection) share the single `request_tx` sender.
    request_rx: Option<UnboundedReceiver<BrowserRequest>>,

    /// Broadcast receiver for Hub events (agent created/deleted/status, shutdown).
    ///
    /// Taken once by `run_task()` via `take_hub_event_rx()` and consumed in the
    /// async event loop. `None` after first take or if not provided at construction.
    hub_event_rx: Option<tokio::sync::broadcast::Receiver<crate::hub::HubEvent>>,
}

impl BrowserClient {
    /// Create a new browser client with connection config.
    ///
    /// # Arguments
    ///
    /// * `hub_handle` - Handle for Hub communication and agent queries.
    /// * `identity` - Signal identity key from browser handshake.
    /// * `config` - Connection config for ActionCable channels.
    /// * `hub_event_rx` - Optional broadcast receiver for Hub events.
    #[must_use]
    pub fn new(
        hub_handle: HubHandle,
        identity: String,
        config: BrowserClientConfig,
        hub_event_rx: Option<tokio::sync::broadcast::Receiver<crate::hub::HubEvent>>,
    ) -> Self {
        let (request_tx, request_rx) = mpsc::unbounded_channel();

        // Default terminal size: 80 columns x 24 rows
        Self {
            id: ClientId::Browser(identity.clone()),
            dims: (80, 24),
            identity,
            hub_handle,
            config,
            terminal_channels: HashMap::new(),
            http_channels: HashMap::new(),
            hub_channel: None,
            hub_sender: None,
            request_tx,
            request_rx: Some(request_rx),
            hub_event_rx,
        }
    }

    /// Get the Signal identity key.
    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Get a clone of the request sender for testing.
    ///
    /// Allows tests to send `BrowserRequest` messages directly to the client's
    /// request channel, simulating what background PTY input receiver tasks do
    /// in production.
    #[cfg(test)]
    pub fn request_sender_for_test(&self) -> UnboundedSender<BrowserRequest> {
        self.request_tx.clone()
    }

    /// Get the currently connected PTY indices.
    ///
    /// Returns an iterator over `(agent_index, pty_index)` pairs for all
    /// connected PTYs. BrowserClient can have multiple simultaneous connections.
    pub fn connected_ptys(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.terminal_channels.keys().copied()
    }

    /// Handle a single request from a browser background task.
    ///
    /// Routes the request to the appropriate Client trait method. Unlike
    /// TuiClient's handler, every variant includes explicit PTY indices
    /// because Browser supports multiple simultaneous connections.
    async fn handle_request(&mut self, request: BrowserRequest) {
        match request {
            BrowserRequest::SendInput {
                agent_index,
                pty_index,
                data,
            } => {
                log::info!(
                    "[BrowserClient] Received SendInput request: agent={}, pty={}, data_len={}",
                    agent_index,
                    pty_index,
                    data.len()
                );
                if let Err(e) = self.send_input(agent_index, pty_index, &data).await {
                    log::error!(
                        "Failed to send input to PTY ({}, {}): {}",
                        agent_index,
                        pty_index,
                        e
                    );
                } else {
                    log::info!(
                        "[BrowserClient] Successfully sent input to PTY ({}, {})",
                        agent_index,
                        pty_index
                    );
                }
            }
            BrowserRequest::Resize {
                agent_index,
                pty_index,
                rows,
                cols,
            } => {
                self.dims = (cols, rows);
                if let Err(e) = self.resize_pty(agent_index, pty_index, rows, cols).await {
                    log::error!(
                        "Failed to resize PTY ({}, {}): {}",
                        agent_index,
                        pty_index,
                        e
                    );
                }
            }
        }
    }

    /// Handle a browser command received via the hub channel.
    ///
    /// Parses the incoming JSON payload as a [`BrowserCommand`] and dispatches
    /// to the appropriate Client trait method or local handler. This is the
    /// Phase 2.3 direct handling path -- browser commands arrive over the
    /// per-browser hub channel and are processed here without going through
    /// the relay/browser.rs layer.
    ///
    /// # Command Routing
    ///
    /// - **Agent management** (`CreateAgent`, `DeleteAgent`, `SelectAgent`) -- use Client trait
    /// - **View commands** (`Scroll`, `ScrollToTop`, `ScrollToBottom`, `TogglePtyView`, `SetMode`)
    ///   -- browser-side state, logged here
    /// - **Data queries** (`ListAgents`, `ListWorktrees`) -- send via hub channel
    /// - **Input/Resize** -- arrive via TerminalRelayChannel, not here
    /// - **PTY connections** -- handled via `PtyConnectionRequested` HubEvent, not commands
    /// - **GenerateInvite** -- handled by bootstrap relay, not BrowserClient
    async fn handle_browser_command(&mut self, payload: &[u8]) {
        let command: BrowserCommand = match serde_json::from_slice(payload) {
            Ok(cmd) => cmd,
            Err(e) => {
                log::warn!(
                    "BrowserClient {}: failed to parse browser command: {}",
                    &self.identity[..8.min(self.identity.len())],
                    e
                );
                return;
            }
        };

        match command {
            BrowserCommand::SelectAgent { id } => {
                log::info!(
                    "BrowserClient {}: SelectAgent {}",
                    &self.identity[..8.min(self.identity.len())],
                    &id[..8.min(id.len())]
                );
                // Find the agent index by scanning the handle cache.
                let mut agent_index = None;
                for i in 0.. {
                    match self.hub_handle.get_agent(i) {
                        Some(handle) if handle.agent_id() == id => {
                            agent_index = Some(i);
                            break;
                        }
                        Some(_) => continue,
                        None => break,
                    }
                }

                if let Some(index) = agent_index {
                    match self.select_agent(index).await {
                        Ok(_metadata) => {
                            self.send_agent_selected(&id).await;
                            log::info!(
                                "BrowserClient {}: agent {} selected successfully",
                                &self.identity[..8.min(self.identity.len())],
                                &id[..8.min(id.len())]
                            );
                        }
                        Err(e) => {
                            log::error!("Failed to select agent {}: {}", &id[..8.min(id.len())], e);
                            self.send_error_to_browser(&e).await;
                        }
                    }
                } else {
                    log::warn!(
                        "BrowserClient {}: agent {} not found",
                        &self.identity[..8.min(self.identity.len())],
                        &id[..8.min(id.len())]
                    );
                    self.send_error_to_browser("Agent not found").await;
                }
            }

            BrowserCommand::CreateAgent {
                issue_or_branch,
                prompt,
            } => {
                let identifier = issue_or_branch.clone().unwrap_or_default();
                log::info!(
                    "BrowserClient {}: CreateAgent '{}'",
                    &self.identity[..8.min(self.identity.len())],
                    &identifier
                );
                let request = CreateAgentRequest {
                    issue_or_branch: identifier,
                    prompt,
                    from_worktree: None,
                    dims: Some(self.dims),
                };
                if let Err(e) = self.create_agent(request).await {
                    log::error!("Failed to create agent: {}", e);
                    self.send_error_to_browser(&e).await;
                }
            }

            BrowserCommand::ReopenWorktree {
                path,
                branch,
                prompt,
            } => {
                log::info!(
                    "BrowserClient {}: ReopenWorktree '{}' branch '{}'",
                    &self.identity[..8.min(self.identity.len())],
                    &path,
                    &branch
                );
                let request = CreateAgentRequest {
                    issue_or_branch: branch,
                    prompt,
                    from_worktree: Some(std::path::PathBuf::from(&path)),
                    dims: Some(self.dims),
                };
                if let Err(e) = self.create_agent(request).await {
                    log::error!("Failed to reopen worktree '{}': {}", &path, e);
                    self.send_error_to_browser(&e).await;
                }
            }

            BrowserCommand::DeleteAgent {
                id,
                delete_worktree,
            } => {
                log::info!(
                    "BrowserClient {}: DeleteAgent {} (delete_worktree={})",
                    &self.identity[..8.min(self.identity.len())],
                    &id[..8.min(id.len())],
                    delete_worktree.unwrap_or(false)
                );
                let request = DeleteAgentRequest {
                    agent_id: id.clone(),
                    delete_worktree: delete_worktree.unwrap_or(false),
                };
                if let Err(e) = self.delete_agent(request).await {
                    log::error!("Failed to delete agent {}: {}", &id[..8.min(id.len())], e);
                    self.send_error_to_browser(&e).await;
                }
            }

            BrowserCommand::Resize { cols, rows } => {
                log::debug!(
                    "BrowserClient {}: Resize {}x{}",
                    &self.identity[..8.min(self.identity.len())],
                    cols,
                    rows
                );
                self.dims = (cols, rows);
                // Resize all currently connected PTYs to the new dimensions.
                let connected: Vec<(usize, usize)> =
                    self.terminal_channels.keys().copied().collect();
                for (agent_index, pty_index) in connected {
                    if let Err(e) = self.resize_pty(agent_index, pty_index, rows, cols).await {
                        log::debug!(
                            "Failed to resize PTY ({}, {}): {}",
                            agent_index,
                            pty_index,
                            e
                        );
                    }
                }
            }

            BrowserCommand::SetMode { mode } => {
                log::info!(
                    "BrowserClient {}: SetMode '{}'",
                    &self.identity[..8.min(self.identity.len())],
                    mode
                );
                // Mode state is managed by the browser. The old relay path updates
                // BrowserState on Hub, but BrowserClient doesn't own that state.
                // TODO Phase 2.4: consider whether BrowserClient needs mode tracking.
            }

            BrowserCommand::Scroll { direction, lines } => {
                log::debug!(
                    "BrowserClient {}: Scroll {} {} lines",
                    &self.identity[..8.min(self.identity.len())],
                    direction,
                    lines.unwrap_or(10)
                );
                // Scroll is browser-side view state. The browser's xterm.js handles
                // scrolling directly. No server-side action needed.
            }

            BrowserCommand::ScrollToTop => {
                log::debug!(
                    "BrowserClient {}: ScrollToTop",
                    &self.identity[..8.min(self.identity.len())]
                );
                // Browser-side view state, no server action needed.
            }

            BrowserCommand::ScrollToBottom => {
                log::debug!(
                    "BrowserClient {}: ScrollToBottom",
                    &self.identity[..8.min(self.identity.len())]
                );
                // Browser-side view state, no server action needed.
            }

            BrowserCommand::TogglePtyView => {
                log::info!(
                    "BrowserClient {}: TogglePtyView",
                    &self.identity[..8.min(self.identity.len())]
                );
                // PTY view toggle is browser-side state. The browser decides which
                // PTY (CLI vs Server) to display and connects accordingly.
                // TODO Phase 2.4: may need to coordinate PTY connections here.
            }

            BrowserCommand::Input { data } => {
                // Input should arrive via TerminalRelayChannel (per-PTY channels),
                // not the hub control channel. Log if received here.
                log::debug!(
                    "BrowserClient {}: Input received on hub channel ({} bytes) -- expected on PTY channel",
                    &self.identity[..8.min(self.identity.len())],
                    data.len()
                );
            }

            BrowserCommand::ListAgents => {
                log::info!(
                    "BrowserClient {}: ListAgents",
                    &self.identity[..8.min(self.identity.len())]
                );
                self.send_agent_list_to_browser().await;
            }

            BrowserCommand::ListWorktrees => {
                log::info!(
                    "BrowserClient {}: ListWorktrees",
                    &self.identity[..8.min(self.identity.len())]
                );
                self.send_worktree_list_to_browser().await;
            }

            BrowserCommand::GenerateInvite => {
                // GenerateInvite is handled by the bootstrap relay connection,
                // not by individual BrowserClients.
                log::warn!(
                    "BrowserClient {}: GenerateInvite received on hub channel -- should be handled by bootstrap relay",
                    &self.identity[..8.min(self.identity.len())]
                );
            }

            BrowserCommand::Handshake { device_name, .. } => {
                log::info!(
                    "BrowserClient {}: Handshake from '{}', sending ack",
                    &self.identity[..8.min(self.identity.len())],
                    device_name
                );
                // Send handshake acknowledgment to complete E2E session establishment.
                self.send_handshake_ack().await;
                // Also send initial data so browser has agents/worktrees immediately.
                self.send_agent_list_to_browser().await;
                self.send_worktree_list_to_browser().await;
            }
        }
    }

    /// Connect the per-browser hub channel for control plane communication.
    ///
    /// Builds an `ActionCableChannel` via the builder pattern, connects to the
    /// `HubChannel` with both `browser_identity` and `cli_subscription: true` to
    /// indicate this is the CLI side of the per-browser bidirectional stream.
    ///
    /// The browser subscribes first (without cli_subscription), listening on
    /// `hub:{id}:browser:{identity}`. This BrowserClient subscribes with
    /// `cli_subscription: true`, listening on `hub:{id}:browser:{identity}:cli`.
    /// Rails routes messages between the paired streams.
    ///
    /// Called early in `run_task()` before the event loop. If connection fails,
    /// the caller logs the error but continues -- the browser can reconnect later.
    ///
    /// # Errors
    ///
    /// Returns an error string if the channel fails to connect.
    async fn connect_hub_channel(&mut self) -> Result<(), String> {
        let mut channel = ActionCableChannel::builder()
            .server_url(&self.config.server_url)
            .api_key(&self.config.api_key)
            .crypto_service(self.config.crypto_service.clone())
            .reliable(true)
            .cli_subscription(true) // Mark as CLI side of per-browser stream
            .build();

        channel
            .connect(ChannelConfig {
                channel_name: "HubChannel".to_string(),
                hub_id: self.config.server_hub_id.clone(),
                agent_index: None,
                pty_index: None,
                browser_identity: Some(self.identity.clone()),
                encrypt: true,
                compression_threshold: Some(4096),
                cli_subscription: true, // CLI side of per-browser stream
            })
            .await
            .map_err(|e| format!("Hub channel connect failed: {}", e))?;

        self.hub_sender = channel.get_sender_handle();
        if let Some(ref sender) = self.hub_sender {
            sender.register_peer(PeerId(self.identity.clone()));
        }
        self.hub_channel = Some(channel);

        log::info!(
            "BrowserClient {} connected hub channel (CLI side)",
            &self.identity[..8.min(self.identity.len())]
        );

        Ok(())
    }

    /// Run BrowserClient as an independent async task.
    ///
    /// Processes requests from browser input receiver tasks via `request_rx`
    /// and hub events via broadcast in a `tokio::select!` loop.
    pub async fn run_task(mut self) {
        let Some(mut request_rx) = self.request_rx.take() else {
            log::error!("BrowserClient has no request receiver");
            return;
        };

        // If hub_event_rx is None (tests), create a dummy channel that never sends.
        let (_dummy_tx, mut hub_event_rx) = if let Some(rx) = self.take_hub_event_rx() {
            (None, rx)
        } else {
            let (tx, rx) = tokio::sync::broadcast::channel::<crate::hub::HubEvent>(1);
            (Some(tx), rx)
        };

        // Connect per-browser hub channel for control plane communication.
        // Non-fatal: if this fails, browser can reconnect later. The hub channel
        // receiver branch in the select loop simply stays inert (pending forever).
        if let Err(e) = self.connect_hub_channel().await {
            log::error!(
                "BrowserClient {} failed to connect hub channel: {}",
                &self.identity[..8.min(self.identity.len())],
                e
            );
        }

        // Take the receiver handle from the hub channel (if connected).
        // This must happen after connect_hub_channel() populates self.hub_channel.
        let mut hub_channel_rx = self
            .hub_channel
            .as_mut()
            .and_then(|ch| ch.take_receiver_handle());

        // Send initial data to browser after hub channel connects.
        self.send_agent_list_to_browser().await;
        self.send_worktree_list_to_browser().await;

        loop {
            tokio::select! {
                request = request_rx.recv() => {
                    match request {
                        Some(req) => self.handle_request(req).await,
                        None => {
                            log::info!("Browser request channel closed, stopping BrowserClient task");
                            break;
                        }
                    }
                }
                event = hub_event_rx.recv() => {
                    match event {
                        Ok(hub_event) => self.handle_hub_event(hub_event).await,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            log::warn!("BrowserClient lagged {} hub events", n);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            log::info!("Hub event channel closed, stopping BrowserClient");
                            break;
                        }
                    }
                }
                // Per-browser hub channel: receives control messages from the browser
                // (ListAgents, SelectAgent, CreateAgent, etc.). If the channel was not
                // connected, this branch never resolves (pending forever).
                msg = async {
                    match hub_channel_rx {
                        Some(ref mut rx) => rx.recv().await,
                        None => {
                            // No channel connected -- sleep forever so this branch
                            // never fires and the other select branches run normally.
                            std::future::pending::<Option<crate::channel::IncomingMessage>>().await
                        }
                    }
                } => {
                    match msg {
                        Some(incoming) => {
                            log::debug!(
                                "BrowserClient received hub channel message ({} bytes) from {}",
                                incoming.payload.len(),
                                incoming.sender,
                            );
                            self.handle_browser_command(&incoming.payload).await;
                        }
                        None => {
                            log::warn!("Hub channel closed, stopping BrowserClient");
                            break;
                        }
                    }
                }
            }
        }

        log::info!("BrowserClient task stopped");
    }

    // ========================================================================
    // Hub channel send methods
    //
    // These methods send control-plane messages to the browser via the
    // per-browser hub channel. They use the exact TerminalMessage JSON format
    // that the browser JavaScript expects.
    // ========================================================================

    /// Send serialized data to the browser via the hub channel.
    ///
    /// Logs a warning and returns gracefully if the hub channel is not connected.
    /// This is the low-level helper used by all other send methods.
    async fn send_to_browser(&self, data: &[u8]) {
        if let Some(ref sender) = self.hub_sender {
            if let Err(e) = sender.send(data).await {
                log::warn!("Failed to send to browser: {}", e);
            }
        }
    }

    /// Send a serialized [`TerminalMessage`] to the browser.
    ///
    /// Serializes the message to JSON and sends it via [`send_to_browser()`].
    /// Logs and returns on serialization or send failure.
    async fn send_terminal_message(&self, message: &TerminalMessage) {
        match serde_json::to_string(message) {
            Ok(json) => self.send_to_browser(json.as_bytes()).await,
            Err(e) => log::warn!("Failed to serialize TerminalMessage: {}", e),
        }
    }

    /// Send the current agent list to the browser.
    ///
    /// Reads cached agent handles from [`HubHandle`] (non-blocking via
    /// `HandleCache`) and sends them as a [`TerminalMessage::Agents`] message.
    /// Uses the standard [`TerminalMessage::Agents`] JSON format.
    ///
    /// The `hub_identifier` field on each [`AgentInfo`] is populated from
    /// [`BrowserClientConfig::server_hub_id`] to match what Hub's
    /// `build_agent_list()` produces.
    async fn send_agent_list_to_browser(&self) {
        let handles = self.hub_handle.get_all_agent_handles();
        let hub_id = &self.config.server_hub_id;

        let agents: Vec<_> = handles
            .iter()
            .map(|h| {
                let mut info = h.info().clone();
                // Ensure hub_identifier is set (cached info may have None).
                if info.hub_identifier.is_none() {
                    info.hub_identifier = Some(hub_id.clone());
                }
                info
            })
            .collect();

        let message = TerminalMessage::Agents { agents };
        self.send_terminal_message(&message).await;
    }

    /// Send the current worktree list to the browser.
    ///
    /// Reads cached worktrees from [`HubHandle`] (non-blocking via
    /// `HandleCache`) and sends them as a [`TerminalMessage::Worktrees`]
    /// message. Uses the standard [`TerminalMessage::Worktrees`] JSON format.
    async fn send_worktree_list_to_browser(&self) {
        let worktrees_raw = match self.hub_handle.list_worktrees() {
            Ok(wt) => wt,
            Err(e) => {
                log::warn!("Failed to list worktrees: {}", e);
                return;
            }
        };

        let worktrees: Vec<WorktreeInfo> = worktrees_raw
            .iter()
            .map(|(path, branch)| build_worktree_info(path, branch))
            .collect();

        let repo = crate::WorktreeManager::detect_current_repo()
            .map(|(_, name)| name)
            .ok();

        let message = TerminalMessage::Worktrees { worktrees, repo };
        self.send_terminal_message(&message).await;
    }

    /// Send agent selection confirmation to the browser.
    ///
    /// Sends a [`TerminalMessage::AgentSelected`] message matching the format
    /// previously provided by the now-removed relay send functions.
    async fn send_agent_selected(&self, agent_id: &str) {
        let message = TerminalMessage::AgentSelected {
            id: agent_id.to_string(),
        };
        self.send_terminal_message(&message).await;
    }

    /// Send agent creation progress to the browser.
    ///
    /// Sends a [`TerminalMessage::AgentCreatingProgress`] message matching
    /// the standard [`TerminalMessage::AgentCreatingProgress`] format.
    async fn send_creation_progress(&self, identifier: &str, stage: &AgentCreationStage) {
        let message = TerminalMessage::AgentCreatingProgress {
            identifier: identifier.to_string(),
            stage: *stage,
            message: stage.description().to_string(),
        };
        self.send_terminal_message(&message).await;
    }

    /// Send agent created confirmation to the browser.
    ///
    /// Sends a [`TerminalMessage::AgentCreated`] message matching the format
    /// the standard [`TerminalMessage::AgentCreated`] format.
    async fn send_agent_created(&self, agent_id: &str) {
        let message = TerminalMessage::AgentCreated {
            id: agent_id.to_string(),
        };
        self.send_terminal_message(&message).await;
    }

    /// Send an error message to the browser.
    ///
    /// Sends a [`TerminalMessage::Error`] message matching the format used
    /// throughout the relay protocol.
    async fn send_error_to_browser(&self, error_message: &str) {
        let message = TerminalMessage::Error {
            message: error_message.to_string(),
        };
        self.send_terminal_message(&message).await;
    }

    /// Send handshake acknowledgment to the browser.
    ///
    /// Completes the E2E session establishment. Browser waits for this
    /// before considering the connection fully established.
    async fn send_handshake_ack(&self) {
        let message = TerminalMessage::HandshakeAck;
        self.send_terminal_message(&message).await;
    }

    // ========================================================================
    // Hub event handling
    // ========================================================================

    /// Handle a hub event broadcast.
    ///
    /// Routes hub-level events to browser-specific actions. Sends updated
    /// state to the browser via the per-browser hub channel and disconnects
    /// PTYs on agent deletion.
    ///
    /// # Event Handling
    ///
    /// - `AgentCreated` / `AgentDeleted` / `AgentStatusChanged` - Send updated agent list
    /// - `AgentDeleted` - Also disconnects PTYs for the deleted agent
    /// - `AgentCreationProgress` - Forward progress updates to browser
    /// - `Shutdown` - Log; run_task loop handles actual shutdown via channel close
    /// - `Error` - Forward error message to browser
    async fn handle_hub_event(&mut self, event: crate::hub::HubEvent) {
        use crate::hub::HubEvent;
        match event {
            HubEvent::AgentCreated { ref agent_id, .. } => {
                log::info!(
                    "BrowserClient: agent created {}, sending agent list + confirmation",
                    &agent_id[..8.min(agent_id.len())]
                );
                self.send_agent_list_to_browser().await;
                self.send_agent_created(agent_id).await;
                // Scrollback is available when the browser connects to the PTY
                // via PtyConnectionRequested, which subscribes to live output events.
            }
            HubEvent::AgentDeleted { ref agent_id } => {
                log::info!(
                    "BrowserClient: agent deleted {}, disconnecting PTYs",
                    &agent_id[..8.min(agent_id.len())]
                );

                // Find the agent's index by scanning cached handles.
                // Must be done before the cache is updated (Hub broadcasts before removal).
                let mut agent_index = None;
                for i in 0.. {
                    match self.hub_handle.get_agent(i) {
                        Some(handle) if handle.agent_id() == agent_id => {
                            agent_index = Some(i);
                            break;
                        }
                        Some(_) => continue,
                        None => break,
                    }
                }

                if let Some(idx) = agent_index {
                    // Disconnect both CLI PTY (index 0) and Server PTY (index 1).
                    self.disconnect_from_pty(idx, 0).await;
                    self.disconnect_from_pty(idx, 1).await;

                    // Remove any HTTP channels for this agent.
                    if self.http_channels.remove(&(idx, 0)).is_some() {
                        log::info!(
                            "BrowserClient {}: removed HttpChannel ({}, 0) on agent delete",
                            &self.identity[..8.min(self.identity.len())],
                            idx
                        );
                    }
                    if self.http_channels.remove(&(idx, 1)).is_some() {
                        log::info!(
                            "BrowserClient {}: removed HttpChannel ({}, 1) on agent delete",
                            &self.identity[..8.min(self.identity.len())],
                            idx
                        );
                    }
                } else {
                    log::warn!(
                        "BrowserClient: could not find index for deleted agent {}",
                        &agent_id[..8.min(agent_id.len())]
                    );
                }

                self.send_agent_list_to_browser().await;
            }
            HubEvent::AgentStatusChanged { ref agent_id, .. } => {
                log::info!(
                    "BrowserClient: agent status changed {}",
                    &agent_id[..8.min(agent_id.len())]
                );
                self.send_agent_list_to_browser().await;
            }
            HubEvent::AgentCreationProgress {
                ref identifier,
                ref stage,
            } => {
                log::info!(
                    "BrowserClient: creation progress for {} - {:?}",
                    identifier,
                    stage
                );
                self.send_creation_progress(identifier, stage).await;
            }
            HubEvent::Shutdown => {
                log::info!("BrowserClient received shutdown event");
                // run_task loop will break on Closed channel or we handle here
            }
            HubEvent::Error { ref message } => {
                log::info!("BrowserClient: error event: {}", message);
                self.send_error_to_browser(message).await;
            }
            HubEvent::PtyConnectionRequested {
                ref client_id,
                agent_index,
                pty_index,
            } => {
                // Only handle if this is for us.
                if client_id == &self.id {
                    log::info!(
                        "BrowserClient {}: PtyConnectionRequested({}, {})",
                        &self.identity[..8.min(self.identity.len())],
                        agent_index,
                        pty_index
                    );
                    if let Err(e) = self.connect_to_pty(agent_index, pty_index).await {
                        log::error!(
                            "Failed to connect to PTY ({}, {}): {}",
                            agent_index,
                            pty_index,
                            e
                        );
                        self.send_error_to_browser(&e).await;
                    }
                }
            }
            HubEvent::PtyDisconnectionRequested {
                ref client_id,
                agent_index,
                pty_index,
            } => {
                // Only handle if this is for us.
                if client_id == &self.id {
                    log::info!(
                        "BrowserClient {}: PtyDisconnectionRequested({}, {})",
                        &self.identity[..8.min(self.identity.len())],
                        agent_index,
                        pty_index
                    );
                    self.disconnect_from_pty(agent_index, pty_index).await;
                }
            }
            HubEvent::HttpConnectionRequested {
                ref client_id,
                agent_index,
                pty_index,
                ref browser_identity,
            } => {
                // Only handle if this is for us.
                if client_id == &self.id {
                    log::info!(
                        "BrowserClient {}: HttpConnectionRequested({}, {}, {})",
                        &self.identity[..8.min(self.identity.len())],
                        agent_index,
                        pty_index,
                        &browser_identity[..8.min(browser_identity.len())]
                    );

                    // Check if we already have an HttpChannel for this agent/pty
                    let key = (agent_index, pty_index);
                    if self.http_channels.contains_key(&key) {
                        log::debug!(
                            "BrowserClient {}: HttpChannel already exists for ({}, {})",
                            &self.identity[..8.min(self.identity.len())],
                            agent_index,
                            pty_index
                        );
                        return;
                    }

                    // Get agent handle via hub_handle
                    let agent_handle = match self.hub_handle.get_agent(agent_index) {
                        Some(handle) => handle,
                        None => {
                            log::error!(
                                "BrowserClient {}: agent {} not found for HttpConnectionRequested",
                                &self.identity[..8.min(self.identity.len())],
                                agent_index
                            );
                            self.send_error_to_browser("Agent not found for preview").await;
                            return;
                        }
                    };

                    // Get PtyHandle from agent
                    let pty_handle = match agent_handle.get_pty(pty_index) {
                        Some(handle) => handle.clone(),
                        None => {
                            log::error!(
                                "BrowserClient {}: PTY {} not found on agent {}",
                                &self.identity[..8.min(self.identity.len())],
                                pty_index,
                                agent_index
                            );
                            self.send_error_to_browser("PTY not found for preview").await;
                            return;
                        }
                    };

                    // Create HttpChannel
                    match HttpChannel::new(
                        agent_index,
                        pty_index,
                        &pty_handle,
                        &self.config,
                        browser_identity.clone(),
                    )
                    .await
                    {
                        Ok(http_channel) => {
                            self.http_channels.insert(key, http_channel);
                            log::info!(
                                "BrowserClient {}: HttpChannel created for ({}, {})",
                                &self.identity[..8.min(self.identity.len())],
                                agent_index,
                                pty_index
                            );
                        }
                        Err(e) => {
                            log::error!(
                                "BrowserClient {}: failed to create HttpChannel for ({}, {}): {}",
                                &self.identity[..8.min(self.identity.len())],
                                agent_index,
                                pty_index,
                                e
                            );
                            self.send_error_to_browser(&format!("Failed to create preview channel: {}", e)).await;
                        }
                    }
                }
            }
        }
    }
}

impl Client for BrowserClient {
    fn hub_handle(&self) -> &HubHandle {
        &self.hub_handle
    }

    fn id(&self) -> &ClientId {
        &self.id
    }

    fn dims(&self) -> (u16, u16) {
        self.dims
    }

    fn take_hub_event_rx(
        &mut self,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::hub::HubEvent>> {
        self.hub_event_rx.take()
    }

    async fn connect_to_pty_with_handle(
        &mut self,
        agent_handle: &super::AgentHandle,
        agent_index: usize,
        pty_index: usize,
    ) -> Result<(), String> {
        let key = (agent_index, pty_index);

        // Idempotent: return Ok if already connected to this PTY.
        if self.terminal_channels.contains_key(&key) {
            return Ok(());
        }

        // Get PTY handle from agent.
        let pty_handle = agent_handle
            .get_pty(pty_index)
            .ok_or_else(|| format!("PTY at index {} not found for agent", pty_index))?
            .clone();

        // Use stored config (avoids blocking hub_handle calls that would deadlock).
        let crypto_service = self.config.crypto_service.clone();
        let server_url = self.config.server_url.clone();
        let api_key = self.config.api_key.clone();
        let hub_id = self.config.server_hub_id.clone();

        // Create ActionCableChannel with E2E encryption and reliable delivery.
        // Must use `.reliable(true)` to match the browser's Channel which also
        // uses reliable delivery. Without this, browser→CLI messages arrive
        // wrapped in `{ type: "data", seq, payload }` but the CLI skips the
        // reliable layer and tries to parse the wrapper as a BrowserCommand,
        // which silently fails.
        let mut channel = ActionCableChannel::builder()
            .server_url(server_url)
            .api_key(api_key)
            .crypto_service(crypto_service)
            .reliable(true)
            .build();

        // Connect to TerminalRelayChannel (already in async context).
        // Each browser has dedicated streams (like TUI has dedicated I/O).
        // CLI subscribes to: terminal_relay:{hub}:{agent}:{pty}:{browser}:cli
        // Browser subscribes to: terminal_relay:{hub}:{agent}:{pty}:{browser}
        channel
            .connect(ChannelConfig {
                channel_name: "TerminalRelayChannel".into(),
                hub_id,
                agent_index: Some(agent_index),
                pty_index: Some(pty_index),
                browser_identity: Some(self.identity.clone()),
                encrypt: true,
                // Threshold for gzip compression (4KB)
                compression_threshold: Some(4096),
                cli_subscription: true, // Subscribe to CLI stream (receives from browser)
            })
            .await
            .map_err(|e| format!("Failed to connect channel: {}", e))?;

        // Get sender and receiver handles BEFORE spawning tasks.
        let sender_handle = channel
            .get_sender_handle()
            .ok_or_else(|| "Failed to get channel sender handle".to_string())?;
        let receiver_handle = channel
            .take_receiver_handle()
            .ok_or_else(|| "Failed to get channel receiver handle".to_string())?;

        // Pre-register the browser as a peer so broadcast works immediately.
        // Without this, the peer set is empty until the browser's first message
        // arrives, causing scrollback (and early output) to be silently dropped.
        sender_handle.register_peer(PeerId(self.identity.clone()));

        // Connect to PTY and get scrollback BEFORE spawning forwarder.
        // This ensures the browser receives historical output first.
        let scrollback = pty_handle.connect(self.id.clone(), self.dims).await?;

        // Send scrollback to browser if available.
        if !scrollback.is_empty() {
            let scrollback_msg = build_scrollback_message(scrollback);
            if let Ok(json) = serde_json::to_string(&scrollback_msg) {
                let sender_clone = sender_handle.clone();
                tokio::spawn(async move {
                    if let Err(e) = sender_clone.send(json.as_bytes()).await {
                        log::debug!("Failed to send scrollback: {}", e);
                    }
                });
            }
        }

        // Subscribe to PTY events for output forwarding.
        let pty_rx = pty_handle.subscribe();

        // Clone values for spawned tasks.
        let browser_identity = self.identity.clone();
        let agent_id = agent_handle.agent_id().to_string();

        // Spawn output forwarder: PTY -> Browser.
        let output_task = tokio::spawn(spawn_pty_output_forwarder(
            pty_rx,
            sender_handle,
            browser_identity.clone(),
            agent_id.clone(),
            pty_index,
        ));

        // Spawn input receiver: Browser -> BrowserRequest channel -> Client trait.
        let request_tx = self.request_tx.clone();
        let input_task = tokio::spawn(spawn_pty_input_receiver(
            receiver_handle,
            request_tx,
            agent_index,
            pty_index,
            browser_identity,
            agent_id,
        ));

        // Store channel and task handles.
        self.terminal_channels.insert(
            key,
            TerminalChannel {
                channel,
                output_task,
                input_task,
            },
        );

        log::info!(
            "Browser {} connected to PTY ({}, {})",
            &self.identity[..8.min(self.identity.len())],
            agent_index,
            pty_index
        );

        Ok(())
    }

    /// Disconnect from a PTY using an already-resolved handle.
    ///
    /// Overrides the default to also remove the terminal channel from tracking.
    async fn disconnect_from_pty_with_handle(
        &mut self,
        pty: &crate::hub::agent_handle::PtyHandle,
        agent_index: usize,
        pty_index: usize,
    ) {
        // Remove channel from map - dropping it cleans up tasks.
        if self
            .terminal_channels
            .remove(&(agent_index, pty_index))
            .is_some()
        {
            // Notify PTY of disconnection.
            let pty = pty.clone();
            let client_id = self.id.clone();
            let _ = pty.disconnect(client_id).await;

            log::info!(
                "Browser {} disconnected from PTY ({}, {})",
                &self.identity[..8.min(self.identity.len())],
                agent_index,
                pty_index
            );
        }
    }

    async fn disconnect_from_pty(&mut self, agent_index: usize, pty_index: usize) {
        // Remove channel from map - dropping it cleans up tasks.
        if let Some(_channel) = self.terminal_channels.remove(&(agent_index, pty_index)) {
            // Notify PTY of disconnection.
            // hub_handle.get_agent() reads from HandleCache (non-blocking).
            if let Some(agent) = self.hub_handle.get_agent(agent_index) {
                if let Some(pty) = agent.get_pty(pty_index) {
                    let pty = pty.clone();
                    let client_id = self.id.clone();
                    let _ = pty.disconnect(client_id).await;
                }
            }

            log::info!(
                "Browser {} disconnected from PTY ({}, {})",
                &self.identity[..8.min(self.identity.len())],
                agent_index,
                pty_index
            );
        }
    }

    // NOTE: get_agent, send_input, resize_pty, select_agent, quit, create_agent,
    // delete_agent, regenerate_connection_code, copy_connection_url, list_worktrees,
    // get_connection_code all use DEFAULT IMPLEMENTATIONS from the trait

    async fn regenerate_prekey_bundle(
        &self,
    ) -> Result<crate::relay::signal::PreKeyBundleData, String> {
        let next_id = self
            .config
            .crypto_service
            .next_prekey_id()
            .await
            .unwrap_or(1);
        self.config
            .crypto_service
            .get_prekey_bundle(next_id)
            .await
            .map_err(|e| format!("Failed to regenerate bundle: {}", e))
    }
}

/// Background task that forwards PTY output to browser via ActionCableChannel.
///
/// Subscribes to PTY events and sends `Output` events through the channel.
/// Exits when the PTY closes or channel disconnects.
///
/// Uses a UTF-8 streaming decoder to handle multi-byte characters that may be
/// split across PTY read chunks. Without this, box-drawing characters and other
/// multi-byte UTF-8 sequences could be corrupted into replacement characters.
async fn spawn_pty_output_forwarder(
    mut pty_rx: broadcast::Receiver<PtyEvent>,
    sender: crate::channel::ChannelSenderHandle,
    browser_identity: String,
    agent_id: String,
    pty_index: usize,
) {
    log::info!(
        "Started PTY output forwarder for browser {} agent {} pty {}",
        &browser_identity[..8.min(browser_identity.len())],
        &agent_id[..8.min(agent_id.len())],
        pty_index
    );

    // Buffer for incomplete UTF-8 sequences between chunks.
    // UTF-8 sequences are at most 4 bytes, so we only need to buffer 0-3 bytes.
    let mut utf8_buffer: Vec<u8> = Vec::with_capacity(4);

    loop {
        match pty_rx.recv().await {
            Ok(PtyEvent::Output(data)) => {
                // Prepend any leftover bytes from the previous chunk.
                let combined = if utf8_buffer.is_empty() {
                    data
                } else {
                    let mut combined = std::mem::take(&mut utf8_buffer);
                    combined.extend(data);
                    combined
                };

                // Find the last valid UTF-8 boundary.
                // We split at the point where everything before is valid UTF-8.
                let (valid_end, leftover_start) = find_utf8_boundary(&combined);

                // Store any incomplete sequence for the next chunk.
                if leftover_start < combined.len() {
                    utf8_buffer.extend(&combined[leftover_start..]);
                }

                // Skip if there's nothing valid to send.
                if valid_end == 0 {
                    continue;
                }

                // Convert the valid portion to string (should never produce replacement chars).
                let output_str = String::from_utf8_lossy(&combined[..valid_end]);

                // Wrap in TerminalMessage for proper parsing on browser.
                let message = TerminalMessage::Output {
                    data: output_str.into_owned(),
                };

                // Serialize to JSON.
                let json = match serde_json::to_string(&message) {
                    Ok(j) => j,
                    Err(e) => {
                        log::error!("Failed to serialize terminal output: {}", e);
                        continue;
                    }
                };

                // Send through channel (broadcast to browser).
                if let Err(e) = sender.send(json.as_bytes()).await {
                    log::debug!(
                        "PTY forwarder send failed (channel closed?): {} - stopping",
                        e
                    );
                    break;
                }
            }
            Ok(PtyEvent::ProcessExited { exit_code }) => {
                log::info!(
                    "PTY process exited (code={:?}) for browser {} agent {} pty {}",
                    exit_code,
                    &browser_identity[..8.min(browser_identity.len())],
                    &agent_id[..8.min(agent_id.len())],
                    pty_index
                );
                // Send exit notification to browser, then continue - may have final output.
                let message = TerminalMessage::ProcessExited { exit_code };
                if let Ok(json) = serde_json::to_string(&message) {
                    let _ = sender.send(json.as_bytes()).await;
                }
            }
            Ok(_other_event) => {
                // Ignore other events (Resized, OwnerChanged).
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                log::warn!(
                    "PTY forwarder lagged by {} events for browser {} agent {}",
                    n,
                    &browser_identity[..8.min(browser_identity.len())],
                    &agent_id[..8.min(agent_id.len())]
                );
                // Continue - we'll catch up with future events.
            }
            Err(broadcast::error::RecvError::Closed) => {
                log::info!(
                    "PTY channel closed for browser {} agent {} pty {}",
                    &browser_identity[..8.min(browser_identity.len())],
                    &agent_id[..8.min(agent_id.len())],
                    pty_index
                );
                break;
            }
        }
    }

    log::info!(
        "Stopped PTY output forwarder for browser {} agent {} pty {}",
        &browser_identity[..8.min(browser_identity.len())],
        &agent_id[..8.min(agent_id.len())],
        pty_index
    );
}

/// Find the boundary where a byte slice can be split into valid UTF-8 and leftover bytes.
///
/// Returns `(valid_end, leftover_start)` where:
/// - `data[..valid_end]` is guaranteed to be valid UTF-8
/// - `data[leftover_start..]` contains incomplete multi-byte sequences to buffer
///
/// For fully valid UTF-8, returns `(len, len)`.
/// For data ending in an incomplete sequence, `leftover_start < len`.
fn find_utf8_boundary(data: &[u8]) -> (usize, usize) {
    let len = data.len();
    if len == 0 {
        return (0, 0);
    }

    // Check if the entire slice is valid UTF-8
    if std::str::from_utf8(data).is_ok() {
        return (len, len);
    }

    // Look backwards from the end to find incomplete sequences.
    // UTF-8 multi-byte sequences:
    // - 2-byte: starts with 110xxxxx (0xC0-0xDF)
    // - 3-byte: starts with 1110xxxx (0xE0-0xEF)
    // - 4-byte: starts with 11110xxx (0xF0-0xF7)
    // Continuation bytes: 10xxxxxx (0x80-0xBF)
    //
    // We scan backwards up to 3 bytes looking for a multi-byte start byte
    // that doesn't have enough continuation bytes after it.
    for i in 1..=3.min(len) {
        let idx = len - i;
        let byte = data[idx];

        // Check if this is a multi-byte start byte
        let expected_len = if byte & 0b1111_1000 == 0b1111_0000 {
            4 // 4-byte sequence
        } else if byte & 0b1111_0000 == 0b1110_0000 {
            3 // 3-byte sequence
        } else if byte & 0b1110_0000 == 0b1100_0000 {
            2 // 2-byte sequence
        } else if byte & 0b1000_0000 == 0 {
            1 // ASCII, complete
        } else {
            continue; // Continuation byte, keep looking
        };

        let bytes_after = len - idx;
        if bytes_after < expected_len {
            // Incomplete sequence found - split here
            // Verify the portion before is valid UTF-8
            if std::str::from_utf8(&data[..idx]).is_ok() {
                return (idx, idx);
            }
        }
    }

    // Fallback: find the last valid UTF-8 boundary by scanning forward
    // This handles cases with invalid bytes in the middle
    let mut valid_end = 0;
    let mut pos = 0;
    while pos < len {
        let byte = data[pos];
        let char_len = if byte & 0b1000_0000 == 0 {
            1
        } else if byte & 0b1110_0000 == 0b1100_0000 {
            2
        } else if byte & 0b1111_0000 == 0b1110_0000 {
            3
        } else if byte & 0b1111_1000 == 0b1111_0000 {
            4
        } else {
            // Invalid start byte - stop here
            break;
        };

        if pos + char_len > len {
            // Incomplete sequence at end
            break;
        }

        // Verify continuation bytes
        let mut valid = true;
        for j in 1..char_len {
            if data[pos + j] & 0b1100_0000 != 0b1000_0000 {
                valid = false;
                break;
            }
        }

        if valid {
            pos += char_len;
            valid_end = pos;
        } else {
            break;
        }
    }

    (valid_end, valid_end)
}

/// Background task that receives input from browser and sends BrowserRequest to BrowserClient.
///
/// Listens for incoming messages from the browser (through the encrypted channel)
/// and sends them as `BrowserRequest` variants through the channel. BrowserClient's
/// request handler routes these through the Client trait to the correct PTY.
///
/// This task does NOT call PtyHandle directly. All PTY operations go through
/// the BrowserRequest channel -> run_task() -> Client trait methods.
///
/// # Message Types
///
/// - `BrowserCommand::Input { data }` -> `BrowserRequest::SendInput`
/// - `BrowserCommand::Resize { cols, rows }` -> `BrowserRequest::Resize`
///
/// Other `BrowserCommand` variants should go through the main hub channel.
async fn spawn_pty_input_receiver(
    mut receiver: crate::channel::ChannelReceiverHandle,
    request_tx: UnboundedSender<BrowserRequest>,
    agent_index: usize,
    pty_index: usize,
    browser_identity: String,
    agent_id: String,
) {
    log::info!(
        "Started PTY input receiver for browser {} agent {} pty {}",
        &browser_identity[..8.min(browser_identity.len())],
        &agent_id[..8.min(agent_id.len())],
        pty_index
    );

    let mut message_count = 0u64;
    while let Some(incoming) = receiver.recv().await {
        message_count += 1;
        log::info!(
            "[PTY Input] Received message #{} from {} ({} bytes)",
            message_count,
            &incoming.sender.0[..8.min(incoming.sender.0.len())],
            incoming.payload.len()
        );
        // Parse the incoming payload as JSON.
        let payload_str = match String::from_utf8(incoming.payload.clone()) {
            Ok(s) => s,
            Err(e) => {
                log::warn!(
                    "Non-UTF8 payload from browser {} agent {}: {}",
                    &browser_identity[..8.min(browser_identity.len())],
                    &agent_id[..8.min(agent_id.len())],
                    e
                );
                continue;
            }
        };

        // Try to parse as BrowserCommand (browser -> CLI messages).
        let command: BrowserCommand = match serde_json::from_str(&payload_str) {
            Ok(cmd) => cmd,
            Err(e) => {
                log::debug!(
                    "Failed to parse browser command from browser {} agent {}: {} (payload: {})",
                    &browser_identity[..8.min(browser_identity.len())],
                    &agent_id[..8.min(agent_id.len())],
                    e,
                    &payload_str[..100.min(payload_str.len())]
                );
                continue;
            }
        };

        match command {
            BrowserCommand::Input { data } => {
                log::info!(
                    "[PTY Input] Parsed Input command: {} bytes of data",
                    data.len()
                );
                // Send input request through channel to BrowserClient.
                if request_tx
                    .send(BrowserRequest::SendInput {
                        agent_index,
                        pty_index,
                        data: data.into_bytes(),
                    })
                    .is_err()
                {
                    log::debug!("BrowserRequest channel closed, stopping input receiver");
                    break;
                }
                log::info!("[PTY Input] Sent to BrowserClient request channel");
            }
            BrowserCommand::Resize { cols, rows } => {
                // Send resize request through channel to BrowserClient.
                if request_tx
                    .send(BrowserRequest::Resize {
                        agent_index,
                        pty_index,
                        rows,
                        cols,
                    })
                    .is_err()
                {
                    log::debug!("BrowserRequest channel closed, stopping input receiver");
                    break;
                }
            }
            _ => {
                // Other command types (ListAgents, SelectAgent, CreateAgent, etc.)
                // should go through the main hub channel, not the PTY channel.
                log::debug!(
                    "Received non-PTY command on PTY channel from browser {} (ignoring): {:?}",
                    &browser_identity[..8.min(browser_identity.len())],
                    std::mem::discriminant(&command)
                );
            }
        }
    }

    log::info!(
        "Stopped PTY input receiver for browser {} agent {} pty {}",
        &browser_identity[..8.min(browser_identity.len())],
        &agent_id[..8.min(agent_id.len())],
        pty_index
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a mock BrowserClientConfig for testing.
    fn mock_config() -> BrowserClientConfig {
        BrowserClientConfig {
            crypto_service: CryptoServiceHandle::mock(),
            server_url: "http://localhost:3000".to_string(),
            api_key: "test-api-key".to_string(),
            server_hub_id: "test-hub-id".to_string(),
        }
    }

    /// Helper to create a BrowserClient with mock dependencies for testing.
    fn test_client(identity: &str) -> BrowserClient {
        BrowserClient::new(HubHandle::mock(), identity.to_string(), mock_config(), None)
    }

    // ========== Unit Tests (No Async) ==========

    #[test]
    fn test_browser_client_creation() {
        let client = test_client("test-identity-12345678");
        assert!(client.id().is_browser());
        assert_eq!(client.dims(), (80, 24)); // Default size
    }

    #[test]
    fn test_browser_client_identity() {
        let client = test_client("my-signal-key");
        assert_eq!(client.identity(), "my-signal-key");

        match client.id() {
            ClientId::Browser(ref id) => assert_eq!(id, "my-signal-key"),
            _ => panic!("Should be a Browser client"),
        }
    }

    #[test]
    fn test_browser_client_dims() {
        let mut client = test_client("test");

        // Default dims
        assert_eq!(client.dims(), (80, 24));

        // Update dims directly
        client.dims = (120, 40);
        assert_eq!(client.dims(), (120, 40));
    }

    #[test]
    fn test_browser_client_get_agent_returns_none_with_mock_handle() {
        let client = test_client("test");

        // Mock hub_handle returns None.
        assert!(client.get_agent(0).is_none());
        assert!(client.get_agent(99).is_none());
    }

    // ========== UTF-8 Boundary Tests ==========

    #[test]
    fn test_utf8_boundary_empty() {
        let (valid, leftover) = super::find_utf8_boundary(&[]);
        assert_eq!(valid, 0);
        assert_eq!(leftover, 0);
    }

    #[test]
    fn test_utf8_boundary_ascii_only() {
        let data = b"Hello, world!";
        let (valid, leftover) = super::find_utf8_boundary(data);
        assert_eq!(valid, data.len());
        assert_eq!(leftover, data.len());
    }

    #[test]
    fn test_utf8_boundary_complete_multibyte() {
        // Box-drawing character ─ (E2 94 80)
        let data = b"\xe2\x94\x80";
        let (valid, leftover) = super::find_utf8_boundary(data);
        assert_eq!(valid, 3);
        assert_eq!(leftover, 3);
    }

    #[test]
    fn test_utf8_boundary_incomplete_2byte() {
        // Start of 2-byte sequence (C2) without continuation
        let data = b"hello\xc2";
        let (valid, leftover) = super::find_utf8_boundary(data);
        assert_eq!(valid, 5, "Should stop before incomplete sequence");
        assert_eq!(leftover, 5);
    }

    #[test]
    fn test_utf8_boundary_incomplete_3byte_1of3() {
        // Start of 3-byte sequence (E2) without continuations
        let data = b"hello\xe2";
        let (valid, leftover) = super::find_utf8_boundary(data);
        assert_eq!(valid, 5, "Should stop before incomplete 3-byte sequence");
        assert_eq!(leftover, 5);
    }

    #[test]
    fn test_utf8_boundary_incomplete_3byte_2of3() {
        // 2 bytes of a 3-byte sequence (E2 94) missing final byte
        let data = b"hello\xe2\x94";
        let (valid, leftover) = super::find_utf8_boundary(data);
        assert_eq!(valid, 5, "Should stop before incomplete 3-byte sequence");
        assert_eq!(leftover, 5);
    }

    #[test]
    fn test_utf8_boundary_incomplete_4byte() {
        // Start of 4-byte sequence (F0) without all continuations
        let data = b"hello\xf0\x9f";
        let (valid, leftover) = super::find_utf8_boundary(data);
        assert_eq!(valid, 5, "Should stop before incomplete 4-byte sequence");
        assert_eq!(leftover, 5);
    }

    #[test]
    fn test_utf8_boundary_mixed_valid() {
        // Mix of ASCII and complete multi-byte: "Hi─"
        let data = b"Hi\xe2\x94\x80";
        let (valid, leftover) = super::find_utf8_boundary(data);
        assert_eq!(valid, 5);
        assert_eq!(leftover, 5);
    }

    #[test]
    fn test_utf8_boundary_mixed_with_incomplete() {
        // "Hi─" followed by incomplete E2
        let data = b"Hi\xe2\x94\x80\xe2";
        let (valid, leftover) = super::find_utf8_boundary(data);
        assert_eq!(valid, 5, "Should include complete chars, exclude incomplete");
        assert_eq!(leftover, 5);
    }

    #[test]
    fn test_utf8_boundary_emoji() {
        // Complete emoji 😀 (F0 9F 98 80)
        let data = b"\xf0\x9f\x98\x80";
        let (valid, leftover) = super::find_utf8_boundary(data);
        assert_eq!(valid, 4);
        assert_eq!(leftover, 4);
    }

    // ========== Debug Format Tests ==========

    #[test]
    fn test_debug_format() {
        let client = test_client("test");
        let debug = format!("{:?}", client);

        // These fields SHOULD exist.
        assert!(debug.contains("id:"), "BrowserClient should have id field");
        assert!(
            debug.contains("dims:"),
            "BrowserClient should have dims field"
        );
        assert!(
            debug.contains("identity:"),
            "BrowserClient should have identity field"
        );
        assert!(
            debug.contains("hub_handle:"),
            "BrowserClient should have hub_handle field"
        );
        assert!(
            debug.contains("terminal_channels:"),
            "BrowserClient should have terminal_channels field"
        );
    }

    // ========== PTY Communication Tests ==========

    #[tokio::test]
    async fn test_browser_client_connect_to_pty_fails_without_agent() {
        // With mock hub_handle, connect_to_pty will fail because
        // hub_handle.get_agent() returns None.
        let mut client = test_client("test");

        let result = client.connect_to_pty(0, 0).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));

        // terminal_channels should remain empty.
        assert!(client.terminal_channels.is_empty());
    }

    #[tokio::test]
    async fn test_browser_client_disconnect_from_pty_is_safe_when_not_connected() {
        let mut client = test_client("test");

        // Should not panic when not connected.
        client.disconnect_from_pty(0, 0).await;
        client.disconnect_from_pty(99, 99).await;

        // terminal_channels should remain empty.
        assert!(client.terminal_channels.is_empty());
    }

    #[tokio::test]
    async fn test_browser_client_trait_default_send_input_fails_without_agent() {
        let mut client = test_client("test");

        // Default implementation looks up via hub_handle, which returns None.
        let result = Client::send_input(&mut client, 0, 0, b"test input").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[tokio::test]
    async fn test_browser_client_trait_default_resize_pty_fails_without_agent() {
        let mut client = test_client("test");

        // Default implementation looks up via hub_handle, which returns None.
        let result = Client::resize_pty(&mut client, 0, 0, 24, 80).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    // ========== BrowserRequest Channel Tests ==========

    #[test]
    fn test_request_channel_created_at_construction() {
        let client = test_client("test");
        // Channel is created at construction - tx can send without error.
        assert!(client
            .request_tx
            .send(BrowserRequest::SendInput {
                agent_index: 0,
                pty_index: 0,
                data: vec![b'x'],
            })
            .is_ok());
    }

    #[tokio::test]
    async fn test_handle_request_send_input() {
        let mut client = test_client("test");

        // Send input request (will fail since mock hub has no agents, but should not panic).
        client
            .handle_request(BrowserRequest::SendInput {
                agent_index: 0,
                pty_index: 0,
                data: vec![b'h', b'i'],
            })
            .await;
    }

    #[tokio::test]
    async fn test_handle_request_resize() {
        let mut client = test_client("test");

        // Send resize request (will fail since mock hub has no agents, but should not panic).
        client
            .handle_request(BrowserRequest::Resize {
                agent_index: 0,
                pty_index: 0,
                rows: 40,
                cols: 120,
            })
            .await;
    }

    #[tokio::test]
    async fn test_handle_request_multiple() {
        let mut client = test_client("test");

        // Send multiple requests from different PTYs.
        client
            .handle_request(BrowserRequest::SendInput {
                agent_index: 0,
                pty_index: 0,
                data: vec![b'a'],
            })
            .await;
        client
            .handle_request(BrowserRequest::Resize {
                agent_index: 1,
                pty_index: 0,
                rows: 24,
                cols: 80,
            })
            .await;
        client
            .handle_request(BrowserRequest::SendInput {
                agent_index: 0,
                pty_index: 1,
                data: vec![b'b'],
            })
            .await;
    }

    #[test]
    fn test_browser_request_debug() {
        // Verify BrowserRequest variants can be debugged.
        let send_input = BrowserRequest::SendInput {
            agent_index: 0,
            pty_index: 1,
            data: vec![1, 2, 3],
        };
        let resize = BrowserRequest::Resize {
            agent_index: 2,
            pty_index: 0,
            rows: 40,
            cols: 120,
        };

        assert!(format!("{:?}", send_input).contains("SendInput"));
        assert!(format!("{:?}", resize).contains("Resize"));
    }

    #[tokio::test]
    async fn test_run_task_shutdown() {
        let mut client = test_client("test");

        // Give client a hub event receiver, then drop the sender to close the channel.
        // The broadcast channel closing causes run_task to exit.
        let (hub_event_tx, hub_event_rx) =
            tokio::sync::broadcast::channel::<crate::hub::HubEvent>(16);
        client.hub_event_rx = Some(hub_event_rx);

        // Send Shutdown event; run_task handles it by not forwarding but the
        // sender drop will close the channel causing the Closed branch to fire.
        drop(hub_event_tx);

        // run_task should detect the closed broadcast channel and exit
        client.run_task().await;
        // If we reach here, the task completed successfully
    }

    // =========================================================================
    // Integration Tests: BrowserRequest full flow with real Hub
    // =========================================================================
    //
    // These tests exercise the complete BrowserRequest pipeline:
    //   Background task sends BrowserRequest -> BrowserClient.handle_request() ->
    //   Client trait method -> PTY

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

        /// Set up a Hub with a BrowserClient wired up.
        ///
        /// Returns:
        /// - The Hub (owns all state)
        /// - The BrowserClient (with request_tx for sending BrowserRequests)
        fn setup_browser_integration() -> (Hub, BrowserClient) {
            let config = test_config();
            let hub = Hub::new(config).unwrap();

            let hub_handle = hub.handle();
            let browser_config = BrowserClientConfig {
                crypto_service: CryptoServiceHandle::mock(),
                server_url: "http://localhost:3000".to_string(),
                api_key: "test-api-key".to_string(),
                server_hub_id: "test-hub-id".to_string(),
            };

            let client = BrowserClient::new(
                hub_handle,
                "test-browser-identity-12345678".to_string(),
                browser_config,
                None,
            );

            (hub, client)
        }

        /// Add an agent to the hub and sync the handle cache.
        fn add_agent_to_hub(hub: &mut Hub, issue: u32) -> String {
            let (key, agent) = create_test_agent(issue);
            hub.state.write().unwrap().add_agent(key.clone(), agent);
            hub.sync_handle_cache();
            key
        }

        // =====================================================================
        // TEST 1: SendInput reaches PTY via handle_request pipeline
        // =====================================================================

        /// Verify that BrowserRequest::SendInput routes keyboard input to the PTY.
        #[test]
        fn test_browser_send_input_reaches_pty() {
            let (mut hub, mut client) = setup_browser_integration();
            let agent_key = add_agent_to_hub(&mut hub, 42);

            // Connect BrowserClient to the agent's PTY directly (bypassing ActionCable).
            {
                let state = hub.state.read().unwrap();
                let _ = state
                    .agents
                    .get(&agent_key)
                    .unwrap()
                    .cli_pty
                    .connect(client.id().clone(), (80, 24));
            }

            // Process SendInput request (async, must run on Hub's runtime)
            hub.tokio_runtime.block_on(async {
                client
                    .handle_request(BrowserRequest::SendInput {
                        agent_index: 0,
                        pty_index: 0,
                        data: b"echo hello\n".to_vec(),
                    })
                    .await;
            });

            // Verify the input command arrived at the PTY.
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
        // TEST 2: Resize reaches PTY via handle_request pipeline
        // =====================================================================

        /// Verify that BrowserRequest::Resize updates PTY dimensions.
        #[test]
        fn test_browser_resize_reaches_pty() {
            let (mut hub, mut client) = setup_browser_integration();
            let agent_key = add_agent_to_hub(&mut hub, 42);

            // Connect BrowserClient to the agent's PTY (becomes size owner).
            {
                let state = hub.state.read().unwrap();
                let _ = state
                    .agents
                    .get(&agent_key)
                    .unwrap()
                    .cli_pty
                    .connect(client.id().clone(), (80, 24));
            }

            // Process Resize request (async, must run on Hub's runtime)
            hub.tokio_runtime.block_on(async {
                client
                    .handle_request(BrowserRequest::Resize {
                        agent_index: 0,
                        pty_index: 0,
                        rows: 40,
                        cols: 120,
                    })
                    .await;
            });

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
                "PTY dimensions should be (rows=40, cols=120) after Resize"
            );
        }

        // =====================================================================
        // TEST 3: Multi-connection - input to multiple PTYs independently
        // =====================================================================

        /// Verify that BrowserClient can route input to multiple agents' PTYs
        /// independently.
        #[test]
        fn test_browser_multi_connection() {
            let (mut hub, mut client) = setup_browser_integration();
            let agent_key_0 = add_agent_to_hub(&mut hub, 42);
            let agent_key_1 = add_agent_to_hub(&mut hub, 43);

            // Connect BrowserClient to both agents' PTYs directly.
            {
                let state = hub.state.read().unwrap();
                let _ = state
                    .agents
                    .get(&agent_key_0)
                    .unwrap()
                    .cli_pty
                    .connect(client.id().clone(), (80, 24));
                let _ = state
                    .agents
                    .get(&agent_key_1)
                    .unwrap()
                    .cli_pty
                    .connect(client.id().clone(), (80, 24));
            }

            // Process async requests on Hub's runtime
            hub.tokio_runtime.block_on(async {
                // Send input to agent 0
                client
                    .handle_request(BrowserRequest::SendInput {
                        agent_index: 0,
                        pty_index: 0,
                        data: b"agent-0-input\n".to_vec(),
                    })
                    .await;

                // Send input to agent 1
                client
                    .handle_request(BrowserRequest::SendInput {
                        agent_index: 1,
                        pty_index: 0,
                        data: b"agent-1-input\n".to_vec(),
                    })
                    .await;
            });

            // Verify agent 0's PTY received its command
            let commands_0 = hub
                .state
                .write()
                .unwrap()
                .agents
                .get_mut(&agent_key_0)
                .unwrap()
                .cli_pty
                .process_commands();
            assert!(
                commands_0 > 0,
                "Agent 0's PTY should have received input command"
            );

            // Verify agent 1's PTY received its command
            let commands_1 = hub
                .state
                .write()
                .unwrap()
                .agents
                .get_mut(&agent_key_1)
                .unwrap()
                .cli_pty
                .process_commands();
            assert!(
                commands_1 > 0,
                "Agent 1's PTY should have received input command"
            );
        }

        // =====================================================================
        // TEST 4: SendInput without connection is a no-op (no crash)
        // =====================================================================

        /// Verify that BrowserRequest::SendInput is handled gracefully when
        /// no PTY connection exists.
        #[test]
        fn test_browser_send_input_without_connection_is_noop() {
            let (hub, mut client) = setup_browser_integration();

            // Do NOT connect to any PTY.

            // Send input (should be gracefully handled - error logged, no crash)
            // (async, must run on Hub's runtime)
            hub.tokio_runtime.block_on(async {
                client
                    .handle_request(BrowserRequest::SendInput {
                        agent_index: 0,
                        pty_index: 0,
                        data: b"echo hello\n".to_vec(),
                    })
                    .await;
            });

            // If we got here without panic, the test passes.
        }
    }
}
