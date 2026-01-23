//! Client abstraction for TUI and browser connections.
//!
//! This module provides a unified interface for all client types (TUI, Browser).
//! The `Client` trait is a clean API layer - both implementations have identical
//! interfaces with thread safety hidden behind handles.
//!
//! # Architecture
//!
//! ```text
//! Hub (owns state via Arc<RwLock<HubState>>)
//!   │
//!   ├── HubCommandSender (fire-and-forget via channel)
//!   │
//!   └── Clients
//!         ├── TuiClient (owns local view state: selection, scroll, vt100)
//!         │
//!         └── BrowserClient (thin IO pipe, no UI state)
//! ```
//!
//! # Data Access Pattern
//!
//! Clients read agent data via `get_agents()` / `get_agent(index)` which return
//! snapshots. For live PTY interaction, clients get handles:
//!
//! ```text
//! client.get_agent(0) → AgentHandle
//!                          ├── info() → AgentInfo (snapshot)
//!                          └── get_pty(0) → PtyHandle
//!                                              ├── subscribe() → events
//!                                              ├── connect()
//!                                              ├── send_input()
//!                                              └── resize()
//! ```
//!
//! # Hub Commands (fire-and-forget)
//!
//! Commands to mutate state go through channels. Results come back as events.
//!
//! ```text
//! client.request_create_agent(req) → Hub processes → HubEvent::AgentCreated
//! client.request_delete_agent(id)  → Hub processes → HubEvent::AgentDeleted
//! ```
//!
//! # Event Handlers (Hub/PTY push to Client)
//!
//! Hub and PTY broadcast events. Clients handle IO delivery:
//! - TUI: renders via ratatui
//! - Browser: encrypts and sends via ActionCable

// Rust guideline compliant 2026-01

mod browser;
mod registry;
mod tui;
mod types;

pub use browser::BrowserClient;
pub use registry::ClientRegistry;
pub use tui::TuiClient;
pub use types::{CreateAgentRequest, DeleteAgentRequest, Response};

pub use crate::hub::agent_handle::{AgentHandle, PtyCommand, PtyHandle};
pub use crate::relay::AgentInfo;

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

/// The Client trait - clean API layer for Hub/PTY interaction.
///
/// Both TUI and Browser implement this identically. Thread safety is hidden
/// behind handles. UI state (selection, scroll, vt100) is NOT on this trait -
/// each implementation manages its own.
///
/// # Design Principles
///
/// 1. **Index-based access** - Clients work with agent indices, not IDs
/// 2. **Handles for interaction** - `AgentHandle` and `PtyHandle` encapsulate channels
/// 3. **Fire-and-forget commands** - Hub commands return immediately, results via events
/// 4. **Push events** - Hub/PTY call `on_*` methods, client handles IO
/// 5. **No downcasts** - No `as_any`, `as_tui`, etc.
///
/// # What's NOT on this trait (UI state)
///
/// - Selected agent index (TUI manages locally)
/// - Scroll position (TUI manages locally)
/// - Active PTY view (TUI manages locally)
/// - vt100 parser (TUI owns)
pub trait Client: Send {
    // ============================================================
    // Identity
    // ============================================================

    /// Unique identifier for this client.
    fn id(&self) -> &ClientId;

    /// Terminal dimensions (cols, rows).
    ///
    /// Used when connecting to PTY to report initial size.
    fn dims(&self) -> (u16, u16);

    // ============================================================
    // Data Access (reads from Hub state)
    // ============================================================

    /// Get snapshot of all agents.
    ///
    /// Returns `AgentInfo` for all active agents in display order.
    /// This is a snapshot - changes won't be reflected until next call.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let agents = client.get_agents();
    /// for (i, info) in agents.iter().enumerate() {
    ///     println!("{}: {}", i, info.id);
    /// }
    /// ```
    fn get_agents(&self) -> Vec<AgentInfo>;

    /// Get handle for agent at index.
    ///
    /// Returns `AgentHandle` for the agent at the given index in display order,
    /// or `None` if index is out of bounds.
    ///
    /// The handle provides:
    /// - Agent metadata via `info()`
    /// - PTY access via `get_pty(pty_index)` where 0=CLI, 1=Server
    ///
    /// # Example
    ///
    /// ```ignore
    /// if let Some(handle) = client.get_agent(0) {
    ///     println!("Agent: {}", handle.info().id);
    ///
    ///     // Connect to CLI PTY
    ///     if let Some(pty) = handle.get_pty(0) {
    ///         pty.connect(client.id().clone(), client.dims()).await?;
    ///         let mut rx = pty.subscribe();
    ///         // ...
    ///     }
    /// }
    /// ```
    fn get_agent(&self, index: usize) -> Option<AgentHandle>;

    /// Get agent count.
    ///
    /// Convenience method, equivalent to `get_agents().len()` but potentially
    /// more efficient.
    fn agent_count(&self) -> usize {
        self.get_agents().len()
    }

    // ============================================================
    // Hub Commands (fire-and-forget via channel)
    // ============================================================

    /// Request to create an agent.
    ///
    /// This is fire-and-forget. The Hub processes the request asynchronously.
    /// Results are communicated via `on_agent_created` event (success) or
    /// `on_error` event (failure).
    ///
    /// # Arguments
    ///
    /// * `request` - Agent creation parameters
    ///
    /// # Returns
    ///
    /// `Ok(())` if command was sent, `Err` if channel closed.
    fn request_create_agent(&self, request: CreateAgentRequest) -> Result<(), String>;

    /// Request to delete an agent.
    ///
    /// This is fire-and-forget. The Hub processes the request asynchronously.
    /// Results are communicated via `on_agent_deleted` event.
    ///
    /// # Arguments
    ///
    /// * `agent_id` - Session key of the agent to delete
    ///
    /// # Returns
    ///
    /// `Ok(())` if command was sent, `Err` if channel closed.
    fn request_delete_agent(&self, agent_id: &str) -> Result<(), String>;

    // ============================================================
    // Event Handlers (Hub/PTY push to Client)
    // ============================================================

    /// Terminal output from PTY.
    ///
    /// Called when PTY broadcasts output to viewing clients.
    ///
    /// - TUI: Feeds to vt100 parser, schedules re-render
    /// - Browser: Encrypts and sends via PTY channel
    fn on_output(&mut self, data: &[u8]);

    /// PTY was resized.
    ///
    /// Called when PTY broadcasts resize event.
    ///
    /// - TUI: May update local state
    /// - Browser: Sends via PTY channel (xterm.js handles)
    fn on_resized(&mut self, rows: u16, cols: u16);

    /// PTY process exited.
    ///
    /// Called when process in PTY terminates.
    ///
    /// - TUI: Shows exit notification, may clear view
    /// - Browser: Sends status update via hub channel
    fn on_process_exit(&mut self, exit_code: Option<i32>);

    /// Agent was created.
    ///
    /// Called when Hub broadcasts agent creation event.
    ///
    /// - TUI: Updates agent list, may auto-select
    /// - Browser: Sends via hub channel
    fn on_agent_created(&mut self, index: usize, info: &AgentInfo);

    /// Agent was deleted.
    ///
    /// Called when Hub broadcasts agent deletion event.
    ///
    /// - TUI: Updates agent list, clears selection if needed
    /// - Browser: Sends via hub channel, cleans up PTY channels
    fn on_agent_deleted(&mut self, index: usize);

    /// Hub is shutting down.
    ///
    /// Called when Hub broadcasts shutdown event.
    ///
    /// - TUI: Shows shutdown message, exits gracefully
    /// - Browser: Sends shutdown notification via hub channel
    fn on_hub_shutdown(&mut self);

    // ============================================================
    // Connection State
    // ============================================================

    /// Check if client connection is healthy.
    ///
    /// - TUI: Always true (local)
    /// - Browser: Channel is open and session is valid
    fn is_connected(&self) -> bool;
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
    fn test_client_id_browser_identity() {
        let id = ClientId::browser("test-identity");
        assert_eq!(id.browser_identity(), Some("test-identity"));
        assert_eq!(ClientId::Tui.browser_identity(), None);
    }
}
