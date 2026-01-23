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
//! - [`HubCommand::GetAgent`] - Get agent handle for PTY access
//!
//! # Hierarchy
//!
//! ```text
//! Client → HubCommand::GetAgent(id) → AgentHandle
//!                                        ├── cli_pty() → PtyHandle → subscribe()
//!                                        └── server_pty() → Option<PtyHandle>
//! ```
//!
//! # Actor Pattern
//!
//! Commands use oneshot channels for responses, enabling request/response semantics:
//!
//! ```ignore
//! // Client sends command
//! let (cmd, rx) = HubCommand::get_agent("agent-123");
//! hub_tx.send(cmd).await;
//! let handle = rx.await?;
//!
//! // Client subscribes to PTY events via handle
//! let mut pty_rx = handle.cli_pty().subscribe();
//! ```

// Rust guideline compliant 2026-01

use std::path::PathBuf;
use tokio::sync::oneshot;

use super::actions::HubAction;
use super::agent_handle::AgentHandle;
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
}

impl CreateAgentRequest {
    /// Create a new agent request for an issue or branch.
    #[must_use]
    pub fn new(issue_or_branch: impl Into<String>) -> Self {
        Self {
            issue_or_branch: issue_or_branch.into(),
            prompt: None,
            from_worktree: None,
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

/// Result of getting an agent.
pub type GetAgentResult = Result<AgentHandle, String>;

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
    /// Get an agent handle for PTY access.
    ///
    /// Returns an `AgentHandle` that provides:
    /// - Agent info snapshot
    /// - CLI PTY handle (for subscribing to events)
    /// - Server PTY handle (if available)
    ///
    /// Clients subscribe to PTY events via the handle, then send input
    /// via `SendInput` command.
    GetAgent {
        /// Agent ID.
        agent_id: String,
        /// Channel for sending the agent handle back.
        response_tx: oneshot::Sender<GetAgentResult>,
    },

    /// Get an agent handle by display index.
    ///
    /// Similar to `GetAgent` but uses the display index instead of agent ID.
    /// Returns `None` if the index is out of bounds.
    ///
    /// Useful for clients that navigate agents by position rather than ID.
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

    /// Create a command to get an agent handle.
    ///
    /// Returns the command and a receiver for the `AgentHandle`.
    /// The handle provides PTY access via `cli_pty().subscribe()`.
    #[must_use]
    pub fn get_agent(agent_id: impl Into<String>) -> (Self, oneshot::Receiver<GetAgentResult>) {
        let (tx, rx) = oneshot::channel();
        (
            Self::GetAgent {
                agent_id: agent_id.into(),
                response_tx: tx,
            },
            rx,
        )
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

    /// Check if this is a get agent command.
    #[must_use]
    pub fn is_get_agent(&self) -> bool {
        matches!(self, Self::GetAgent { .. })
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

    /// Get an agent handle for PTY access.
    ///
    /// Returns an `AgentHandle` that provides access to:
    /// - Agent info snapshot
    /// - CLI PTY (always present)
    /// - Server PTY (if server running)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let handle = sender.get_agent("agent-123").await?;
    /// let mut rx = handle.cli_pty().subscribe();
    /// while let Ok(event) = rx.recv().await {
    ///     // Handle PTY events
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The command channel is closed
    /// - The agent doesn't exist
    pub async fn get_agent(&self, agent_id: impl Into<String>) -> Result<AgentHandle, String> {
        let (cmd, rx) = HubCommand::get_agent(agent_id);
        self.tx
            .send(cmd)
            .await
            .map_err(|_| "Hub command channel closed".to_string())?;
        rx.await
            .map_err(|_| "Response channel dropped".to_string())?
    }

    /// Get an agent handle (blocking version).
    ///
    /// Use this from synchronous code.
    ///
    /// # Errors
    ///
    /// Returns an error if the command channel is closed or the agent doesn't exist.
    pub fn get_agent_blocking(&self, agent_id: impl Into<String>) -> Result<AgentHandle, String> {
        let (cmd, rx) = HubCommand::get_agent(agent_id);
        self.tx
            .blocking_send(cmd)
            .map_err(|_| "Hub command channel closed".to_string())?;
        rx.blocking_recv()
            .map_err(|_| "Response channel dropped".to_string())?
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_hub_command_get_agent() {
        let (cmd, _rx) = HubCommand::get_agent("agent-789");

        assert!(cmd.is_get_agent());
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
                tunnel_port: None,
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
