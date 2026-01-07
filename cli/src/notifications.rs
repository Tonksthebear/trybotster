//! Agent notification handling for botster-hub.
//!
//! This module provides functionality for collecting and sending agent notifications
//! to the Rails server. Notifications are used to alert users when agents need attention,
//! typically triggered by OSC 9 or OSC 777 escape sequences.
//!
//! # Notification Types
//!
//! - **OSC 9**: Standard desktop notification escape sequence
//! - **OSC 777**: Extended notification (rxvt-unicode style)
//!
//! Both are treated as "question_asked" notifications, indicating the agent
//! needs user input.
//!
//! # Example
//!
//! ```ignore
//! let sender = NotificationSender::new(
//!     client,
//!     "https://api.example.com".to_string(),
//!     "api-key".to_string(),
//! );
//!
//! sender.send_notification(
//!     "owner/repo",
//!     Some(42),
//!     Some("https://invocation.url"),
//!     NotificationType::QuestionAsked,
//! )?;
//! ```

use anyhow::Result;
use reqwest::blocking::Client;

// Re-export the notification type from agent module
pub use crate::agent::AgentNotification;

/// Types of notifications that can be sent to the Rails server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationType {
    /// Agent is asking a question and needs user input.
    QuestionAsked,
}

impl NotificationType {
    /// Returns the string representation used in the API payload.
    pub fn as_str(&self) -> &'static str {
        match self {
            NotificationType::QuestionAsked => "question_asked",
        }
    }
}

impl std::fmt::Display for NotificationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Handles sending notifications to the Rails server.
///
/// Encapsulates the HTTP client and server configuration needed
/// to send agent notifications.
#[derive(Debug, Clone)]
pub struct NotificationSender {
    client: Client,
    server_url: String,
    api_key: String,
}

impl NotificationSender {
    /// Creates a new notification sender with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `client` - HTTP client for making requests
    /// * `server_url` - Base URL of the Rails server
    /// * `api_key` - API key for authentication
    pub fn new(client: Client, server_url: String, api_key: String) -> Self {
        Self {
            client,
            server_url,
            api_key,
        }
    }

    /// Sends a notification to the Rails server.
    ///
    /// The notification includes:
    /// - Repository identifier
    /// - Issue number (if available)
    /// - Invocation URL (preferred identifier if available)
    /// - Notification type
    ///
    /// # Arguments
    ///
    /// * `repo` - Repository in "owner/repo" format
    /// * `issue_number` - Optional issue number
    /// * `invocation_url` - Optional invocation URL (preferred over issue_number)
    /// * `notification_type` - Type of notification to send
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The HTTP request fails
    /// - The server returns a non-success status
    pub fn send(
        &self,
        repo: &str,
        issue_number: Option<u32>,
        invocation_url: Option<&str>,
        notification_type: NotificationType,
    ) -> Result<()> {
        let url = format!("{}/api/agent_notifications", self.server_url);

        // Build payload - include both old and new fields for backwards compatibility
        let payload = serde_json::json!({
            "repo": repo,
            "issue_number": issue_number,
            "invocation_url": invocation_url,
            "notification_type": notification_type.as_str(),
        });

        let response = self
            .client
            .post(&url)
            .header("X-API-Key", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()?;

        if response.status().is_success() {
            log::info!(
                "Sent notification to Rails: repo={}, issue={:?}, url={:?}, type={}",
                repo,
                issue_number,
                invocation_url,
                notification_type
            );
            Ok(())
        } else {
            anyhow::bail!(
                "Failed to send notification: {} - {}",
                response.status(),
                response.text().unwrap_or_default()
            )
        }
    }
}

/// Converts an AgentNotification to its notification type.
///
/// Currently, all terminal notifications (OSC 9 and OSC 777) are treated
/// as "question_asked" since they indicate the agent needs user attention.
pub fn notification_type_from_agent(notification: &AgentNotification) -> NotificationType {
    match notification {
        AgentNotification::Osc9(_) | AgentNotification::Osc777 { .. } => {
            NotificationType::QuestionAsked
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notification_type_as_str() {
        assert_eq!(NotificationType::QuestionAsked.as_str(), "question_asked");
    }

    #[test]
    fn test_notification_type_display() {
        assert_eq!(
            format!("{}", NotificationType::QuestionAsked),
            "question_asked"
        );
    }

    #[test]
    fn test_notification_type_equality() {
        assert_eq!(NotificationType::QuestionAsked, NotificationType::QuestionAsked);
    }

    #[test]
    fn test_notification_type_from_osc9() {
        let notification = AgentNotification::Osc9(Some("test message".to_string()));
        let notification_type = notification_type_from_agent(&notification);
        assert_eq!(notification_type, NotificationType::QuestionAsked);
    }

    #[test]
    fn test_notification_type_from_osc9_none() {
        let notification = AgentNotification::Osc9(None);
        let notification_type = notification_type_from_agent(&notification);
        assert_eq!(notification_type, NotificationType::QuestionAsked);
    }

    #[test]
    fn test_notification_type_from_osc777() {
        let notification = AgentNotification::Osc777 {
            title: "Test".to_string(),
            body: "Test message".to_string(),
        };
        let notification_type = notification_type_from_agent(&notification);
        assert_eq!(notification_type, NotificationType::QuestionAsked);
    }

    #[test]
    fn test_notification_sender_creation() {
        let client = Client::new();
        let sender = NotificationSender::new(
            client,
            "https://example.com".to_string(),
            "test-key".to_string(),
        );

        assert_eq!(sender.server_url, "https://example.com");
        assert_eq!(sender.api_key, "test-key");
    }
}
