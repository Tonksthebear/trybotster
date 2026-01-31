//! Server communication for Hub.
//!
//! This module handles all communication with the Rails server, including:
//!
//! - WebSocket command channel for real-time message delivery
//! - Heartbeat sending via command channel
//! - Agent notification delivery via background worker
//! - Device and hub registration
//!
//! # Architecture
//!
//! The command channel (WebSocket) is the sole path for message delivery
//! and heartbeat. The NotificationWorker handles agent notification
//! delivery in a background thread.

// Rust guideline compliant 2026-01-29

use std::time::{Duration, Instant};

use crate::agent::AgentNotification;
use crate::client::ClientId;
use crate::hub::actions::{self, HubAction};
use crate::hub::events::HubEvent;
use crate::hub::lifecycle::SpawnResult;
use crate::hub::{command_channel, registration, workers, AgentProgressEvent, Hub, PendingAgentResult};
use crate::server::messages::{message_to_hub_action, MessageContext, ParsedMessage};

impl Hub {
    /// Perform periodic tasks (command channel polling, heartbeat, notifications).
    ///
    /// Call this from your event loop to handle time-based operations.
    /// This method is **non-blocking** - all network I/O is handled via
    /// the WebSocket command channel and background notification worker.
    pub fn tick(&mut self) {
        self.poll_pending_agents();
        self.poll_progress_events();
        self.poll_command_channel();
        self.send_command_channel_heartbeat();
        self.poll_agent_notifications_async();
    }

    // === Command Channel (WebSocket) Methods ===

    /// Poll command channel for messages (non-blocking).
    ///
    /// Messages arrive in real-time via WebSocket instead of HTTP polling.
    ///
    /// Collects all pending messages first (releasing the mutable borrow on the
    /// channel), then processes each message with full `&mut self` access.
    fn poll_command_channel(&mut self) {
        // Drain all pending messages first to release the mutable borrow
        let messages: Vec<command_channel::CommandMessage> = {
            let Some(ref mut channel) = self.command_channel else {
                return;
            };
            let mut msgs = Vec::new();
            while let Some(msg) = channel.try_recv() {
                msgs.push(msg);
            }
            msgs
        };

        // Now process each message with full &mut self access
        for msg in &messages {
            let sequence = msg.sequence;

            match msg.event_type.as_str() {
                "browser_connected" => {
                    // Browser subscribed to HubChannel - create BrowserClient to pair with it.
                    // The BrowserClient will subscribe to HubChannel with the same identity,
                    // establishing the per-browser bidirectional stream.
                    let browser_identity = msg.payload
                        .get("browser_identity")
                        .and_then(|v| v.as_str())
                        .map(String::from);

                    if let Some(identity) = browser_identity {
                        log::info!(
                            "Browser connected (command channel): {} - dispatching ClientConnected",
                            &identity[..identity.len().min(8)]
                        );
                        // Dispatch action to create BrowserClient
                        self.handle_action(HubAction::ClientConnected {
                            client_id: ClientId::Browser(identity),
                        });
                    } else {
                        log::warn!("Browser connected event missing browser_identity");
                    }
                }
                "browser_disconnected" => {
                    // Browser unsubscribed from HubChannel - clean up BrowserClient.
                    let browser_identity = msg.payload
                        .get("browser_identity")
                        .and_then(|v| v.as_str())
                        .map(String::from);

                    if let Some(identity) = browser_identity {
                        log::info!(
                            "Browser disconnected (command channel): {} - dispatching ClientDisconnected",
                            &identity[..identity.len().min(8)]
                        );
                        // Dispatch action to clean up BrowserClient
                        self.handle_action(HubAction::ClientDisconnected {
                            client_id: ClientId::Browser(identity),
                        });
                    } else {
                        log::warn!("Browser disconnected event missing browser_identity");
                    }
                }
                "terminal_connected" => {
                    // Browser subscribed to TerminalRelayChannel for a specific PTY.
                    // Trigger PTY connection for the matching BrowserClient.
                    let agent_index = msg.payload.get("agent_index").and_then(|v| v.as_u64());
                    let pty_index = msg.payload.get("pty_index").and_then(|v| v.as_u64());
                    let browser_identity = msg.payload
                        .get("browser_identity")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");

                    log::info!(
                        "[INPUT-TRACE] Hub received terminal_connected: browser={}, agent={:?}, pty={:?}",
                        &browser_identity[..browser_identity.len().min(8)],
                        agent_index,
                        pty_index
                    );

                    if let (Some(ai), Some(pi)) = (agent_index, pty_index) {
                        let agent_index = ai as usize;
                        let pty_index = pi as usize;

                        // Ensure agent channels are connected (preview, etc.)
                        let agent_key = self.state.read().unwrap()
                            .agent_keys_ordered
                            .get(agent_index)
                            .cloned();

                        if let Some(key) = agent_key {
                            self.connect_agent_channels(&key, agent_index);
                        }

                        // Broadcast to BrowserClient so it sets up TerminalRelayChannel.
                        let client_id = ClientId::Browser(browser_identity.to_string());
                        log::info!(
                            "[INPUT-TRACE] Broadcasting PtyConnectionRequested for agent={} pty={}",
                            agent_index,
                            pty_index
                        );
                        self.broadcast(HubEvent::PtyConnectionRequested {
                            client_id,
                            agent_index,
                            pty_index,
                        });
                    } else {
                        log::warn!(
                            "Terminal connected missing agent_index or pty_index: {:?}",
                            msg.payload
                        );
                    }
                }
                "terminal_disconnected" => {
                    // Browser unsubscribed from TerminalRelayChannel for a specific PTY.
                    // Trigger PTY disconnection for the matching BrowserClient.
                    let agent_index = msg.payload.get("agent_index").and_then(|v| v.as_u64());
                    let pty_index = msg.payload.get("pty_index").and_then(|v| v.as_u64());
                    let browser_identity = msg.payload
                        .get("browser_identity")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");

                    log::info!(
                        "Terminal disconnected (command channel): browser={}, agent={:?}, pty={:?}",
                        &browser_identity[..browser_identity.len().min(8)],
                        agent_index,
                        pty_index
                    );

                    if let (Some(ai), Some(pi)) = (agent_index, pty_index) {
                        let client_id = ClientId::Browser(browser_identity.to_string());
                        self.broadcast(HubEvent::PtyDisconnectionRequested {
                            client_id,
                            agent_index: ai as usize,
                            pty_index: pi as usize,
                        });
                    } else {
                        log::warn!(
                            "Terminal disconnected missing agent_index or pty_index: {:?}",
                            msg.payload
                        );
                    }
                }
                "browser_wants_preview" => {
                    // Browser subscribed to PreviewChannel - notify BrowserClient to create HttpChannel.
                    let agent_index = msg.payload.get("agent_index").and_then(|v| v.as_u64());
                    let pty_index = msg
                        .payload
                        .get("pty_index")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(1); // Default to server PTY
                    let browser_identity = msg.payload
                        .get("browser_identity")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");

                    log::info!(
                        "[CommandChannel] Browser wants preview: browser={}, agent={:?}, pty={}",
                        &browser_identity[..browser_identity.len().min(8)],
                        agent_index,
                        pty_index
                    );

                    if let Some(ai) = agent_index {
                        let client_id = ClientId::Browser(browser_identity.to_string());
                        self.broadcast(HubEvent::HttpConnectionRequested {
                            client_id,
                            agent_index: ai as usize,
                            pty_index: pty_index as usize,
                            browser_identity: browser_identity.to_string(),
                        });
                    } else {
                        log::warn!(
                            "Browser wants preview missing agent_index: {:?}",
                            msg.payload
                        );
                    }
                }
                _ => {
                    self.process_command_channel_message(msg);
                }
            }

            // Acknowledge after processing
            if let Some(ref channel) = self.command_channel {
                channel.acknowledge(sequence);
            }
        }
    }

    /// Process a standard (non-browser) message from the command channel.
    ///
    /// Converts command channel messages to the same ParsedMessage/HubAction flow
    /// used for message processing.
    fn process_command_channel_message(&mut self, msg: &command_channel::CommandMessage) {
        use crate::server::types::MessageData;

        // Convert CommandMessage to MessageData for compatibility with existing parsing
        let message_data = MessageData {
            id: msg.id,
            event_type: msg.event_type.clone(),
            payload: msg.payload.clone(),
        };

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

        let context = MessageContext {
            repo_path,
            repo_name: repo_name.clone(),
            worktree_base: self.config.worktree_base.clone(),
            max_sessions: self.config.max_sessions,
            current_agent_count: self.state.read().unwrap().agent_count(),
        };

        let parsed = ParsedMessage::from_message_data(&message_data);

        // Try to notify existing agent first
        if self.try_notify_existing_agent(&parsed, &context.repo_name) {
            return;
        }

        // Convert to action and dispatch
        match message_to_hub_action(&parsed, &context) {
            Ok(Some(action)) => {
                self.handle_action(action);
            }
            Ok(None) => {}
            Err(e) => {
                log::error!(
                    "Failed to process command channel message {}: {e}",
                    msg.id
                );
            }
        }
    }

    /// Send heartbeat via command channel (non-blocking).
    fn send_command_channel_heartbeat(&mut self) {
        let Some(ref channel) = self.command_channel else {
            return;
        };

        // Only send every 30 seconds
        if self.last_heartbeat.elapsed() < Duration::from_secs(30) {
            return;
        }
        self.last_heartbeat = Instant::now();

        let state = self.state.read().unwrap();
        let agents: Vec<serde_json::Value> = state
            .agents
            .values()
            .map(|agent| {
                serde_json::json!({
                    "session_key": agent.agent_id(),
                    "last_invocation_url": agent.last_invocation_url
                })
            })
            .collect();

        channel.send_heartbeat(serde_json::json!(agents));
        log::debug!("Sent heartbeat via command channel ({} agents)", agents.len());
    }

    // === Background Worker Methods ===

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

        // Broadcast progress event to all subscribers
        // BrowserClient reacts via handle_hub_event() -> send_creation_progress()
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

        match pending.result {
            Ok(_) => {
                // Background work succeeded - now spawn the agent (fast, needs &mut state)
                log::info!(
                    "Background worktree ready for {:?}, spawning agent...",
                    pending.client_id
                );

                // Enter tokio runtime context for spawn_command_processor() which uses tokio::spawn()
                let _runtime_guard = self.tokio_runtime.enter();

                // Allocate a unique port for HTTP forwarding (before spawning)
                let port = self.allocate_unique_port();

                // Spawn agent (fast - just PTY creation) - release lock after spawning
                // Dims are carried in pending.config from the requesting client
                let spawn_result = {
                    let mut state = self.state.write().unwrap();
                    lifecycle::spawn_agent(&mut state, &pending.config, port)
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

        // Sync handle cache for thread-safe agent access
        self.sync_handle_cache();

        // Connect agent's channels (terminal + preview if port assigned)
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

        // Auto-select the new agent for the requesting client
        let session_key = result.agent_id.clone();
        actions::dispatch(
            self,
            HubAction::SelectAgentForClient {
                client_id: client_id.clone(),
                agent_key: session_key.clone(),
            },
        );

        // Broadcast AgentCreated event to all subscribers
        // BrowserClient reacts via handle_hub_event() -> sends agent list, agent_created, scrollback
        if let Some(info) = self.state.read().unwrap().get_agent_info(&session_key) {
            self.broadcast(HubEvent::agent_created(session_key, info));
        }

        // Refresh worktree cache - this agent's worktree is now in use
        if let Err(e) = self.load_available_worktrees() {
            log::warn!("Failed to refresh worktree cache after agent creation: {}", e);
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

    /// Initialize Signal Protocol CryptoService for E2E encryption.
    ///
    /// Starts the CryptoService only. PreKeyBundle generation is deferred until
    /// the connection URL is first requested (lazy initialization via
    /// `get_or_generate_connection_url()`).
    pub fn init_signal_protocol(&mut self) {
        registration::init_signal_protocol(&mut self.browser, &self.hub_identifier);
    }

    /// Get or generate the connection URL (lazy bundle generation).
    ///
    /// On first call, generates the PreKeyBundle and writes the URL to disk.
    /// Subsequent calls return the cached bundle unless it was used.
    ///
    /// # Returns
    ///
    /// The connection URL string, or an error message.
    pub fn get_or_generate_connection_url(&mut self) -> Result<String, String> {
        // Extract values before mutable borrow of browser
        let server_hub_id = self.server_hub_id().to_string();
        let local_id = self.hub_identifier.clone();
        let server_url = self.config.server_url.clone();

        registration::write_connection_url_lazy(
            &mut self.browser,
            &self.tokio_runtime,
            &server_hub_id,
            &local_id,
            &server_url,
        )
    }
}
