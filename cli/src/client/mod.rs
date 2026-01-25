//! Client abstraction for TUI and browser connections.
//!
//! This module provides a unified interface for all client types (TUI, Browser).
//! The `Client` trait uses **index-based routing** - all PTY operations take explicit
//! `(agent_index, pty_index)` parameters. "Current" PTY state belongs in the GUI layer
//! (TuiRunner for TUI, JavaScript for Browser), NOT in Client implementations.
//!
//! # Architecture
//!
//! ```text
//! Hub (owns state via Arc<RwLock<HubState>>)
//!   │
//!   └── HubHandle (thread-safe access to Hub operations)
//!         │
//!         └── Clients (store HubHandle, implement trait methods)
//!               ├── TuiClient
//!               │     └── hub_handle: HubHandle
//!               │
//!               └── BrowserClient
//!                     └── hub_handle: HubHandle
//! ```
//!
//! **Key insight**: Clients access Hub data through `hub_handle()`. PTY operations
//! like `send_input` and `resize_pty` look up agents/PTYs via `HubHandle` on each
//! call - no handles are stored in the client.
//!
//! # Index-Based Routing
//!
//! All PTY operations take explicit indices:
//!
//! ```text
//! client.connect_to_pty(agent_idx, pty_idx)     // Connect to specific PTY
//! client.disconnect_from_pty(agent_idx, pty_idx) // Disconnect from specific PTY
//! client.send_input(agent_idx, pty_idx, data)   // Send to specific PTY
//! client.resize_pty(agent_idx, pty_idx, r, c)   // Resize specific PTY
//! ```
//!
//! This design:
//! - Makes TUI and Browser implementations symmetric
//! - Allows multiple simultaneous PTY connections (Browser can have multiple tabs)
//! - Keeps "current" state in the GUI where it belongs
//!
//! # Data Access Pattern
//!
//! Clients read agent data via `hub_handle().get_agents()` / `hub_handle().get_agent(index)`.
//! Default implementations provide convenient wrappers:
//!
//! ```text
//! client.get_agents() → Vec<AgentInfo> (via hub_handle)
//! client.get_agent(0) → Option<AgentHandle> (via hub_handle)
//! client.send_input(0, 0, data) → looks up agent/PTY via hub_handle
//! client.resize_pty(0, 0, r, c) → looks up agent/PTY via hub_handle
//! ```

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
pub use crate::hub::HubHandle;
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
/// 1. **Index-based routing** - All PTY ops take `(agent_index, pty_index)` explicitly
/// 2. **GUI owns "current"** - Which PTY is active lives in TuiRunner/JavaScript
/// 3. **HubHandle for data** - All data access goes through `hub_handle()`
/// 4. **Default implementations** - `get_agents`, `get_agent`, `send_input`, `resize_pty`, `agent_count`
///    have default implementations using `hub_handle()`
/// 5. **No downcasts** - No `as_any`, `as_tui`, etc.
///
/// # Required Methods (clients must implement)
///
/// - `hub_handle()` - Access to Hub data and operations
/// - `id()` - Unique client identifier
/// - `dims()` - Terminal dimensions for PTY resize
/// - `connect_to_pty()` - Establish PTY connection
/// - `disconnect_from_pty()` - Terminate PTY connection
///
/// # Default Methods (using hub_handle)
///
/// - `get_agents()` - Get all agent info snapshots
/// - `get_agent()` - Get specific agent handle by index
/// - `send_input()` - Send input to PTY (looks up via hub_handle)
/// - `resize_pty()` - Resize PTY (looks up via hub_handle)
/// - `agent_count()` - Number of active agents
///
/// # What's NOT on this trait (GUI state)
///
/// - Current agent/PTY selection (TuiRunner/JavaScript manages)
/// - Scroll position (TuiRunner manages)
/// - vt100 parser state (TuiRunner owns)
pub trait Client: Send {
    // ============================================================
    // Required (clients must implement)
    // ============================================================

    /// Access to Hub data and operations.
    ///
    /// All data access and PTY operations go through this handle.
    /// The default implementations for `get_agents`, `get_agent`,
    /// `send_input`, and `resize_pty` use this.
    fn hub_handle(&self) -> &HubHandle;

    /// Unique identifier for this client.
    fn id(&self) -> &ClientId;

    /// Terminal dimensions (cols, rows).
    ///
    /// Used when connecting to PTY to report initial size.
    fn dims(&self) -> (u16, u16);

    /// Update terminal dimensions.
    ///
    /// Called when the client's terminal is resized. Each client implementation
    /// should update its internal stored dimensions.
    ///
    /// # Arguments
    ///
    /// * `cols` - New terminal width in columns
    /// * `rows` - New terminal height in rows
    fn set_dims(&mut self, cols: u16, rows: u16);

    /// Connect to an agent's PTY.
    ///
    /// Establishes a connection to the specified PTY. The client is responsible
    /// for subscribing to PTY events and handling output delivery.
    ///
    /// # Arguments
    ///
    /// * `agent_index` - Index of the agent in the Hub's ordered list
    /// * `pty_index` - Index of the PTY within the agent (0 = CLI, 1 = Server)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The agent at the given index doesn't exist
    /// - The PTY at the given index doesn't exist
    /// - The connection fails for any other reason
    fn connect_to_pty(&mut self, agent_index: usize, pty_index: usize) -> Result<(), String>;

    /// Disconnect from a specific PTY.
    ///
    /// Notifies the PTY that this client is disconnecting. Should be called when:
    /// - Client explicitly disconnects from a PTY
    /// - Client session ends
    /// - Agent is deleted while client is connected
    ///
    /// Safe to call when not connected to the specified PTY (no-op).
    ///
    /// # Arguments
    ///
    /// * `agent_index` - Index of the agent
    /// * `pty_index` - Index of the PTY within the agent
    fn disconnect_from_pty(&mut self, agent_index: usize, pty_index: usize);

    // ============================================================
    // Default implementations (using hub_handle)
    // ============================================================

    /// Get snapshot of all agents.
    ///
    /// Returns `AgentInfo` for all active agents in display order.
    /// This is a snapshot - changes won't be reflected until next call.
    ///
    /// Default implementation delegates to `hub_handle().get_agents()`.
    fn get_agents(&self) -> Vec<AgentInfo> {
        self.hub_handle().get_agents()
    }

    /// Get handle for agent at index.
    ///
    /// Returns `AgentHandle` for the agent at the given index in display order,
    /// or `None` if index is out of bounds.
    ///
    /// Default implementation delegates to `hub_handle().get_agent(index)`.
    fn get_agent(&self, index: usize) -> Option<AgentHandle> {
        self.hub_handle().get_agent(index)
    }

    /// Send input to a specific PTY.
    ///
    /// Routes input to the PTY identified by the given indices. Looks up the
    /// agent and PTY via `hub_handle()` on each call.
    ///
    /// # Arguments
    ///
    /// * `agent_index` - Index of the agent
    /// * `pty_index` - Index of the PTY within the agent
    /// * `data` - Raw bytes to send to the PTY
    ///
    /// # Errors
    ///
    /// Returns an error if the agent or PTY doesn't exist at the given indices,
    /// or if the write fails.
    fn send_input(&self, agent_index: usize, pty_index: usize, data: &[u8]) -> Result<(), String> {
        let agent = self
            .hub_handle()
            .get_agent(agent_index)
            .ok_or_else(|| format!("Agent at index {} not found", agent_index))?;
        let pty = agent
            .get_pty(pty_index)
            .ok_or_else(|| format!("PTY at index {} not found", pty_index))?;
        pty.write_input_blocking(data)
    }

    /// Resize a specific PTY.
    ///
    /// Sends a resize request to the PTY identified by the given indices. Looks
    /// up the agent and PTY via `hub_handle()` on each call.
    ///
    /// # Arguments
    ///
    /// * `agent_index` - Index of the agent
    /// * `pty_index` - Index of the PTY within the agent
    /// * `rows` - New terminal height in rows
    /// * `cols` - New terminal width in columns
    ///
    /// # Errors
    ///
    /// Returns an error if the agent or PTY doesn't exist at the given indices,
    /// or if the resize fails.
    fn resize_pty(
        &self,
        agent_index: usize,
        pty_index: usize,
        rows: u16,
        cols: u16,
    ) -> Result<(), String> {
        let agent = self
            .hub_handle()
            .get_agent(agent_index)
            .ok_or_else(|| format!("Agent at index {} not found", agent_index))?;
        let pty = agent
            .get_pty(pty_index)
            .ok_or_else(|| format!("PTY at index {} not found", pty_index))?;
        pty.resize_blocking(self.id().clone(), rows, cols)
    }

    /// Get agent count.
    ///
    /// Convenience method, equivalent to `get_agents().len()`.
    fn agent_count(&self) -> usize {
        self.get_agents().len()
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
    fn test_client_id_browser_identity() {
        let id = ClientId::browser("test-identity");
        assert_eq!(id.browser_identity(), Some("test-identity"));
        assert_eq!(ClientId::Tui.browser_identity(), None);
    }
}
