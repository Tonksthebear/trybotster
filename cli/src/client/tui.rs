//! TUI client implementation.
//!
//! The TUI client represents the local terminal interface. Unlike browser clients
//! which need data pushed over WebSocket, the TUI reads directly from Hub state
//! during its render cycle. Therefore, most receive methods are no-ops.
//!
//! The TUI client still maintains view state (selected agent, dimensions) which
//! the Hub uses to determine which agent's PTY to resize and where to route input.

use super::{AgentInfo, Client, ClientId, ClientState, Response, WorktreeInfo};

/// TUI client - the local terminal interface.
///
/// TUI reads Hub state directly during render (via ratatui), so it doesn't
/// need output pushed to it. The receive methods are no-ops.
#[derive(Debug)]
pub struct TuiClient {
    id: ClientId,
    state: ClientState,
}

impl TuiClient {
    /// Create a new TUI client.
    pub fn new() -> Self {
        Self {
            id: ClientId::Tui,
            state: ClientState::default(),
        }
    }
}

impl Default for TuiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl Client for TuiClient {
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
        // No-op: TUI reads from agent.cli_pty.vt100_parser during render cycle
    }

    fn receive_scrollback(&mut self, _lines: Vec<String>) {
        // No-op: TUI reads scrollback from agent's buffer directly
    }

    fn receive_agent_list(&mut self, _agents: Vec<AgentInfo>) {
        // No-op: TUI iterates hub.state.agents during render
    }

    fn receive_worktree_list(&mut self, _worktrees: Vec<WorktreeInfo>) {
        // No-op: TUI reads hub.state.available_worktrees during render
    }

    fn receive_response(&mut self, _response: Response) {
        // Could show toast/notification, but TUI re-renders constantly anyway.
        // Response is visible through state changes (new agent in list, etc.)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tui_client_default_state() {
        let client = TuiClient::new();
        assert_eq!(client.id(), &ClientId::Tui);
        assert!(client.state().selected_agent.is_none());
        assert!(client.state().dims.is_none());
    }

    #[test]
    fn test_tui_client_select_agent() {
        let mut client = TuiClient::new();
        client.select_agent("agent-123");
        assert_eq!(
            client.state().selected_agent,
            Some("agent-123".to_string())
        );
    }

    #[test]
    fn test_tui_client_clear_selection() {
        let mut client = TuiClient::new();
        client.select_agent("agent-123");
        client.clear_selection();
        assert!(client.state().selected_agent.is_none());
    }

    #[test]
    fn test_tui_client_resize() {
        let mut client = TuiClient::new();
        client.resize(120, 40);
        assert_eq!(client.state().dims, Some((120, 40)));
    }

    #[test]
    fn test_tui_client_is_connected() {
        let client = TuiClient::new();
        // TUI is always connected
        assert!(client.is_connected());
    }
}
