//! Background worker threads for non-blocking I/O operations.
//!
//! This module provides background workers that run network operations
//! on dedicated threads, keeping the main event loop responsive.
//!
//! # Architecture
//!
//! ```text
//! Main Thread (60 FPS)              Background Workers
//! ┌─────────────────────┐           ┌─────────────────────┐
//! │ Event Loop          │           │ PollingWorker       │
//! │   │                 │◄─channel──│   loop {            │
//! │   ├─ try_recv()     │           │     sleep(interval) │
//! │   │  (non-blocking) │           │     HTTP GET /msgs  │
//! │   │                 │──channel─►│     tx.send(msgs)   │
//! │   └─ render TUI     │           │   }                 │
//! └─────────────────────┘           └─────────────────────┘
//! ```
//!
//! # Workers
//!
//! - [`PollingWorker`] - Polls server for messages in background
//! - [`HeartbeatWorker`] - Sends heartbeat every 30 seconds
//! - [`NotificationWorker`] - Sends agent notifications to Rails

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use reqwest::blocking::Client;

use crate::server::types::MessageData;

use super::polling::MessageResponse;

/// Configuration for background workers.
///
/// Contains owned copies of config values needed by background threads.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Base URL for the Rails server.
    pub server_url: String,
    /// API key for authentication.
    pub api_key: String,
    /// Hub ID for server communication.
    pub server_hub_id: String,
    /// Poll interval in seconds.
    pub poll_interval: u64,
    /// Repository name.
    pub repo_name: String,
    /// Device ID for authentication.
    pub device_id: Option<i64>,
}

// ============================================================================
// Polling Worker
// ============================================================================

/// Result from polling worker.
#[derive(Debug)]
pub enum PollingResult {
    /// Successfully polled messages.
    Messages(Vec<MessageData>),
    /// Polling was skipped (offline mode, etc.).
    Skipped,
    /// Polling failed with error.
    Error(String),
}

/// Message acknowledgment request.
#[derive(Debug)]
pub struct AckRequest {
    /// Message ID to acknowledge.
    pub message_id: i64,
}

/// Background worker for message polling.
///
/// Runs polling on a dedicated thread to avoid blocking the main event loop.
/// Communicates via channels for non-blocking integration.
pub struct PollingWorker {
    /// Receiver for polling results (messages from server).
    result_rx: std_mpsc::Receiver<PollingResult>,
    /// Sender for acknowledgment requests.
    ack_tx: std_mpsc::Sender<AckRequest>,
    /// Shutdown flag shared with worker thread.
    shutdown: Arc<AtomicBool>,
    /// Worker thread handle.
    thread_handle: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for PollingWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PollingWorker")
            .field("shutdown", &self.shutdown.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl PollingWorker {
    /// Create and start a new polling worker.
    ///
    /// The worker immediately starts polling in the background.
    pub fn new(config: WorkerConfig) -> Self {
        let (result_tx, result_rx) = std_mpsc::channel();
        let (ack_tx, ack_rx) = std_mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);

        let thread_handle = thread::spawn(move || {
            Self::worker_loop(config, result_tx, ack_rx, shutdown_clone);
        });

        Self {
            result_rx,
            ack_tx,
            shutdown,
            thread_handle: Some(thread_handle),
        }
    }

    /// Worker loop - runs on dedicated thread.
    fn worker_loop(
        config: WorkerConfig,
        result_tx: std_mpsc::Sender<PollingResult>,
        ack_rx: std_mpsc::Receiver<AckRequest>,
        shutdown: Arc<AtomicBool>,
    ) {
        // Create HTTP client for this thread
        let client = match Client::builder().timeout(Duration::from_secs(10)).build() {
            Ok(c) => c,
            Err(e) => {
                log::error!("Failed to create HTTP client for polling worker: {e}");
                return;
            }
        };

        log::info!(
            "Polling worker started: interval={}s, repo={}",
            config.poll_interval,
            config.repo_name
        );

        loop {
            // Check for shutdown
            if shutdown.load(Ordering::SeqCst) {
                log::info!("Polling worker shutting down");
                break;
            }

            // Process any pending acknowledgments (non-blocking drain)
            while let Ok(ack) = ack_rx.try_recv() {
                Self::acknowledge_message(&client, &config, ack.message_id);
            }

            // Check offline mode
            if std::env::var("BOTSTER_OFFLINE_MODE").is_ok() {
                let _ = result_tx.send(PollingResult::Skipped);
            } else {
                // Poll for messages
                let result = Self::poll_messages(&client, &config);
                if result_tx.send(result).is_err() {
                    log::warn!("Polling worker: main thread disconnected");
                    break;
                }
            }

            // Sleep for poll interval (checking shutdown periodically)
            let sleep_duration = Duration::from_secs(config.poll_interval);
            let check_interval = Duration::from_millis(100);
            let mut elapsed = Duration::ZERO;

            while elapsed < sleep_duration {
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                thread::sleep(check_interval);
                elapsed += check_interval;

                // Process acks while sleeping too
                while let Ok(ack) = ack_rx.try_recv() {
                    Self::acknowledge_message(&client, &config, ack.message_id);
                }
            }
        }
    }

    /// Poll server for messages.
    fn poll_messages(client: &Client, config: &WorkerConfig) -> PollingResult {
        let url = format!(
            "{}/hubs/{}/messages?repo={}",
            config.server_url, config.server_hub_id, config.repo_name
        );

        let response = match client.get(&url).bearer_auth(&config.api_key).send() {
            Ok(r) => r,
            Err(e) => {
                log::warn!("Polling worker: connection failed: {e}");
                return PollingResult::Error(e.to_string());
            }
        };

        if !response.status().is_success() {
            log::warn!("Polling worker: server returned {}", response.status());
            return PollingResult::Error(format!("HTTP {}", response.status()));
        }

        match response.json::<MessageResponse>() {
            Ok(r) => {
                if !r.messages.is_empty() {
                    log::info!("Polling worker: received {} messages", r.messages.len());
                }
                PollingResult::Messages(r.messages)
            }
            Err(e) => {
                log::warn!("Polling worker: failed to parse response: {e}");
                PollingResult::Error(e.to_string())
            }
        }
    }

    /// Acknowledge a message to the server.
    fn acknowledge_message(client: &Client, config: &WorkerConfig, message_id: i64) {
        let url = format!(
            "{}/hubs/{}/messages/{message_id}",
            config.server_url, config.server_hub_id
        );

        match client
            .patch(&url)
            .bearer_auth(&config.api_key)
            .header("Content-Type", "application/json")
            .send()
        {
            Ok(response) if response.status().is_success() => {
                log::debug!("Polling worker: acknowledged message {message_id}");
            }
            Ok(response) => {
                log::warn!(
                    "Polling worker: failed to ack message {message_id}: {}",
                    response.status()
                );
            }
            Err(e) => {
                log::warn!("Polling worker: failed to ack message {message_id}: {e}");
            }
        }
    }

    /// Try to receive polling results (non-blocking).
    ///
    /// Returns `Some(result)` if messages are available, `None` otherwise.
    pub fn try_recv(&self) -> Option<PollingResult> {
        self.result_rx.try_recv().ok()
    }

    /// Queue a message acknowledgment.
    ///
    /// The acknowledgment is sent in the background.
    pub fn acknowledge(&self, message_id: i64) {
        let _ = self.ack_tx.send(AckRequest { message_id });
    }

    /// Request graceful shutdown.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    /// Check if worker is still running.
    pub fn is_running(&self) -> bool {
        !self.shutdown.load(Ordering::SeqCst)
    }
}

impl Drop for PollingWorker {
    fn drop(&mut self) {
        self.shutdown();
        if let Some(handle) = self.thread_handle.take() {
            // Give thread time to shutdown gracefully
            let _ = handle.join();
        }
    }
}

// ============================================================================
// Heartbeat Worker
// ============================================================================

/// Agent info for heartbeat payload.
#[derive(Debug, Clone)]
pub struct HeartbeatAgentData {
    /// Session key identifying the agent.
    pub session_key: String,
    /// URL of the last invocation that triggered this agent.
    pub last_invocation_url: Option<String>,
}

/// Request to update heartbeat agent list.
#[derive(Debug)]
pub struct HeartbeatUpdate {
    /// Current list of agents.
    pub agents: Vec<HeartbeatAgentData>,
}

/// Background worker for heartbeat sending.
///
/// Sends heartbeat every 30 seconds (or configured interval).
pub struct HeartbeatWorker {
    /// Sender for agent updates.
    update_tx: std_mpsc::Sender<HeartbeatUpdate>,
    /// Shutdown flag shared with worker thread.
    shutdown: Arc<AtomicBool>,
    /// Worker thread handle.
    thread_handle: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for HeartbeatWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeartbeatWorker")
            .field("shutdown", &self.shutdown.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl HeartbeatWorker {
    /// Heartbeat interval in seconds.
    const HEARTBEAT_INTERVAL: u64 = 30;

    /// Create and start a new heartbeat worker.
    pub fn new(config: WorkerConfig) -> Self {
        let (update_tx, update_rx) = std_mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);

        let thread_handle = thread::spawn(move || {
            Self::worker_loop(config, update_rx, shutdown_clone);
        });

        Self {
            update_tx,
            shutdown,
            thread_handle: Some(thread_handle),
        }
    }

    /// Worker loop - runs on dedicated thread.
    fn worker_loop(
        config: WorkerConfig,
        update_rx: std_mpsc::Receiver<HeartbeatUpdate>,
        shutdown: Arc<AtomicBool>,
    ) {
        // Create HTTP client for this thread
        let client = match Client::builder().timeout(Duration::from_secs(10)).build() {
            Ok(c) => c,
            Err(e) => {
                log::error!("Failed to create HTTP client for heartbeat worker: {e}");
                return;
            }
        };

        log::info!(
            "Heartbeat worker started: interval={}s",
            Self::HEARTBEAT_INTERVAL
        );

        // Current agent list (updated via channel)
        let mut agents: Vec<HeartbeatAgentData> = Vec::new();

        loop {
            // Check for shutdown
            if shutdown.load(Ordering::SeqCst) {
                log::info!("Heartbeat worker shutting down");
                break;
            }

            // Update agent list from main thread (non-blocking drain)
            while let Ok(update) = update_rx.try_recv() {
                agents = update.agents;
            }

            // Skip if offline mode
            if std::env::var("BOTSTER_OFFLINE_MODE").is_err() {
                Self::send_heartbeat(&client, &config, &agents);
            }

            // Sleep for heartbeat interval (checking shutdown periodically)
            let interval = if crate::env::is_any_test() {
                2
            } else {
                Self::HEARTBEAT_INTERVAL
            };
            let sleep_duration = Duration::from_secs(interval);
            let check_interval = Duration::from_millis(100);
            let mut elapsed = Duration::ZERO;

            while elapsed < sleep_duration {
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                thread::sleep(check_interval);
                elapsed += check_interval;

                // Update agents while sleeping too
                while let Ok(update) = update_rx.try_recv() {
                    agents = update.agents;
                }
            }
        }
    }

    /// Send heartbeat to server.
    fn send_heartbeat(client: &Client, config: &WorkerConfig, agents: &[HeartbeatAgentData]) {
        let agents_list: Vec<serde_json::Value> = agents
            .iter()
            .map(|agent| {
                serde_json::json!({
                    "session_key": agent.session_key,
                    "last_invocation_url": agent.last_invocation_url,
                })
            })
            .collect();

        let url = format!("{}/hubs/{}", config.server_url, config.server_hub_id);
        let payload = serde_json::json!({
            "repo": config.repo_name,
            "agents": agents_list,
            "device_id": config.device_id,
        });

        match client
            .put(&url)
            .bearer_auth(&config.api_key)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
        {
            Ok(response) if response.status().is_success() => {
                log::debug!(
                    "Heartbeat worker: sent heartbeat with {} agents",
                    agents.len()
                );
            }
            Ok(response) => {
                log::warn!("Heartbeat worker: server returned {}", response.status());
            }
            Err(e) => {
                log::warn!("Heartbeat worker: failed to send heartbeat: {e}");
            }
        }
    }

    /// Update the agent list.
    ///
    /// Called from main thread when agents change.
    pub fn update_agents(&self, agents: Vec<HeartbeatAgentData>) {
        let _ = self.update_tx.send(HeartbeatUpdate { agents });
    }

    /// Request graceful shutdown.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

impl Drop for HeartbeatWorker {
    fn drop(&mut self) {
        self.shutdown();
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

// ============================================================================
// Notification Worker
// ============================================================================

/// Agent notification to send to Rails.
#[derive(Debug, Clone)]
pub struct NotificationRequest {
    /// Repository name.
    pub repo: String,
    /// GitHub issue number.
    pub issue_number: Option<u32>,
    /// URL that triggered the agent.
    pub invocation_url: Option<String>,
    /// Type of notification (e.g., "question_asked").
    pub notification_type: String,
}

/// Background worker for sending agent notifications.
///
/// Queues notifications and sends them in the background.
pub struct NotificationWorker {
    /// Sender for notification requests.
    request_tx: std_mpsc::Sender<NotificationRequest>,
    /// Shutdown flag shared with worker thread.
    shutdown: Arc<AtomicBool>,
    /// Worker thread handle.
    thread_handle: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for NotificationWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotificationWorker")
            .field("shutdown", &self.shutdown.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl NotificationWorker {
    /// Create and start a new notification worker.
    pub fn new(config: WorkerConfig) -> Self {
        let (request_tx, request_rx) = std_mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);

        let thread_handle = thread::spawn(move || {
            Self::worker_loop(config, request_rx, shutdown_clone);
        });

        Self {
            request_tx,
            shutdown,
            thread_handle: Some(thread_handle),
        }
    }

    /// Worker loop - runs on dedicated thread.
    fn worker_loop(
        config: WorkerConfig,
        request_rx: std_mpsc::Receiver<NotificationRequest>,
        shutdown: Arc<AtomicBool>,
    ) {
        // Create HTTP client for this thread
        let client = match Client::builder().timeout(Duration::from_secs(10)).build() {
            Ok(c) => c,
            Err(e) => {
                log::error!("Failed to create HTTP client for notification worker: {e}");
                return;
            }
        };

        log::info!("Notification worker started");

        loop {
            // Check for shutdown
            if shutdown.load(Ordering::SeqCst) {
                // Drain remaining notifications before shutdown
                while let Ok(request) = request_rx.try_recv() {
                    Self::send_notification(&client, &config, &request);
                }
                log::info!("Notification worker shutting down");
                break;
            }

            // Wait for notification requests (with timeout for shutdown check)
            match request_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(request) => {
                    Self::send_notification(&client, &config, &request);
                }
                Err(std_mpsc::RecvTimeoutError::Timeout) => {
                    // Continue checking for shutdown
                }
                Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                    log::info!("Notification worker: channel disconnected");
                    break;
                }
            }
        }
    }

    /// Send notification to Rails.
    fn send_notification(client: &Client, config: &WorkerConfig, request: &NotificationRequest) {
        let url = format!(
            "{}/hubs/{}/notifications",
            config.server_url, config.server_hub_id
        );

        let payload = serde_json::json!({
            "repo": request.repo,
            "issue_number": request.issue_number,
            "invocation_url": request.invocation_url,
            "notification_type": request.notification_type,
        });

        match client
            .post(&url)
            .bearer_auth(&config.api_key)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
        {
            Ok(response) if response.status().is_success() => {
                log::info!(
                    "Notification worker: sent {} notification",
                    request.notification_type
                );
            }
            Ok(response) => {
                log::warn!(
                    "Notification worker: server returned {} for notification",
                    response.status()
                );
            }
            Err(e) => {
                log::warn!("Notification worker: failed to send notification: {e}");
            }
        }
    }

    /// Queue a notification to be sent.
    pub fn send(&self, request: NotificationRequest) {
        let _ = self.request_tx.send(request);
    }

    /// Request graceful shutdown.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

impl Drop for NotificationWorker {
    fn drop(&mut self) {
        self.shutdown();
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> WorkerConfig {
        WorkerConfig {
            server_url: "http://localhost:3000".to_string(),
            api_key: "test-key".to_string(),
            server_hub_id: "test-hub".to_string(),
            poll_interval: 1,
            repo_name: "test/repo".to_string(),
            device_id: Some(123),
        }
    }

    #[test]
    fn test_polling_worker_creation() {
        // Worker should start without panicking
        let config = test_config();
        let worker = PollingWorker::new(config);
        assert!(worker.is_running());
        worker.shutdown();
    }

    #[test]
    fn test_polling_worker_try_recv_empty() {
        let config = test_config();
        let worker = PollingWorker::new(config);

        // Should return None immediately when no messages
        // (first poll hasn't completed yet)
        // Note: This might return Some if the poll completes fast
        // so we just verify it doesn't panic
        let _ = worker.try_recv();

        worker.shutdown();
    }

    #[test]
    fn test_polling_worker_acknowledge() {
        let config = test_config();
        let worker = PollingWorker::new(config);

        // Should not panic when acknowledging
        worker.acknowledge(12345);

        worker.shutdown();
    }

    #[test]
    fn test_heartbeat_worker_creation() {
        let config = test_config();
        let worker = HeartbeatWorker::new(config);

        // Should start without panicking
        worker.shutdown();
    }

    #[test]
    fn test_heartbeat_worker_update_agents() {
        let config = test_config();
        let worker = HeartbeatWorker::new(config);

        // Should not panic when updating agents
        worker.update_agents(vec![HeartbeatAgentData {
            session_key: "test-session".to_string(),
            last_invocation_url: None,
        }]);

        worker.shutdown();
    }

    #[test]
    fn test_notification_worker_creation() {
        let config = test_config();
        let worker = NotificationWorker::new(config);

        // Should start without panicking
        worker.shutdown();
    }

    #[test]
    fn test_notification_worker_send() {
        let config = test_config();
        let worker = NotificationWorker::new(config);

        // Should not panic when sending notification
        worker.send(NotificationRequest {
            repo: "test/repo".to_string(),
            issue_number: Some(123),
            invocation_url: None,
            notification_type: "question_asked".to_string(),
        });

        worker.shutdown();
    }
}
