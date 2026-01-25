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
//!   └── terminal_channels (HashMap keyed by (agent_index, pty_index))
//!         └── TerminalChannel
//!               ├── channel (ActionCableChannel for WebSocket)
//!               └── task handles for cleanup
//! ```
//!
//! # PTY I/O Routing
//!
//! When a browser connects to a PTY via `connect_to_pty()`:
//!
//! 1. Creates a TerminalRelayChannel (ActionCable with E2E encryption)
//! 2. Subscribes to PTY events via PtyHandle
//! 3. Spawns output forwarder: PTY events -> channel -> browser
//! 4. Spawns input receiver: channel -> PTY (keyboard input, resize)
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

use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::agent::pty::PtyEvent;
use crate::channel::{ActionCableChannel, Channel, ChannelConfig};
use crate::hub::agent_handle::PtyHandle;
use crate::hub::HubHandle;
use crate::relay::{BrowserCommand, TerminalMessage};

use super::{Client, ClientId};

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

    /// Terminal channels keyed by (agent_index, pty_index).
    ///
    /// Browser can have multiple simultaneous connections (one per tab).
    terminal_channels: HashMap<(usize, usize), TerminalChannel>,
}

impl BrowserClient {
    /// Create a new browser client.
    ///
    /// # Arguments
    ///
    /// * `hub_handle` - Handle for Hub communication and agent queries.
    /// * `identity` - Signal identity key from browser handshake.
    #[must_use]
    pub fn new(hub_handle: HubHandle, identity: String) -> Self {
        // Default terminal size: 80 columns x 24 rows
        Self {
            id: ClientId::Browser(identity.clone()),
            dims: (80, 24),
            identity,
            hub_handle,
            terminal_channels: HashMap::new(),
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

    fn set_dims(&mut self, cols: u16, rows: u16) {
        self.update_dims(cols, rows);

        // Propagate resize to all connected PTYs
        let pty_indices: Vec<_> = self.terminal_channels.keys().copied().collect();
        for (agent_idx, pty_idx) in pty_indices {
            if let Err(e) = self.resize_pty(agent_idx, pty_idx, rows, cols) {
                log::debug!("Failed to resize PTY ({}, {}): {}", agent_idx, pty_idx, e);
            }
        }
    }

    fn connect_to_pty(&mut self, agent_index: usize, pty_index: usize) -> Result<(), String> {
        let key = (agent_index, pty_index);

        // Idempotent: return Ok if already connected to this PTY.
        if self.terminal_channels.contains_key(&key) {
            return Ok(());
        }

        // Get crypto service from hub_handle for E2E encryption.
        let crypto_service = self
            .hub_handle
            .crypto_service()
            .ok_or_else(|| "No crypto service available for E2E encryption".to_string())?;

        // Get agent handle from Hub.
        let agent_handle = self
            .hub_handle
            .get_agent(agent_index)
            .ok_or_else(|| format!("Agent at index {} not found", agent_index))?;

        // Get PTY handle from agent.
        let pty_handle = agent_handle
            .get_pty(pty_index)
            .ok_or_else(|| format!("PTY at index {} not found for agent", pty_index))?
            .clone();

        // Get connection config from hub_handle.
        let hub_id = self
            .hub_handle
            .server_hub_id()
            .ok_or_else(|| "No hub ID available".to_string())?;
        let server_url = self.hub_handle.server_url();
        let api_key = self.hub_handle.api_key();

        // Create ActionCableChannel with E2E encryption.
        let mut channel = ActionCableChannel::encrypted(crypto_service, server_url, api_key);

        // Connect to TerminalRelayChannel.
        // Uses tokio runtime from hub_handle for blocking connect.
        let runtime = self
            .hub_handle
            .tokio_runtime()
            .ok_or_else(|| "No tokio runtime available".to_string())?;

        let connect_result = runtime.block_on(async {
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

        // Subscribe to PTY events for output forwarding.
        let pty_rx = pty_handle.subscribe();

        // Clone values for spawned tasks.
        let browser_identity = self.identity.clone();
        let agent_id = agent_handle.agent_id().to_string();

        // Spawn output forwarder: PTY -> Browser.
        let output_task = runtime.spawn(spawn_pty_output_forwarder(
            pty_rx,
            sender_handle,
            browser_identity.clone(),
            agent_id.clone(),
            pty_index,
        ));

        // Clone for input receiver.
        let pty_handle_clone = pty_handle.clone();
        let client_id = self.id.clone();

        // Spawn input receiver: Browser -> PTY.
        let input_task = runtime.spawn(spawn_pty_input_receiver(
            receiver_handle,
            pty_handle_clone,
            client_id,
            browser_identity,
            agent_id,
            pty_index,
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

        // Notify PTY that we connected and get scrollback (currently unused).
        let _scrollback = pty_handle.connect_blocking(self.id.clone(), self.dims);

        log::info!(
            "Browser {} connected to PTY ({}, {})",
            &self.identity[..8.min(self.identity.len())],
            agent_index,
            pty_index
        );

        Ok(())
    }

    fn disconnect_from_pty(&mut self, agent_index: usize, pty_index: usize) {
        // Remove channel from map - dropping it cleans up tasks.
        if let Some(_channel) = self.terminal_channels.remove(&(agent_index, pty_index)) {
            // Notify PTY of disconnection.
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
                // Continue receiving - may have final output.
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

/// Background task that receives input from browser via ActionCableChannel and routes to PTY.
///
/// Listens for incoming messages from the browser (through the encrypted channel)
/// and routes them to the appropriate PTY session. Handles both input data and
/// resize commands.
///
/// # Message Types
///
/// - `BrowserCommand::Input { data }` - Keyboard input to send to PTY
/// - `BrowserCommand::Resize { cols, rows }` - Terminal resize from browser
///
/// Other `BrowserCommand` variants should go through the main hub channel.
async fn spawn_pty_input_receiver(
    mut receiver: crate::channel::ChannelReceiverHandle,
    pty_handle: PtyHandle,
    client_id: ClientId,
    browser_identity: String,
    agent_id: String,
    pty_index: usize,
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
                // Route input to PTY.
                if let Err(e) = pty_handle.write_input(data.as_bytes()).await {
                    log::warn!(
                        "Failed to write input to PTY for browser {} agent {}: {}",
                        &browser_identity[..8.min(browser_identity.len())],
                        &agent_id[..8.min(agent_id.len())],
                        e
                    );
                    // Continue - PTY might recover.
                }
            }
            BrowserCommand::Resize { cols, rows } => {
                // Route resize to PTY.
                if let Err(e) = pty_handle.resize(client_id.clone(), rows, cols).await {
                    log::warn!(
                        "Failed to resize PTY for browser {} agent {}: {}",
                        &browser_identity[..8.min(browser_identity.len())],
                        &agent_id[..8.min(agent_id.len())],
                        e
                    );
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

    // ========== Unit Tests (No Async) ==========

    #[test]
    fn test_browser_client_creation() {
        let client = BrowserClient::new(HubHandle::mock(), "test-identity-12345678".to_string());
        assert!(client.id().is_browser());
        assert_eq!(client.dims(), (80, 24)); // Default size
    }

    #[test]
    fn test_browser_client_identity() {
        let client = BrowserClient::new(HubHandle::mock(), "my-signal-key".to_string());
        assert_eq!(client.identity(), "my-signal-key");

        match client.id() {
            ClientId::Browser(ref id) => assert_eq!(id, "my-signal-key"),
            _ => panic!("Should be a Browser client"),
        }
    }

    #[test]
    fn test_browser_client_dims() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Default dims
        assert_eq!(client.dims(), (80, 24));

        // Update dims
        client.update_dims(120, 40);
        assert_eq!(client.dims(), (120, 40));
    }

    #[test]
    fn test_browser_client_get_agents_returns_empty_with_mock_handle() {
        let client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Mock hub_handle returns empty.
        let agents = client.get_agents();
        assert!(agents.is_empty());
    }

    #[test]
    fn test_browser_client_get_agent_returns_none_with_mock_handle() {
        let client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Mock hub_handle returns None.
        assert!(client.get_agent(0).is_none());
        assert!(client.get_agent(99).is_none());
    }

    // ========== Debug Format Tests ==========

    #[test]
    fn test_debug_format() {
        let client = BrowserClient::new(HubHandle::mock(), "test".to_string());
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
    fn test_browser_client_connect_to_pty_fails_without_crypto() {
        // With mock hub_handle, connect_to_pty will fail because
        // crypto_service is not available.
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        let result = client.connect_to_pty(0, 0);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("crypto service"));

        // terminal_channels should remain empty.
        assert!(client.terminal_channels.is_empty());
    }

    #[test]
    fn test_browser_client_disconnect_from_pty_is_safe_when_not_connected() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Should not panic when not connected.
        client.disconnect_from_pty(0, 0);
        client.disconnect_from_pty(99, 99);

        // terminal_channels should remain empty.
        assert!(client.terminal_channels.is_empty());
    }

    #[test]
    fn test_browser_client_trait_default_send_input_fails_without_agent() {
        let client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Default implementation looks up via hub_handle, which returns None.
        let result = Client::send_input(&client, 0, 0, b"test input");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_browser_client_trait_default_resize_pty_fails_without_agent() {
        let client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Default implementation looks up via hub_handle, which returns None.
        let result = Client::resize_pty(&client, 0, 0, 24, 80);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_browser_client_trait_default_agent_count() {
        let client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Mock returns empty, so count is 0.
        assert_eq!(Client::agent_count(&client), 0);
    }
}
