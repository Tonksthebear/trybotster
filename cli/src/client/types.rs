//! Types for client communication.
//!
//! These types define the request/response protocol between clients and Hub.
//! Note: AgentInfo and WorktreeInfo are re-exported from relay::types.

use std::path::PathBuf;

/// Request to create an agent.
///
/// Sent by clients when user wants to create a new agent.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateAgentRequest {
    /// Issue number or branch name for the new agent.
    pub issue_or_branch: String,

    /// Optional initial prompt for the agent.
    pub prompt: Option<String>,

    /// Optional path to an existing worktree to reopen.
    /// If provided, reuses existing worktree instead of creating new.
    pub from_worktree: Option<PathBuf>,

    /// Terminal dimensions (rows, cols) from the requesting client.
    /// Used to size the PTY when spawning the agent.
    /// If None, a default of (24, 80) is used.
    pub dims: Option<(u16, u16)>,
}

impl CreateAgentRequest {
    /// Create a new agent request for an issue or branch.
    pub fn new(issue_or_branch: impl Into<String>) -> Self {
        Self {
            issue_or_branch: issue_or_branch.into(),
            prompt: None,
            from_worktree: None,
            dims: None,
        }
    }

    /// Add an initial prompt.
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = Some(prompt.into());
        self
    }

    /// Reopen an existing worktree.
    pub fn from_worktree(mut self, path: PathBuf) -> Self {
        self.from_worktree = Some(path);
        self
    }

    /// Set terminal dimensions for PTY sizing.
    pub fn with_dims(mut self, dims: (u16, u16)) -> Self {
        self.dims = Some(dims);
        self
    }
}

/// Request to delete an agent.
///
/// Sent by clients when user wants to delete an agent.
#[derive(Debug, Clone, PartialEq)]
pub struct DeleteAgentRequest {
    /// Agent ID of the agent to delete.
    pub agent_id: String,

    /// Whether to also delete the worktree (files on disk).
    /// If false, only the PTY/process is terminated.
    pub delete_worktree: bool,
}

impl DeleteAgentRequest {
    /// Create a delete request (keeping worktree by default).
    pub fn new(agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            delete_worktree: false,
        }
    }

    /// Also delete the worktree.
    pub fn with_worktree_deletion(mut self) -> Self {
        self.delete_worktree = true;
        self
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
        assert!(req.dims.is_none());
    }

    #[test]
    fn test_create_agent_request_with_dims() {
        let req = CreateAgentRequest::new("42").with_dims((40, 120));

        assert_eq!(req.dims, Some((40, 120)));
    }

    #[test]
    fn test_delete_agent_request_builder() {
        let req = DeleteAgentRequest::new("agent-123").with_worktree_deletion();

        assert_eq!(req.agent_id, "agent-123");
        assert!(req.delete_worktree);
    }
}
