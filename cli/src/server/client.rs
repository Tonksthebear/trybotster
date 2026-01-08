//! API client for communicating with the Rails server.
//!
//! This module provides the [`ApiClient`] struct which handles all HTTP
//! communication with the botster Rails backend.

use anyhow::Result;
use reqwest::blocking::Client;

use super::types::{AgentHeartbeatInfo, HeartbeatPayload, MessageResponse, NotificationPayload};
use crate::constants;

/// API client for the botster Rails server.
///
/// Encapsulates HTTP client configuration and provides methods for
/// all server communication operations.
#[derive(Debug, Clone)]
pub struct ApiClient {
    client: Client,
    server_url: String,
    api_key: String,
}

impl ApiClient {
    /// Creates a new API client with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `server_url` - Base URL of the Rails server
    /// * `api_key` - API key for authentication
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be created.
    pub fn new(server_url: String, api_key: String) -> Result<Self> {
        let client = Client::builder()
            .timeout(constants::HTTP_REQUEST_TIMEOUT)
            .build()?;

        Ok(Self {
            client,
            server_url,
            api_key,
        })
    }

    /// Creates an API client with a pre-configured HTTP client.
    ///
    /// Useful for testing or when custom client configuration is needed.
    pub fn with_client(client: Client, server_url: String, api_key: String) -> Self {
        Self {
            client,
            server_url,
            api_key,
        }
    }

    /// Returns the server URL.
    pub fn server_url(&self) -> &str {
        &self.server_url
    }

    /// Polls for pending messages for a specific repository.
    ///
    /// # Arguments
    ///
    /// * `hub_identifier` - Hub identifier for routing
    /// * `repo` - Repository name in "owner/repo" format
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be parsed.
    pub fn poll_messages(&self, hub_identifier: &str, repo: &str) -> Result<MessageResponse> {
        let url = format!(
            "{}/hubs/{}/messages?repo={}",
            self.server_url, hub_identifier, repo
        );

        let response = self
            .client
            .get(&url)
            .header("X-API-Key", &self.api_key)
            .send()?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to poll messages: {}", response.status());
        }

        let message_response: MessageResponse = response.json()?;
        Ok(message_response)
    }

    /// Acknowledges a message to trigger the eyes emoji reaction.
    ///
    /// # Arguments
    ///
    /// * `hub_identifier` - Hub identifier for routing
    /// * `message_id` - ID of the message to acknowledge
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails.
    pub fn acknowledge_message(&self, hub_identifier: &str, message_id: i64) -> Result<()> {
        let url = format!(
            "{}/hubs/{}/messages/{}",
            self.server_url, hub_identifier, message_id
        );

        let response = self
            .client
            .patch(&url)
            .header("X-API-Key", &self.api_key)
            .header("Content-Type", "application/json")
            .send()?;

        if response.status().is_success() {
            log::debug!("Acknowledged message {}", message_id);
            Ok(())
        } else {
            anyhow::bail!(
                "Failed to acknowledge message {}: {}",
                message_id,
                response.status()
            )
        }
    }

    /// Sends a heartbeat to register the hub and its agents.
    ///
    /// Uses RESTful PUT for upsert semantics.
    ///
    /// # Arguments
    ///
    /// * `hub_identifier` - Unique identifier for this hub instance
    /// * `repo` - Repository this hub is monitoring
    /// * `agents` - List of active agents
    ///
    /// # Returns
    ///
    /// Returns Ok(true) if the heartbeat was successful, Ok(false) if it failed
    /// but we should continue, or an error for fatal failures.
    pub fn send_heartbeat(
        &self,
        hub_identifier: &str,
        repo: &str,
        agents: Vec<AgentHeartbeatInfo>,
    ) -> Result<bool> {
        let url = format!("{}/hubs/{}", self.server_url, hub_identifier);

        let payload = HeartbeatPayload {
            repo: repo.to_string(),
            agents,
        };

        log::debug!("Sending heartbeat to {}", url);

        match self
            .client
            .put(&url)
            .header("X-API-Key", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
        {
            Ok(response) => {
                if response.status().is_success() {
                    log::debug!(
                        "Heartbeat sent successfully: {} agents registered",
                        payload.agents.len()
                    );
                    Ok(true)
                } else {
                    log::warn!(
                        "Heartbeat failed: {} - {}",
                        response.status(),
                        response.text().unwrap_or_default()
                    );
                    Ok(false)
                }
            }
            Err(e) => {
                log::warn!("Failed to send heartbeat: {}", e);
                Ok(false)
            }
        }
    }

    /// Sends an agent notification to trigger a GitHub comment.
    ///
    /// # Arguments
    ///
    /// * `hub_identifier` - Hub identifier for routing
    /// * `repo` - Repository in "owner/repo" format
    /// * `issue_number` - Optional issue number
    /// * `invocation_url` - Optional invocation URL (preferred identifier)
    /// * `notification_type` - Type of notification (e.g., "question_asked")
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails.
    pub fn send_notification(
        &self,
        hub_identifier: &str,
        repo: &str,
        issue_number: Option<u32>,
        invocation_url: Option<&str>,
        notification_type: &str,
    ) -> Result<()> {
        let url = format!(
            "{}/hubs/{}/notifications",
            self.server_url, hub_identifier
        );

        let payload = NotificationPayload {
            repo: repo.to_string(),
            issue_number,
            invocation_url: invocation_url.map(String::from),
            notification_type: notification_type.to_string(),
        };

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_client_creation() {
        let client = ApiClient::new(
            "https://example.com".to_string(),
            "test-key".to_string(),
        );

        assert!(client.is_ok());
        let client = client.unwrap();
        assert_eq!(client.server_url(), "https://example.com");
    }

    #[test]
    fn test_api_client_with_custom_client() {
        let http_client = Client::new();
        let client = ApiClient::with_client(
            http_client,
            "https://custom.example.com".to_string(),
            "custom-key".to_string(),
        );

        assert_eq!(client.server_url(), "https://custom.example.com");
    }
}
