//! Background worker threads for non-blocking I/O operations.
//!
//! This module provides background workers that run network operations
//! on dedicated threads, keeping the main event loop responsive.
//!
//! # Workers
//!
//! - [`NotificationWorker`] - Sends agent notifications to Rails

// Rust guideline compliant 2025-01

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use reqwest::blocking::Client;

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
        let client = match Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent(crate::constants::user_agent())
            .build()
        {
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

    /// Base delay for exponential backoff in seconds.
    const BACKOFF_BASE_SECS: u64 = 2;

    /// Maximum backoff delay in seconds.
    const BACKOFF_MAX_SECS: u64 = 60;

    /// Tracks consecutive failures and computes exponential backoff delays.
    ///
    /// # Backoff Formula
    ///
    /// delay = min(BASE * 2^(consecutive_failures - 1), MAX)
    ///
    /// - After 1 failure: 2s
    /// - After 2 failures: 4s
    /// - After 3 failures: 8s
    /// - After 4 failures: 16s
    /// - After 5 failures: 32s
    /// - After 6+ failures: 60s (capped)
    #[derive(Debug, Default)]
    struct BackoffState {
        consecutive_failures: u32,
    }

    impl BackoffState {
        fn record_success(&mut self) {
            self.consecutive_failures = 0;
        }

        fn record_failure(&mut self) {
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        }

        fn current_delay(&self) -> Duration {
            if self.consecutive_failures == 0 {
                return Duration::ZERO;
            }
            let exponent = self.consecutive_failures.saturating_sub(1).min(6);
            let multiplier = 1u64 << exponent;
            let delay_secs = BACKOFF_BASE_SECS.saturating_mul(multiplier).min(BACKOFF_MAX_SECS);
            Duration::from_secs(delay_secs)
        }

        fn is_backing_off(&self) -> bool {
            self.consecutive_failures > 0
        }
    }

    fn test_config() -> WorkerConfig {
        WorkerConfig {
            server_url: "http://localhost:3000".to_string(),
            api_key: "test-key".to_string(),
            server_hub_id: "test-hub".to_string(),
        }
    }

    // ========================================================================
    // BackoffState Tests
    // ========================================================================

    #[test]
    fn test_backoff_state_default() {
        let backoff = BackoffState::default();
        assert_eq!(backoff.consecutive_failures, 0);
        assert!(!backoff.is_backing_off());
        assert_eq!(backoff.current_delay(), Duration::ZERO);
    }

    #[test]
    fn test_backoff_exponential_growth() {
        let mut backoff = BackoffState::default();

        // After 1 failure: 2s
        backoff.record_failure();
        assert_eq!(backoff.consecutive_failures, 1);
        assert!(backoff.is_backing_off());
        assert_eq!(backoff.current_delay(), Duration::from_secs(2));

        // After 2 failures: 4s
        backoff.record_failure();
        assert_eq!(backoff.consecutive_failures, 2);
        assert_eq!(backoff.current_delay(), Duration::from_secs(4));

        // After 3 failures: 8s
        backoff.record_failure();
        assert_eq!(backoff.consecutive_failures, 3);
        assert_eq!(backoff.current_delay(), Duration::from_secs(8));

        // After 4 failures: 16s
        backoff.record_failure();
        assert_eq!(backoff.consecutive_failures, 4);
        assert_eq!(backoff.current_delay(), Duration::from_secs(16));

        // After 5 failures: 32s
        backoff.record_failure();
        assert_eq!(backoff.consecutive_failures, 5);
        assert_eq!(backoff.current_delay(), Duration::from_secs(32));

        // After 6 failures: 60s (capped at BACKOFF_MAX_SECS)
        backoff.record_failure();
        assert_eq!(backoff.consecutive_failures, 6);
        assert_eq!(backoff.current_delay(), Duration::from_secs(60));

        // After 7+ failures: still 60s (stays at max)
        backoff.record_failure();
        assert_eq!(backoff.consecutive_failures, 7);
        assert_eq!(backoff.current_delay(), Duration::from_secs(60));
    }

    #[test]
    fn test_backoff_reset_on_success() {
        let mut backoff = BackoffState::default();

        // Accumulate some failures
        backoff.record_failure();
        backoff.record_failure();
        backoff.record_failure();
        assert_eq!(backoff.consecutive_failures, 3);
        assert!(backoff.is_backing_off());

        // Success resets to zero
        backoff.record_success();
        assert_eq!(backoff.consecutive_failures, 0);
        assert!(!backoff.is_backing_off());
        assert_eq!(backoff.current_delay(), Duration::ZERO);
    }

    #[test]
    fn test_backoff_overflow_protection() {
        let mut backoff = BackoffState::default();

        // Simulate many failures to test overflow protection
        for _ in 0..100 {
            backoff.record_failure();
        }

        // Should be capped at max delay, not overflow
        assert_eq!(backoff.current_delay(), Duration::from_secs(BACKOFF_MAX_SECS));
    }

    // ========================================================================
    // Worker Tests
    // ========================================================================

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
