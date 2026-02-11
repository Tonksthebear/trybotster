//! Server message parsing logic.
//!
//! This module provides the [`ParsedMessage`] type for extracting structured
//! data from server message payloads. Message routing to Hub actions is
//! handled directly in `server_comms.rs`.
//!
//! # Message Flow
//!
//! ```text
//! Rails Server ──► MessageData ──► ParsedMessage ──► server_comms.rs routing
//! ```
//!
//! # Event Types
//!
//! The server sends various event types:
//! - `issue_comment` - Comment on an issue mentioning the bot
//! - `pull_request` - PR event mentioning the bot
//! - `agent_cleanup` - Issue/PR was closed, clean up the agent
//! - `webrtc_offer` - WebRTC signaling for P2P browser connections

// Rust guideline compliant 2026-02

use crate::server::types::MessageData;

/// Parsed message information extracted from server payload.
#[derive(Debug, Clone)]
pub struct ParsedMessage {
    /// Message ID for acknowledgment.
    pub message_id: i64,
    /// Event type from the server.
    pub event_type: String,
    /// Repository name (owner/repo format).
    pub repo: Option<String>,
    /// Issue number if applicable.
    pub issue_number: Option<u32>,
    /// Task prompt/description.
    pub prompt: Option<String>,
    /// URL where this interaction originated (for responding).
    pub invocation_url: Option<String>,
    /// Comment author if from a comment.
    pub comment_author: Option<String>,
    /// Comment body if from a comment.
    pub comment_body: Option<String>,
}

impl ParsedMessage {
    /// Parse a `MessageData` into a `ParsedMessage`.
    ///
    /// Extracts all relevant fields from the payload for easier processing.
    #[must_use]
    pub fn from_message_data(data: &MessageData) -> Self {
        let payload = &data.payload;

        // Extract repo - try multiple paths
        let repo = payload
            .get("repository")
            .and_then(|r| r.get("full_name"))
            .and_then(|n| n.as_str())
            .or_else(|| payload.get("repo").and_then(|r| r.as_str()))
            .map(String::from);

        // Extract issue number
        let issue_number = payload
            .get("issue_number")
            .and_then(serde_json::Value::as_u64)
            .or_else(|| {
                payload
                    .get("issue")
                    .and_then(|i| i.get("number"))
                    .and_then(serde_json::Value::as_u64)
            })
            .or_else(|| {
                payload
                    .get("pull_request")
                    .and_then(|pr| pr.get("number"))
                    .and_then(serde_json::Value::as_u64)
            })
            .map(|n| n as u32);

        // Extract prompt/task description (prefer explicit prompt over comment_body)
        let prompt = payload
            .get("prompt")
            .and_then(|p| p.as_str())
            .or_else(|| payload.get("context").and_then(|c| c.as_str()))
            .map(String::from);

        // Extract invocation URL
        let invocation_url = payload
            .get("issue_url")
            .and_then(|u| u.as_str())
            .map(String::from);

        // Extract comment author
        let comment_author = payload
            .get("comment_author")
            .and_then(|a| a.as_str())
            .map(String::from);

        // Extract comment body
        let comment_body = payload
            .get("comment_body")
            .and_then(|b| b.as_str())
            .map(String::from);

        Self {
            message_id: data.id,
            event_type: data.event_type.clone(),
            repo,
            issue_number,
            prompt,
            invocation_url,
            comment_author,
            comment_body,
        }
    }

    /// Check if this message is a cleanup request.
    #[must_use]
    pub fn is_cleanup(&self) -> bool {
        self.event_type == "agent_cleanup"
    }

    /// Check if this message is a WebRTC offer.
    #[must_use]
    pub fn is_webrtc_offer(&self) -> bool {
        self.event_type == "webrtc_offer"
    }

    /// Get a notification string for pinging an existing agent.
    ///
    /// Used when an agent already exists for this issue and we need
    /// to notify it of a new mention.
    #[must_use]
    pub fn format_notification(&self) -> String {
        if let Some(prompt) = &self.prompt {
            format!(
                "=== NEW MENTION (automated notification) ===\n\n{}\n\n==================",
                prompt
            )
        } else {
            let author = self.comment_author.as_deref().unwrap_or("unknown");
            let body = self.comment_body.as_deref().unwrap_or("New mention");
            format!(
                "=== NEW MENTION (automated notification) ===\n{} mentioned you: {}\n==================",
                author, body
            )
        }
    }

    /// Get the task description for spawning a new agent.
    #[must_use]
    pub fn task_description(&self) -> String {
        self.prompt
            .clone()
            .or_else(|| self.comment_body.clone())
            .unwrap_or_else(|| "Work on this issue".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_message(id: i64, event_type: &str, payload: serde_json::Value) -> MessageData {
        MessageData {
            id,
            event_type: event_type.to_string(),
            payload,
        }
    }

    #[test]
    fn test_parse_message_with_issue_number() {
        let data = make_message(
            1,
            "issue_comment",
            serde_json::json!({
                "issue_number": 42,
                "prompt": "Fix the bug",
                "issue_url": "https://github.com/owner/repo/issues/42"
            }),
        );

        let parsed = ParsedMessage::from_message_data(&data);

        assert_eq!(parsed.message_id, 1);
        assert_eq!(parsed.event_type, "issue_comment");
        assert_eq!(parsed.issue_number, Some(42));
        assert_eq!(parsed.prompt, Some("Fix the bug".to_string()));
        assert_eq!(
            parsed.invocation_url,
            Some("https://github.com/owner/repo/issues/42".to_string())
        );
    }

    #[test]
    fn test_parse_message_with_nested_issue() {
        let data = make_message(
            2,
            "issue_comment",
            serde_json::json!({
                "issue": {
                    "number": 123
                },
                "repository": {
                    "full_name": "owner/repo"
                }
            }),
        );

        let parsed = ParsedMessage::from_message_data(&data);

        assert_eq!(parsed.issue_number, Some(123));
        assert_eq!(parsed.repo, Some("owner/repo".to_string()));
    }

    #[test]
    fn test_parse_cleanup_message() {
        let data = make_message(
            3,
            "agent_cleanup",
            serde_json::json!({
                "repo": "owner/repo",
                "issue_number": 42
            }),
        );

        let parsed = ParsedMessage::from_message_data(&data);

        assert!(parsed.is_cleanup());
        assert!(!parsed.is_webrtc_offer());
    }

    #[test]
    fn test_parse_webrtc_offer() {
        let data = make_message(
            4,
            "webrtc_offer",
            serde_json::json!({
                "sdp": "offer..."
            }),
        );

        let parsed = ParsedMessage::from_message_data(&data);

        assert!(!parsed.is_cleanup());
        assert!(parsed.is_webrtc_offer());
    }

    #[test]
    fn test_format_notification_with_prompt() {
        let data = make_message(
            1,
            "issue_comment",
            serde_json::json!({
                "prompt": "Please review this PR"
            }),
        );

        let parsed = ParsedMessage::from_message_data(&data);
        let notification = parsed.format_notification();

        assert!(notification.contains("NEW MENTION"));
        assert!(notification.contains("Please review this PR"));
    }

    #[test]
    fn test_format_notification_without_prompt() {
        let data = make_message(
            1,
            "issue_comment",
            serde_json::json!({
                "comment_author": "alice",
                "comment_body": "Hey bot, help!"
            }),
        );

        let parsed = ParsedMessage::from_message_data(&data);
        let notification = parsed.format_notification();

        assert!(notification.contains("NEW MENTION"));
        assert!(notification.contains("alice"));
        assert!(notification.contains("Hey bot, help!"));
    }

    #[test]
    fn test_task_description_fallback() {
        let data = make_message(
            1,
            "issue_comment",
            serde_json::json!({
                "issue_number": 42
            }),
        );

        let parsed = ParsedMessage::from_message_data(&data);

        assert_eq!(parsed.task_description(), "Work on this issue");
    }
}
