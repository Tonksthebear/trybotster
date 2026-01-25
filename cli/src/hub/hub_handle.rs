//! Hub handle for thread-safe client communication.
//!
//! `HubHandle` provides a clean, synchronous API for clients to query Hub state.
//! It wraps the command channel and provides blocking methods suitable for
//! client threads that don't run in an async context.
//!
//! # Design
//!
//! Similar to how `PtyHandle` wraps PTY channels, `HubHandle` wraps the Hub
//! command channel. This pattern:
//! - Encapsulates channel details from clients
//! - Provides type-safe, discoverable API
//! - Handles errors gracefully (returns empty/None on channel errors)
//!
//! # Thread Safety
//!
//! `HubHandle` is `Clone + Send + Sync`, allowing it to be:
//! - Cloned and passed to client threads
//! - Shared between async tasks
//! - Used from any thread safely
//!
//! # Example
//!
//! ```ignore
//! // Get handle from Hub
//! let handle = hub.handle();
//!
//! // Pass to client thread
//! std::thread::spawn(move || {
//!     // Query agents (blocking)
//!     let agents = handle.get_agents();
//!     for info in &agents {
//!         println!("{}: {:?}", info.id, info.status);
//!     }
//!
//!     // Get specific agent by index
//!     if let Some(agent_handle) = handle.get_agent(0) {
//!         let pty = agent_handle.get_pty(0).expect("CLI PTY always present");
//!         // Use PTY handle...
//!     }
//! });
//! ```

// Rust guideline compliant 2026-01-23

use super::agent_handle::AgentHandle;
use super::commands::{CreateAgentRequest, DeleteAgentRequest, HubCommand, HubCommandSender};
use crate::relay::types::AgentInfo;

/// Handle for thread-safe Hub communication.
///
/// Clients obtain this via `Hub::handle()` and use it to query Hub state
/// from their own threads. All methods are blocking and suitable for
/// synchronous code.
///
/// # Thread Safety
///
/// This handle is `Clone + Send + Sync`:
/// - Clone it freely to share across threads
/// - All methods use `blocking_recv()` for synchronous operation
/// - Channel errors are handled gracefully (return empty/None)
///
/// # Example
///
/// ```ignore
/// let handle = hub.handle();
///
/// // Get all agents
/// let agents = handle.get_agents();
///
/// // Get agent by index
/// if let Some(agent) = handle.get_agent(0) {
///     println!("First agent: {}", agent.info().id);
/// }
///
/// // Fire-and-forget operations
/// handle.create_agent(CreateAgentRequest::new("42"))?;
/// handle.delete_agent("agent-123")?;
/// ```
#[derive(Debug, Clone)]
pub struct HubHandle {
    /// Underlying command sender.
    command_tx: HubCommandSender,
}

impl HubHandle {
    /// Create a new `HubHandle` from a command sender.
    ///
    /// Called internally by `Hub::handle()`.
    #[must_use]
    pub fn new(command_tx: HubCommandSender) -> Self {
        Self { command_tx }
    }

    /// Create a mock `HubHandle` for testing.
    ///
    /// Creates a channel that immediately closes, suitable for tests that
    /// don't need actual Hub communication. Operations will gracefully fail
    /// with empty results or errors.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let client = TuiClient::new(HubHandle::mock());
    /// // Client can be used, but Hub operations will fail gracefully
    /// ```
    #[must_use]
    pub fn mock() -> Self {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        Self {
            command_tx: HubCommandSender::new(tx),
        }
    }

    /// Get a snapshot of all agents.
    ///
    /// Returns agent info for all currently active agents in display order.
    /// This is a snapshot - changes won't be reflected until the next call.
    ///
    /// Returns an empty vector if the Hub is shutting down or the channel
    /// is closed.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let agents = handle.get_agents();
    /// for (i, info) in agents.iter().enumerate() {
    ///     println!("[{}] {}: {:?}", i, info.id, info.status);
    /// }
    /// ```
    #[must_use]
    pub fn get_agents(&self) -> Vec<AgentInfo> {
        self.command_tx.list_agents_blocking().unwrap_or_default()
    }

    /// Get an agent handle by display index.
    ///
    /// Returns an `AgentHandle` for the agent at the given position in the
    /// display order. The handle provides access to:
    /// - Agent metadata via `info()`
    /// - CLI PTY via `cli_pty()`
    /// - Server PTY via `server_pty()` (if running)
    ///
    /// Returns `None` if:
    /// - The index is out of bounds
    /// - The Hub is shutting down
    /// - The channel is closed
    ///
    /// # Arguments
    ///
    /// * `index` - Zero-based index in display order
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Get the first agent
    /// if let Some(agent) = handle.get_agent(0) {
    ///     println!("Agent: {}", agent.info().id);
    ///
    ///     // Subscribe to CLI PTY events (index 0)
    ///     let pty = agent.get_pty(0).expect("CLI PTY always present");
    ///     let mut rx = pty.subscribe();
    /// }
    /// ```
    #[must_use]
    pub fn get_agent(&self, index: usize) -> Option<AgentHandle> {
        self.command_tx
            .get_agent_by_index_blocking(index)
            .ok()
            .flatten()
    }

    /// Request agent creation (fire-and-forget).
    ///
    /// Sends a request to create a new agent. This method returns immediately
    /// after queuing the request - it does not wait for the agent to be created.
    ///
    /// For synchronous creation with result, use the async `HubCommandSender`
    /// methods instead.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed (Hub is shutting down).
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Create agent for issue #42
    /// handle.create_agent(CreateAgentRequest::new("42"))?;
    ///
    /// // Create agent with initial prompt
    /// handle.create_agent(
    ///     CreateAgentRequest::new("fix-bug")
    ///         .with_prompt("Fix the authentication bug")
    /// )?;
    /// ```
    pub fn create_agent(&self, request: CreateAgentRequest) -> Result<(), String> {
        let (cmd, _rx) = HubCommand::create_agent(request);
        self.command_tx
            .inner()
            .blocking_send(cmd)
            .map_err(|e| format!("Failed to send create agent command: {}", e))
    }

    /// Request agent deletion (fire-and-forget).
    ///
    /// Sends a request to delete an existing agent. This method returns
    /// immediately after queuing the request - it does not wait for the
    /// agent to be deleted.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed (Hub is shutting down).
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Delete agent (keep worktree)
    /// handle.delete_agent("owner-repo-42")?;
    /// ```
    pub fn delete_agent(&self, agent_id: &str) -> Result<(), String> {
        let (cmd, _rx) = HubCommand::delete_agent(DeleteAgentRequest::new(agent_id));
        self.command_tx
            .inner()
            .blocking_send(cmd)
            .map_err(|e| format!("Failed to send delete agent command: {}", e))
    }

    /// Request agent deletion with worktree removal (fire-and-forget).
    ///
    /// Like `delete_agent`, but also deletes the agent's worktree from disk.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed (Hub is shutting down).
    pub fn delete_agent_with_worktree(&self, agent_id: &str) -> Result<(), String> {
        let request = DeleteAgentRequest::new(agent_id).with_worktree_deletion();
        let (cmd, _rx) = HubCommand::delete_agent(request);
        self.command_tx
            .inner()
            .blocking_send(cmd)
            .map_err(|e| format!("Failed to send delete agent command: {}", e))
    }

    /// Request Hub shutdown (fire-and-forget).
    ///
    /// Sends a quit command to the Hub. This method returns immediately.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub fn quit(&self) -> Result<(), String> {
        self.command_tx.quit_blocking()
    }

    /// Check if the Hub command channel is closed.
    ///
    /// Returns `true` if the Hub has shut down and commands can no longer
    /// be sent.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.command_tx.is_closed()
    }

    /// Get the number of active agents.
    ///
    /// Convenience method that returns the count of agents without
    /// allocating the full agent info vector.
    ///
    /// Returns 0 if the channel is closed.
    #[must_use]
    pub fn agent_count(&self) -> usize {
        self.get_agents().len()
    }

    /// Check if there are any active agents.
    ///
    /// Returns `false` if there are no agents or the channel is closed.
    #[must_use]
    pub fn has_agents(&self) -> bool {
        !self.get_agents().is_empty()
    }

    // ============================================================
    // Connection Code Methods
    // ============================================================

    /// Get the current connection code URL (blocking).
    ///
    /// Returns the full URL containing the Signal PreKeyBundle for browser
    /// connection. The URL format is:
    /// `{server_url}/hubs/{id}#{base32_binary_bundle}`
    ///
    /// The bundle in the fragment contains the full Kyber prekey bundle
    /// (~2900 chars Base32 encoded).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The Signal bundle is not initialized
    /// - The command channel is closed
    ///
    /// # Example
    ///
    /// ```ignore
    /// match handle.get_connection_code() {
    ///     Ok(url) => println!("Connection URL: {}", url),
    ///     Err(e) => eprintln!("Error: {}", e),
    /// }
    /// ```
    pub fn get_connection_code(&self) -> Result<String, String> {
        self.command_tx.get_connection_code_blocking()
    }

    /// Refresh the connection code (regenerate Signal bundle) (blocking).
    ///
    /// Requests regeneration of the Signal PreKeyBundle. This invalidates
    /// the previous connection code and returns a new one.
    ///
    /// Note: This operation may take some time as it waits for the relay
    /// to generate and return a new bundle.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The relay is not connected
    /// - Bundle regeneration fails
    /// - The command channel is closed
    ///
    /// # Example
    ///
    /// ```ignore
    /// match handle.refresh_connection_code() {
    ///     Ok(new_url) => println!("New connection URL: {}", new_url),
    ///     Err(e) => eprintln!("Failed to refresh: {}", e),
    /// }
    /// ```
    pub fn refresh_connection_code(&self) -> Result<String, String> {
        self.command_tx.refresh_connection_code_blocking()
    }

    // ============================================================
    // Browser Client Support Methods
    // ============================================================

    /// Get the crypto service handle for E2E encryption.
    ///
    /// Returns `None` if crypto service is not initialized (no browser connected).
    /// Used by `BrowserClient::connect_to_pty()` for ActionCable channel setup.
    #[must_use]
    pub fn crypto_service(&self) -> Option<crate::relay::crypto_service::CryptoServiceHandle> {
        self.command_tx.get_crypto_service_blocking().ok().flatten()
    }

    /// Get the server hub ID.
    ///
    /// Returns `None` if hub ID is not set.
    /// Used by `BrowserClient::connect_to_pty()` for ActionCable channel setup.
    #[must_use]
    pub fn server_hub_id(&self) -> Option<String> {
        self.command_tx.get_server_hub_id_blocking().ok().flatten()
    }

    /// Get the server URL.
    ///
    /// Returns the server URL from Hub config.
    /// Used by `BrowserClient::connect_to_pty()` for ActionCable channel setup.
    #[must_use]
    pub fn server_url(&self) -> String {
        self.command_tx
            .get_server_url_blocking()
            .unwrap_or_default()
    }

    /// Get the API key.
    ///
    /// Returns the API key from Hub config.
    /// Used by `BrowserClient::connect_to_pty()` for ActionCable channel setup.
    #[must_use]
    pub fn api_key(&self) -> String {
        self.command_tx.get_api_key_blocking().unwrap_or_default()
    }

    /// Get a handle to the tokio runtime.
    ///
    /// Returns `None` if the runtime is not available.
    /// Used by `BrowserClient::connect_to_pty()` for async task spawning.
    #[must_use]
    pub fn tokio_runtime(&self) -> Option<tokio::runtime::Handle> {
        self.command_tx.get_tokio_runtime_blocking().ok().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[test]
    fn test_hub_handle_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<HubHandle>();
    }

    #[test]
    fn test_hub_handle_is_clone() {
        fn assert_clone<T: Clone>() {}
        assert_clone::<HubHandle>();
    }

    #[test]
    fn test_hub_handle_get_agents_empty_on_closed_channel() {
        // Create a runtime just for channel creation, then drop it
        // before calling blocking methods
        let (tx, rx) = {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async { tokio::sync::mpsc::channel::<HubCommand>(16) })
        };
        let sender = HubCommandSender::new(tx);
        let handle = HubHandle::new(sender);

        // Drop receiver to close channel
        drop(rx);

        // Should return empty vector on error
        let agents = handle.get_agents();
        assert!(agents.is_empty());
    }

    #[test]
    fn test_hub_handle_get_agent_none_on_closed_channel() {
        // Create a runtime just for channel creation, then drop it
        // before calling blocking methods
        let (tx, rx) = {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async { tokio::sync::mpsc::channel::<HubCommand>(16) })
        };
        let sender = HubCommandSender::new(tx);
        let handle = HubHandle::new(sender);

        // Drop receiver to close channel
        drop(rx);

        // Should return None on error
        let agent = handle.get_agent(0);
        assert!(agent.is_none());
    }

    #[tokio::test]
    async fn test_hub_handle_is_closed() {
        let (tx, rx) = mpsc::channel::<HubCommand>(16);
        let sender = HubCommandSender::new(tx);
        let handle = HubHandle::new(sender);

        assert!(!handle.is_closed());

        // Drop receiver to close channel
        drop(rx);

        assert!(handle.is_closed());
    }

    #[tokio::test]
    async fn test_hub_handle_get_agents_with_response() {
        let (tx, mut rx) = mpsc::channel::<HubCommand>(16);
        let sender = HubCommandSender::new(tx);
        let handle = HubHandle::new(sender);

        // Spawn task to handle the command
        let handler = tokio::spawn(async move {
            if let Some(cmd) = rx.recv().await {
                if let HubCommand::ListAgents { response_tx } = cmd {
                    let agents = vec![AgentInfo {
                        id: "test-agent".to_string(),
                        repo: Some("owner/repo".to_string()),
                        issue_number: Some(42),
                        branch_name: Some("botster-issue-42".to_string()),
                        name: None,
                        status: Some("Running".to_string()),
                        tunnel_port: None,
                        server_running: None,
                        has_server_pty: None,
                        active_pty_view: None,
                        scroll_offset: None,
                        hub_identifier: None,
                    }];
                    let _ = response_tx.send(agents);
                }
            }
        });

        // Use spawn_blocking for the blocking call
        let agents = tokio::task::spawn_blocking(move || handle.get_agents())
            .await
            .unwrap();

        handler.await.unwrap();

        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, "test-agent");
    }

    // ============================================================
    // Connection Code Tests
    // ============================================================

    #[test]
    fn test_hub_handle_get_connection_code_error_on_closed_channel() {
        // Create a runtime just for channel creation
        let (tx, rx) = {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async { tokio::sync::mpsc::channel::<HubCommand>(16) })
        };
        let sender = HubCommandSender::new(tx);
        let handle = HubHandle::new(sender);

        // Drop receiver to close channel
        drop(rx);

        // Should return error on closed channel
        let result = handle.get_connection_code();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("channel"));
    }

    #[test]
    fn test_hub_handle_refresh_connection_code_error_on_closed_channel() {
        // Create a runtime just for channel creation
        let (tx, rx) = {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async { tokio::sync::mpsc::channel::<HubCommand>(16) })
        };
        let sender = HubCommandSender::new(tx);
        let handle = HubHandle::new(sender);

        // Drop receiver to close channel
        drop(rx);

        // Should return error on closed channel
        let result = handle.refresh_connection_code();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("channel"));
    }

    #[tokio::test]
    async fn test_hub_handle_get_connection_code_success() {
        let (tx, mut rx) = mpsc::channel::<HubCommand>(16);
        let sender = HubCommandSender::new(tx);
        let handle = HubHandle::new(sender);

        // Spawn task to handle the command
        let handler = tokio::spawn(async move {
            if let Some(cmd) = rx.recv().await {
                if let HubCommand::GetConnectionCode { response_tx } = cmd {
                    let url = "https://botster.dev/hubs/123#GEZDGNBVGY3TQOJQ".to_string();
                    let _ = response_tx.send(Ok(url));
                }
            }
        });

        // Use spawn_blocking for the blocking call
        let result = tokio::task::spawn_blocking(move || handle.get_connection_code())
            .await
            .unwrap();

        handler.await.unwrap();

        assert!(result.is_ok());
        let url = result.unwrap();
        assert!(url.contains("botster.dev"));
        assert!(url.contains("#")); // Fragment with bundle
    }

    #[tokio::test]
    async fn test_hub_handle_get_connection_code_no_bundle() {
        let (tx, mut rx) = mpsc::channel::<HubCommand>(16);
        let sender = HubCommandSender::new(tx);
        let handle = HubHandle::new(sender);

        // Spawn task to handle the command with error response
        let handler = tokio::spawn(async move {
            if let Some(cmd) = rx.recv().await {
                if let HubCommand::GetConnectionCode { response_tx } = cmd {
                    let _ = response_tx.send(Err("Signal bundle not initialized".to_string()));
                }
            }
        });

        // Use spawn_blocking for the blocking call
        let result = tokio::task::spawn_blocking(move || handle.get_connection_code())
            .await
            .unwrap();

        handler.await.unwrap();

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Signal bundle"));
    }

    #[tokio::test]
    async fn test_hub_handle_refresh_connection_code_success() {
        let (tx, mut rx) = mpsc::channel::<HubCommand>(16);
        let sender = HubCommandSender::new(tx);
        let handle = HubHandle::new(sender);

        // Spawn task to handle the command
        let handler = tokio::spawn(async move {
            if let Some(cmd) = rx.recv().await {
                if let HubCommand::RefreshConnectionCode { response_tx } = cmd {
                    let new_url = "https://botster.dev/hubs/123#NEWBUNDLEDATA".to_string();
                    let _ = response_tx.send(Ok(new_url));
                }
            }
        });

        // Use spawn_blocking for the blocking call
        let result = tokio::task::spawn_blocking(move || handle.refresh_connection_code())
            .await
            .unwrap();

        handler.await.unwrap();

        assert!(result.is_ok());
        let url = result.unwrap();
        assert!(url.contains("NEWBUNDLEDATA"));
    }

    #[tokio::test]
    async fn test_hub_handle_refresh_connection_code_relay_not_connected() {
        let (tx, mut rx) = mpsc::channel::<HubCommand>(16);
        let sender = HubCommandSender::new(tx);
        let handle = HubHandle::new(sender);

        // Spawn task to handle the command with error response
        let handler = tokio::spawn(async move {
            if let Some(cmd) = rx.recv().await {
                if let HubCommand::RefreshConnectionCode { response_tx } = cmd {
                    let _ = response_tx.send(Err("Relay not connected".to_string()));
                }
            }
        });

        // Use spawn_blocking for the blocking call
        let result = tokio::task::spawn_blocking(move || handle.refresh_connection_code())
            .await
            .unwrap();

        handler.await.unwrap();

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Relay not connected"));
    }
}
