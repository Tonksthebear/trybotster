//! Server polling and heartbeat logic.
//!
//! Handles communication with the Rails server for:
//! - Message polling (fetching pending bot messages)
//! - Message acknowledgment
//! - Heartbeat registration
//! - Agent notifications
//!
//! # Design
//!
//! Functions in this module are regular functions (M-REGULAR-FN) rather than
//! methods on Hub, making them independently testable.

use anyhow::Result;
use reqwest::blocking::Client;
use std::time::{Duration, Instant};

use crate::server::types::MessageData;

use super::Hub;

/// Notification data collected from an agent for sending to Rails.
struct AgentNotificationData {
    session_key: String,
    repo: String,
    issue_number: Option<u32>,
    invocation_url: Option<String>,
    notification_type: String,
}

/// Configuration for server polling operations.
#[derive(Debug)]
pub struct PollingConfig<'a> {
    /// HTTP client for requests.
    pub client: &'a Client,
    /// Base URL for the Rails server.
    pub server_url: &'a str,
    /// API key for authentication.
    pub api_key: &'a str,
    /// Poll interval in seconds.
    pub poll_interval: u64,
    /// Hub identifier for heartbeats.
    pub hub_identifier: &'a str,
}

/// Timing state for polling operations.
#[derive(Debug)]
pub struct PollingState {
    /// Last poll timestamp.
    pub last_poll: Instant,
    /// Last heartbeat timestamp.
    pub last_heartbeat: Instant,
}

impl Default for PollingState {
    fn default() -> Self {
        // Initialize timestamps far in the past to trigger immediate poll/heartbeat
        // on first tick. This ensures the CLI starts working immediately.
        let past = Instant::now() - Duration::from_secs(3600);
        Self {
            last_poll: past,
            last_heartbeat: past,
        }
    }
}

impl PollingState {
    /// Check if enough time has elapsed for a poll.
    pub fn should_poll(&self, interval_secs: u64) -> bool {
        self.last_poll.elapsed() >= Duration::from_secs(interval_secs)
    }

    /// Check if enough time has elapsed for a heartbeat.
    pub fn should_heartbeat(&self) -> bool {
        const HEARTBEAT_INTERVAL: u64 = 30;
        self.last_heartbeat.elapsed() >= Duration::from_secs(HEARTBEAT_INTERVAL)
    }

    /// Mark poll as completed.
    pub fn mark_polled(&mut self) {
        self.last_poll = Instant::now();
    }

    /// Mark heartbeat as sent.
    pub fn mark_heartbeat_sent(&mut self) {
        self.last_heartbeat = Instant::now();
    }
}

/// Response from polling messages.
#[derive(Debug, serde::Deserialize)]
pub struct MessageResponse {
    /// List of pending messages.
    pub messages: Vec<MessageData>,
}

/// Poll the server for pending messages.
///
/// Returns messages if polling succeeds, or an empty vec if skipped/failed.
/// Logs warnings on failure but does not propagate errors.
pub fn poll_messages(config: &PollingConfig, repo_name: &str) -> Vec<MessageData> {
    let url = format!(
        "{}/hubs/{}/messages?repo={}",
        config.server_url, config.hub_identifier, repo_name
    );

    let response = match config
        .client
        .get(&url)
        .bearer_auth(config.api_key)
        .send()
    {
        Ok(r) => r,
        Err(e) => {
            log::warn!("Failed to connect to server: {e}");
            return Vec::new();
        }
    };

    if !response.status().is_success() {
        log::warn!("Failed to poll messages: {}", response.status());
        return Vec::new();
    }

    match response.json::<MessageResponse>() {
        Ok(r) => {
            if !r.messages.is_empty() {
                log::info!("Polled {} pending messages", r.messages.len());
            }
            r.messages
        }
        Err(e) => {
            log::warn!("Failed to parse message response: {e}");
            Vec::new()
        }
    }
}

/// Acknowledge a message to the server.
///
/// Marks the message as processed so it won't be returned again.
pub fn acknowledge_message(config: &PollingConfig, message_id: i64) {
    let url = format!(
        "{}/hubs/{}/messages/{message_id}",
        config.server_url, config.hub_identifier
    );

    match config
        .client
        .patch(&url)
        .bearer_auth(config.api_key)
        .header("Content-Type", "application/json")
        .send()
    {
        Ok(response) if response.status().is_success() => {
            log::debug!("Acknowledged message {message_id}");
        }
        Ok(response) => {
            log::warn!(
                "Failed to acknowledge message {message_id}: {}",
                response.status()
            );
        }
        Err(e) => {
            log::warn!("Failed to acknowledge message {message_id}: {e}");
        }
    }
}

/// Agent info for heartbeat payload.
#[derive(Debug)]
pub struct HeartbeatAgentInfo {
    /// Session key identifying the agent.
    pub session_key: String,
    /// URL of the last invocation that triggered this agent.
    pub last_invocation_url: Option<String>,
}

/// Send heartbeat to register hub with server.
///
/// Reports active agents and hub status.
pub fn send_heartbeat(
    config: &PollingConfig,
    repo_name: &str,
    agents: &[HeartbeatAgentInfo],
    device_id: Option<i64>,
) {
    let agents_list: Vec<serde_json::Value> = agents
        .iter()
        .map(|agent| {
            serde_json::json!({
                "session_key": agent.session_key,
                "last_invocation_url": agent.last_invocation_url,
            })
        })
        .collect();

    let url = format!("{}/hubs/{}", config.server_url, config.hub_identifier);
    let payload = serde_json::json!({
        "repo": repo_name,
        "agents": agents_list,
        "device_id": device_id,
    });

    match config
        .client
        .put(&url)
        .bearer_auth(config.api_key)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
    {
        Ok(response) if response.status().is_success() => {
            log::debug!("Heartbeat sent: {} agents registered", agents_list.len());
        }
        Ok(response) => {
            log::warn!("Heartbeat failed: {}", response.status());
        }
        Err(e) => {
            log::warn!("Failed to send heartbeat: {e}");
        }
    }
}

/// Notification to send to Rails.
#[derive(Debug)]
pub struct AgentNotificationPayload<'a> {
    /// Repository name.
    pub repo: &'a str,
    /// GitHub issue number.
    pub issue_number: Option<u32>,
    /// URL that triggered the agent.
    pub invocation_url: Option<&'a str>,
    /// Type of notification (e.g., "question_asked").
    pub notification_type: &'a str,
}

/// Send an agent notification to Rails.
///
/// Used when agents emit OSC notifications that should be forwarded.
pub fn send_agent_notification(config: &PollingConfig, payload: &AgentNotificationPayload) -> Result<()> {
    let url = format!(
        "{}/hubs/{}/notifications",
        config.server_url, config.hub_identifier
    );

    let json_payload = serde_json::json!({
        "repo": payload.repo,
        "issue_number": payload.issue_number,
        "invocation_url": payload.invocation_url,
        "notification_type": payload.notification_type,
    });

    let response = config
        .client
        .post(&url)
        .bearer_auth(config.api_key)
        .header("Content-Type", "application/json")
        .json(&json_payload)
        .send()?;

    if response.status().is_success() {
        log::info!(
            "Sent notification to Rails: type={}",
            payload.notification_type
        );
        Ok(())
    } else {
        anyhow::bail!("Failed to send notification: {}", response.status())
    }
}

/// Check if polling should be skipped.
///
/// Returns true if:
/// - Quit flag is set
/// - Polling is disabled
/// - Offline mode is enabled
pub fn should_skip_polling(quit: bool, polling_enabled: bool) -> bool {
    if quit || !polling_enabled {
        return true;
    }

    if std::env::var("BOTSTER_OFFLINE_MODE").is_ok() {
        return true;
    }

    false
}

/// Send heartbeat to server if due.
///
/// Checks interval, gathers agent info, and sends heartbeat.
/// This function encapsulates the entire heartbeat logic from Hub.
pub fn send_heartbeat_if_due(hub: &mut Hub) {
    // Skip if shutdown requested or offline
    if should_skip_polling(hub.quit, true) {
        return;
    }

    // Check heartbeat interval (shorter in test mode for faster tests)
    let heartbeat_interval = if crate::env::is_test_mode() { 2 } else { 30 };
    if hub.last_heartbeat.elapsed() < Duration::from_secs(heartbeat_interval) {
        return;
    }
    hub.last_heartbeat = Instant::now();

    // Detect current repo
    let repo_name = match crate::git::WorktreeManager::detect_current_repo() {
        Ok((_, name)) => name,
        Err(e) => {
            log::debug!("Not in a git repository, skipping heartbeat: {e}");
            return;
        }
    };

    // Build agents list for heartbeat
    let agents: Vec<HeartbeatAgentInfo> = hub
        .state
        .agents
        .values()
        .map(|agent| HeartbeatAgentInfo {
            session_key: agent.session_key(),
            last_invocation_url: agent.last_invocation_url.clone(),
        })
        .collect();

    let config = PollingConfig {
        client: &hub.client,
        server_url: &hub.config.server_url,
        api_key: hub.config.get_api_key(),
        poll_interval: hub.config.poll_interval,
        hub_identifier: &hub.hub_identifier,
    };

    send_heartbeat(&config, &repo_name, &agents, hub.device.device_id);
}

/// Poll agents for terminal notifications and send to Rails.
///
/// When agents emit notifications (OSC 9, OSC 777), sends them to Rails
/// for GitHub comments.
pub fn poll_and_send_agent_notifications(hub: &mut Hub) {
    use crate::agent::AgentNotification;

    // Collect notifications
    let mut notifications: Vec<AgentNotificationData> = Vec::new();

    for (session_key, agent) in &hub.state.agents {
        for notification in agent.poll_notifications() {
            let notification_type = match &notification {
                AgentNotification::Osc9(_) | AgentNotification::Osc777 { .. } => "question_asked",
            };

            notifications.push(AgentNotificationData {
                session_key: session_key.clone(),
                repo: agent.repo.clone(),
                issue_number: agent.issue_number,
                invocation_url: agent.last_invocation_url.clone(),
                notification_type: notification_type.to_string(),
            });
        }
    }

    // Send notifications to Rails
    let config = PollingConfig {
        client: &hub.client,
        server_url: &hub.config.server_url,
        api_key: hub.config.get_api_key(),
        poll_interval: hub.config.poll_interval,
        hub_identifier: &hub.hub_identifier,
    };

    for notif in notifications {
        if notif.issue_number.is_some() || notif.invocation_url.is_some() {
            log::info!(
                "Agent {} sent notification: {} (url: {:?})",
                notif.session_key, notif.notification_type, notif.invocation_url
            );

            let payload = AgentNotificationPayload {
                repo: &notif.repo,
                issue_number: notif.issue_number,
                invocation_url: notif.invocation_url.as_deref(),
                notification_type: &notif.notification_type,
            };

            if let Err(e) = send_agent_notification(&config, &payload) {
                log::error!("Failed to send notification to Rails: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_polling_state_default() {
        let state = PollingState::default();
        // Should poll immediately after creation (timestamps initialized to past)
        assert!(state.should_poll(1));
        assert!(state.should_heartbeat());
    }

    #[test]
    fn test_should_skip_polling() {
        assert!(should_skip_polling(true, true)); // quit = true
        assert!(should_skip_polling(false, false)); // polling disabled
        assert!(!should_skip_polling(false, true)); // normal operation
    }
}
