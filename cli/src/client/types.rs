//! Types for client communication.
//!
//! These types define the request/response protocol between clients and Hub.
//! Note: AgentInfo and WorktreeInfo are re-exported from relay::types.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Response to a client action.
///
/// Hub sends these to clients after processing their requests.
/// Browser clients serialize and send via WebSocket.
/// TUI client may show toast/notification (or ignore since it re-renders).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Response {
    /// Agent was successfully selected.
    #[serde(rename = "agent_selected")]
    AgentSelected {
        /// Selected agent's session key.
        id: String,
    },

    /// Agent was successfully created.
    #[serde(rename = "agent_created")]
    AgentCreated {
        /// Created agent's session key.
        id: String,
    },

    /// Agent was successfully deleted.
    #[serde(rename = "agent_deleted")]
    AgentDeleted {
        /// Deleted agent's session key.
        id: String,
    },

    /// An error occurred processing the request.
    #[serde(rename = "error")]
    Error {
        /// Human-readable error message.
        message: String,
    },
}

impl Response {
    /// Create an error response.
    pub fn error(message: impl Into<String>) -> Self {
        Response::Error {
            message: message.into(),
        }
    }

    /// Create an agent selected response.
    pub fn agent_selected(id: impl Into<String>) -> Self {
        Response::AgentSelected { id: id.into() }
    }

    /// Create an agent created response.
    pub fn agent_created(id: impl Into<String>) -> Self {
        Response::AgentCreated { id: id.into() }
    }

    /// Create an agent deleted response.
    pub fn agent_deleted(id: impl Into<String>) -> Self {
        Response::AgentDeleted { id: id.into() }
    }
}

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
}

impl CreateAgentRequest {
    /// Create a new agent request for an issue or branch.
    pub fn new(issue_or_branch: impl Into<String>) -> Self {
        Self {
            issue_or_branch: issue_or_branch.into(),
            prompt: None,
            from_worktree: None,
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
}

/// Request to delete an agent.
///
/// Sent by clients when user wants to delete an agent.
#[derive(Debug, Clone, PartialEq)]
pub struct DeleteAgentRequest {
    /// Session key of the agent to delete.
    pub agent_key: String,

    /// Whether to also delete the worktree (files on disk).
    /// If false, only the PTY/process is terminated.
    pub delete_worktree: bool,
}

impl DeleteAgentRequest {
    /// Create a delete request (keeping worktree by default).
    pub fn new(agent_key: impl Into<String>) -> Self {
        Self {
            agent_key: agent_key.into(),
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
    fn test_response_serialization() {
        let resp = Response::AgentSelected {
            id: "agent-123".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"agent_selected""#));
        assert!(json.contains(r#""id":"agent-123""#));
    }

    #[test]
    fn test_response_error_serialization() {
        let resp = Response::error("Something went wrong");
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"error""#));
        assert!(json.contains(r#""message":"Something went wrong""#));
    }

    #[test]
    fn test_create_agent_request_builder() {
        let req = CreateAgentRequest::new("42")
            .with_prompt("Fix the bug");

        assert_eq!(req.issue_or_branch, "42");
        assert_eq!(req.prompt, Some("Fix the bug".to_string()));
        assert!(req.from_worktree.is_none());
    }

    #[test]
    fn test_delete_agent_request_builder() {
        let req = DeleteAgentRequest::new("agent-123").with_worktree_deletion();

        assert_eq!(req.agent_key, "agent-123");
        assert!(req.delete_worktree);
    }
}
