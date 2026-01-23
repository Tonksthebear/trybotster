//! Browser client - thin IO pipe for encrypted ActionCable communication.
//!
//! BrowserClient is responsible ONLY for IO concerns:
//! - Managing ActionCable channel connections (hub channel, per-PTY channels)
//! - Encrypting outbound messages via Signal Protocol
//! - Routing terminal output to the correct PTY channel
//!
//! The web frontend (Rails) owns all UI state:
//! - Selected agent
//! - Active PTY view
//! - Scroll position
//!
//! # Architecture
//!
//! ```text
//! BrowserClient (IO pipe)
//!     |
//!     +-- hub_handle: HubHandle
//!     |       -> Agent queries and Hub commands
//!     |
//!     +-- hub_channel: ActionCableChannel
//!     |       -> Agent CRUD events (created, deleted, shutdown)
//!     |
//!     +-- pty_channels: HashMap<String, ActionCableChannel>
//!     |       -> Per-PTY terminal output
//!     |
//!     +-- active_pty_sender: ChannelSenderHandle
//!             -> Cached sender for fast output routing
//! ```
//!
//! # Channel Routing
//!
//! - Hub-level events (agent list changes) -> hub_channel
//! - Terminal output -> active PTY channel (cached for performance)
//! - Input from browser -> written to PTY via Hub
//!
//! # Agent Data Access
//!
//! BrowserClient queries agent data via HubHandle. `get_agents()` and
//! `get_agent()` return real data from the Hub.

// Rust guideline compliant 2026-01

use std::collections::HashMap;

use super::types::CreateAgentRequest;
use super::{AgentHandle, AgentInfo, Client, ClientId};
use crate::channel::{ActionCableChannel, Channel, ChannelSenderHandle};
use crate::hub::HubHandle;
use crate::relay::types::TerminalMessage;

/// Browser connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionState {
    /// Connected and ready for IO.
    Connected,
    /// Disconnected - no active WebSocket.
    #[default]
    Disconnected,
}

/// Browser client - thin IO pipe for encrypted ActionCable communication.
///
/// Owns only IO-related state:
/// - Identity and dimensions for PTY sizing
/// - ActionCable channels for encrypted message delivery
/// - Connection state
/// - HubHandle for agent queries
///
/// Does NOT own:
/// - Agent state (hub owns, browser queries via HubHandle)
/// - Selected agent (web frontend tracks)
/// - Active PTY view (web frontend tracks)
/// - Scroll position (xterm.js tracks)
///
/// # Agent Data Access
///
/// BrowserClient queries Hub state via HubHandle the same way TuiClient does.
/// `get_agents()` and `get_agent()` return real data from the Hub.
#[derive(Debug)]
pub struct BrowserClient {
    /// Unique client identifier.
    id: ClientId,

    /// Terminal dimensions from browser (cols, rows).
    /// Used for PTY sizing when this client is the size owner.
    dims: (u16, u16),

    /// Signal identity key for encryption routing.
    identity: String,

    /// WebSocket connection state.
    connection: ConnectionState,

    /// Whether this client owns the PTY size.
    /// Only the size owner's dimensions are applied to the PTY.
    is_size_owner: bool,

    // === ActionCable Channels ===
    /// Hub-level channel for agent CRUD events.
    /// Shared by all browsers viewing this hub.
    hub_channel: Option<ActionCableChannel>,

    /// Per-PTY channels for terminal output.
    /// Key format: "{agent_id}:{pty_index}" (e.g., "agent-123:0" for CLI PTY).
    pty_channels: HashMap<String, ActionCableChannel>,

    /// Cached sender handle for the active PTY channel.
    /// Avoids HashMap lookup on every output chunk.
    active_pty_sender: Option<ChannelSenderHandle>,

    /// Cache key for the active PTY sender.
    /// Used to invalidate cache when PTY changes.
    active_pty_key: Option<String>,

    /// Handle for querying Hub state.
    ///
    /// Allows BrowserClient to query agent data like TuiClient.
    hub_handle: HubHandle,
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
        Self {
            id: ClientId::Browser(identity.clone()),
            dims: (80, 24), // Default terminal size
            identity,
            connection: ConnectionState::Connected,
            is_size_owner: false,
            hub_channel: None,
            pty_channels: HashMap::new(),
            active_pty_sender: None,
            active_pty_key: None,
            hub_handle,
        }
    }

    /// Get a reference to the Hub handle.
    #[must_use]
    pub fn hub_handle(&self) -> &HubHandle {
        &self.hub_handle
    }

    /// Get the Signal identity key.
    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Get connection state.
    #[must_use]
    pub fn connection_state(&self) -> ConnectionState {
        self.connection
    }

    /// Mark as disconnected.
    pub fn set_disconnected(&mut self) {
        self.connection = ConnectionState::Disconnected;
    }

    /// Update terminal dimensions.
    ///
    /// Called when browser reports new terminal size.
    pub fn update_dims(&mut self, cols: u16, rows: u16) {
        self.dims = (cols, rows);
    }

    /// Handle ownership change notification.
    ///
    /// Called when this client becomes (or stops being) the PTY size owner.
    pub fn on_owner_changed(&mut self, is_owner: bool) {
        self.is_size_owner = is_owner;
    }

    /// Check if this client is the size owner.
    #[must_use]
    pub fn is_size_owner(&self) -> bool {
        self.is_size_owner
    }

    // === Channel Management ===

    /// Set the hub-level channel for agent CRUD events.
    pub fn set_hub_channel(&mut self, channel: ActionCableChannel) {
        self.hub_channel = Some(channel);
    }

    /// Get a reference to the hub channel.
    #[must_use]
    pub fn hub_channel(&self) -> Option<&ActionCableChannel> {
        self.hub_channel.as_ref()
    }

    /// Get a mutable reference to the hub channel.
    #[must_use]
    pub fn hub_channel_mut(&mut self) -> Option<&mut ActionCableChannel> {
        self.hub_channel.as_mut()
    }

    /// Connect a PTY channel for terminal output.
    ///
    /// # Arguments
    ///
    /// * `agent_id` - Agent session key.
    /// * `pty_index` - PTY index (0=CLI, 1=Server).
    /// * `channel` - The ActionCable channel for this PTY.
    pub fn connect_pty_channel(
        &mut self,
        agent_id: &str,
        pty_index: usize,
        channel: ActionCableChannel,
    ) {
        let key = format!("{agent_id}:{pty_index}");
        self.pty_channels.insert(key, channel);
    }

    /// Disconnect a PTY channel.
    ///
    /// If this was the active channel, clears the cache.
    pub fn disconnect_pty_channel(&mut self, agent_id: &str, pty_index: usize) {
        let key = format!("{agent_id}:{pty_index}");

        // Clear cache if this was the active channel
        if self.active_pty_key.as_deref() == Some(&key) {
            self.active_pty_sender = None;
            self.active_pty_key = None;
        }

        self.pty_channels.remove(&key);
    }

    /// Set the active PTY channel for output routing.
    ///
    /// Caches the sender handle for fast output delivery.
    ///
    /// # Arguments
    ///
    /// * `agent_id` - Agent session key.
    /// * `pty_index` - PTY index (0=CLI, 1=Server).
    ///
    /// # Returns
    ///
    /// `true` if the channel exists and was activated, `false` otherwise.
    pub fn set_active_pty_channel(&mut self, agent_id: &str, pty_index: usize) -> bool {
        let key = format!("{agent_id}:{pty_index}");

        // Already active?
        if self.active_pty_key.as_deref() == Some(&key) {
            return true;
        }

        // Get sender handle from channel
        if let Some(channel) = self.pty_channels.get(&key) {
            if let Some(sender) = channel.get_sender_handle() {
                self.active_pty_sender = Some(sender);
                self.active_pty_key = Some(key);
                return true;
            }
        }

        false
    }

    /// Clear the active PTY channel.
    pub fn clear_active_pty_channel(&mut self) {
        self.active_pty_sender = None;
        self.active_pty_key = None;
    }

    /// Disconnect all channels (hub and PTY).
    ///
    /// Called on client disconnect to clean up resources.
    pub async fn disconnect_all_channels(&mut self) {
        // Disconnect hub channel
        if let Some(ref mut channel) = self.hub_channel {
            channel.disconnect().await;
        }
        self.hub_channel = None;

        // Disconnect all PTY channels
        for (_, channel) in self.pty_channels.iter_mut() {
            channel.disconnect().await;
        }
        self.pty_channels.clear();

        // Clear cache
        self.active_pty_sender = None;
        self.active_pty_key = None;
    }

    /// Get the active PTY sender handle for direct sending.
    #[must_use]
    pub fn active_pty_sender(&self) -> Option<&ChannelSenderHandle> {
        self.active_pty_sender.as_ref()
    }

    /// Get a PTY channel by key.
    #[must_use]
    pub fn get_pty_channel(&self, agent_id: &str, pty_index: usize) -> Option<&ActionCableChannel> {
        let key = format!("{agent_id}:{pty_index}");
        self.pty_channels.get(&key)
    }

    /// Get a mutable PTY channel by key.
    #[must_use]
    pub fn get_pty_channel_mut(
        &mut self,
        agent_id: &str,
        pty_index: usize,
    ) -> Option<&mut ActionCableChannel> {
        let key = format!("{agent_id}:{pty_index}");
        self.pty_channels.get_mut(&key)
    }

    /// Clean up PTY channels for a deleted agent.
    ///
    /// Called by Hub when an agent is deleted, providing the agent_id
    /// that was removed. This is separate from `on_agent_deleted` because
    /// that method only receives an index (which may already be invalid
    /// if the agent was removed from state before the callback).
    pub fn cleanup_agent_channels(&mut self, agent_id: &str) {
        // Clear cache if active PTY belongs to deleted agent
        if let Some(ref key) = self.active_pty_key {
            if key.starts_with(&format!("{agent_id}:")) {
                self.active_pty_sender = None;
                self.active_pty_key = None;
            }
        }

        // Clean up any PTY channels for this agent
        let keys_to_remove: Vec<String> = self
            .pty_channels
            .keys()
            .filter(|k| k.starts_with(&format!("{agent_id}:")))
            .cloned()
            .collect();

        for key in keys_to_remove {
            self.pty_channels.remove(&key);
        }
    }
}

/// Client trait implementation for BrowserClient.
///
/// Event-driven interface for Hub to push data to clients.
impl Client for BrowserClient {
    // ============================================================
    // Identity
    // ============================================================

    fn id(&self) -> &ClientId {
        &self.id
    }

    fn dims(&self) -> (u16, u16) {
        self.dims
    }

    // ============================================================
    // Data Access
    // ============================================================

    fn get_agents(&self) -> Vec<AgentInfo> {
        self.hub_handle.get_agents()
    }

    fn get_agent(&self, index: usize) -> Option<AgentHandle> {
        self.hub_handle.get_agent(index)
    }

    // ============================================================
    // Hub Commands (fire-and-forget via channel)
    // ============================================================

    fn request_create_agent(&self, request: CreateAgentRequest) -> Result<(), String> {
        use crate::hub::CreateAgentRequest as HubCreateRequest;
        let hub_request = HubCreateRequest::new(&request.issue_or_branch);
        self.hub_handle.create_agent(hub_request)
    }

    fn request_delete_agent(&self, agent_id: &str) -> Result<(), String> {
        self.hub_handle.delete_agent(agent_id)
    }

    // ============================================================
    // Event Handlers (Hub/PTY push to Client)
    // ============================================================

    fn on_output(&mut self, data: &[u8]) {
        // Send output via active PTY channel
        if let Some(ref sender) = self.active_pty_sender {
            let msg = TerminalMessage::Output {
                data: String::from_utf8_lossy(data).to_string(),
            };
            if let Ok(json) = serde_json::to_vec(&msg) {
                let sender = sender.clone();
                tokio::spawn(async move {
                    if let Err(e) = sender.send(&json).await {
                        log::warn!("Failed to send output to browser: {e}");
                    }
                });
            }
        }
    }

    fn on_resized(&mut self, rows: u16, cols: u16) {
        // Update stored dims (cols, rows) format.
        // The browser sends resize events to us, and we store them so the hub
        // can use these dims when spawning/selecting agents for this client.
        self.dims = (cols, rows);
    }

    fn on_process_exit(&mut self, _exit_code: Option<i32>) {
        // Process exit is sent via hub channel as part of agent status update.
        // No special handling needed here.
    }

    fn on_agent_created(&mut self, _index: usize, info: &AgentInfo) {
        // Send via hub channel
        if let Some(ref channel) = self.hub_channel {
            if let Some(sender) = channel.get_sender_handle() {
                let msg = TerminalMessage::AgentCreated {
                    id: info.id.clone(),
                };
                if let Ok(json) = serde_json::to_vec(&msg) {
                    let sender = sender.clone();
                    tokio::spawn(async move {
                        if let Err(e) = sender.send(&json).await {
                            log::warn!("Failed to send agent_created to browser: {e}");
                        }
                    });
                }
            }
        }
    }

    fn on_agent_deleted(&mut self, index: usize) {
        // Send via hub channel with index (Hub caller knows the agent_id and can
        // include it in a separate message if needed)
        if let Some(ref channel) = self.hub_channel {
            if let Some(sender) = channel.get_sender_handle() {
                let msg = TerminalMessage::AgentDeleted {
                    id: format!("index:{index}"),
                };
                if let Ok(json) = serde_json::to_vec(&msg) {
                    let sender = sender.clone();
                    tokio::spawn(async move {
                        if let Err(e) = sender.send(&json).await {
                            log::warn!("Failed to send agent_deleted to browser: {e}");
                        }
                    });
                }
            }
        }

        // Note: PTY channel cleanup requires knowing the agent_id. The Hub should
        // call cleanup_agent_channels() directly when deleting an agent, passing
        // the agent_id explicitly.
    }

    fn on_hub_shutdown(&mut self) {
        // Notify browser via hub channel
        if let Some(ref channel) = self.hub_channel {
            if let Some(sender) = channel.get_sender_handle() {
                let msg = TerminalMessage::Error {
                    message: "Hub is shutting down".to_string(),
                };
                if let Ok(json) = serde_json::to_vec(&msg) {
                    let sender = sender.clone();
                    tokio::spawn(async move {
                        let _ = sender.send(&json).await;
                    });
                }
            }
        }
    }

    // ============================================================
    // Connection State
    // ============================================================

    fn is_connected(&self) -> bool {
        self.connection == ConnectionState::Connected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== Unit Tests (No Async) ==========

    #[test]
    fn test_browser_client_creation() {
        let client = BrowserClient::new(HubHandle::mock(), "test-identity-12345678".to_string());
        assert!(client.id().is_browser());
        assert!(client.is_connected());
        assert_eq!(client.dims(), (80, 24)); // Default size
                                             // BrowserClient starts as non-owner (verified via Debug)
        let debug = format!("{:?}", client);
        assert!(debug.contains("is_size_owner: false"));
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
    fn test_browser_client_disconnected() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test-identity".to_string());

        client.set_disconnected();
        assert!(!client.is_connected());
        assert_eq!(client.connection_state(), ConnectionState::Disconnected);
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
    fn test_browser_client_on_owner_changed() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // BrowserClient starts as non-owner
        // on_owner_changed updates internal state (tested via Debug output)
        client.on_owner_changed(true);
        let debug = format!("{:?}", client);
        assert!(
            debug.contains("is_size_owner: true"),
            "Expected is_size_owner: true in {:?}",
            debug
        );

        client.on_owner_changed(false);
        let debug = format!("{:?}", client);
        assert!(
            debug.contains("is_size_owner: false"),
            "Expected is_size_owner: false in {:?}",
            debug
        );
    }

    #[test]
    fn test_browser_client_active_pty_cache() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // No channels yet
        assert!(!client.set_active_pty_channel("agent-123", 0));
        assert!(client.active_pty_sender().is_none());

        // Clear should be safe even with no active channel
        client.clear_active_pty_channel();
        assert!(client.active_pty_sender().is_none());
    }

    #[test]
    fn test_browser_client_pty_channel_disconnect_clears_cache() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Set up fake active key (simulating having had an active channel)
        client.active_pty_key = Some("agent-123:0".to_string());

        // Disconnect the channel
        client.disconnect_pty_channel("agent-123", 0);

        // Cache should be cleared
        assert!(client.active_pty_key.is_none());
    }

    #[test]
    fn test_browser_client_agent_deleted_does_not_panic() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Simulate having PTY channels for an agent
        client.active_pty_key = Some("agent-123:0".to_string());

        // Delete by index - should not panic
        // Note: on_agent_deleted doesn't cleanup channels directly;
        // use cleanup_agent_channels() for that
        client.on_agent_deleted(0);

        // Cache remains because on_agent_deleted doesn't do channel cleanup
        assert_eq!(client.active_pty_key.as_deref(), Some("agent-123:0"));
    }

    #[test]
    fn test_browser_client_cleanup_agent_channels() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Set up channels
        let channel = ActionCableChannel::builder()
            .server_url("https://test.example.com")
            .api_key("test-key")
            .build();
        client.connect_pty_channel("agent-123", 0, channel);
        client.active_pty_key = Some("agent-123:0".to_string());

        // Use cleanup_agent_channels directly with agent_id
        client.cleanup_agent_channels("agent-123");

        // Cache and channels should be cleared
        assert!(client.active_pty_key.is_none());
        assert!(client.get_pty_channel("agent-123", 0).is_none());
    }

    #[test]
    fn test_browser_client_get_agents_returns_empty_without_hub_handle() {
        let client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Without hub_handle, returns empty
        let agents = client.get_agents();
        assert!(agents.is_empty());
    }

    #[test]
    fn test_browser_client_get_agent_returns_none_without_hub_handle() {
        let client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Without hub_handle, returns None
        assert!(client.get_agent(0).is_none());
        assert!(client.get_agent(99).is_none());
    }

    #[test]
    fn test_browser_client_request_create_agent_with_mock_handle() {
        let client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Mock hub_handle has closed channel, so create_agent fails
        let result = client.request_create_agent(CreateAgentRequest::new("42"));
        assert!(result.is_err());
    }

    #[test]
    fn test_browser_client_request_delete_agent_with_mock_handle() {
        let client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Mock hub_handle has closed channel, so delete_agent fails
        let result = client.request_delete_agent("agent-123");
        assert!(result.is_err());
    }

    // ========== PTY Channel Lifecycle Tests ==========

    #[test]
    fn test_connect_pty_channel_creates_entry() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Create a mock channel (without connection - just for structure testing)
        let channel = ActionCableChannel::builder()
            .server_url("https://test.example.com")
            .api_key("test-key")
            .build();

        // Connect PTY channel for CLI (index 0)
        client.connect_pty_channel("agent-123", 0, channel);

        // Channel should be stored
        assert!(client.get_pty_channel("agent-123", 0).is_some());
        assert!(client.get_pty_channel("agent-123", 1).is_none()); // Server PTY not connected
    }

    #[test]
    fn test_connect_multiple_pty_channels_same_agent() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Connect CLI PTY (index 0)
        let cli_channel = ActionCableChannel::builder()
            .server_url("https://test.example.com")
            .api_key("test-key")
            .build();
        client.connect_pty_channel("agent-123", 0, cli_channel);

        // Connect Server PTY (index 1)
        let server_channel = ActionCableChannel::builder()
            .server_url("https://test.example.com")
            .api_key("test-key")
            .build();
        client.connect_pty_channel("agent-123", 1, server_channel);

        // Both channels should exist
        assert!(client.get_pty_channel("agent-123", 0).is_some());
        assert!(client.get_pty_channel("agent-123", 1).is_some());
    }

    #[test]
    fn test_disconnect_pty_channel_removes_entry() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        let channel = ActionCableChannel::builder()
            .server_url("https://test.example.com")
            .api_key("test-key")
            .build();
        client.connect_pty_channel("agent-123", 0, channel);

        // Verify channel exists
        assert!(client.get_pty_channel("agent-123", 0).is_some());

        // Disconnect
        client.disconnect_pty_channel("agent-123", 0);

        // Channel should be removed
        assert!(client.get_pty_channel("agent-123", 0).is_none());
    }

    #[test]
    fn test_disconnect_pty_channel_only_clears_cache_if_active() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Set up channels for two agents
        let channel_a = ActionCableChannel::builder()
            .server_url("https://test.example.com")
            .api_key("test-key")
            .build();
        let channel_b = ActionCableChannel::builder()
            .server_url("https://test.example.com")
            .api_key("test-key")
            .build();

        client.connect_pty_channel("agent-a", 0, channel_a);
        client.connect_pty_channel("agent-b", 0, channel_b);

        // Simulate agent-a being active
        client.active_pty_key = Some("agent-a:0".to_string());

        // Disconnect agent-b (not active)
        client.disconnect_pty_channel("agent-b", 0);

        // Cache for agent-a should still be set
        assert_eq!(client.active_pty_key.as_deref(), Some("agent-a:0"));
    }

    // ========== Channel Switching Tests ==========

    #[test]
    fn test_set_active_pty_channel_returns_false_when_channel_missing() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // No channels connected
        let result = client.set_active_pty_channel("agent-123", 0);
        assert!(!result);
        assert!(client.active_pty_key.is_none());
    }

    #[test]
    fn test_set_active_pty_channel_returns_false_when_no_sender() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Channel exists but has no sender (not connected)
        let channel = ActionCableChannel::builder()
            .server_url("https://test.example.com")
            .api_key("test-key")
            .build();
        client.connect_pty_channel("agent-123", 0, channel);

        // Should fail because channel has no sender_handle (not connected)
        let result = client.set_active_pty_channel("agent-123", 0);
        assert!(!result);
    }

    #[test]
    fn test_set_active_pty_channel_is_idempotent() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Manually set active key to simulate previous activation
        client.active_pty_key = Some("agent-123:0".to_string());

        // Calling again with same key should return true immediately
        let result = client.set_active_pty_channel("agent-123", 0);
        assert!(result);
        assert_eq!(client.active_pty_key.as_deref(), Some("agent-123:0"));
    }

    #[test]
    fn test_channel_key_format() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        let channel = ActionCableChannel::builder()
            .server_url("https://test.example.com")
            .api_key("test-key")
            .build();

        // Connect with specific agent key and PTY index
        client.connect_pty_channel("my-agent-key", 1, channel);

        // Key should be "agent_id:pty_index"
        assert!(client.pty_channels.contains_key("my-agent-key:1"));
    }

    // ========== Agent Deletion Tests ==========
    //
    // Note: on_agent_deleted takes an index and doesn't do channel cleanup.
    // Use cleanup_agent_channels() for channel cleanup with agent_id.

    #[test]
    fn test_agent_deleted_does_not_cleanup_channels() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Connect a PTY channel
        let cli_channel = ActionCableChannel::builder()
            .server_url("https://test.example.com")
            .api_key("test-key")
            .build();
        client.connect_pty_channel("agent-to-delete", 0, cli_channel);

        // Delete by index - channels remain (Hub must call cleanup_agent_channels)
        client.on_agent_deleted(0);

        // Channels remain because on_agent_deleted doesn't do cleanup
        assert!(client.get_pty_channel("agent-to-delete", 0).is_some());
    }

    #[test]
    fn test_cleanup_agent_channels_clears_cache() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Set up channels
        let channel_a = ActionCableChannel::builder()
            .server_url("https://test.example.com")
            .api_key("test-key")
            .build();

        client.connect_pty_channel("agent-a", 0, channel_a);
        client.active_pty_key = Some("agent-a:0".to_string());

        // Use cleanup_agent_channels to clear cache
        client.cleanup_agent_channels("agent-a");

        // Cache should be cleared
        assert!(client.active_pty_key.is_none());
    }

    #[test]
    fn test_cleanup_agent_channels_only_affects_target_agent() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Set up channels for two agents
        let channel_a = ActionCableChannel::builder()
            .server_url("https://test.example.com")
            .api_key("test-key")
            .build();
        let channel_b = ActionCableChannel::builder()
            .server_url("https://test.example.com")
            .api_key("test-key")
            .build();

        client.connect_pty_channel("agent-a", 0, channel_a);
        client.connect_pty_channel("agent-b", 0, channel_b);
        client.active_pty_key = Some("agent-b:0".to_string());

        // Clean up agent-a
        client.cleanup_agent_channels("agent-a");

        // agent-a channel gone, agent-b remains, cache for agent-b unchanged
        assert!(client.get_pty_channel("agent-a", 0).is_none());
        assert!(client.get_pty_channel("agent-b", 0).is_some());
        assert_eq!(client.active_pty_key.as_deref(), Some("agent-b:0"));
    }

    #[test]
    fn test_agent_deleted_multiple_indices_safe() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Multiple deletions should not panic
        client.on_agent_deleted(0);
        client.on_agent_deleted(1);
        client.on_agent_deleted(99);
    }

    // ========== Hub Channel Tests ==========

    #[test]
    fn test_hub_channel_lifecycle() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Initially no hub channel
        assert!(client.hub_channel().is_none());

        // Set hub channel
        let hub_channel = ActionCableChannel::builder()
            .server_url("https://test.example.com")
            .api_key("test-key")
            .build();
        client.set_hub_channel(hub_channel);

        // Hub channel should be set
        assert!(client.hub_channel().is_some());

        // Mutable access
        assert!(client.hub_channel_mut().is_some());
    }

    // ========== Output Routing Logic Tests ==========

    #[test]
    fn test_on_output_does_nothing_without_active_sender() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // No active channel/sender
        assert!(client.active_pty_sender().is_none());

        // on_output should not panic, just silently do nothing
        client.on_output(b"test output");

        // Still no sender (nothing changed)
        assert!(client.active_pty_sender().is_none());
    }

    #[test]
    fn test_on_resized_updates_dims() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());
        let initial_dims = client.dims();
        assert_eq!(initial_dims, (80, 24)); // Default dims (cols, rows)

        // on_resized should update client dims (rows, cols) -> stored as (cols, rows)
        client.on_resized(200, 50);

        // Dims updated to (cols, rows) format
        assert_eq!(client.dims(), (50, 200));
    }

    #[test]
    fn test_on_process_exit_is_noop() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Should not panic, process exit is handled via hub channel status updates
        client.on_process_exit(Some(0));
        client.on_process_exit(Some(1));
        client.on_process_exit(Some(-1));
        client.on_process_exit(None);
    }

    // ========== Edge Case Tests ==========

    #[test]
    fn test_disconnect_nonexistent_channel_is_safe() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Should not panic
        client.disconnect_pty_channel("nonexistent-agent", 0);
        client.disconnect_pty_channel("agent", 99);
    }

    #[test]
    fn test_delete_nonexistent_agent_is_safe() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Should not panic (by index, no hub_state)
        client.on_agent_deleted(999);
    }

    #[test]
    fn test_clear_active_pty_channel_is_idempotent() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // Multiple clears should be safe
        client.clear_active_pty_channel();
        client.clear_active_pty_channel();
        client.clear_active_pty_channel();

        assert!(client.active_pty_key.is_none());
        assert!(client.active_pty_sender.is_none());
    }

    #[test]
    fn test_agent_id_prefix_matching_logic() {
        // Test the prefix matching logic used in cleanup
        // Without hub_state, on_agent_deleted can't do cleanup, so we test the logic directly

        let agent_id = "agent";
        let key1 = "agent:0";
        let key2 = "agent-extended:0";

        // The format!("{agent_id}:") pattern should match key1 but not key2
        assert!(key1.starts_with(&format!("{agent_id}:")));
        assert!(!key2.starts_with(&format!("{agent_id}:")));
    }

    // ========== on_agent_created Tests ==========

    #[test]
    fn test_on_agent_created_without_hub_channel() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // No hub channel set
        assert!(client.hub_channel().is_none());

        // Should not panic (takes index and info now)
        let info = AgentInfo {
            id: "new-agent".to_string(),
            repo: None,
            issue_number: None,
            branch_name: None,
            name: None,
            status: None,
            tunnel_port: None,
            server_running: None,
            has_server_pty: None,
            active_pty_view: None,
            scroll_offset: None,
            hub_identifier: None,
        };
        client.on_agent_created(0, &info);
    }

    // ========== on_hub_shutdown Tests ==========

    #[test]
    fn test_on_hub_shutdown_without_hub_channel() {
        let mut client = BrowserClient::new(HubHandle::mock(), "test".to_string());

        // No hub channel set
        assert!(client.hub_channel().is_none());

        // Should not panic
        client.on_hub_shutdown();
    }
}
