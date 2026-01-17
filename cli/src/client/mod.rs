//! Client abstraction for TUI and browser connections.
//!
//! This module provides a unified interface for all client types (TUI, Browser).
//! Each client owns its view state (which agent it's viewing, terminal dimensions)
//! while the Hub owns all data (agents, worktrees, PTY output).
//!
//! # Key Principles
//!
//! - Hub owns all data (agents, worktrees, PTY output)
//! - Clients own their view state (selected agent, terminal dims)
//! - Client trait defines the interface, implementations handle transport
//! - TUI is just another client (no special-casing in Hub logic)
//!
//! # Architecture
//!
//! ```text
//! Hub (owns data)
//!   ├── ClientRegistry
//!   │     ├── TuiClient (renders via ratatui)
//!   │     ├── BrowserClient "abc123" (sends via WebSocket)
//!   │     └── BrowserClient "def456" (sends via WebSocket)
//!   └── viewers: HashMap<agent_key, Set<ClientId>>  (reverse index for O(1) routing)
//! ```

mod types;
mod registry;
mod tui;
mod browser;

pub use types::{Response, CreateAgentRequest, DeleteAgentRequest};
pub use registry::ClientRegistry;
pub use tui::TuiClient;
pub use browser::BrowserClient;

// Re-use existing types from relay
pub use crate::relay::{AgentInfo, WorktreeInfo};

/// Unique identifier for a client session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ClientId {
    /// The local TUI client.
    Tui,
    /// A browser client, identified by Signal identity key.
    Browser(String),
}

impl ClientId {
    /// Create a browser client ID from a Signal identity key.
    pub fn browser(identity: impl Into<String>) -> Self {
        ClientId::Browser(identity.into())
    }

    /// Check if this is the TUI client.
    pub fn is_tui(&self) -> bool {
        matches!(self, ClientId::Tui)
    }

    /// Check if this is a browser client.
    pub fn is_browser(&self) -> bool {
        matches!(self, ClientId::Browser(_))
    }

    /// Get the browser identity if this is a browser client.
    pub fn browser_identity(&self) -> Option<&str> {
        match self {
            ClientId::Browser(id) => Some(id),
            ClientId::Tui => None,
        }
    }
}

impl std::fmt::Display for ClientId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientId::Tui => write!(f, "tui"),
            ClientId::Browser(id) => write!(f, "browser:{}", &id[..8.min(id.len())]),
        }
    }
}

/// Per-client view state.
///
/// Each client maintains its own view state independent of other clients.
/// This enables TUI and each browser to view different agents simultaneously.
#[derive(Debug, Clone, Default)]
pub struct ClientState {
    /// Currently selected agent (session key).
    /// None if no agent is selected.
    pub selected_agent: Option<String>,

    /// Terminal dimensions (cols, rows).
    /// Used for PTY resizing when client selects an agent.
    pub dims: Option<(u16, u16)>,

    // Note: active_pty is NOT stored - always default to Cli on agent selection
    // Note: scroll_offset is NOT stored - handled locally by xterm.js / vt100_parser
}

/// The Client trait - interface for all client types.
///
/// Clients receive data pushed from the Hub (output, agent lists, etc.)
/// and maintain their own view state (which agent they're viewing).
pub trait Client: Send {
    /// Unique identifier for this client.
    fn id(&self) -> &ClientId;

    /// Access view state (immutable).
    fn state(&self) -> &ClientState;

    /// Access view state (mutable).
    fn state_mut(&mut self) -> &mut ClientState;

    // === Receive: Hub pushes data to client ===

    /// Terminal output from PTY (raw bytes).
    ///
    /// No-op for both TUI and Browser clients:
    /// - TUI reads directly from vt100_parser during render
    /// - Browser clients receive output via agent-owned channels
    fn receive_output(&mut self, data: &[u8]);

    /// Scrollback history (sent on agent selection).
    ///
    /// When a client selects an agent, Hub sends the scrollback history
    /// so the client can populate its terminal buffer.
    fn receive_scrollback(&mut self, lines: Vec<String>);

    /// Full agent list (sent on change or request).
    ///
    /// Hub sends the complete agent list; clients replace their local copy.
    /// No delta logic - simpler and more reliable.
    fn receive_agent_list(&mut self, agents: Vec<AgentInfo>);

    /// Available worktrees (sent on request).
    fn receive_worktree_list(&mut self, worktrees: Vec<WorktreeInfo>);

    /// Response to client action (confirmation or error).
    fn receive_response(&mut self, response: Response);

    // === State mutations: Hub calls these after processing actions ===

    /// Update selection (called by Hub after validation).
    fn select_agent(&mut self, agent_key: &str) {
        self.state_mut().selected_agent = Some(agent_key.to_string());
    }

    /// Clear selection (called when agent is deleted).
    fn clear_selection(&mut self) {
        self.state_mut().selected_agent = None;
    }

    /// Update dimensions.
    fn resize(&mut self, cols: u16, rows: u16) {
        self.state_mut().dims = Some((cols, rows));
    }

    // === Lifecycle ===

    /// Called periodically to flush buffered output (for batching).
    ///
    /// Browser clients buffer output at ~60fps to prevent WebSocket flooding.
    /// TUI client is a no-op.
    fn flush(&mut self) {}

    /// Check if client connection is healthy.
    ///
    /// Always true for TUI. Browser clients track connection state.
    fn is_connected(&self) -> bool {
        true
    }

    /// Drain buffered output (legacy - always returns None).
    ///
    /// Terminal output now goes through agent-owned channels.
    /// This method exists for trait compatibility but is unused.
    fn drain_buffered_output(&mut self) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_id_display() {
        assert_eq!(format!("{}", ClientId::Tui), "tui");
        assert_eq!(
            format!("{}", ClientId::Browser("abcd1234efgh5678".to_string())),
            "browser:abcd1234"
        );
        // Short identity
        assert_eq!(
            format!("{}", ClientId::Browser("abc".to_string())),
            "browser:abc"
        );
    }

    #[test]
    fn test_client_id_equality() {
        assert_eq!(ClientId::Tui, ClientId::Tui);
        assert_eq!(
            ClientId::Browser("abc".to_string()),
            ClientId::Browser("abc".to_string())
        );
        assert_ne!(ClientId::Tui, ClientId::Browser("abc".to_string()));
    }

    #[test]
    fn test_client_id_browser_constructor() {
        let id = ClientId::browser("test-identity");
        assert!(id.is_browser());
        assert!(!id.is_tui());
    }

    #[test]
    fn test_client_state_default() {
        let state = ClientState::default();
        assert!(state.selected_agent.is_none());
        assert!(state.dims.is_none());
    }
}
