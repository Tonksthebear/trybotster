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

use std::any::Any;

pub mod browser;
mod registry;
mod tui;
mod types;

pub use browser::{BrowserClient, BrowserClientConfig, BrowserRequest};
pub use registry::ClientRegistry;
pub use tui::{TuiAgentMetadata, TuiClient, TuiOutput, TuiRequest};
pub use types::{CreateAgentRequest, DeleteAgentRequest, Response};
// AgentMetadata is defined directly in this module (below)

pub use crate::agent::pty::PtyCommand;
pub use crate::hub::agent_handle::{AgentHandle, PtyHandle};
pub use crate::hub::HubHandle;
pub use crate::relay::AgentInfo;

/// Metadata about a selected agent for UI display.
///
/// Contains the essential information clients need after selecting an agent,
/// without exposing Hub internals. This struct is returned by `Client::select_agent()`
/// and can be used by any client implementation.
#[derive(Debug, Clone)]
pub struct AgentMetadata {
    /// The agent's unique identifier (session key).
    pub agent_id: String,
    /// The agent's index in the Hub's ordered list.
    pub agent_index: usize,
    /// Whether this agent has a server PTY (index 1).
    pub has_server_pty: bool,
}

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
/// 4. **Default implementations** - Data access (`get_agents`, `get_agent`, `agent_count`),
///    PTY ops (`send_input`, `resize_pty`, `select_agent`), and hub management
///    (`quit`, `create_agent`, `delete_agent`, etc.) all have defaults using `hub_handle()`
/// 5. **Downcasts via `as_any`** - Used for client-specific operations (e.g., accessing TuiClient or BrowserClient)
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
/// ## Data Access
/// - `get_agents()` - Get all agent info snapshots
/// - `get_agent()` - Get specific agent handle by index
/// - `agent_count()` - Number of active agents
///
/// ## PTY Operations
/// - `send_input()` - Send input to PTY (looks up via hub_handle)
/// - `resize_pty()` - Resize PTY (looks up via hub_handle)
/// - `select_agent()` - Select agent and connect to its CLI PTY
///
/// ## Hub Management
/// - `quit()` - Request Hub shutdown
/// - `list_worktrees()` - List available worktrees
/// - `get_connection_code()` - Get browser connection URL
/// - `create_agent()` - Create a new agent
/// - `delete_agent()` - Delete an agent
/// - `regenerate_connection_code()` - Regenerate Signal bundle
/// - `copy_connection_url()` - Copy connection URL to clipboard
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

    /// Downcast to `Any` for type-specific access.
    ///
    /// Used by Hub to access client-specific methods not on the trait.
    /// Default returns `None`. Override in concrete implementations.
    fn as_any(&self) -> Option<&dyn Any> {
        None
    }

    /// Downcast to `Any` (mutable) for type-specific access.
    ///
    /// Used by Hub to access client-specific methods not on the trait.
    /// Default returns `None`. Override in concrete implementations.
    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        None
    }

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

    /// Connect to an agent's PTY using an already-resolved AgentHandle.
    ///
    /// This is the primary connection method. When Hub's action handlers need to
    /// connect a client, they should look up the AgentHandle directly from Hub state
    /// and call this method. This avoids re-acquiring locks via hub_handle().
    ///
    /// # Arguments
    ///
    /// * `agent_handle` - Handle to the agent (obtained from Hub state)
    /// * `agent_index` - Index of the agent (for tracking connected PTY)
    /// * `pty_index` - Index of the PTY within the agent (0 = CLI, 1 = Server)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The PTY at the given index doesn't exist
    /// - The connection fails for any other reason
    fn connect_to_pty_with_handle(
        &mut self,
        agent_handle: &AgentHandle,
        agent_index: usize,
        pty_index: usize,
    ) -> Result<(), String>;

    /// Connect to an agent's PTY by index.
    ///
    /// Convenience method that looks up the agent via `hub_handle()` then calls
    /// `connect_to_pty_with_handle()`. Safe to call from TuiClient's request handler
    /// or any code that doesn't already hold Hub locks.
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
    fn connect_to_pty(&mut self, agent_index: usize, pty_index: usize) -> Result<(), String> {
        let agent_handle = self
            .hub_handle()
            .get_agent(agent_index)
            .ok_or_else(|| format!("Agent at index {} not found", agent_index))?;
        self.connect_to_pty_with_handle(&agent_handle, agent_index, pty_index)
    }

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

    /// Select an agent by index and connect to its CLI PTY.
    ///
    /// This is the standard way for clients to switch between agents.
    /// Returns metadata about the selected agent for UI display.
    ///
    /// # Arguments
    ///
    /// * `index` - Zero-based index in display order
    ///
    /// # Returns
    ///
    /// - `Ok(AgentMetadata)` on success with agent info for UI
    /// - `Err(String)` if the agent doesn't exist or connection fails
    ///
    /// # Example
    ///
    /// ```ignore
    /// match client.select_agent(0) {
    ///     Ok(meta) => {
    ///         println!("Selected agent: {}", meta.agent_id);
    ///         if meta.has_server_pty {
    ///             println!("Server PTY available");
    ///         }
    ///     }
    ///     Err(e) => eprintln!("Selection failed: {}", e),
    /// }
    /// ```
    fn select_agent(&mut self, index: usize) -> Result<AgentMetadata, String> {
        let agent = self
            .hub_handle()
            .get_agent(index)
            .ok_or_else(|| format!("Agent at index {} not found", index))?;

        let agent_id = agent.agent_id().to_string();
        let agent_index = agent.agent_index();
        let has_server_pty = agent.get_pty(1).is_some();

        // Connect to CLI PTY (index 0)
        self.connect_to_pty(index, 0)?;

        Ok(AgentMetadata {
            agent_id,
            agent_index,
            has_server_pty,
        })
    }

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
    /// **WARNING**: Do NOT call this from Hub's command handler - it will deadlock.
    /// Hub should look up the PTY directly from its state and call
    /// `send_input_with_handle()` instead.
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
        self.send_input_with_handle(pty, data)
    }

    /// Resize a specific PTY.
    ///
    /// Sends a resize request to the PTY identified by the given indices. Looks
    /// up the agent and PTY via `hub_handle()` on each call.
    ///
    /// **WARNING**: Do NOT call this from Hub's command handler - it will deadlock.
    /// Hub should look up the PTY directly from its state and call
    /// `resize_pty_with_handle()` instead.
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
        self.resize_pty_with_handle(pty, rows, cols)
    }

    /// Resize a PTY using an already-resolved handle.
    ///
    /// Use when Hub action handlers have already looked up the PTY handle.
    ///
    /// # Arguments
    ///
    /// * `pty` - Handle to the PTY (obtained from Hub state)
    /// * `rows` - New terminal height in rows
    /// * `cols` - New terminal width in columns
    ///
    /// # Errors
    ///
    /// Returns an error if the resize fails.
    fn resize_pty_with_handle(&self, pty: &PtyHandle, rows: u16, cols: u16) -> Result<(), String> {
        pty.resize_blocking(self.id().clone(), rows, cols)
    }

    /// Send input to a PTY using an already-resolved handle.
    ///
    /// Use when Hub action handlers have already looked up the PTY handle.
    ///
    /// # Arguments
    ///
    /// * `pty` - Handle to the PTY (obtained from Hub state)
    /// * `data` - Raw bytes to send to the PTY
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    fn send_input_with_handle(&self, pty: &PtyHandle, data: &[u8]) -> Result<(), String> {
        pty.write_input_blocking(data)
    }

    /// Disconnect from a PTY using an already-resolved handle.
    ///
    /// Use when Hub action handlers have already looked up the PTY handle.
    ///
    /// # Arguments
    ///
    /// * `pty` - Handle to the PTY (obtained from Hub state)
    /// * `agent_index` - Index of the agent (for tracking)
    /// * `pty_index` - Index of the PTY within the agent (for tracking)
    fn disconnect_from_pty_with_handle(
        &mut self,
        pty: &PtyHandle,
        agent_index: usize,
        pty_index: usize,
    ) {
        // Default implementation just notifies PTY of disconnection.
        // Clients with additional cleanup (like TuiClient's output task abort)
        // should override this method.
        let _ = pty.disconnect_blocking(self.id().clone());
        // Suppress unused variable warnings - these are used by overriding implementations
        let _ = (agent_index, pty_index);
    }

    /// Get agent count.
    ///
    /// Convenience method, equivalent to `get_agents().len()`.
    fn agent_count(&self) -> usize {
        self.get_agents().len()
    }

    // ============================================================
    // Hub management operations (default implementations)
    // ============================================================

    /// Request Hub shutdown.
    ///
    /// Sends a quit command to the Hub. Returns immediately after queuing.
    ///
    /// # Errors
    ///
    /// Returns an error if the Hub command channel is closed.
    fn quit(&self) -> Result<(), String> {
        self.hub_handle().quit()
    }

    /// List available worktrees for agent creation.
    ///
    /// Returns a list of `(path, branch_name)` pairs for existing worktrees
    /// that can be reopened. Returns an empty vector on error.
    fn list_worktrees(&self) -> Vec<(String, String)> {
        self.hub_handle().list_worktrees().unwrap_or_default()
    }

    /// Get the current connection code URL.
    ///
    /// Returns the full URL containing the Signal PreKeyBundle for browser
    /// connection.
    ///
    /// # Errors
    ///
    /// Returns an error if the Signal bundle is not initialized or the
    /// command channel is closed.
    fn get_connection_code(&self) -> Result<String, String> {
        self.hub_handle().get_connection_code()
    }

    /// Create a new agent from a client request.
    ///
    /// Converts the client-layer `CreateAgentRequest` to the hub-layer
    /// `CreateAgentRequest` and dispatches the creation command.
    ///
    /// # Errors
    ///
    /// Returns an error if the Hub command channel is closed.
    fn create_agent(&self, request: CreateAgentRequest) -> Result<(), String> {
        let mut hub_request = crate::hub::CreateAgentRequest::new(&request.issue_or_branch);
        if let Some(prompt) = request.prompt {
            hub_request = hub_request.with_prompt(prompt);
        }
        if let Some(path) = request.from_worktree {
            hub_request = hub_request.from_worktree(path);
        }
        self.hub_handle().create_agent(hub_request)
    }

    /// Delete an existing agent.
    ///
    /// Dispatches the deletion command, optionally including worktree removal.
    ///
    /// # Errors
    ///
    /// Returns an error if the Hub command channel is closed.
    fn delete_agent(&self, request: DeleteAgentRequest) -> Result<(), String> {
        if request.delete_worktree {
            self.hub_handle().delete_agent_with_worktree(&request.agent_id)
        } else {
            self.hub_handle().delete_agent(&request.agent_id)
        }
    }

    /// Regenerate the connection code (Signal bundle).
    ///
    /// Dispatches the regeneration action to Hub. Fire-and-forget.
    ///
    /// # Errors
    ///
    /// Returns an error if the Hub command channel is closed.
    fn regenerate_connection_code(&self) -> Result<(), String> {
        self.hub_handle().dispatch_action(
            crate::hub::HubAction::RegenerateConnectionCode,
        )
    }

    /// Copy connection URL to clipboard.
    ///
    /// Dispatches the copy action to Hub. Fire-and-forget.
    ///
    /// # Errors
    ///
    /// Returns an error if the Hub command channel is closed.
    fn copy_connection_url(&self) -> Result<(), String> {
        self.hub_handle().dispatch_action(
            crate::hub::HubAction::CopyConnectionUrl,
        )
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
