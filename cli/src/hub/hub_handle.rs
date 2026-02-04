//! Hub handle for thread-safe client communication.
//!
//! `HubHandle` provides a clean, synchronous API for clients to query Hub state.
//! It wraps the command channel AND the HandleCache to provide safe access
//! from any thread without deadlocks.
//!
//! # Design
//!
//! Similar to how `PtyHandle` wraps PTY channels, `HubHandle` wraps the Hub
//! command channel. This pattern:
//! - Encapsulates channel details from clients
//! - Provides type-safe, discoverable API
//! - Handles errors gracefully (returns empty/None on channel errors)
//!
//! # Agent Access
//!
//! `get_agent()` reads from `HandleCache` directly - it does NOT send a
//! blocking command. This is critical for avoiding deadlocks when called
//! from Hub's thread (TuiClient).
//!
//! TuiRunner runs on a separate thread and uses `GetAgentByIndex` command
//! instead (see `runner_agent.rs`).
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
//! // TuiClient on Hub's thread
//! if let Some(agent_handle) = handle.get_agent(0) {
//!     // Safe - reads from cache, no blocking command
//!     let pty = agent_handle.get_pty(0).expect("CLI PTY always present");
//!     pty.write_input_blocking(b"hello")?;
//! }
//!
//! // Query agents (uses ListAgents command)
//! let agents = handle.get_agents();
//! ```

// Rust guideline compliant 2026-01-23

use std::sync::Arc;

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
    /// Thread-safe cache for direct agent handle access and shared state.
    ///
    /// Provides non-blocking reads for agent handles, worktrees, and
    /// connection URLs. Hub updates the cache on lifecycle events.
    handle_cache: Arc<super::handle_cache::HandleCache>,
}

impl HubHandle {
    /// Create a new `HubHandle` from a command sender and handle cache.
    ///
    /// Called internally by `Hub::handle()`.
    #[must_use]
    pub fn new(command_tx: HubCommandSender, handle_cache: Arc<super::handle_cache::HandleCache>) -> Self {
        Self { command_tx, handle_cache }
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
            handle_cache: Arc::new(super::handle_cache::HandleCache::new()),
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

    /// Get an agent handle by display index (non-blocking).
    ///
    /// Returns an `AgentHandle` for the agent at the given position in the
    /// display order. The handle provides access to:
    /// - Agent metadata via `info()`
    /// - CLI PTY via `cli_pty()`
    /// - Server PTY via `server_pty()` (if running)
    ///
    /// Returns `None` if:
    /// - The index is out of bounds
    /// - The cache is empty
    ///
    /// **NOTE**: Reads directly from HandleCache without sending commands to Hub.
    /// This allows clients to access agent handles from any context, including
    /// within Hub command handlers, without blocking or deadlocking.
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
        // Read directly from cache - no blocking command channel
        self.handle_cache.get_agent(index)
    }

    /// Get all agent handles from the cache (non-blocking).
    ///
    /// Returns a snapshot of all cached `AgentHandle` instances in display order.
    /// Useful for searching agents by ID without knowing their index.
    ///
    /// **NOTE**: Reads directly from HandleCache without sending commands to Hub.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let handles = handle.get_all_agent_handles();
    /// if let Some(idx) = handles.iter().position(|h| h.agent_id() == "target-id") {
    ///     // Found the agent at index `idx`
    /// }
    /// ```
    #[must_use]
    pub fn get_all_agent_handles(&self) -> Vec<AgentHandle> {
        self.handle_cache.get_all_agents()
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

    /// Request Hub shutdown (async version).
    ///
    /// For use from async client tasks. Sends a quit command to the Hub.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub async fn quit_async(&self) -> Result<(), String> {
        self.command_tx.quit().await
    }

    /// Create a new agent (async version).
    ///
    /// For use from async client tasks. Sends a create agent command to Hub.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub async fn create_agent_async(&self, request: super::commands::CreateAgentRequest) -> Result<(), String> {
        let (cmd, _rx) = HubCommand::create_agent(request);
        self.command_tx
            .inner()
            .send(cmd)
            .await
            .map_err(|e| format!("Failed to send create agent command: {}", e))
    }

    /// Delete an agent (async version).
    ///
    /// For use from async client tasks. Sends a delete agent command to Hub.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub async fn delete_agent_async(&self, agent_id: &str) -> Result<(), String> {
        let (cmd, _rx) = HubCommand::delete_agent(super::commands::DeleteAgentRequest::new(agent_id));
        self.command_tx
            .inner()
            .send(cmd)
            .await
            .map_err(|e| format!("Failed to send delete agent command: {}", e))
    }

    /// Delete an agent with worktree removal (async version).
    ///
    /// For use from async client tasks.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub async fn delete_agent_with_worktree_async(&self, agent_id: &str) -> Result<(), String> {
        let request = super::commands::DeleteAgentRequest::new(agent_id).with_worktree_deletion();
        let (cmd, _rx) = HubCommand::delete_agent(request);
        self.command_tx
            .inner()
            .send(cmd)
            .await
            .map_err(|e| format!("Failed to send delete agent command: {}", e))
    }

    /// Dispatch a HubAction (async version).
    ///
    /// For use from async client tasks. Sends an action to Hub for processing.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub async fn dispatch_action_async(&self, action: super::HubAction) -> Result<(), String> {
        self.command_tx.dispatch_action_async(action).await
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
    // Worktree Methods
    // ============================================================

    /// List available worktrees for agent creation (non-blocking).
    ///
    /// Reads from HandleCache. Cache is refreshed by Hub on agent lifecycle
    /// changes (create/delete) to exclude worktrees with active agents.
    ///
    /// Returns a list of (path, branch_name) pairs for existing worktrees
    /// that can be reopened.
    pub fn list_worktrees(&self) -> Result<Vec<(String, String)>, String> {
        Ok(self.handle_cache.get_worktrees())
    }

    // ============================================================
    // Action Dispatch Methods
    // ============================================================

    /// Dispatch a HubAction (fire-and-forget).
    ///
    /// Sends an action to the Hub for processing. Returns immediately
    /// without waiting for the action to complete.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub fn dispatch_action(&self, action: super::HubAction) -> Result<(), String> {
        self.command_tx.dispatch_action_blocking(action)
    }

    // ============================================================
    // Connection Code Methods
    // ============================================================

    /// Get the current connection code URL (non-blocking).
    ///
    /// Reads the cached URL directly from shared state. Hub updates this
    /// cache whenever the Signal bundle changes (initialization or refresh).
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
    /// - The connection code has not yet been generated
    /// - The Signal bundle was not initialized
    /// - The state lock is poisoned
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
        self.handle_cache.get_connection_url()
    }

    /// Request connection code refresh (fire-and-forget).
    ///
    /// Sends a command to Hub to regenerate the Signal bundle. The new URL
    /// will be available via `get_connection_code()` after Hub processes the
    /// command and updates shared state.
    ///
    /// This is non-blocking and safe to call from Hub's thread.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is full or closed.
    pub fn refresh_connection_code(&self) -> Result<(), String> {
        let (response_tx, _rx) = tokio::sync::oneshot::channel();
        let cmd = HubCommand::RefreshConnectionCode { response_tx };
        self.command_tx.try_send(cmd)
    }

    // ============================================================
    // Browser Client Support Methods
    // ============================================================

    /// Get the crypto service handle for E2E encryption.
    ///
    /// Returns `None` if crypto service is not initialized (no browser connected).
    /// Used for ActionCable channel setup.
    #[must_use]
    pub fn crypto_service(&self) -> Option<crate::relay::crypto_service::CryptoServiceHandle> {
        self.command_tx.get_crypto_service_blocking().ok().flatten()
    }

    /// Get the server hub ID.
    ///
    /// Returns `None` if hub ID is not set.
    /// Used for ActionCable channel setup.
    #[must_use]
    pub fn server_hub_id(&self) -> Option<String> {
        self.command_tx.get_server_hub_id_blocking().ok().flatten()
    }

    /// Get the server URL.
    ///
    /// Returns the server URL from Hub config.
    /// Used for ActionCable channel setup.
    #[must_use]
    pub fn server_url(&self) -> String {
        self.command_tx
            .get_server_url_blocking()
            .unwrap_or_default()
    }

    /// Get the API key.
    ///
    /// Returns the API key from Hub config.
    /// Used for ActionCable channel setup.
    #[must_use]
    pub fn api_key(&self) -> String {
        self.command_tx.get_api_key_blocking().unwrap_or_default()
    }

    /// Get a handle to the tokio runtime.
    ///
    /// Returns `None` if the runtime is not available.
    /// Used for async task spawning.
    #[must_use]
    pub fn tokio_runtime(&self) -> Option<tokio::runtime::Handle> {
        self.command_tx.get_tokio_runtime_blocking().ok().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::handle_cache::HandleCache;
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

    /// Helper to create a handle with custom cache for tests
    fn test_handle_with_cache(tx: tokio::sync::mpsc::Sender<HubCommand>, cache: Arc<HandleCache>) -> HubHandle {
        HubHandle::new(HubCommandSender::new(tx), cache)
    }

    #[test]
    fn test_hub_handle_get_agents_empty_on_closed_channel() {
        // Create a runtime just for channel creation, then drop it
        // before calling blocking methods
        let (tx, rx) = {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async { tokio::sync::mpsc::channel::<HubCommand>(16) })
        };
        let cache = Arc::new(HandleCache::new());
        let handle = test_handle_with_cache(tx, cache);

        // Drop receiver to close channel
        drop(rx);

        // Should return empty vector on error
        let agents = handle.get_agents();
        assert!(agents.is_empty());
    }

    #[test]
    fn test_hub_handle_get_agent_none_on_empty_cache() {
        // get_agent now reads from cache, not channel
        let handle = HubHandle::mock();

        // Should return None on empty cache
        let agent = handle.get_agent(0);
        assert!(agent.is_none());
    }

    #[tokio::test]
    async fn test_hub_handle_is_closed() {
        let (tx, rx) = mpsc::channel::<HubCommand>(16);
        let cache = Arc::new(HandleCache::new());
        let handle = test_handle_with_cache(tx, cache);

        assert!(!handle.is_closed());

        // Drop receiver to close channel
        drop(rx);

        assert!(handle.is_closed());
    }

    #[tokio::test]
    async fn test_hub_handle_get_agents_with_response() {
        let (tx, mut rx) = mpsc::channel::<HubCommand>(16);
        let cache = Arc::new(HandleCache::new());
        let handle = test_handle_with_cache(tx, cache);

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
                        port: None,
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
    // Connection Code Tests (reads from shared state)
    // ============================================================

    #[test]
    fn test_hub_handle_get_connection_code_not_yet_generated() {
        // With no cached URL, should return error
        let handle = HubHandle::mock();
        let result = handle.get_connection_code();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not yet generated"));
    }

    #[test]
    fn test_hub_handle_get_connection_code_success() {
        let (tx, _rx) = mpsc::channel::<HubCommand>(16);
        let cache = Arc::new(HandleCache::new());

        // Pre-populate the cached connection URL in HandleCache
        cache.set_connection_url(Ok(
            "https://botster.dev/hubs/123#GEZDGNBVGY3TQOJQ".to_string(),
        ));

        let handle = test_handle_with_cache(tx, cache);
        let result = handle.get_connection_code();
        assert!(result.is_ok());
        let url = result.unwrap();
        assert!(url.contains("botster.dev"));
        assert!(url.contains("#")); // Fragment with bundle
    }

    #[test]
    fn test_hub_handle_get_connection_code_no_bundle() {
        let (tx, _rx) = mpsc::channel::<HubCommand>(16);
        let cache = Arc::new(HandleCache::new());

        // Pre-populate with an error result
        cache.set_connection_url(Err("Signal bundle not initialized".to_string()));

        let handle = test_handle_with_cache(tx, cache);
        let result = handle.get_connection_code();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Signal bundle"));
    }

    #[test]
    fn test_hub_handle_refresh_connection_code_error_on_closed_channel() {
        // Create a runtime just for channel creation
        let (tx, rx) = {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async { tokio::sync::mpsc::channel::<HubCommand>(16) })
        };
        let cache = Arc::new(HandleCache::new());
        let handle = test_handle_with_cache(tx, cache);

        // Drop receiver to close channel
        drop(rx);

        // Should return error on closed channel (try_send fails)
        let result = handle.refresh_connection_code();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_hub_handle_refresh_connection_code_sends_command() {
        let (tx, mut rx) = mpsc::channel::<HubCommand>(16);
        let cache = Arc::new(HandleCache::new());
        let handle = test_handle_with_cache(tx, cache);

        // Fire-and-forget refresh
        let result = handle.refresh_connection_code();
        assert!(result.is_ok());

        // Verify the command was sent
        let cmd = rx.try_recv().expect("Should have received a command");
        assert!(cmd.is_refresh_connection_code());
    }

    // ============================================================
    // List Worktrees Tests (reads from shared state)
    // ============================================================

    #[test]
    fn test_hub_handle_list_worktrees_empty() {
        let handle = HubHandle::mock();
        let result = handle.list_worktrees();
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_hub_handle_list_worktrees_from_cache() {
        let (tx, _rx) = mpsc::channel::<HubCommand>(16);
        let cache = Arc::new(HandleCache::new());

        // Pre-populate worktrees in HandleCache
        cache.set_worktrees(vec![
            ("/tmp/wt1".to_string(), "feature-1".to_string()),
            ("/tmp/wt2".to_string(), "feature-2".to_string()),
        ]);

        let handle = test_handle_with_cache(tx, cache);
        let result = handle.list_worktrees();
        assert!(result.is_ok());
        let worktrees = result.unwrap();
        assert_eq!(worktrees.len(), 2);
        assert_eq!(worktrees[0].1, "feature-1");
        assert_eq!(worktrees[1].1, "feature-2");
    }
}
