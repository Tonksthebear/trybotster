//! Browser client implementation.
//!
//! Browser clients represent remote connections from web browsers.
//! Like TUI, browser clients track per-client state (selected agent, dims).
//!
//! # Output Routing
//!
//! Terminal output goes directly through agent-owned channels, not this client.
//! Hub-level messages (agent lists, selections) go through HubRelay.
//!
//! BrowserClient tracks per-client state for:
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

/// Browser client - tracks per-browser state.
///
/// Terminal output goes through agent-owned channels.
/// This client tracks state for routing and viewer management.
#[derive(Debug)]
pub struct BrowserClient {
    id: ClientId,
    state: ClientState,

    /// Signal identity key (used for encryption routing).
    identity: String,

    /// Connection tracking.
    connection: ConnectionState,
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

    fn receive_output(&mut self, _data: &[u8]) {
        // No-op: Terminal output goes through agent-owned channels
    }

    fn receive_scrollback(&mut self, _lines: Vec<String>) {
        // No-op: Scrollback sent via agent channel
    }

    fn receive_agent_list(&mut self, _agents: Vec<AgentInfo>) {
        // No-op: Agent list sent via HubRelay
    }

    fn receive_worktree_list(&mut self, _worktrees: Vec<WorktreeInfo>) {
        // No-op: Worktree list sent via HubRelay
    }

    fn receive_response(&mut self, _response: Response) {
        // No-op: Responses sent via HubRelay
    }

    fn is_connected(&self) -> bool {
        self.connection == ConnectionState::Connected
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

    #[test]
    fn test_browser_client_identity_tracking() {
        let client = BrowserClient::new("my-unique-identity-key".to_string());

        // BrowserClient must track its identity for routing
        assert_eq!(client.identity(), "my-unique-identity-key");

        // The identity is used for per-agent channel routing
        match client.id() {
            ClientId::Browser(ref id) => assert_eq!(id, "my-unique-identity-key"),
            _ => panic!("Should be a Browser client"),
        }
    }
}
