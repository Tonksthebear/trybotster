//! Server communication types for the botster-hub API.
//!
//! This module defines the data structures used for serialization and
//! deserialization when communicating with the Rails server.

use serde::{Deserialize, Serialize};

/// Individual message data from the server.
#[derive(Debug, Clone, Deserialize)]
pub struct MessageData {
    /// Unique message identifier.
    pub id: i64,
    /// Type of event (e.g., "issue_comment", "pull_request").
    pub event_type: String,
    /// Event payload containing details.
    pub payload: serde_json::Value,
}

impl MessageData {
    /// Extracts the repository name from the payload.
    ///
    /// Returns None if the repository field is missing or invalid.
    pub fn repo(&self) -> Option<&str> {
        self.payload
            .get("repository")
            .and_then(|r| r.get("full_name"))
            .and_then(|n| n.as_str())
    }

    /// Extracts the issue number from the payload.
    ///
    /// Works for both issue events and pull request events.
    pub fn issue_number(&self) -> Option<u32> {
        // Try issue.number first
        self.payload
            .get("issue")
            .and_then(|i| i.get("number"))
            .and_then(serde_json::Value::as_u64)
            .map(|n| n as u32)
            .or_else(|| {
                // Fall back to pull_request.number
                self.payload
                    .get("pull_request")
                    .and_then(|pr| pr.get("number"))
                    .and_then(serde_json::Value::as_u64)
                    .map(|n| n as u32)
            })
    }
}

/// Agent information included in heartbeat payloads.
#[derive(Debug, Clone, Serialize)]
pub struct AgentHeartbeatInfo {
    /// Unique session key for the agent.
    pub session_key: String,
    /// Last invocation URL for the agent (if any).
    pub last_invocation_url: Option<String>,
}

impl AgentHeartbeatInfo {
    /// Creates a new agent heartbeat info.
    pub fn new(session_key: String, last_invocation_url: Option<String>) -> Self {
        Self {
            session_key,
            last_invocation_url,
        }
    }
}

/// Heartbeat request payload.
#[derive(Debug, Clone, Serialize)]
pub struct HeartbeatPayload {
    /// Repository this hub is monitoring.
    pub repo: String,
    /// List of active agents.
    pub agents: Vec<AgentHeartbeatInfo>,
}

/// Notification request payload.
#[derive(Debug, Clone, Serialize)]
pub struct NotificationPayload {
    /// Repository in "owner/repo" format.
    pub repo: String,
    /// Issue number (optional, for backward compatibility).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_number: Option<u32>,
    /// Invocation URL (preferred identifier).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invocation_url: Option<String>,
    /// Type of notification.
    pub notification_type: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_data_repo_extraction() {
        let data = MessageData {
            id: 1,
            event_type: "issue_comment".to_string(),
            payload: serde_json::json!({
                "repository": {
                    "full_name": "owner/repo"
                }
            }),
        };

        assert_eq!(data.repo(), Some("owner/repo"));
    }

    #[test]
    fn test_message_data_issue_number_from_issue() {
        let data = MessageData {
            id: 1,
            event_type: "issue_comment".to_string(),
            payload: serde_json::json!({
                "issue": {
                    "number": 42
                }
            }),
        };

        assert_eq!(data.issue_number(), Some(42));
    }

    #[test]
    fn test_message_data_issue_number_from_pr() {
        let data = MessageData {
            id: 1,
            event_type: "pull_request".to_string(),
            payload: serde_json::json!({
                "pull_request": {
                    "number": 123
                }
            }),
        };

        assert_eq!(data.issue_number(), Some(123));
    }

    #[test]
    fn test_message_data_missing_issue_number() {
        let data = MessageData {
            id: 1,
            event_type: "push".to_string(),
            payload: serde_json::json!({}),
        };

        assert_eq!(data.issue_number(), None);
    }

    #[test]
    fn test_agent_heartbeat_info_creation() {
        let info = AgentHeartbeatInfo::new(
            "owner-repo-42".to_string(),
            Some("https://example.com/invoke".to_string()),
        );

        assert_eq!(info.session_key, "owner-repo-42");
        assert_eq!(
            info.last_invocation_url,
            Some("https://example.com/invoke".to_string())
        );
    }

    #[test]
    fn test_heartbeat_payload_serialization() {
        let payload = HeartbeatPayload {
            repo: "owner/repo".to_string(),
            agents: vec![AgentHeartbeatInfo::new("key1".to_string(), None)],
        };

        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("\"repo\":\"owner/repo\""));
        assert!(json.contains("\"session_key\":\"key1\""));
    }
}
