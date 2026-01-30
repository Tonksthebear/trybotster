//! Hub commands for client-to-hub communication.
//!
//! This module defines commands that clients send to the Hub via `tokio::sync::mpsc`
//! channels. The Hub processes commands in its main loop (actor pattern), avoiding
//! shared mutable state and lock contention.
//!
//! # Command Types
//!
//! - [`HubCommand::CreateAgent`] - Create a new agent
//! - [`HubCommand::DeleteAgent`] - Delete an existing agent
//! - [`HubCommand::ListAgents`] - Get list of all agents
//! - [`HubCommand::GetAgentByIndex`] - Get agent handle by display index (cross-thread only)
//!
//! # Agent Handle Access Patterns
//!
//! There are two ways to get agent handles, depending on thread context:
//!
//! ## 1. Via HandleCache (clients on Hub's thread)
//!
//! Clients using `HubHandle` (TuiClient, BrowserClient) read from the cache directly:
//!
//! ```text
//! HubHandle::get_agent(idx) → HandleCache → Option<AgentHandle>
//! ```
//!
//! This avoids deadlocks since it doesn't send commands through the Hub.
//!
//! ## 2. Via GetAgentByIndex command (cross-thread access)
//!
//! `TuiRunner` runs on a separate thread and uses blocking commands:
//!
//! ```text
//! TuiRunner → HubCommand::GetAgentByIndex(idx) → Hub → AgentHandle
//! ```
//!
//! This is safe because it's cross-thread communication (no deadlock risk).
//!
//! # Actor Pattern
//!
//! Commands use oneshot channels for responses, enabling request/response semantics:
//!
//! ```ignore
//! // TuiRunner (cross-thread) sends blocking command
//! let (cmd, rx) = HubCommand::get_agent_by_index(0);
//! hub_tx.blocking_send(cmd)?;
//! let handle = rx.blocking_recv()?.unwrap();
//!
//! // Clients on Hub's thread use HubHandle instead (no command)
//! let handle = hub_handle.get_agent(0).unwrap();
//! ```

// Rust guideline compliant 2026-01

use std::path::PathBuf;
use tokio::sync::oneshot;

use super::actions::HubAction;
use super::agent_handle::AgentHandle;
use crate::client::ClientId;
use crate::relay::types::AgentInfo;

/// Request to create an agent.
///
/// Encapsulates all parameters needed to create a new agent.
#[derive(Debug, Clone)]
pub struct CreateAgentRequest {
    /// Issue number or branch name for the new agent.
    pub issue_or_branch: String,

    /// Optional initial prompt for the agent.
    pub prompt: Option<String>,

    /// Optional path to an existing worktree to reopen.
    pub from_worktree: Option<PathBuf>,

    /// Terminal dimensions (rows, cols) from the requesting client.
    /// If None, a default of (24, 80) is used when spawning.
    pub dims: Option<(u16, u16)>,
}

impl CreateAgentRequest {
    /// Create a new agent request for an issue or branch.
    #[must_use]
    pub fn new(issue_or_branch: impl Into<String>) -> Self {
        Self {
            issue_or_branch: issue_or_branch.into(),
            prompt: None,
            from_worktree: None,
            dims: None,
        }
    }

    /// Add an initial prompt.
    #[must_use]
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = Some(prompt.into());
        self
    }

    /// Reopen an existing worktree.
    #[must_use]
    pub fn from_worktree(mut self, path: PathBuf) -> Self {
        self.from_worktree = Some(path);
        self
    }

    /// Set terminal dimensions for PTY sizing.
    #[must_use]
    pub fn with_dims(mut self, dims: (u16, u16)) -> Self {
        self.dims = Some(dims);
        self
    }
}

/// Request to delete an agent.
#[derive(Debug, Clone)]
pub struct DeleteAgentRequest {
    /// Agent ID of the agent to delete.
    pub agent_id: String,

    /// Whether to also delete the worktree (files on disk).
    pub delete_worktree: bool,
}

impl DeleteAgentRequest {
    /// Create a delete request (keeping worktree by default).
    #[must_use]
    pub fn new(agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            delete_worktree: false,
        }
    }

    /// Also delete the worktree.
    #[must_use]
    pub fn with_worktree_deletion(mut self) -> Self {
        self.delete_worktree = true;
        self
    }
}

/// Result of agent creation.
pub type CreateAgentResult = Result<AgentInfo, String>;

/// Result of agent deletion.
pub type DeleteAgentResult = Result<(), String>;

/// Commands sent from clients to the Hub.
///
/// The Hub processes these commands in its main loop using the actor pattern.
/// Each command includes a oneshot response channel for returning results.
///
/// This pattern avoids shared mutable state and lock contention by serializing
/// all Hub operations through a single command channel.
#[derive(Debug)]
pub enum HubCommand {
    /// Create a new agent.
    ///
    /// The Hub will create a worktree, spawn the agent process, and return
    /// the agent info on success.
    CreateAgent {
        /// Creation parameters.
        request: CreateAgentRequest,
        /// Channel for sending the result back to the client.
        response_tx: oneshot::Sender<CreateAgentResult>,
    },

    /// Delete an existing agent.
    ///
    /// The Hub will terminate the agent process and optionally delete the
    /// worktree.
    DeleteAgent {
        /// Deletion parameters.
        request: DeleteAgentRequest,
        /// Channel for sending the result back to the client.
        response_tx: oneshot::Sender<DeleteAgentResult>,
    },

    /// List all agents.
    ///
    /// Returns information about all currently running agents.
    ListAgents {
        /// Channel for sending the agent list back to the client.
        response_tx: oneshot::Sender<Vec<AgentInfo>>,
    },

    // === Agent Access ===
    /// Get an agent handle by display index (for cross-thread access).
    ///
    /// Returns an `AgentHandle` that provides:
    /// - Agent info snapshot via `info()`
    /// - PTY handles via `get_pty(index)` - index 0 = CLI, index 1 = Server
    ///
    /// Returns `None` if the index is out of bounds.
    ///
    /// # When to Use
    ///
    /// Use this command **only from TuiRunner** (which runs on a separate thread).
    /// This is safe because cross-thread blocking commands don't cause deadlocks.
    ///
    /// # When NOT to Use
    ///
    /// Clients on Hub's thread (TuiClient, BrowserClient) should use
    /// `HubHandle::get_agent()` instead, which reads from HandleCache directly.
    /// Using blocking commands from Hub's thread causes deadlocks.
    GetAgentByIndex {
        /// Display index (0-based).
        index: usize,
        /// Channel for sending the agent handle back.
        response_tx: oneshot::Sender<Option<AgentHandle>>,
    },

    /// Request Hub shutdown.
    Quit,

    /// Dispatch a HubAction.
    ///
    /// Allows clients to send any HubAction for processing by the Hub.
    /// This is a fire-and-forget operation - no response is returned.
    /// Used by TuiRunner for actions that don't need a response.
    DispatchAction(HubAction),

    /// Request worktree list.
    ///
    /// Returns the list of available worktrees for agent creation.
    ListWorktrees {
        /// Channel for sending the worktree list back.
        response_tx: oneshot::Sender<Vec<(String, String)>>,
    },

    // ============================================================
    // Client PTY I/O Commands (fire-and-forget)
    // ============================================================

    /// Browser PTY input (routes through Client trait).
    ///
    /// Routes keyboard input from a browser client to the appropriate PTY
    /// via the Client trait's `send_input()` method. This ensures all PTY I/O
    /// goes through the same code path regardless of client type.
    BrowserPtyInput {
        /// Client identifier for routing.
        client_id: ClientId,
        /// Agent index in the Hub's ordered list.
        agent_index: usize,
        /// PTY index within the agent (0 = CLI, 1 = Server).
        pty_index: usize,
        /// Raw input data.
        data: Vec<u8>,
    },

    // ============================================================
    // Connection Code Commands
    // ============================================================

    /// Get the current connection code URL.
    ///
    /// Returns the full URL containing the Signal PreKeyBundle for browser
    /// connection. Format: `{server_url}/hubs/{id}#{base32_binary_bundle}`
    ///
    /// Returns an error if the Signal bundle is not initialized.
    GetConnectionCode {
        /// Channel for sending the connection code URL back.
        response_tx: oneshot::Sender<ConnectionCodeResult>,
    },

    /// Refresh the connection code (regenerate Signal bundle).
    ///
    /// Requests regeneration of the Signal PreKeyBundle, which invalidates
    /// the previous connection code. Returns the new connection code URL.
    ///
    /// Returns an error if the relay is not connected.
    RefreshConnectionCode {
        /// Channel for sending the new connection code URL back.
        response_tx: oneshot::Sender<ConnectionCodeResult>,
    },

    // ============================================================
    // Browser Client Support Commands
    // ============================================================

    /// Get the crypto service handle for E2E encryption.
    ///
    /// Returns the crypto service handle if initialized, None otherwise.
    GetCryptoService {
        /// Channel for sending the crypto service handle back.
        response_tx:
            oneshot::Sender<Option<crate::relay::crypto_service::CryptoServiceHandle>>,
    },

    /// Get the server hub ID.
    ///
    /// Returns the hub ID if set, None otherwise.
    GetServerHubId {
        /// Channel for sending the hub ID back.
        response_tx: oneshot::Sender<Option<String>>,
    },

    /// Get the server URL from config.
    GetServerUrl {
        /// Channel for sending the server URL back.
        response_tx: oneshot::Sender<String>,
    },

    /// Get the API key from config.
    GetApiKey {
        /// Channel for sending the API key back.
        response_tx: oneshot::Sender<String>,
    },

    /// Get a handle to the tokio runtime.
    GetTokioRuntime {
        /// Channel for sending the runtime handle back.
        response_tx: oneshot::Sender<Option<tokio::runtime::Handle>>,
    },
}

impl HubCommand {
    /// Create a command to create an agent.
    ///
    /// Returns the command and a receiver for the response.
    #[must_use]
    pub fn create_agent(
        request: CreateAgentRequest,
    ) -> (Self, oneshot::Receiver<CreateAgentResult>) {
        let (tx, rx) = oneshot::channel();
        (
            Self::CreateAgent {
                request,
                response_tx: tx,
            },
            rx,
        )
    }

    /// Create a command to delete an agent.
    ///
    /// Returns the command and a receiver for the response.
    #[must_use]
    pub fn delete_agent(
        request: DeleteAgentRequest,
    ) -> (Self, oneshot::Receiver<DeleteAgentResult>) {
        let (tx, rx) = oneshot::channel();
        (
            Self::DeleteAgent {
                request,
                response_tx: tx,
            },
            rx,
        )
    }

    /// Create a command to list all agents.
    ///
    /// Returns the command and a receiver for the response.
    #[must_use]
    pub fn list_agents() -> (Self, oneshot::Receiver<Vec<AgentInfo>>) {
        let (tx, rx) = oneshot::channel();
        (Self::ListAgents { response_tx: tx }, rx)
    }

    /// Create a command to get an agent handle by display index.
    ///
    /// Returns the command and a receiver for the optional `AgentHandle`.
    /// Returns `None` if the index is out of bounds.
    #[must_use]
    pub fn get_agent_by_index(index: usize) -> (Self, oneshot::Receiver<Option<AgentHandle>>) {
        let (tx, rx) = oneshot::channel();
        (
            Self::GetAgentByIndex {
                index,
                response_tx: tx,
            },
            rx,
        )
    }

    /// Check if this is a create agent command.
    #[must_use]
    pub fn is_create_agent(&self) -> bool {
        matches!(self, Self::CreateAgent { .. })
    }

    /// Check if this is a delete agent command.
    #[must_use]
    pub fn is_delete_agent(&self) -> bool {
        matches!(self, Self::DeleteAgent { .. })
    }

    /// Check if this is a list agents command.
    #[must_use]
    pub fn is_list_agents(&self) -> bool {
        matches!(self, Self::ListAgents { .. })
    }

    /// Check if this is a get agent by index command.
    #[must_use]
    pub fn is_get_agent_by_index(&self) -> bool {
        matches!(self, Self::GetAgentByIndex { .. })
    }

    /// Create a quit command.
    #[must_use]
    pub fn quit() -> Self {
        Self::Quit
    }

    /// Create a command to dispatch a HubAction.
    ///
    /// This is fire-and-forget - no response channel is created.
    #[must_use]
    pub fn dispatch_action(action: HubAction) -> Self {
        Self::DispatchAction(action)
    }

    /// Create a command to list worktrees.
    ///
    /// Returns the command and a receiver for the worktree list.
    #[must_use]
    pub fn list_worktrees() -> (Self, oneshot::Receiver<Vec<(String, String)>>) {
        let (tx, rx) = oneshot::channel();
        (Self::ListWorktrees { response_tx: tx }, rx)
    }

    /// Check if this is a dispatch action command.
    #[must_use]
    pub fn is_dispatch_action(&self) -> bool {
        matches!(self, Self::DispatchAction(_))
    }

    /// Check if this is a list worktrees command.
    #[must_use]
    pub fn is_list_worktrees(&self) -> bool {
        matches!(self, Self::ListWorktrees { .. })
    }

    // ============================================================
    // Connection Code Command Constructors
    // ============================================================

    /// Create a command to get the current connection code.
    ///
    /// Returns the command and a receiver for the connection code URL.
    #[must_use]
    pub fn get_connection_code() -> (Self, oneshot::Receiver<ConnectionCodeResult>) {
        let (tx, rx) = oneshot::channel();
        (Self::GetConnectionCode { response_tx: tx }, rx)
    }

    /// Create a command to refresh the connection code.
    ///
    /// Returns the command and a receiver for the new connection code URL.
    #[must_use]
    pub fn refresh_connection_code() -> (Self, oneshot::Receiver<ConnectionCodeResult>) {
        let (tx, rx) = oneshot::channel();
        (Self::RefreshConnectionCode { response_tx: tx }, rx)
    }

    /// Check if this is a get connection code command.
    #[must_use]
    pub fn is_get_connection_code(&self) -> bool {
        matches!(self, Self::GetConnectionCode { .. })
    }

    /// Check if this is a refresh connection code command.
    #[must_use]
    pub fn is_refresh_connection_code(&self) -> bool {
        matches!(self, Self::RefreshConnectionCode { .. })
    }
}

/// Handle for sending commands to the Hub.
///
/// Clients hold this handle and use it to send commands to the Hub's
/// command processing loop.
#[derive(Debug, Clone)]
pub struct HubCommandSender {
    tx: tokio::sync::mpsc::Sender<HubCommand>,
}

impl HubCommandSender {
    /// Create a new command sender from an mpsc sender.
    #[must_use]
    pub fn new(tx: tokio::sync::mpsc::Sender<HubCommand>) -> Self {
        Self { tx }
    }

    /// Get reference to the inner sender (for testing).
    #[must_use]
    pub fn inner(&self) -> &tokio::sync::mpsc::Sender<HubCommand> {
        &self.tx
    }

    /// Send a command without blocking.
    ///
    /// Used by TuiClient to forward requests to Hub's command channel
    /// without blocking. Since TuiClient runs on the Hub thread,
    /// it cannot use `blocking_send()` for commands that require a
    /// response (would deadlock waiting for `process_commands()`).
    pub fn try_send(&self, cmd: HubCommand) -> Result<(), String> {
        self.tx
            .try_send(cmd)
            .map_err(|e| format!("Hub command channel: {}", e))
    }

    /// Send a create agent command and await the response.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed or the response
    /// channel is dropped.
    pub async fn create_agent(&self, request: CreateAgentRequest) -> Result<AgentInfo, String> {
        let (cmd, rx) = HubCommand::create_agent(request);
        self.tx
            .send(cmd)
            .await
            .map_err(|_| "Hub command channel closed".to_string())?;
        rx.await
            .map_err(|_| "Response channel dropped".to_string())?
    }

    /// Send a delete agent command and await the response.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed or the response
    /// channel is dropped.
    pub async fn delete_agent(&self, request: DeleteAgentRequest) -> Result<(), String> {
        let (cmd, rx) = HubCommand::delete_agent(request);
        self.tx
            .send(cmd)
            .await
            .map_err(|_| "Hub command channel closed".to_string())?;
        rx.await
            .map_err(|_| "Response channel dropped".to_string())?
    }

    /// Send a list agents command and await the response.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed or the response
    /// channel is dropped.
    pub async fn list_agents(&self) -> Result<Vec<AgentInfo>, String> {
        let (cmd, rx) = HubCommand::list_agents();
        self.tx
            .send(cmd)
            .await
            .map_err(|_| "Hub command channel closed".to_string())?;
        rx.await.map_err(|_| "Response channel dropped".to_string())
    }

    /// Get an agent handle by display index (blocking version).
    ///
    /// Use this from synchronous code.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    /// Returns `Ok(None)` if the index is out of bounds.
    pub fn get_agent_by_index_blocking(&self, index: usize) -> Result<Option<AgentHandle>, String> {
        let (cmd, rx) = HubCommand::get_agent_by_index(index);
        self.tx
            .blocking_send(cmd)
            .map_err(|_| "Hub command channel closed".to_string())?;
        rx.blocking_recv()
            .map_err(|_| "Response channel dropped".to_string())
    }

    /// List all agents (blocking version).
    ///
    /// Use this from synchronous code.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub fn list_agents_blocking(&self) -> Result<Vec<AgentInfo>, String> {
        let (cmd, rx) = HubCommand::list_agents();
        self.tx
            .blocking_send(cmd)
            .map_err(|_| "Hub command channel closed".to_string())?;
        rx.blocking_recv()
            .map_err(|_| "Response channel dropped".to_string())
    }

    /// Request Hub shutdown.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub async fn quit(&self) -> Result<(), String> {
        let cmd = HubCommand::quit();
        self.tx
            .send(cmd)
            .await
            .map_err(|_| "Hub command channel closed".to_string())
    }

    /// Request Hub shutdown (blocking version).
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub fn quit_blocking(&self) -> Result<(), String> {
        let cmd = HubCommand::quit();
        self.tx
            .blocking_send(cmd)
            .map_err(|_| "Hub command channel closed".to_string())
    }

    /// Check if the command channel is closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    /// Dispatch a HubAction (fire-and-forget, blocking version).
    ///
    /// Use this from synchronous code to send actions to the Hub.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub fn dispatch_action_blocking(&self, action: HubAction) -> Result<(), String> {
        let cmd = HubCommand::dispatch_action(action);
        self.tx
            .blocking_send(cmd)
            .map_err(|_| "Hub command channel closed".to_string())
    }

    /// Dispatch a HubAction (fire-and-forget, async version).
    ///
    /// Use this from async client tasks.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub async fn dispatch_action_async(&self, action: HubAction) -> Result<(), String> {
        let cmd = HubCommand::dispatch_action(action);
        self.tx
            .send(cmd)
            .await
            .map_err(|_| "Hub command channel closed".to_string())
    }

    /// List available worktrees (blocking version).
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub fn list_worktrees_blocking(&self) -> Result<Vec<(String, String)>, String> {
        let (cmd, rx) = HubCommand::list_worktrees();
        self.tx
            .blocking_send(cmd)
            .map_err(|_| "Hub command channel closed".to_string())?;
        rx.blocking_recv()
            .map_err(|_| "Response channel dropped".to_string())
    }

    // ============================================================
    // Connection Code Methods
    // ============================================================

    /// Get the current connection code URL.
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
    pub async fn get_connection_code(&self) -> Result<String, String> {
        let (cmd, rx) = HubCommand::get_connection_code();
        self.tx
            .send(cmd)
            .await
            .map_err(|_| "Hub command channel closed".to_string())?;
        rx.await
            .map_err(|_| "Response channel dropped".to_string())?
    }

    /// Get the current connection code URL (blocking version).
    ///
    /// Use this from synchronous code.
    ///
    /// # Errors
    ///
    /// Returns an error if the Signal bundle is not initialized or the
    /// channel is closed.
    pub fn get_connection_code_blocking(&self) -> Result<String, String> {
        // Check if channel is closed before sending to avoid blocking forever
        // on a closed channel (happens with HubHandle::mock() or dropped receivers).
        if self.tx.is_closed() {
            return Err("Hub command channel closed".to_string());
        }

        let (cmd, rx) = HubCommand::get_connection_code();
        self.tx
            .blocking_send(cmd)
            .map_err(|_| "Hub command channel closed".to_string())?;
        rx.blocking_recv()
            .map_err(|_| "Response channel dropped".to_string())?
    }

    /// Refresh the connection code (regenerate Signal bundle).
    ///
    /// Requests regeneration of the Signal PreKeyBundle. This invalidates
    /// the previous connection code and returns a new one.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The relay is not connected
    /// - Bundle regeneration fails
    /// - The command channel is closed
    pub async fn refresh_connection_code(&self) -> Result<String, String> {
        let (cmd, rx) = HubCommand::refresh_connection_code();
        self.tx
            .send(cmd)
            .await
            .map_err(|_| "Hub command channel closed".to_string())?;
        rx.await
            .map_err(|_| "Response channel dropped".to_string())?
    }

    /// Refresh the connection code (blocking version).
    ///
    /// Use this from synchronous code.
    ///
    /// # Errors
    ///
    /// Returns an error if the relay is not connected or the channel is closed.
    pub fn refresh_connection_code_blocking(&self) -> Result<String, String> {
        let (cmd, rx) = HubCommand::refresh_connection_code();
        self.tx
            .blocking_send(cmd)
            .map_err(|_| "Hub command channel closed".to_string())?;
        rx.blocking_recv()
            .map_err(|_| "Response channel dropped".to_string())?
    }

    // ============================================================
    // Browser Client Support Methods
    // ============================================================

    /// Get the crypto service handle (blocking).
    ///
    /// Used by `BrowserClient::connect_to_pty()` for ActionCable channel setup.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub fn get_crypto_service_blocking(
        &self,
    ) -> Result<Option<crate::relay::crypto_service::CryptoServiceHandle>, String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .blocking_send(HubCommand::GetCryptoService { response_tx: tx })
            .map_err(|_| "Hub command channel closed".to_string())?;
        rx.blocking_recv()
            .map_err(|_| "Response channel dropped".to_string())
    }

    /// Get the server hub ID (blocking).
    ///
    /// Used by `BrowserClient::connect_to_pty()` for ActionCable channel setup.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub fn get_server_hub_id_blocking(&self) -> Result<Option<String>, String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .blocking_send(HubCommand::GetServerHubId { response_tx: tx })
            .map_err(|_| "Hub command channel closed".to_string())?;
        rx.blocking_recv()
            .map_err(|_| "Response channel dropped".to_string())
    }

    /// Get the server URL (blocking).
    ///
    /// Used by `BrowserClient::connect_to_pty()` for ActionCable channel setup.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub fn get_server_url_blocking(&self) -> Result<String, String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .blocking_send(HubCommand::GetServerUrl { response_tx: tx })
            .map_err(|_| "Hub command channel closed".to_string())?;
        rx.blocking_recv()
            .map_err(|_| "Response channel dropped".to_string())
    }

    /// Get the API key (blocking).
    ///
    /// Used by `BrowserClient::connect_to_pty()` for ActionCable channel setup.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub fn get_api_key_blocking(&self) -> Result<String, String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .blocking_send(HubCommand::GetApiKey { response_tx: tx })
            .map_err(|_| "Hub command channel closed".to_string())?;
        rx.blocking_recv()
            .map_err(|_| "Response channel dropped".to_string())
    }

    /// Get the tokio runtime handle (blocking).
    ///
    /// Used by `BrowserClient::connect_to_pty()` for async task spawning.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed.
    pub fn get_tokio_runtime_blocking(&self) -> Result<Option<tokio::runtime::Handle>, String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .blocking_send(HubCommand::GetTokioRuntime { response_tx: tx })
            .map_err(|_| "Hub command channel closed".to_string())?;
        rx.blocking_recv()
            .map_err(|_| "Response channel dropped".to_string())
    }
}

/// Result type for connection code operations.
pub type ConnectionCodeResult = Result<String, String>;

#[cfg(test)]
mod tests {
    use super::*;

    // ============================================================
    // Connection Code Command Tests (TDD - tests written first)
    // ============================================================

    #[test]
    fn test_hub_command_get_connection_code() {
        let (cmd, _rx) = HubCommand::get_connection_code();

        assert!(cmd.is_get_connection_code());
        assert!(!cmd.is_list_agents());
        assert!(!cmd.is_create_agent());
    }

    #[test]
    fn test_hub_command_refresh_connection_code() {
        let (cmd, _rx) = HubCommand::refresh_connection_code();

        assert!(cmd.is_refresh_connection_code());
        assert!(!cmd.is_get_connection_code());
    }

    #[tokio::test]
    async fn test_hub_command_get_connection_code_response_flow() {
        let (cmd, rx) = HubCommand::get_connection_code();

        // Simulate Hub processing the command
        if let HubCommand::GetConnectionCode { response_tx } = cmd {
            let url = "https://botster.dev/hubs/123#GEZDGNBVGY3TQOJQ".to_string();
            let _ = response_tx.send(Ok(url));
        }

        // Client receives response
        let result = rx.await.unwrap();
        assert!(result.is_ok());
        assert!(result.unwrap().contains("botster.dev"));
    }

    #[tokio::test]
    async fn test_hub_command_get_connection_code_no_bundle() {
        let (cmd, rx) = HubCommand::get_connection_code();

        // Simulate Hub responding with no bundle available
        if let HubCommand::GetConnectionCode { response_tx } = cmd {
            let _ = response_tx.send(Err("Signal bundle not initialized".to_string()));
        }

        let result = rx.await.unwrap();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Signal bundle"));
    }

    #[tokio::test]
    async fn test_hub_command_refresh_connection_code_response_flow() {
        let (cmd, rx) = HubCommand::refresh_connection_code();

        // Simulate Hub processing the command
        if let HubCommand::RefreshConnectionCode { response_tx } = cmd {
            let new_url = "https://botster.dev/hubs/123#NEWBUNDLEDATA".to_string();
            let _ = response_tx.send(Ok(new_url));
        }

        // Client receives response
        let result = rx.await.unwrap();
        assert!(result.is_ok());
        assert!(result.unwrap().contains("NEWBUNDLEDATA"));
    }

    // ============================================================
    // Original Tests
    // ============================================================

    #[test]
    fn test_create_agent_request_builder() {
        let req = CreateAgentRequest::new("42").with_prompt("Fix the bug");

        assert_eq!(req.issue_or_branch, "42");
        assert_eq!(req.prompt, Some("Fix the bug".to_string()));
        assert!(req.from_worktree.is_none());
    }

    #[test]
    fn test_create_agent_request_from_worktree() {
        let path = PathBuf::from("/tmp/worktree");
        let req = CreateAgentRequest::new("feature-branch").from_worktree(path.clone());

        assert_eq!(req.issue_or_branch, "feature-branch");
        assert_eq!(req.from_worktree, Some(path));
    }

    #[test]
    fn test_delete_agent_request_builder() {
        let req = DeleteAgentRequest::new("agent-123").with_worktree_deletion();

        assert_eq!(req.agent_id, "agent-123");
        assert!(req.delete_worktree);
    }

    #[test]
    fn test_delete_agent_request_default() {
        let req = DeleteAgentRequest::new("agent-456");

        assert_eq!(req.agent_id, "agent-456");
        assert!(!req.delete_worktree);
    }

    #[test]
    fn test_hub_command_create_agent() {
        let req = CreateAgentRequest::new("42");
        let (cmd, _rx) = HubCommand::create_agent(req);

        assert!(cmd.is_create_agent());
        assert!(!cmd.is_delete_agent());
        assert!(!cmd.is_list_agents());
    }

    #[test]
    fn test_hub_command_delete_agent() {
        let req = DeleteAgentRequest::new("agent-123");
        let (cmd, _rx) = HubCommand::delete_agent(req);

        assert!(cmd.is_delete_agent());
        assert!(!cmd.is_create_agent());
    }

    #[test]
    fn test_hub_command_list_agents() {
        let (cmd, _rx) = HubCommand::list_agents();

        assert!(cmd.is_list_agents());
        assert!(!cmd.is_create_agent());
    }

    #[test]
    fn test_hub_command_get_agent_by_index() {
        let (cmd, _rx) = HubCommand::get_agent_by_index(0);

        assert!(cmd.is_get_agent_by_index());
        assert!(!cmd.is_list_agents());
    }

    #[tokio::test]
    async fn test_hub_command_response_flow() {
        let req = CreateAgentRequest::new("42");
        let (cmd, rx) = HubCommand::create_agent(req);

        // Simulate Hub processing the command
        if let HubCommand::CreateAgent { response_tx, .. } = cmd {
            let info = AgentInfo {
                id: "new-agent".to_string(),
                repo: None,
                issue_number: Some(42),
                branch_name: None,
                name: None,
                status: None,
                port: None,
                server_running: None,
                has_server_pty: None,
                active_pty_view: None,
                scroll_offset: None,
                hub_identifier: None,
            };
            let _ = response_tx.send(Ok(info));
        }

        // Client receives response
        let result = rx.await.unwrap();
        assert!(result.is_ok());
        assert_eq!(result.unwrap().id, "new-agent");
    }

    #[tokio::test]
    async fn test_hub_command_sender() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<HubCommand>(16);
        let sender = HubCommandSender::new(tx);

        // Spawn a task to handle the command
        let handle = tokio::spawn(async move {
            if let Some(cmd) = rx.recv().await {
                if let HubCommand::ListAgents { response_tx } = cmd {
                    let _ = response_tx.send(vec![]);
                }
            }
        });

        // Send command via sender
        let result = sender.list_agents().await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());

        handle.await.unwrap();
    }
}
