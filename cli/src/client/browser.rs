//! Browser client implementation.
//!
//! Browser clients represent remote connections from web browsers.
//! Like TUI, browser clients track per-client state (selected agent, dims).
//!
//! # Output Routing
//!
//! Output to browsers flows through `hub.browser.sender` (the relay's encrypted
//! channel), NOT through this client directly. This is because:
//! 1. The relay handles Signal Protocol encryption per-browser
//! 2. Multiple browsers share the same relay sender
//!
//! The `receive_*` methods are no-ops since actual output goes via relay.
//! BrowserClient exists to track per-client state for:
//! - Independent agent selection per browser
//! - Per-client terminal dimensions
//! - Viewer tracking (who's viewing which agent)

use super::{AgentInfo, Client, ClientId, ClientState, Response, WorktreeInfo};

/// Browser connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Connected and ready.
    Connected,
    /// Disconnected.
    Disconnected,
}

/// Browser client - tracks per-browser state and buffers output.
///
/// Output is buffered in `output_buffer` and drained by the Hub event loop.
/// The Hub then sends buffered output via relay to this specific browser.
#[derive(Debug)]
pub struct BrowserClient {
    id: ClientId,
    state: ClientState,

    /// Signal identity key (used for encryption routing).
    identity: String,

    /// Connection tracking.
    connection: ConnectionState,

    /// Output buffer - accumulated PTY output waiting to be sent.
    /// Drained by Hub.drain_browser_outputs() and sent via relay.
    output_buffer: Vec<u8>,
}

impl BrowserClient {
    /// Create a new browser client.
    ///
    /// # Arguments
    ///
    /// * `identity` - Signal identity key (from browser handshake)
    pub fn new(identity: String) -> Self {
        Self {
            id: ClientId::Browser(identity.clone()),
            state: ClientState::default(),
            identity,
            connection: ConnectionState::Connected,
            output_buffer: Vec::new(),
        }
    }

    /// Drain buffered output for sending via relay.
    ///
    /// Returns `Some(data)` if there's buffered output, `None` if empty.
    /// The Hub calls this method and sends the data via relay to this browser.
    pub fn drain_output(&mut self) -> Option<Vec<u8>> {
        if self.output_buffer.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.output_buffer))
        }
    }

    /// Get the Signal identity key.
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Get connection state.
    pub fn connection_state(&self) -> ConnectionState {
        self.connection
    }

    /// Mark as disconnected.
    pub fn set_disconnected(&mut self) {
        self.connection = ConnectionState::Disconnected;
    }
}

impl Client for BrowserClient {
    fn id(&self) -> &ClientId {
        &self.id
    }

    fn state(&self) -> &ClientState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut ClientState {
        &mut self.state
    }

    fn receive_output(&mut self, data: &[u8]) {
        // Buffer output data for later draining.
        // Hub.drain_browser_outputs() will send this via relay to this browser.
        self.output_buffer.extend_from_slice(data);
    }

    fn receive_scrollback(&mut self, _lines: Vec<String>) {
        // No-op: Scrollback sent via relay in browser.rs::send_scrollback_for_selected_agent()
    }

    fn receive_agent_list(&mut self, _agents: Vec<AgentInfo>) {
        // No-op: Agent list sent via relay in browser.rs::send_agent_list()
    }

    fn receive_worktree_list(&mut self, _worktrees: Vec<WorktreeInfo>) {
        // No-op: Worktree list sent via relay in browser.rs::send_worktree_list()
    }

    fn receive_response(&mut self, _response: Response) {
        // No-op: Responses sent via relay
    }

    fn is_connected(&self) -> bool {
        self.connection == ConnectionState::Connected
    }

    fn drain_buffered_output(&mut self) -> Option<Vec<u8>> {
        self.drain_output()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_browser_client_creation() {
        let client = BrowserClient::new("test-identity-12345678".to_string());
        assert!(client.id().is_browser());
        assert!(client.state().selected_agent.is_none());
        assert!(client.is_connected());
    }

    #[test]
    fn test_browser_client_select_agent() {
        let mut client = BrowserClient::new("test-identity".to_string());
        client.select_agent("agent-123");
        assert_eq!(
            client.state().selected_agent,
            Some("agent-123".to_string())
        );
    }

    #[test]
    fn test_browser_client_disconnected() {
        let mut client = BrowserClient::new("test-identity".to_string());

        client.set_disconnected();
        assert!(!client.is_connected());
        assert_eq!(client.connection_state(), ConnectionState::Disconnected);
    }

    #[test]
    fn test_browser_client_identity() {
        let client = BrowserClient::new("my-signal-key".to_string());
        assert_eq!(client.identity(), "my-signal-key");
    }

    #[test]
    fn test_browser_client_resize() {
        let mut client = BrowserClient::new("test".to_string());
        client.resize(120, 40);
        assert_eq!(client.state().dims, Some((120, 40)));
    }

    // === Phase 3: BrowserClient should buffer output for relay ===
    //
    // Instead of no-op, BrowserClient should buffer output data.
    // The buffer is drained by the Hub and sent via relay to the specific browser.

    #[test]
    fn test_browser_client_buffers_output() {
        let mut client = BrowserClient::new("test-identity".to_string());

        // Initially no buffered output
        assert!(client.drain_output().is_none(), "Should have no output initially");

        // receive_output should buffer the data
        client.receive_output(b"Hello, ");
        client.receive_output(b"world!");

        // drain_output should return all buffered data
        let output = client.drain_output();
        assert_eq!(output, Some(b"Hello, world!".to_vec()), "Should drain buffered output");

        // After draining, buffer should be empty
        assert!(client.drain_output().is_none(), "Should be empty after drain");
    }

    #[test]
    fn test_browser_client_output_identity_tracking() {
        let client = BrowserClient::new("my-unique-identity-key".to_string());

        // BrowserClient must track its identity for routing
        assert_eq!(client.identity(), "my-unique-identity-key");

        // The identity is used by Hub to route output to the correct browser
        match client.id() {
            ClientId::Browser(ref id) => assert_eq!(id, "my-unique-identity-key"),
            _ => panic!("Should be a Browser client"),
        }
    }
}
