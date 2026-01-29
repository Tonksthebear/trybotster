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
//! client.connect_to_pty(agent_idx, pty_idx).await     // Connect to specific PTY
//! client.disconnect_from_pty(agent_idx, pty_idx).await // Disconnect from specific PTY
//! client.send_input(agent_idx, pty_idx, data).await    // Send to specific PTY
//! client.resize_pty(agent_idx, pty_idx, r, c).await    // Resize specific PTY
//! ```
//!
//! This design:
//! - Makes TUI and Browser implementations symmetric
//! - Allows multiple simultaneous PTY connections (Browser can have multiple tabs)
//! - Keeps "current" state in the GUI where it belongs
//!
//! # Async Design
//!
//! All PTY operations and Hub management methods are async, using native `async fn`
//! in traits (Rust 1.75+). Client tasks run as independent async tasks via `run_task()`,
//! processing requests from their GUI layer and hub events via broadcast.
//!
//! # Data Access Pattern
//!
//! Clients read agent data via `hub_handle().get_agent(index)` (non-blocking cache read).
//! Default implementations provide convenient wrappers:
//!
//! ```text
//! client.get_agent(0) → Option<AgentHandle> (via hub_handle, non-blocking)
//! client.send_input(0, 0, data).await → looks up agent/PTY via hub_handle
//! client.resize_pty(0, 0, r, c).await → looks up agent/PTY via hub_handle
//! ```

// Rust guideline compliant 2026-01

pub mod browser;
mod registry;
mod tui;
mod types;

pub use browser::{BrowserClient, BrowserClientConfig, BrowserRequest};
pub use registry::{ClientRegistry, ClientTaskHandle};
pub use tui::{TuiAgentMetadata, TuiClient, TuiOutput, TuiRequest};
pub use types::{CreateAgentRequest, DeleteAgentRequest, Response};
// AgentMetadata is defined directly in this module (below)

pub use crate::agent::pty::PtyCommand;
pub use crate::hub::agent_handle::{AgentHandle, PtyHandle};
pub use crate::hub::HubHandle;
pub use crate::relay::signal::PreKeyBundleData;
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

/// The Client trait - async API layer for Hub/PTY interaction.
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
/// 4. **Async operations** - All PTY and Hub management methods are async, using
///    native `async fn` in traits (Rust 1.75+)
/// 5. **Default implementations** - PTY ops (`send_input`, `resize_pty`, `select_agent`)
///    and hub management (`quit`, `create_agent`, `delete_agent`, etc.) all have
///    defaults using `hub_handle()`
///
/// # Required Methods (clients must implement)
///
/// - `hub_handle()` - Access to Hub data and operations
/// - `id()` - Unique client identifier
/// - `dims()` - Terminal dimensions for PTY resize
/// - `connect_to_pty_with_handle()` - Establish PTY connection (async)
/// - `disconnect_from_pty()` - Terminate PTY connection (async)
///
/// # Default Methods (using hub_handle)
///
/// ## Data Access (sync - reads from HandleCache)
/// - `get_agent()` - Get specific agent handle by index
/// - `list_worktrees()` - List available worktrees
/// - `get_connection_code()` - Get browser connection URL
///
/// ## PTY Operations (async)
/// - `send_input()` - Send input to PTY (looks up via hub_handle)
/// - `resize_pty()` - Resize PTY (looks up via hub_handle)
/// - `select_agent()` - Select agent, notify Hub, and connect to CLI PTY
///
/// ## Hub Management (async)
/// - `quit()` - Request Hub shutdown
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
    // Required - sync (clients must implement)
    // ============================================================

    /// Access to Hub data and operations.
    ///
    /// All data access and PTY operations go through this handle.
    /// The default implementations for `get_agent`, `send_input`,
    /// and `resize_pty` use this.
    fn hub_handle(&self) -> &HubHandle;

    /// Unique identifier for this client.
    fn id(&self) -> &ClientId;

    /// Terminal dimensions (cols, rows).
    ///
    /// Used when connecting to PTY to report initial size.
    fn dims(&self) -> (u16, u16);

    /// Take the Hub event broadcast receiver from this client.
    ///
    /// Returns the `broadcast::Receiver<HubEvent>` if it has not already been
    /// taken. Subsequent calls return `None`. This is designed for `run_task()`
    /// to extract the receiver once and consume it in its event loop.
    ///
    /// Both `TuiClient` and `BrowserClient` must implement this -- there is
    /// no default implementation.
    fn take_hub_event_rx(&mut self) -> Option<tokio::sync::broadcast::Receiver<crate::hub::HubEvent>>;

    // ============================================================
    // Required - async (clients must implement)
    // ============================================================

    /// Connect to an agent's PTY using an already-resolved AgentHandle.
    ///
    /// This is the primary connection method. Looks up the PTY from the
    /// given AgentHandle, connects to it, and sets up output forwarding.
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
    async fn connect_to_pty_with_handle(
        &mut self,
        agent_handle: &AgentHandle,
        agent_index: usize,
        pty_index: usize,
    ) -> Result<(), String>;

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
    async fn disconnect_from_pty(&mut self, agent_index: usize, pty_index: usize);

    // ============================================================
    // Default implementations - sync (non-blocking cache reads)
    // ============================================================

    /// Get handle for agent at index.
    ///
    /// Returns `AgentHandle` for the agent at the given index in display order,
    /// or `None` if index is out of bounds.
    ///
    /// Default implementation delegates to `hub_handle().get_agent(index)`.
    /// Reads from HandleCache (non-blocking).
    fn get_agent(&self, index: usize) -> Option<AgentHandle> {
        self.hub_handle().get_agent(index)
    }

    /// List available worktrees for agent creation.
    ///
    /// Returns a list of `(path, branch_name)` pairs for existing worktrees
    /// that can be reopened. Returns an empty vector on error.
    /// Reads from HandleCache (non-blocking).
    fn list_worktrees(&self) -> Vec<(String, String)> {
        self.hub_handle().list_worktrees().unwrap_or_default()
    }

    /// Get the current connection code URL.
    ///
    /// Returns the full URL containing the Signal PreKeyBundle for browser
    /// connection. Reads from HandleCache (non-blocking).
    ///
    /// # Errors
    ///
    /// Returns an error if the Signal bundle is not initialized.
    fn get_connection_code(&self) -> Result<String, String> {
        self.hub_handle().get_connection_code()
    }

    // ============================================================
    // Default implementations - async (PTY operations)
    // ============================================================

    /// Connect to an agent's PTY by index.
    ///
    /// Convenience method that looks up the agent via `hub_handle()` then calls
    /// `connect_to_pty_with_handle()`. Safe to call from client request handlers
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
    async fn connect_to_pty(&mut self, agent_index: usize, pty_index: usize) -> Result<(), String> {
        let agent_handle = self
            .hub_handle()
            .get_agent(agent_index)
            .ok_or_else(|| format!("Agent at index {} not found", agent_index))?;
        self.connect_to_pty_with_handle(&agent_handle, agent_index, pty_index).await?;

        // Send initial resize so the PTY knows this client's dimensions.
        let (cols, rows) = self.dims();
        if cols > 0 && rows > 0 {
            let _ = self.resize_pty(agent_index, pty_index, rows, cols).await;
        }
        Ok(())
    }

    /// Send input to a specific PTY.
    ///
    /// Routes input to the PTY identified by the given indices. Looks up the
    /// agent and PTY via `hub_handle()` on each call, clones the PtyHandle,
    /// then awaits the async write.
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
    async fn send_input(&mut self, agent_index: usize, pty_index: usize, data: &[u8]) -> Result<(), String> {
        let pty = self.hub_handle()
            .get_agent(agent_index)
            .ok_or_else(|| format!("Agent at index {} not found", agent_index))?
            .get_pty(pty_index)
            .ok_or_else(|| format!("PTY at index {} not found", pty_index))?
            .clone();
        pty.write_input(data).await
    }

    /// Send input to a PTY using an already-resolved handle.
    ///
    /// Use when the PTY handle has already been looked up.
    ///
    /// # Arguments
    ///
    /// * `pty` - Handle to the PTY (cloned before calling)
    /// * `data` - Raw bytes to send to the PTY
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    async fn send_input_with_handle(&mut self, pty: &PtyHandle, data: &[u8]) -> Result<(), String> {
        let pty = pty.clone();
        pty.write_input(data).await
    }

    /// Resize a specific PTY.
    ///
    /// Sends a resize request to the PTY identified by the given indices. Looks
    /// up the agent and PTY via `hub_handle()`, clones the PtyHandle, then
    /// awaits the async resize.
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
    async fn resize_pty(
        &mut self,
        agent_index: usize,
        pty_index: usize,
        rows: u16,
        cols: u16,
    ) -> Result<(), String> {
        let (pty, client_id) = {
            let pty = self.hub_handle()
                .get_agent(agent_index)
                .ok_or_else(|| format!("Agent at index {} not found", agent_index))?
                .get_pty(pty_index)
                .ok_or_else(|| format!("PTY at index {} not found", pty_index))?
                .clone();
            (pty, self.id().clone())
        };
        pty.resize(client_id, rows, cols).await
    }

    /// Resize a PTY using an already-resolved handle.
    ///
    /// Use when the PTY handle has already been looked up.
    ///
    /// # Arguments
    ///
    /// * `pty` - Handle to the PTY (cloned before calling)
    /// * `rows` - New terminal height in rows
    /// * `cols` - New terminal width in columns
    ///
    /// # Errors
    ///
    /// Returns an error if the resize fails.
    async fn resize_pty_with_handle(&mut self, pty: &PtyHandle, rows: u16, cols: u16) -> Result<(), String> {
        let pty = pty.clone();
        let client_id = self.id().clone();
        pty.resize(client_id, rows, cols).await
    }

    /// Disconnect from a PTY using an already-resolved handle.
    ///
    /// Default implementation notifies the PTY of disconnection.
    /// Clients with additional cleanup (like TuiClient's output task abort)
    /// should override this method.
    ///
    /// # Arguments
    ///
    /// * `pty` - Handle to the PTY (cloned before calling)
    /// * `agent_index` - Index of the agent (for tracking)
    /// * `pty_index` - Index of the PTY within the agent (for tracking)
    async fn disconnect_from_pty_with_handle(
        &mut self,
        pty: &PtyHandle,
        agent_index: usize,
        pty_index: usize,
    ) {
        let pty = pty.clone();
        let client_id = self.id().clone();
        let _ = pty.disconnect(client_id).await;
        // Suppress unused variable warnings - these are used by overriding implementations
        let _ = (agent_index, pty_index);
    }

    /// Select an agent by index, notify Hub, and connect to its CLI PTY.
    ///
    /// This is the standard way for clients to switch between agents.
    /// Dispatches `SelectAgentForClient` to Hub for selection tracking,
    /// then connects to the agent's CLI PTY (index 0).
    ///
    /// # Arguments
    ///
    /// * `index` - Zero-based index in display order
    ///
    /// # Returns
    ///
    /// - `Ok(AgentMetadata)` on success with agent info for UI
    /// - `Err(String)` if the agent doesn't exist or connection fails
    async fn select_agent(&mut self, index: usize) -> Result<AgentMetadata, String> {
        let (agent_id, has_server_pty, hub) = {
            let agent = self.hub_handle()
                .get_agent(index)
                .ok_or_else(|| format!("Agent at index {} not found", index))?;
            (agent.agent_id().to_string(), agent.get_pty(1).is_some(), self.hub_handle().clone())
        };

        hub.dispatch_action_async(crate::hub::HubAction::SelectAgentForClient {
            client_id: self.id().clone(),
            agent_key: agent_id.clone(),
        }).await?;

        self.connect_to_pty(index, 0).await?;

        Ok(AgentMetadata {
            agent_id,
            agent_index: index,
            has_server_pty,
        })
    }

    // ============================================================
    // Hub management operations - async (default implementations)
    // ============================================================

    /// Request Hub shutdown.
    ///
    /// Sends a quit command to the Hub asynchronously.
    ///
    /// # Errors
    ///
    /// Returns an error if the Hub command channel is closed.
    async fn quit(&mut self) -> Result<(), String> {
        let hub = self.hub_handle().clone();
        hub.quit_async().await
    }

    /// Create a new agent from a client request.
    ///
    /// Converts the client-layer `CreateAgentRequest` to the hub-layer
    /// `CreateAgentRequest` and dispatches the creation command asynchronously.
    ///
    /// # Errors
    ///
    /// Returns an error if the Hub command channel is closed.
    async fn create_agent(&mut self, request: CreateAgentRequest) -> Result<(), String> {
        let mut hub_request = crate::hub::CreateAgentRequest::new(&request.issue_or_branch);
        if let Some(prompt) = request.prompt {
            hub_request = hub_request.with_prompt(prompt);
        }
        if let Some(path) = request.from_worktree {
            hub_request = hub_request.from_worktree(path);
        }
        if let Some(dims) = request.dims {
            hub_request = hub_request.with_dims(dims);
        }
        let hub = self.hub_handle().clone();
        hub.create_agent_async(hub_request).await
    }

    /// Delete an existing agent.
    ///
    /// Dispatches the deletion command asynchronously, optionally including
    /// worktree removal.
    ///
    /// # Errors
    ///
    /// Returns an error if the Hub command channel is closed.
    async fn delete_agent(&mut self, request: DeleteAgentRequest) -> Result<(), String> {
        let hub = self.hub_handle().clone();
        if request.delete_worktree {
            hub.delete_agent_with_worktree_async(&request.agent_id).await
        } else {
            hub.delete_agent_async(&request.agent_id).await
        }
    }

    /// Regenerate the connection code (Signal bundle).
    ///
    /// Dispatches the regeneration action to Hub asynchronously.
    ///
    /// # Errors
    ///
    /// Returns an error if the Hub command channel is closed.
    async fn regenerate_connection_code(&mut self) -> Result<(), String> {
        let hub = self.hub_handle().clone();
        hub.dispatch_action_async(
            crate::hub::HubAction::RegenerateConnectionCode,
        ).await
    }

    /// Copy connection URL to clipboard.
    ///
    /// Dispatches the copy action to Hub asynchronously.
    ///
    /// # Errors
    ///
    /// Returns an error if the Hub command channel is closed.
    async fn copy_connection_url(&mut self) -> Result<(), String> {
        let hub = self.hub_handle().clone();
        hub.dispatch_action_async(
            crate::hub::HubAction::CopyConnectionUrl,
        ).await
    }

    /// Regenerate the PreKeyBundle with a fresh PreKey.
    ///
    /// Used when the user wants a new QR code for browser connection.
    /// The bundle is generated via the crypto service.
    ///
    /// # Errors
    ///
    /// Returns an error if the crypto service is not available or
    /// bundle generation fails.
    async fn regenerate_prekey_bundle(&self) -> Result<PreKeyBundleData, String>;
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
