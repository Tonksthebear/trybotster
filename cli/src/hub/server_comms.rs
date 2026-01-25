//! Server communication for Hub.
//!
//! This module handles all communication with the Rails server, including:
//!
//! - Message polling and processing
//! - Heartbeat sending
//! - Agent notification delivery
//! - Device and hub registration
//!
//! # Architecture
//!
//! Communication can use either background workers (non-blocking) or
//! direct HTTP calls (blocking fallback). The `tick()` method automatically
//! selects the appropriate mode based on worker availability.
//!
//! # Background Workers
//!
//! When available, workers handle network I/O in background threads:
//! - `PollingWorker`: Fetches messages from server
//! - `HeartbeatWorker`: Sends periodic heartbeats
//! - `NotificationWorker`: Delivers agent notifications

// Rust guideline compliant 2025-01

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::agent::AgentNotification;
use crate::client::ClientId;
use crate::hub::actions::{self, HubAction};
use crate::hub::events::HubEvent;
use crate::hub::lifecycle::SpawnResult;
use crate::hub::{polling, registration, workers, AgentProgressEvent, Hub, PendingAgentResult};
use crate::server::messages::{message_to_hub_action, MessageContext, ParsedMessage};

impl Hub {
    /// Perform periodic tasks (polling, heartbeat, notifications).
    ///
    /// Call this from your event loop to handle time-based operations.
    /// This method is **non-blocking** when background workers are running.
    ///
    /// # Worker-based flow (non-blocking)
    ///
    /// When workers are active (after `setup()`):
    /// - `poll_worker_messages()` - non-blocking try_recv from PollingWorker
    /// - `update_heartbeat_agents()` - non-blocking send to HeartbeatWorker
    /// - `poll_agent_notifications_async()` - non-blocking send to NotificationWorker
    ///
    /// # Fallback flow (blocking)
    ///
    /// When workers aren't available (offline mode, testing):
    /// - Falls back to blocking HTTP calls on main thread
    pub fn tick(&mut self) {
        // Ensure TUI has a valid selection if agents exist
        // (fixes visual fallback mismatch where render shows first agent but input routes to None)
        self.ensure_tui_selection();

        // Process completed background agent creations (always non-blocking)
        self.poll_pending_agents();

        // Process progress events from background agent creations
        self.poll_progress_events();

        // Use background workers if available (non-blocking)
        if self.polling_worker.is_some() {
            self.poll_worker_messages();
            self.update_heartbeat_agents();
            self.poll_agent_notifications_async();
        } else {
            // Fallback to blocking calls (offline mode, testing, or before setup)
            self.poll_messages();
            self.send_heartbeat();
            self.poll_agent_notifications();
        }
    }

    /// Poll for messages from background worker (non-blocking).
    ///
    /// Checks the polling worker's result channel for new messages
    /// and processes them without blocking the main thread.
    fn poll_worker_messages(&mut self) {
        // Collect all available results first (to release borrow)
        let results: Vec<workers::PollingResult> = {
            let Some(ref worker) = self.polling_worker else {
                return;
            };

            let mut results = Vec::new();
            while let Some(result) = worker.try_recv() {
                results.push(result);
            }
            results
        };

        // Now process each result (borrow released)
        for result in results {
            match result {
                workers::PollingResult::Messages(messages) => {
                    if !messages.is_empty() {
                        self.process_polled_messages(messages);
                    }
                }
                workers::PollingResult::Skipped => {
                    // Offline mode or similar - nothing to do
                }
                workers::PollingResult::Error(e) => {
                    log::debug!("Background poll error (will retry): {e}");
                }
            }
        }
    }

    /// Process messages received from background polling.
    ///
    /// Converts messages to actions and dispatches them.
    /// Acknowledgments are queued back to the worker thread.
    fn process_polled_messages(&mut self, messages: Vec<crate::server::types::MessageData>) {
        // Detect repo for context
        let (repo_path, repo_name) = if let Ok(repo) = std::env::var("BOTSTER_REPO") {
            (std::path::PathBuf::from("."), repo)
        } else {
            match crate::git::WorktreeManager::detect_current_repo() {
                Ok(result) => result,
                Err(_) if crate::env::is_test_mode() => {
                    (std::path::PathBuf::from("."), "test/repo".to_string())
                }
                Err(e) => {
                    log::warn!("Not in a git repository, skipping message processing: {e}");
                    return;
                }
            }
        };

        log::info!(
            "Processing {} messages from background poll",
            messages.len()
        );

        let context = MessageContext {
            repo_path,
            repo_name: repo_name.clone(),
            worktree_base: self.config.worktree_base.clone(),
            max_sessions: self.config.max_sessions,
            current_agent_count: self.state.read().unwrap().agent_count(),
        };

        for msg in &messages {
            let parsed = ParsedMessage::from_message_data(msg);

            // Try to notify existing agent first
            if self.try_notify_existing_agent(&parsed, &context.repo_name) {
                self.acknowledge_message_async(msg.id);
                continue;
            }

            // Convert to action and dispatch
            match message_to_hub_action(&parsed, &context) {
                Ok(Some(action)) => {
                    self.handle_action(action);
                    self.acknowledge_message_async(msg.id);
                }
                Ok(None) => self.acknowledge_message_async(msg.id),
                Err(e) => {
                    // IMPORTANT: Acknowledge even on error to prevent infinite redelivery.
                    // The message is malformed or we can't handle it - retrying won't help.
                    log::error!(
                        "Failed to process message {}: {e} (acknowledging to prevent redelivery)",
                        msg.id
                    );
                    self.acknowledge_message_async(msg.id);
                }
            }
        }
    }

    /// Queue message acknowledgment to background worker (non-blocking).
    fn acknowledge_message_async(&self, message_id: i64) {
        if let Some(ref worker) = self.polling_worker {
            worker.acknowledge(message_id);
        } else {
            // Fallback to blocking ack
            self.acknowledge_message(message_id);
        }
    }

    /// Update heartbeat worker with current agent list (non-blocking).
    ///
    /// Only sends updates when the agent count changes to avoid
    /// sending redundant data every tick (60 FPS would be wasteful).
    ///
    /// The heartbeat worker maintains its own 30-second timer, so we just
    /// need to keep it updated with the current agent list.
    fn update_heartbeat_agents(&mut self) {
        let Some(ref worker) = self.heartbeat_worker else {
            return;
        };

        let state = self.state.read().unwrap();

        // Only send if agent count changed (simple change detection)
        let current_count = state.agents.len();
        if current_count == self.last_heartbeat_agent_count {
            return;
        }
        self.last_heartbeat_agent_count = current_count;

        // Build agent data for heartbeat
        let agents: Vec<workers::HeartbeatAgentData> = state
            .agents
            .values()
            .map(|agent| workers::HeartbeatAgentData {
                session_key: agent.agent_id(),
                last_invocation_url: agent.last_invocation_url.clone(),
            })
            .collect();

        log::debug!("Heartbeat agent list updated: {} agents", agents.len());
        worker.update_agents(agents);
    }

    /// Poll agents for notifications and send via background worker (non-blocking).
    ///
    /// Collects notifications from all agents and queues them to the
    /// notification worker for background sending to Rails.
    fn poll_agent_notifications_async(&self) {
        let Some(ref worker) = self.notification_worker else {
            return;
        };

        let state = self.state.read().unwrap();

        // Collect and send notifications from all agents
        for agent in state.agents.values() {
            for notification in agent.poll_notifications() {
                // Only send if we have issue context (otherwise there's nowhere to post)
                if agent.issue_number.is_none() && agent.last_invocation_url.is_none() {
                    continue;
                }

                let notification_type = match &notification {
                    AgentNotification::Osc9(_) | AgentNotification::Osc777 { .. } => {
                        "question_asked"
                    }
                };

                log::info!(
                    "Agent {} sent notification: {} (url: {:?})",
                    agent.agent_id(),
                    notification_type,
                    agent.last_invocation_url
                );

                let request = workers::NotificationRequest {
                    repo: agent.repo.clone(),
                    issue_number: agent.issue_number,
                    invocation_url: agent.last_invocation_url.clone(),
                    notification_type: notification_type.to_string(),
                };

                worker.send(request);
            }
        }
    }

    /// Poll for completed background agent creation tasks.
    ///
    /// Non-blocking check for results from spawn_blocking tasks.
    /// Processes all completed creations and sends appropriate responses to clients.
    pub fn poll_pending_agents(&mut self) {
        // Process all pending results (non-blocking)
        while let Ok(pending) = self.pending_agent_rx.try_recv() {
            self.handle_pending_agent_result(pending);
        }
    }

    /// Poll for progress events from background agent creation.
    ///
    /// Non-blocking check for progress updates. Sends progress to the requesting
    /// client (browser or TUI).
    pub fn poll_progress_events(&mut self) {
        while let Ok(event) = self.progress_rx.try_recv() {
            self.handle_progress_event(event);
        }
    }

    /// Handle a progress event from background agent creation.
    fn handle_progress_event(&mut self, event: AgentProgressEvent) {
        log::debug!(
            "Progress: {} -> {:?} for client {:?}",
            event.identifier,
            event.stage,
            event.client_id
        );

        // Send progress to browser clients via relay
        if let ClientId::Browser(ref identity) = event.client_id {
            if let Some(ref sender) = self.browser.sender {
                let ctx = crate::relay::BrowserSendContext {
                    sender,
                    runtime: &self.tokio_runtime,
                };
                crate::relay::send_agent_progress_to(
                    &ctx,
                    identity,
                    &event.identifier,
                    event.stage,
                );
            }
        }

        // Track TUI creation progress for display
        if event.client_id.is_tui() {
            self.creating_agent = Some((event.identifier.clone(), event.stage));
        }

        // Broadcast progress event to all subscribers (including TUI)
        self.broadcast(HubEvent::AgentCreationProgress {
            identifier: event.identifier,
            stage: event.stage,
        });
    }

    /// Handle a completed agent creation from background thread.
    ///
    /// The background thread has completed the slow git/file operations.
    /// Now we do the fast PTY spawn on the main thread (needs &mut state).
    fn handle_pending_agent_result(&mut self, pending: PendingAgentResult) {
        use crate::hub::lifecycle;

        // Clear TUI creating indicator on completion (success or failure)
        if pending.client_id.is_tui() {
            self.creating_agent = None;
        }

        match pending.result {
            Ok(_) => {
                // Background work succeeded - now spawn the agent (fast, needs &mut state)
                log::info!(
                    "Background worktree ready for {:?}, spawning agent...",
                    pending.client_id
                );

                // Get client dims for PTY
                let dims = self
                    .clients
                    .get(&pending.client_id)
                    .map(|c| c.dims())
                    .unwrap_or(self.terminal_dims);

                // Enter tokio runtime context for spawn_command_processor() which uses tokio::spawn()
                let _runtime_guard = self.tokio_runtime.enter();

                // Spawn agent (fast - just PTY creation) - release lock after spawning
                let spawn_result = {
                    let mut state = self.state.write().unwrap();
                    lifecycle::spawn_agent(&mut state, &pending.config, dims)
                };

                match spawn_result {
                    Ok(result) => {
                        self.handle_successful_spawn(result, &pending.client_id);
                    }
                    Err(e) => {
                        log::error!("Failed to spawn agent: {}", e);
                        self.send_error_to(
                            &pending.client_id,
                            format!("Failed to spawn agent: {}", e),
                        );
                    }
                }
            }
            Err(e) => {
                log::error!(
                    "Background agent creation failed for {:?}: {}",
                    pending.client_id,
                    e
                );
                self.send_error_to(&pending.client_id, format!("Failed to create agent: {}", e));
            }
        }
    }

    /// Handle successful agent spawn after background worktree creation.
    fn handle_successful_spawn(&mut self, result: SpawnResult, client_id: &ClientId) {
        log::info!(
            "Agent spawned: {} for client {:?}",
            result.agent_id,
            client_id
        );

        // Register tunnel if port assigned
        if let Some(port) = result.tunnel_port {
            let tm = Arc::clone(&self.tunnel_manager);
            let key = result.agent_id.clone();
            self.tokio_runtime.spawn(async move {
                tm.register_agent(key, port).await;
            });
        }

        // Connect agent's channels (terminal + preview if tunnel exists)
        let agent_index = self
            .state
            .read()
            .unwrap()
            .agents
            .keys()
            .position(|k| k == &result.agent_id);

        if let Some(idx) = agent_index {
            self.connect_agent_channels(&result.agent_id, idx);
        }

        // Response delivery is handled via browser relay channels

        // Send agent_created to browser clients via relay
        if let ClientId::Browser(ref identity) = client_id {
            if let Some(ref sender) = self.browser.sender {
                let ctx = crate::relay::BrowserSendContext {
                    sender,
                    runtime: &self.tokio_runtime,
                };
                crate::relay::send_agent_created_to(&ctx, identity, &result.agent_id);
            }
        }

        // Send updated agent list to all browsers via relay
        // (BrowserClient::receive_agent_list is no-op, must use relay)
        crate::relay::browser::send_agent_list(self);

        // Auto-select the new agent for the requesting client
        let session_key = result.agent_id.clone();
        actions::dispatch(
            self,
            HubAction::SelectAgentForClient {
                client_id: client_id.clone(),
                agent_key: session_key.clone(),
            },
        );

        // Send scrollback to browser client after auto-selection
        // (handle_select_agent_for_client doesn't send scrollback - it's generic)
        if let ClientId::Browser(ref identity) = client_id {
            // Default to CLI view for new agents
            crate::relay::browser::send_scrollback_for_agent_to_browser(
                self,
                identity,
                &session_key,
                crate::agent::PtyView::Cli,
            );
        }

        // Also auto-select for TUI if it has no selection
        // (ensures TUI state matches what's visually displayed)
        if *client_id != ClientId::Tui {
            let tui_has_selection = self.get_tui_selected_agent_key().is_some();
            if !tui_has_selection {
                actions::dispatch(
                    self,
                    HubAction::SelectAgentForClient {
                        client_id: ClientId::Tui,
                        agent_key: session_key.clone(),
                    },
                );
            }
        }

        // Broadcast AgentCreated event to all subscribers (including TUI)
        if let Some(info) = self.state.read().unwrap().get_agent_info(&session_key) {
            self.broadcast(HubEvent::agent_created(session_key, info));
        }
    }

    // === Server Communication ===

    /// Build polling configuration from Hub state.
    pub(crate) fn polling_config(&self) -> polling::PollingConfig<'_> {
        polling::PollingConfig {
            client: &self.client,
            server_url: &self.config.server_url,
            api_key: self.config.get_api_key(),
            poll_interval: self.config.poll_interval,
            server_hub_id: self.server_hub_id(),
        }
    }

    /// Poll the server for new messages and process them.
    ///
    /// This method polls at the configured interval and processes any pending
    /// messages from the server, converting them to HubActions.
    pub fn poll_messages(&mut self) {
        if polling::should_skip_polling(self.quit) {
            return;
        }
        if self.last_poll.elapsed() < Duration::from_secs(self.config.poll_interval) {
            return;
        }
        self.last_poll = Instant::now();

        // Detect repo: env var > git detection > test fallback
        let (repo_path, repo_name) = if let Ok(repo) = std::env::var("BOTSTER_REPO") {
            // Explicit repo override (used in tests and special cases)
            (std::path::PathBuf::from("."), repo)
        } else {
            match crate::git::WorktreeManager::detect_current_repo() {
                Ok(result) => result,
                Err(_) if crate::env::is_test_mode() => {
                    // Test mode fallback - use dummy repo
                    (std::path::PathBuf::from("."), "test/repo".to_string())
                }
                Err(e) => {
                    log::warn!("Not in a git repository, skipping poll: {e}");
                    return;
                }
            }
        };

        let messages = polling::poll_messages(&self.polling_config(), &repo_name);
        if messages.is_empty() {
            return;
        }

        log::info!("Polled {} messages for repo={}", messages.len(), repo_name);

        let context = MessageContext {
            repo_path,
            repo_name: repo_name.clone(),
            worktree_base: self.config.worktree_base.clone(),
            max_sessions: self.config.max_sessions,
            current_agent_count: self.state.read().unwrap().agent_count(),
        };

        for msg in &messages {
            let parsed = ParsedMessage::from_message_data(msg);

            // Try to notify existing agent first
            if self.try_notify_existing_agent(&parsed, &context.repo_name) {
                self.acknowledge_message(msg.id);
                continue;
            }

            // Convert to action and dispatch
            match message_to_hub_action(&parsed, &context) {
                Ok(Some(action)) => {
                    self.handle_action(action);
                    self.acknowledge_message(msg.id);
                }
                Ok(None) => self.acknowledge_message(msg.id),
                Err(e) => log::error!("Failed to process message {}: {e}", msg.id),
            }
        }
    }

    /// Try to send a notification to an existing agent for this issue.
    ///
    /// Returns true if an agent was found and notified, false otherwise.
    /// Does NOT apply to cleanup messages - those need to go through the action dispatch.
    pub(crate) fn try_notify_existing_agent(
        &mut self,
        parsed: &ParsedMessage,
        default_repo: &str,
    ) -> bool {
        // Cleanup messages should not be treated as notifications
        if parsed.is_cleanup() {
            return false;
        }

        let Some(issue_number) = parsed.issue_number else {
            return false;
        };

        let repo_safe = parsed
            .repo
            .as_deref()
            .unwrap_or(default_repo)
            .replace('/', "-");
        let session_key = format!("{repo_safe}-{issue_number}");

        let mut state = self.state.write().unwrap();
        let Some(agent) = state.agents.get_mut(&session_key) else {
            return false;
        };

        log::info!("Agent exists for issue #{issue_number}, sending notification");
        let notification = parsed.format_notification();

        if let Err(e) = agent.write_input_to_cli(notification.as_bytes()) {
            log::error!("Failed to send notification to agent: {e}");
        } else {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let _ = agent.write_input_to_cli(b"\r");
            std::thread::sleep(std::time::Duration::from_millis(50));
            let _ = agent.write_input_to_cli(b"\r");
        }

        true
    }

    /// Acknowledge a message to the server.
    fn acknowledge_message(&self, message_id: i64) {
        let config = self.polling_config();
        polling::acknowledge_message(&config, message_id);
    }

    /// Send heartbeat to the server.
    ///
    /// Registers this hub instance and its active agents with the server.
    /// Delegates to `polling::send_heartbeat_if_due()`.
    pub fn send_heartbeat(&mut self) {
        polling::send_heartbeat_if_due(self);
    }

    /// Poll agents for terminal notifications (OSC 9, OSC 777).
    ///
    /// When agents emit notifications, sends them to Rails for GitHub comments.
    /// Delegates to `polling::poll_and_send_agent_notifications()`.
    pub fn poll_agent_notifications(&mut self) {
        polling::poll_and_send_agent_notifications(self);
    }

    // === Connection Setup ===

    /// Register the device with the server if not already registered.
    pub fn register_device(&mut self) {
        registration::register_device(&mut self.device, &self.client, &self.config);
    }

    /// Register the hub with the server and store the server-assigned ID.
    ///
    /// The server-assigned `botster_id` is used for all URLs and WebSocket subscriptions
    /// to guarantee uniqueness (no collision between different CLI instances).
    /// The local `hub_identifier` is kept for config directories.
    pub fn register_hub_with_server(&mut self) {
        let botster_id = registration::register_hub_with_server(
            &self.hub_identifier,
            &self.config.server_url,
            self.config.get_api_key(),
            self.device.device_id,
        );
        // Store server-assigned ID (used for all server communication)
        self.botster_id = Some(botster_id);
    }

    /// Start the tunnel connection in background.
    pub fn start_tunnel(&self) {
        registration::start_tunnel(&self.tunnel_manager, &self.tokio_runtime);
    }

    /// Connect to hub relay for browser communication (Signal E2E encryption).
    ///
    /// The hub relay handles hub-level commands and broadcasts. Terminal I/O
    /// goes through agent-owned channels, not this relay.
    pub fn connect_hub_relay(&mut self) {
        // Extract values before mutable borrow of browser
        let server_id = self.server_hub_id().to_string();
        let local_id = self.hub_identifier.clone();
        let server_url = self.config.server_url.clone();
        let api_key = self.config.get_api_key().to_string();

        registration::connect_hub_relay(
            &mut self.browser,
            &server_id,
            &local_id,
            &server_url,
            &api_key,
            &self.tokio_runtime,
        );
    }
}
