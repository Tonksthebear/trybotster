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
//!   ├── request_rx (single receiver, processed by poll_requests())
//!   └── terminal_channels (HashMap keyed by (agent_index, pty_index))
//!         └── TerminalChannel
//!               ├── channel (ActionCableChannel for WebSocket)
//!               └── task handles for cleanup
//! ```
//!
//! # Request Channel (BrowserRequest)
//!
//! Symmetric with TuiRequest. Browser background tasks (input receivers) send
//! `BrowserRequest` messages through a channel, which `poll_requests()` processes
//! in Hub's event loop. This routes operations through the Client trait rather
//! than calling PtyHandle directly from background tasks.
//!
//! Unlike TuiRequest, every BrowserRequest variant includes explicit PTY indices
//! because Browser supports multiple simultaneous PTY connections.
//!
//! # PTY I/O Routing
//!
//! When a browser connects to a PTY via `connect_to_pty()`:
//!
//! 1. Creates a TerminalRelayChannel (ActionCable with E2E encryption)
//! 2. Subscribes to PTY events via PtyHandle
//! 3. Spawns output forwarder: PTY events -> channel -> browser
//! 4. Spawns input receiver: channel -> BrowserRequest -> poll_requests() -> Client trait
//!
//! # Minimal Design
//!
//! BrowserClient implements only the required Client trait methods:
//! - `hub_handle()`, `id()`, `dims()`, `connect_to_pty()`, `disconnect_from_pty()`
//!
//! Default trait implementations handle:
//! - `get_agents`, `get_agent`, `send_input`, `resize_pty`, `agent_count`

// Rust guideline compliant 2026-01

use std::collections::HashMap;

use tokio::runtime::Handle;
use tokio::sync::broadcast;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::mpsc::error::TryRecvError;
use tokio::task::JoinHandle;

use crate::agent::pty::PtyEvent;
use crate::channel::{ActionCableChannel, Channel, ChannelConfig};
use crate::hub::HubHandle;
use crate::relay::crypto_service::CryptoServiceHandle;
use crate::relay::{build_scrollback_message, BrowserCommand, TerminalMessage};

use super::{Client, ClientId};

/// Requests from Browser to BrowserClient.
///
/// Symmetric with `TuiRequest`. Browser background tasks send these via channel,
/// BrowserClient routes them through Client trait methods in Hub's event loop.
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

    /// Tokio runtime handle for spawning async tasks.
    ///
    /// Stored directly to avoid blocking cross-thread calls when spawning
    /// forwarder tasks. Hub passes this at construction time.
    runtime: Handle,

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

    /// Sender for browser background tasks to route operations through Client trait.
    ///
    /// Cloned and passed to each `spawn_pty_input_receiver` task. Each task sends
    /// `BrowserRequest` with its specific (agent_index, pty_index) so poll_requests()
    /// can route to the correct PTY via Client trait methods.
    request_tx: UnboundedSender<BrowserRequest>,

    /// Receiver for requests from browser background tasks.
    ///
    /// Processed by `poll_requests()` in Hub's event loop. All input receiver tasks
    /// (one per PTY connection) share the single `request_tx` sender.
    request_rx: UnboundedReceiver<BrowserRequest>,
}

impl BrowserClient {
    /// Create a new browser client with connection config.
    ///
    /// # Arguments
    ///
    /// * `hub_handle` - Handle for Hub communication and agent queries.
    /// * `identity` - Signal identity key from browser handshake.
    /// * `runtime` - Tokio runtime handle for spawning async tasks.
    /// * `config` - Connection config for ActionCable channels.
    #[must_use]
    pub fn new(
        hub_handle: HubHandle,
        identity: String,
        runtime: Handle,
        config: BrowserClientConfig,
    ) -> Self {
        let (request_tx, request_rx) = mpsc::unbounded_channel();

        // Default terminal size: 80 columns x 24 rows
        Self {
            id: ClientId::Browser(identity.clone()),
            dims: (80, 24),
            identity,
            hub_handle,
            runtime,
            config,
            terminal_channels: HashMap::new(),
            request_tx,
            request_rx,
        }
    }

    /// Get the Signal identity key.
    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Update terminal dimensions.
    ///
    /// Called when browser reports new terminal size.
    pub fn update_dims(&mut self, cols: u16, rows: u16) {
        self.dims = (cols, rows);
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
    ///
    /// Used by Hub to look up PTYs directly from state when resizing,
    /// avoiding the deadlock that would occur if we called through `hub_handle`.
    pub fn connected_ptys(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.terminal_channels.keys().copied()
    }

    /// Poll for requests from browser background tasks and process them.
    ///
    /// Called from Hub's event loop. Processes up to 100 requests per tick
    /// to prevent blocking on high-volume input. Symmetric with
    /// `TuiClient::poll_requests()`.
    pub fn poll_requests(&mut self) {
        // Collect requests first to avoid borrow checker issues
        // (can't call handle_request while borrowing request_rx).
        let mut requests = Vec::with_capacity(100);
        for _ in 0..100 {
            match self.request_rx.try_recv() {
                Ok(request) => requests.push(request),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    log::warn!("Browser request channel disconnected");
                    break;
                }
            }
        }

        // Now process all collected requests.
        for request in requests {
            self.handle_request(request);
        }
    }

    /// Handle a single request from a browser background task.
    ///
    /// Routes the request to the appropriate Client trait method. Unlike
    /// TuiClient's handler, every variant includes explicit PTY indices
    /// because Browser supports multiple simultaneous connections.
    fn handle_request(&mut self, request: BrowserRequest) {
        match request {
            BrowserRequest::SendInput { agent_index, pty_index, data } => {
                if let Err(e) = self.send_input(agent_index, pty_index, &data) {
                    log::error!("Failed to send input to PTY ({}, {}): {}", agent_index, pty_index, e);
                }
            }
            BrowserRequest::Resize { agent_index, pty_index, rows, cols } => {
                if let Err(e) = self.resize_pty(agent_index, pty_index, rows, cols) {
                    log::error!("Failed to resize PTY ({}, {}): {}", agent_index, pty_index, e);
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

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn set_dims(&mut self, cols: u16, rows: u16) {
        self.update_dims(cols, rows);
        // NOTE: BrowserClient does NOT propagate resize here.
        // Browser clients manage multiple simultaneous PTY connections,
        // so resize is handled per-PTY via BrowserEvent::Resize in relay/browser.rs.
    }

    fn connect_to_pty_with_handle(
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

        // Create ActionCableChannel with E2E encryption.
        let mut channel = ActionCableChannel::encrypted(crypto_service, server_url, api_key);

        // Connect to TerminalRelayChannel.
        // Uses stored runtime handle - no blocking cross-thread call needed.
        let connect_result = self.runtime.block_on(async {
            channel
                .connect(ChannelConfig {
                    channel_name: "TerminalRelayChannel".into(),
                    hub_id,
                    agent_index: Some(agent_index),
                    pty_index: Some(pty_index),
                    encrypt: true,
                    // Threshold for gzip compression (4KB)
                    compression_threshold: Some(4096),
                })
                .await
        });

        connect_result.map_err(|e| format!("Failed to connect channel: {}", e))?;

        // Get sender and receiver handles BEFORE spawning tasks.
        let sender_handle = channel
            .get_sender_handle()
            .ok_or_else(|| "Failed to get channel sender handle".to_string())?;
        let receiver_handle = channel
            .take_receiver_handle()
            .ok_or_else(|| "Failed to get channel receiver handle".to_string())?;

        // Connect to PTY and get scrollback BEFORE spawning forwarder.
        // This ensures the browser receives historical output first.
        let scrollback = pty_handle.connect_blocking(self.id.clone(), self.dims)?;

        // Send scrollback to browser if available.
        if !scrollback.is_empty() {
            let scrollback_msg = build_scrollback_message(scrollback);
            if let Ok(json) = serde_json::to_string(&scrollback_msg) {
                let sender_clone = sender_handle.clone();
                self.runtime.spawn(async move {
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
        let output_task = self.runtime.spawn(spawn_pty_output_forwarder(
            pty_rx,
            sender_handle,
            browser_identity.clone(),
            agent_id.clone(),
            pty_index,
        ));

        // Spawn input receiver: Browser -> BrowserRequest channel -> Client trait.
        let request_tx = self.request_tx.clone();
        let input_task = self.runtime.spawn(spawn_pty_input_receiver(
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
    fn disconnect_from_pty_with_handle(
        &mut self,
        pty: &crate::hub::agent_handle::PtyHandle,
        agent_index: usize,
        pty_index: usize,
    ) {
        // Remove channel from map - dropping it cleans up tasks.
        if self.terminal_channels.remove(&(agent_index, pty_index)).is_some() {
            // Notify PTY of disconnection.
            let _ = pty.disconnect_blocking(self.id.clone());

            log::info!(
                "Browser {} disconnected from PTY ({}, {})",
                &self.identity[..8.min(self.identity.len())],
                agent_index,
                pty_index
            );
        }
    }

    fn disconnect_from_pty(&mut self, agent_index: usize, pty_index: usize) {
        // Remove channel from map - dropping it cleans up tasks.
        if let Some(_channel) = self.terminal_channels.remove(&(agent_index, pty_index)) {
            // Notify PTY of disconnection.
            // NOTE: hub_handle.get_agent() reads from HandleCache (non-blocking).
            // However, disconnect_blocking() is blocking and must not be called from Hub's event loop.
            if let Some(agent) = self.hub_handle.get_agent(agent_index) {
                if let Some(pty) = agent.get_pty(pty_index) {
                    let _ = pty.disconnect_blocking(self.id.clone());
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

    // NOTE: get_agents, get_agent, send_input, resize_pty, agent_count
    // all use DEFAULT IMPLEMENTATIONS from the trait - not implemented here
}

/// Background task that forwards PTY output to browser via ActionCableChannel.
///
/// Subscribes to PTY events and sends `Output` events through the channel.
/// Exits when the PTY closes or channel disconnects.
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

    loop {
        match pty_rx.recv().await {
            Ok(PtyEvent::Output(data)) => {
                // Convert bytes to string (lossy for non-UTF8).
                let output_str = String::from_utf8_lossy(&data);

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

/// Background task that receives input from browser and sends BrowserRequest to BrowserClient.
///
/// Listens for incoming messages from the browser (through the encrypted channel)
/// and sends them as `BrowserRequest` variants through the channel. BrowserClient's
/// `poll_requests()` routes these through the Client trait to the correct PTY.
///
/// This task does NOT call PtyHandle directly. All PTY operations go through
/// the BrowserRequest channel -> poll_requests() -> Client trait methods.
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

    while let Some(incoming) = receiver.recv().await {
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
                // Send input request through channel to BrowserClient.
                if request_tx.send(BrowserRequest::SendInput {
                    agent_index,
                    pty_index,
                    data: data.into_bytes(),
                }).is_err() {
                    log::debug!("BrowserRequest channel closed, stopping input receiver");
                    break;
                }
            }
            BrowserCommand::Resize { cols, rows } => {
                // Send resize request through channel to BrowserClient.
                if request_tx.send(BrowserRequest::Resize {
                    agent_index,
                    pty_index,
                    rows,
                    cols,
                }).is_err() {
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
        BrowserClient::new(
            HubHandle::mock(),
            identity.to_string(),
            mock_runtime_handle(),
            mock_config(),
        )
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

        // Update dims
        client.update_dims(120, 40);
        assert_eq!(client.dims(), (120, 40));
    }

    #[test]
    fn test_browser_client_get_agents_returns_empty_with_mock_handle() {
        let client = test_client("test");

        // Mock hub_handle returns empty.
        let agents = client.get_agents();
        assert!(agents.is_empty());
    }

    #[test]
    fn test_browser_client_get_agent_returns_none_with_mock_handle() {
        let client = test_client("test");

        // Mock hub_handle returns None.
        assert!(client.get_agent(0).is_none());
        assert!(client.get_agent(99).is_none());
    }

    // ========== Debug Format Tests ==========

    #[test]
    fn test_debug_format() {
        let client = test_client("test");
        let debug = format!("{:?}", client);

        // These fields SHOULD exist.
        assert!(
            debug.contains("id:"),
            "BrowserClient should have id field"
        );
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

        // These fields should NOT exist (removed in refactor).
        assert!(
            !debug.contains("pty_handles:"),
            "BrowserClient should not have pty_handles field"
        );
        assert!(
            !debug.contains("connected_agent_index"),
            "BrowserClient should not have connected_agent_index field"
        );
        assert!(
            !debug.contains("connected_pty_index"),
            "BrowserClient should not have connected_pty_index field"
        );
    }

    // ========== PTY Communication Tests ==========

    #[test]
    fn test_browser_client_connect_to_pty_fails_without_agent() {
        // With mock hub_handle, connect_to_pty will fail because
        // hub_handle.get_agent() returns None.
        let mut client = test_client("test");

        let result = client.connect_to_pty(0, 0);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));

        // terminal_channels should remain empty.
        assert!(client.terminal_channels.is_empty());
    }

    #[test]
    fn test_browser_client_disconnect_from_pty_is_safe_when_not_connected() {
        let mut client = test_client("test");

        // Should not panic when not connected.
        client.disconnect_from_pty(0, 0);
        client.disconnect_from_pty(99, 99);

        // terminal_channels should remain empty.
        assert!(client.terminal_channels.is_empty());
    }

    #[test]
    fn test_browser_client_trait_default_send_input_fails_without_agent() {
        let client = test_client("test");

        // Default implementation looks up via hub_handle, which returns None.
        let result = Client::send_input(&client, 0, 0, b"test input");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_browser_client_trait_default_resize_pty_fails_without_agent() {
        let client = test_client("test");

        // Default implementation looks up via hub_handle, which returns None.
        let result = Client::resize_pty(&client, 0, 0, 24, 80);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_browser_client_trait_default_agent_count() {
        let client = test_client("test");

        // Mock returns empty, so count is 0.
        assert_eq!(Client::agent_count(&client), 0);
    }

    // ========== BrowserRequest Channel Tests ==========

    #[test]
    fn test_request_channel_created_at_construction() {
        let client = test_client("test");
        // Channel is created at construction - tx can send without error.
        assert!(client.request_tx.send(BrowserRequest::SendInput {
            agent_index: 0,
            pty_index: 0,
            data: vec![b'x'],
        }).is_ok());
    }

    #[test]
    fn test_poll_requests_empty_channel() {
        let mut client = test_client("test");

        // Should not panic with empty channel.
        client.poll_requests();
    }

    #[test]
    fn test_poll_requests_send_input() {
        let mut client = test_client("test");

        // Send input request (will fail since mock hub has no agents, but should not panic).
        client.request_tx.send(BrowserRequest::SendInput {
            agent_index: 0,
            pty_index: 0,
            data: vec![b'h', b'i'],
        }).unwrap();

        // Process it - should log error but not panic.
        client.poll_requests();
    }

    #[test]
    fn test_poll_requests_resize() {
        let mut client = test_client("test");

        // Send resize request (will fail since mock hub has no agents, but should not panic).
        client.request_tx.send(BrowserRequest::Resize {
            agent_index: 0,
            pty_index: 0,
            rows: 40,
            cols: 120,
        }).unwrap();

        // Process it - should log error but not panic.
        client.poll_requests();
    }

    #[test]
    fn test_poll_requests_multiple() {
        let mut client = test_client("test");

        // Send multiple requests from different PTYs.
        client.request_tx.send(BrowserRequest::SendInput {
            agent_index: 0,
            pty_index: 0,
            data: vec![b'a'],
        }).unwrap();
        client.request_tx.send(BrowserRequest::Resize {
            agent_index: 1,
            pty_index: 0,
            rows: 24,
            cols: 80,
        }).unwrap();
        client.request_tx.send(BrowserRequest::SendInput {
            agent_index: 0,
            pty_index: 1,
            data: vec![b'b'],
        }).unwrap();

        // Process all - should not panic.
        client.poll_requests();
    }

    #[test]
    fn test_poll_requests_from_cloned_sender() {
        let mut client = test_client("test");

        // Clone the sender (simulates what connect_to_pty_with_handle does).
        let tx_clone = client.request_tx.clone();
        tx_clone.send(BrowserRequest::SendInput {
            agent_index: 0,
            pty_index: 0,
            data: vec![b'z'],
        }).unwrap();

        // Process it - should not panic.
        client.poll_requests();
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

    // =========================================================================
    // Integration Tests: BrowserRequest full flow with real Hub
    // =========================================================================
    //
    // These tests exercise the complete BrowserRequest pipeline:
    //   Background task sends BrowserRequest -> BrowserClient.poll_requests() ->
    //   Client trait method -> PTY
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

        /// Set up a Hub with a BrowserClient wired up.
        ///
        /// Returns:
        /// - The Hub (owns all state)
        /// - The BrowserClient (with request_tx for sending BrowserRequests)
        ///
        /// BrowserClient is NOT registered in the Hub's ClientRegistry here -
        /// it's returned separately because the integration tests need direct
        /// mutable access to call poll_requests(). The Hub is used for state
        /// and agent management.
        fn setup_browser_integration() -> (Hub, BrowserClient) {
            let config = test_config();
            let hub = Hub::new(config, TEST_DIMS).unwrap();

            let hub_handle = hub.handle();
            let runtime_handle = hub.tokio_runtime.handle().clone();
            let browser_config = BrowserClientConfig {
                crypto_service: CryptoServiceHandle::mock(),
                server_url: "http://localhost:3000".to_string(),
                api_key: "test-api-key".to_string(),
                server_hub_id: "test-hub-id".to_string(),
            };

            let client = BrowserClient::new(
                hub_handle,
                "test-browser-identity-12345678".to_string(),
                runtime_handle,
                browser_config,
            );

            (hub, client)
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

        // =====================================================================
        // TEST 1: SendInput reaches PTY via BrowserRequest pipeline
        // =====================================================================

        /// Verify that BrowserRequest::SendInput routes keyboard input to the PTY.
        ///
        /// Full flow:
        /// 1. Setup Hub with agent
        /// 2. Create BrowserClient, connect to agent PTY directly via PtySession
        /// 3. Send BrowserRequest::SendInput via request_tx
        /// 4. Call poll_requests() to process the request
        /// 5. Verify input command arrived at PTY's command channel
        #[test]
        fn test_browser_send_input_reaches_pty() {
            let (mut hub, mut client) = setup_browser_integration();
            let agent_key = add_agent_to_hub(&mut hub, 42);

            // Connect BrowserClient to the agent's PTY directly (bypassing ActionCable).
            // We register the client with the PTY so send_input works via hub_handle.
            {
                let state = hub.state.read().unwrap();
                let _ = state
                    .agents
                    .get(&agent_key)
                    .unwrap()
                    .cli_pty
                    .connect(client.id().clone(), (80, 24));
            }

            // Send input through the BrowserRequest channel
            let input_data = b"echo hello\n".to_vec();
            client.request_tx.send(BrowserRequest::SendInput {
                agent_index: 0,
                pty_index: 0,
                data: input_data.clone(),
            }).unwrap();

            // Process the request through BrowserClient
            client.poll_requests();

            // Verify the input command arrived at the PTY.
            // process_commands() drains the PTY's command channel and handles them.
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
        // TEST 2: Resize reaches PTY via BrowserRequest pipeline
        // =====================================================================

        /// Verify that BrowserRequest::Resize updates PTY dimensions.
        ///
        /// Full flow:
        /// 1. Setup Hub with agent
        /// 2. Create BrowserClient, connect to agent PTY with initial dims
        /// 3. Send BrowserRequest::Resize with new dimensions
        /// 4. Call poll_requests()
        /// 5. Process PTY commands
        /// 6. Verify PTY dimensions updated
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

            // Send Resize through the BrowserRequest channel
            client.request_tx.send(BrowserRequest::Resize {
                agent_index: 0,
                pty_index: 0,
                rows: 40,
                cols: 120,
            }).unwrap();

            // Process the request (BrowserClient routes to Client::resize_pty)
            client.poll_requests();

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
        ///
        /// Browser supports multiple simultaneous PTY connections (one per tab).
        /// This test verifies that SendInput with different agent_index values
        /// routes to the correct PTY.
        ///
        /// Full flow:
        /// 1. Setup Hub with 2 agents
        /// 2. Create BrowserClient, connect to BOTH agent PTYs
        /// 3. Send input to agent 0's PTY
        /// 4. Send input to agent 1's PTY
        /// 5. Verify each PTY received its input independently
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

            // Send input to agent 0
            client.request_tx.send(BrowserRequest::SendInput {
                agent_index: 0,
                pty_index: 0,
                data: b"agent-0-input\n".to_vec(),
            }).unwrap();

            // Send input to agent 1
            client.request_tx.send(BrowserRequest::SendInput {
                agent_index: 1,
                pty_index: 0,
                data: b"agent-1-input\n".to_vec(),
            }).unwrap();

            // Process all requests
            client.poll_requests();

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
        ///
        /// Browser clients might send input before connecting to any PTY
        /// (race condition). This should not crash - the error is logged and
        /// the request is silently dropped.
        ///
        /// Full flow:
        /// 1. Create BrowserClient but do NOT connect to any PTY
        /// 2. Send BrowserRequest::SendInput
        /// 3. Call poll_requests()
        /// 4. Verify no crash (request is gracefully ignored)
        #[test]
        fn test_browser_send_input_without_connection_is_noop() {
            let (_hub, mut client) = setup_browser_integration();

            // Do NOT connect to any PTY.
            // BrowserClient has no connected PTYs.

            // Send input (should be gracefully handled - error logged, no crash)
            client.request_tx.send(BrowserRequest::SendInput {
                agent_index: 0,
                pty_index: 0,
                data: b"echo hello\n".to_vec(),
            }).unwrap();

            // Process the request - should not panic.
            // The Client::send_input default implementation will fail because
            // hub_handle.get_agent(0) returns None (no agents), but BrowserClient
            // logs the error and continues.
            client.poll_requests();

            // If we got here without panic, the test passes.
        }
    }
}
