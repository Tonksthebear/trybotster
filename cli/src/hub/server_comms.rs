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
use crate::channel::Channel;
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
        self.poll_webrtc_channels();
        self.poll_webrtc_pty_output();
        self.poll_hub_events_for_webrtc();
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
                "webrtc_offer" => {
                    // Browser sent WebRTC SDP offer - create answer and send back
                    self.handle_webrtc_offer(&msg.payload);
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

    // === WebRTC Data Routing ===

    /// Poll WebRTC channels for incoming DataChannel messages (non-blocking).
    ///
    /// Processes messages from all connected browsers:
    /// - `subscribe`: Register a virtual subscription for routing
    /// - `unsubscribe`: Remove a virtual subscription
    /// - data messages: Route to appropriate handler based on subscription
    fn poll_webrtc_channels(&mut self) {
        // Collect browser identities to avoid borrowing issues
        let browser_ids: Vec<String> = self.webrtc_channels.keys().cloned().collect();

        if !browser_ids.is_empty() {
            log::trace!("[WebRTC] Polling {} channels", browser_ids.len());
        }

        for browser_identity in browser_ids {
            // Try to receive messages from this browser's DataChannel
            // We need to pass the runtime since try_recv uses block_on internally
            loop {
                let msg = self
                    .webrtc_channels
                    .get(&browser_identity)
                    .and_then(|ch| ch.try_recv(&self.tokio_runtime));

                match msg {
                    Some(m) => {
                        log::info!(
                            "[WebRTC] Received message from {} ({} bytes)",
                            &browser_identity[..browser_identity.len().min(8)],
                            m.payload.len()
                        );
                        self.handle_webrtc_message(&browser_identity, &m.payload);
                    }
                    None => break,
                }
            }
        }
    }

    /// Poll hub events and forward to WebRTC HubChannel subscribers.
    ///
    /// Receives hub events (AgentCreated, AgentDeleted, etc.) and broadcasts
    /// them to all browsers with active HubChannel subscriptions via WebRTC.
    fn poll_hub_events_for_webrtc(&mut self) {
        // Take receiver temporarily to avoid borrow issues
        let Some(mut rx) = self.webrtc_event_rx.take() else {
            return;
        };

        // Process all pending events
        loop {
            match rx.try_recv() {
                Ok(event) => {
                    log::debug!("[WebRTC] Hub event received: {:?}", event);
                    self.broadcast_hub_event_to_webrtc(&event);
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => {
                    log::warn!("[WebRTC] Lagged {} hub events", n);
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    log::info!("[WebRTC] Hub event channel closed");
                    return; // Don't restore rx since channel is closed
                }
            }
        }

        // Restore receiver
        self.webrtc_event_rx = Some(rx);
    }

    /// Broadcast a hub event to all WebRTC HubChannel subscribers.
    fn broadcast_hub_event_to_webrtc(&self, event: &HubEvent) {
        use crate::relay::TerminalMessage;

        // Find all HubChannel subscriptions
        let hub_subs: Vec<(String, String)> = self
            .webrtc_subscriptions
            .iter()
            .filter(|(_, sub)| sub.channel_name == "HubChannel")
            .map(|(sub_id, sub)| (sub_id.clone(), sub.browser_identity.clone()))
            .collect();

        if hub_subs.is_empty() {
            return;
        }

        match event {
            HubEvent::AgentCreated { agent_id, info: _ } => {
                log::info!(
                    "[WebRTC] Broadcasting AgentCreated to {} subscribers",
                    hub_subs.len()
                );

                // Send updated agent list
                for (sub_id, browser_id) in &hub_subs {
                    self.send_webrtc_agent_list(sub_id, browser_id);
                }

                // Send agent_created event
                let message = TerminalMessage::AgentCreated {
                    id: agent_id.clone(),
                };
                if let Ok(json) = serde_json::to_value(&message) {
                    for (sub_id, browser_id) in &hub_subs {
                        self.send_webrtc_message(sub_id, browser_id, json.clone());
                    }
                }
            }
            HubEvent::AgentDeleted { agent_id } => {
                log::info!(
                    "[WebRTC] Broadcasting AgentDeleted to {} subscribers",
                    hub_subs.len()
                );

                // Send updated agent list
                for (sub_id, browser_id) in &hub_subs {
                    self.send_webrtc_agent_list(sub_id, browser_id);
                }

                // Send agent_deleted event
                let message = TerminalMessage::AgentDeleted {
                    id: agent_id.clone(),
                };
                if let Ok(json) = serde_json::to_value(&message) {
                    for (sub_id, browser_id) in &hub_subs {
                        self.send_webrtc_message(sub_id, browser_id, json.clone());
                    }
                }
            }
            HubEvent::AgentStatusChanged { .. } => {
                // Send updated agent list
                for (sub_id, browser_id) in &hub_subs {
                    self.send_webrtc_agent_list(sub_id, browser_id);
                }
            }
            HubEvent::AgentCreationProgress { identifier, stage } => {
                let message = TerminalMessage::AgentCreatingProgress {
                    identifier: identifier.clone(),
                    stage: *stage,
                    message: stage.description().to_string(),
                };
                if let Ok(json) = serde_json::to_value(&message) {
                    for (sub_id, browser_id) in &hub_subs {
                        self.send_webrtc_message(sub_id, browser_id, json.clone());
                    }
                }
            }
            HubEvent::Error { message } => {
                let msg = TerminalMessage::Error {
                    message: message.clone(),
                };
                if let Ok(json) = serde_json::to_value(&msg) {
                    for (sub_id, browser_id) in &hub_subs {
                        self.send_webrtc_message(sub_id, browser_id, json.clone());
                    }
                }
            }
            _ => {
                // Other events not relevant for WebRTC broadcast
            }
        }
    }

    /// Handle a message received from a WebRTC DataChannel.
    ///
    /// Messages arrive in two forms:
    /// 1. Control messages: type="subscribe"/"unsubscribe" (also have subscriptionId)
    /// 2. Data messages: type="input"/"resize"/etc with subscriptionId
    ///
    /// Note: Signal envelope decryption happens inside WebRtcChannel.try_recv(),
    /// so we receive plaintext JSON here.
    fn handle_webrtc_message(&mut self, browser_identity: &str, payload: &[u8]) {
        let msg: serde_json::Value = match serde_json::from_slice(payload) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("[WebRTC] Failed to parse message: {e}");
                return;
            }
        };

        // Check message type first - control messages take priority
        if let Some(msg_type) = msg.get("type").and_then(|t| t.as_str()) {
            match msg_type {
                "subscribe" => {
                    self.handle_webrtc_subscribe(browser_identity, &msg);
                    return;
                }
                "unsubscribe" => {
                    self.handle_webrtc_unsubscribe(&msg);
                    return;
                }
                _ => {
                    // Other types (input, resize, etc.) fall through to data handling
                }
            }
        }

        // Data messages have subscriptionId and a command type (input, resize, etc.)
        if let Some(subscription_id) = msg.get("subscriptionId").and_then(|s| s.as_str()) {
            self.handle_webrtc_data(subscription_id, &msg);
        } else {
            log::debug!("[WebRTC] Message without subscriptionId: {:?}", msg);
        }
    }

    /// Handle WebRTC subscribe message - create virtual subscription.
    fn handle_webrtc_subscribe(&mut self, browser_identity: &str, msg: &serde_json::Value) {
        let subscription_id = match msg.get("subscriptionId").and_then(|s| s.as_str()) {
            Some(id) => id.to_string(),
            None => {
                log::warn!("[WebRTC] Subscribe missing subscriptionId");
                return;
            }
        };

        let channel_name = msg
            .get("channel")
            .and_then(|c| c.as_str())
            .unwrap_or("unknown")
            .to_string();

        let params = msg.get("params").cloned().unwrap_or(serde_json::Value::Null);
        let agent_index = params.get("agent_index").and_then(|a| a.as_u64()).map(|a| a as usize);
        let pty_index = params.get("pty_index").and_then(|p| p.as_u64()).map(|p| p as usize);

        log::info!(
            "[WebRTC] Subscribe: {} -> {} (agent={:?}, pty={:?})",
            &subscription_id[..subscription_id.len().min(16)],
            channel_name,
            agent_index,
            pty_index
        );

        // Store subscription mapping
        self.webrtc_subscriptions.insert(
            subscription_id.clone(),
            crate::hub::WebRtcSubscription {
                browser_identity: browser_identity.to_string(),
                channel_name,
                agent_index,
                pty_index,
            },
        );

        // Handle channel-specific subscription setup
        let sub = self.webrtc_subscriptions.get(&subscription_id).unwrap().clone();
        match sub.channel_name.as_str() {
            "HubChannel" => {
                // Send initial agent and worktree lists
                log::info!("[WebRTC] HubChannel subscription, sending initial data");
                self.send_webrtc_agent_list(&subscription_id, browser_identity);
                self.send_webrtc_worktree_list(&subscription_id, browser_identity);
            }
            "TerminalRelayChannel" => {
                if let (Some(ai), Some(pi)) = (agent_index, pty_index) {
                    log::info!(
                        "[WebRTC] Terminal subscription for agent={} pty={}, spawning PTY forwarder",
                        ai,
                        pi
                    );

                    // Get PTY handle from cache
                    if let Some(agent_handle) = self.handle_cache.get_agent(ai) {
                        if let Some(pty_handle) = agent_handle.get_pty(pi) {
                            // Key forwarders by (browser, agent, pty) to prevent duplicates.
                            // When browser reconnects with new subscription, abort old forwarder.
                            let forwarder_key = format!("{}:{}:{}", browser_identity, ai, pi);
                            if let Some(old_task) = self.webrtc_pty_forwarders.remove(&forwarder_key) {
                                old_task.abort();
                                log::info!("[WebRTC] Aborted old PTY forwarder for {}", forwarder_key);
                            }

                            // Subscribe to PTY events
                            let pty_rx = pty_handle.subscribe();

                            // Spawn forwarder task
                            let sub_id = subscription_id.clone();
                            let browser_id = browser_identity.to_string();
                            let output_tx = self.webrtc_pty_output_tx.clone();

                            let _guard = self.tokio_runtime.enter();
                            let task = tokio::spawn(spawn_webrtc_pty_forwarder(
                                pty_rx,
                                output_tx,
                                sub_id.clone(),
                                browser_id,
                                ai,
                                pi,
                            ));

                            // Store task handle for cleanup (keyed by browser:agent:pty)
                            self.webrtc_pty_forwarders.insert(forwarder_key, task);

                            // Send scrollback
                            let scrollback = pty_handle.get_scrollback();
                            if !scrollback.is_empty() {
                                let mut msg = Vec::with_capacity(1 + scrollback.len());
                                msg.push(0x01); // Raw terminal data prefix
                                msg.extend(&scrollback);
                                self.send_webrtc_raw(&subscription_id, browser_identity, msg);
                            }
                        } else {
                            log::warn!("[WebRTC] No PTY at index {} for agent {}", pi, ai);
                        }
                    } else {
                        log::warn!("[WebRTC] No agent at index {}", ai);
                    }
                }
            }
            _ => {}
        }
    }

    /// Handle WebRTC unsubscribe message - remove virtual subscription.
    fn handle_webrtc_unsubscribe(&mut self, msg: &serde_json::Value) {
        let subscription_id = match msg.get("subscriptionId").and_then(|s| s.as_str()) {
            Some(id) => id,
            None => {
                log::warn!("[WebRTC] Unsubscribe missing subscriptionId");
                return;
            }
        };

        if let Some(sub) = self.webrtc_subscriptions.remove(subscription_id) {
            log::info!(
                "[WebRTC] Unsubscribe: {} (was {})",
                &subscription_id[..subscription_id.len().min(16)],
                sub.channel_name
            );

            // If this was a terminal subscription, abort the forwarder task
            if sub.channel_name == "TerminalRelayChannel" {
                if let (Some(ai), Some(pi)) = (sub.agent_index, sub.pty_index) {
                    let forwarder_key = format!("{}:{}:{}", sub.browser_identity, ai, pi);
                    if let Some(task) = self.webrtc_pty_forwarders.remove(&forwarder_key) {
                        task.abort();
                        log::debug!("[WebRTC] Aborted PTY forwarder for {}", forwarder_key);
                    }
                }
            }
        }
    }

    /// Handle WebRTC data message - route to appropriate handler.
    ///
    /// Messages come in two formats:
    /// 1. From encrypted sendEnvelope: command at top level
    ///    `{ subscriptionId, type: "input", data: "ls" }`
    /// 2. From plaintext sendRaw: command nested under data
    ///    `{ subscriptionId, data: { type: "input", data: "ls" } }`
    fn handle_webrtc_data(&mut self, subscription_id: &str, msg: &serde_json::Value) {
        let sub = match self.webrtc_subscriptions.get(subscription_id) {
            Some(s) => s.clone(),
            None => {
                log::debug!("[WebRTC] Data for unknown subscription: {}", subscription_id);
                return;
            }
        };

        // Determine if command is at top level (encrypted flow) or nested under "data" (plaintext flow).
        // Encrypted messages have `type` at top level (e.g., "input", "resize").
        // Plaintext messages have `data` containing the command object.
        let command = if msg.get("type").is_some() && msg.get("type").and_then(|t| t.as_str()) != Some("subscribe") {
            // Command at top level (encrypted flow) - use whole message
            msg.clone()
        } else if let Some(data) = msg.get("data") {
            // Nested under "data" (plaintext flow)
            data.clone()
        } else {
            log::debug!("[WebRTC] Data message has no command: {:?}", msg);
            return;
        };

        match sub.channel_name.as_str() {
            "TerminalRelayChannel" => {
                if let (Some(ai), Some(pi)) = (sub.agent_index, sub.pty_index) {
                    self.handle_webrtc_terminal_data(&sub.browser_identity, ai, pi, &command);
                }
            }
            "HubChannel" => {
                self.handle_webrtc_hub_data(&sub.browser_identity, &command);
            }
            "PreviewChannel" => {
                // Preview data handled separately via HTTP proxying
                log::debug!("[WebRTC] Preview data received (handled via HTTP proxy)");
            }
            _ => {
                log::debug!("[WebRTC] Data for unknown channel: {}", sub.channel_name);
            }
        }
    }

    /// Handle terminal input data from WebRTC DataChannel.
    fn handle_webrtc_terminal_data(
        &mut self,
        browser_identity: &str,
        agent_index: usize,
        pty_index: usize,
        data: &serde_json::Value,
    ) {
        use crate::agent::PtyView;

        // Convert pty_index to PtyView (0=Cli, 1=Server)
        let view = if pty_index == 0 {
            PtyView::Cli
        } else {
            PtyView::Server
        };

        // Parse the terminal command (Input, Resize, etc.)
        let command: crate::relay::BrowserCommand = match serde_json::from_value(data.clone()) {
            Ok(cmd) => cmd,
            Err(e) => {
                log::debug!("[WebRTC] Failed to parse terminal command: {e}");
                return;
            }
        };

        // Get agent handle and write input
        let state = self.state.read().unwrap();
        let agent_key = state.agent_keys_ordered.get(agent_index).cloned();
        drop(state);

        let Some(key) = agent_key else {
            log::warn!("[WebRTC] No agent at index {agent_index}");
            return;
        };

        match command {
            crate::relay::BrowserCommand::Input { data } => {
                let mut state = self.state.write().unwrap();
                if let Some(agent) = state.agents.get_mut(&key) {
                    if let Err(e) = agent.write_input(view, data.as_bytes()) {
                        log::warn!("[WebRTC] Failed to write input: {e}");
                    }
                }
            }
            crate::relay::BrowserCommand::Resize { cols, rows } => {
                let mut state = self.state.write().unwrap();
                if let Some(agent) = state.agents.get_mut(&key) {
                    agent.resize_pty(view, rows, cols);
                }
            }
            crate::relay::BrowserCommand::Handshake { .. } => {
                log::debug!(
                    "[WebRTC] Handshake from browser {} for agent {} pty {}",
                    &browser_identity[..browser_identity.len().min(8)],
                    agent_index,
                    pty_index
                );
                // TODO: Send handshake ack back via WebRTC DataChannel
            }
            _ => {
                log::debug!("[WebRTC] Unhandled terminal command: {:?}", command);
            }
        }
    }

    /// Handle hub control data from WebRTC DataChannel.
    fn handle_webrtc_hub_data(&mut self, browser_identity: &str, data: &serde_json::Value) {
        // Check if data contains an encrypted envelope
        let decrypted_data = if let Some(envelope_val) = data.get("envelope") {
            let envelope: crate::relay::signal::SignalEnvelope = match serde_json::from_value(envelope_val.clone()) {
                Ok(env) => env,
                Err(e) => {
                    log::debug!("[WebRTC] Failed to parse envelope: {e}");
                    return;
                }
            };

            // Decrypt using crypto service
            let Some(ref crypto_service) = self.browser.crypto_service else {
                log::warn!("[WebRTC] No crypto service for decryption");
                return;
            };

            let _guard = self.tokio_runtime.enter();
            let plaintext = match self.tokio_runtime.block_on(crypto_service.decrypt(&envelope)) {
                Ok(p) => p,
                Err(e) => {
                    log::warn!("[WebRTC] Decryption failed: {e}");
                    return;
                }
            };

            // Strip compression marker (0x00 = uncompressed)
            let plaintext = if !plaintext.is_empty() && plaintext[0] == 0x00 {
                &plaintext[1..]
            } else {
                &plaintext[..]
            };

            match serde_json::from_slice::<serde_json::Value>(plaintext) {
                Ok(v) => v,
                Err(e) => {
                    log::debug!("[WebRTC] Failed to parse decrypted data: {e}");
                    return;
                }
            }
        } else {
            // Plaintext data (e.g., from older clients or control messages)
            data.clone()
        };

        // Parse as BrowserCommand and dispatch
        let command: crate::relay::BrowserCommand = match serde_json::from_value(decrypted_data) {
            Ok(cmd) => cmd,
            Err(e) => {
                log::debug!("[WebRTC] Failed to parse hub command: {e}");
                return;
            }
        };

        log::debug!(
            "[WebRTC] Hub command from {}: {:?}",
            &browser_identity[..browser_identity.len().min(8)],
            command
        );

        // Route to existing command handling
        // This mirrors what BrowserClient::handle_browser_command does
        // Find the subscription ID for this browser's HubChannel
        let subscription_id = self
            .webrtc_subscriptions
            .iter()
            .find(|(_, sub)| {
                sub.browser_identity == browser_identity && sub.channel_name == "HubChannel"
            })
            .map(|(id, _)| id.clone());

        match command {
            crate::relay::BrowserCommand::ListAgents => {
                if let Some(sub_id) = &subscription_id {
                    self.send_webrtc_agent_list(sub_id, browser_identity);
                }
            }
            crate::relay::BrowserCommand::ListWorktrees => {
                if let Some(sub_id) = &subscription_id {
                    self.send_webrtc_worktree_list(sub_id, browser_identity);
                }
            }
            crate::relay::BrowserCommand::SelectAgent { id } => {
                let client_id = crate::client::ClientId::Browser(browser_identity.to_string());
                self.handle_action(crate::hub::HubAction::SelectAgentForClient {
                    client_id,
                    agent_key: id,
                });
            }
            crate::relay::BrowserCommand::CreateAgent {
                issue_or_branch,
                prompt,
            } => {
                let client_id = crate::client::ClientId::Browser(browser_identity.to_string());
                let request = crate::client::CreateAgentRequest {
                    issue_or_branch: issue_or_branch.unwrap_or_default(),
                    prompt,
                    from_worktree: None,
                    dims: Some((80, 24)), // Default terminal size for browser
                };
                log::info!(
                    "[WebRTC] CreateAgent from {}: {:?}",
                    &browser_identity[..browser_identity.len().min(8)],
                    request.issue_or_branch
                );
                self.handle_action(crate::hub::HubAction::CreateAgentForClient {
                    client_id,
                    request,
                });
            }
            crate::relay::BrowserCommand::ReopenWorktree {
                path,
                branch,
                prompt,
            } => {
                let client_id = crate::client::ClientId::Browser(browser_identity.to_string());
                let request = crate::client::CreateAgentRequest {
                    issue_or_branch: branch,
                    prompt,
                    from_worktree: Some(std::path::PathBuf::from(&path)),
                    dims: Some((80, 24)),
                };
                log::info!(
                    "[WebRTC] ReopenWorktree from {}: {}",
                    &browser_identity[..browser_identity.len().min(8)],
                    &path
                );
                self.handle_action(crate::hub::HubAction::CreateAgentForClient {
                    client_id,
                    request,
                });
            }
            crate::relay::BrowserCommand::DeleteAgent { id, delete_worktree } => {
                let client_id = crate::client::ClientId::Browser(browser_identity.to_string());
                let request = crate::client::DeleteAgentRequest {
                    agent_id: id.clone(),
                    delete_worktree: delete_worktree.unwrap_or(false),
                };
                log::info!(
                    "[WebRTC] DeleteAgent from {}: {}",
                    &browser_identity[..browser_identity.len().min(8)],
                    &id
                );
                self.handle_action(crate::hub::HubAction::DeleteAgentForClient {
                    client_id,
                    request,
                });
            }
            crate::relay::BrowserCommand::Handshake { device_name, .. } => {
                log::info!(
                    "[WebRTC] Handshake from {}: device={}",
                    &browser_identity[..browser_identity.len().min(8)],
                    device_name
                );
                // Send Ack to complete handshake - browser buffers commands until it receives this
                if let Some(sub_id) = &subscription_id {
                    let ack = crate::relay::TerminalMessage::Ack {
                        timestamp: Some(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0),
                        ),
                    };
                    let data = serde_json::to_value(&ack).unwrap_or_default();
                    self.send_webrtc_message(sub_id, browser_identity, data);
                    log::debug!("[WebRTC] Sent Ack to {}", &browser_identity[..browser_identity.len().min(8)]);
                }
            }
            crate::relay::BrowserCommand::Ack { .. } => {
                // Browser acknowledged our message - nothing to do
                log::debug!("[WebRTC] Received Ack from {}", &browser_identity[..browser_identity.len().min(8)]);
            }
            _ => {
                log::debug!("[WebRTC] Unhandled hub command: {:?}", command);
            }
        }
    }

    // === WebRTC Send Methods ===

    /// Send a message to a WebRTC subscription.
    ///
    /// Wraps the data with the subscriptionId and sends via DataChannel.
    /// Uses plaintext since control messages don't need E2E encryption
    /// (DTLS provides transport security).
    fn send_webrtc_message(
        &self,
        subscription_id: &str,
        browser_identity: &str,
        data: serde_json::Value,
    ) {
        let Some(channel) = self.webrtc_channels.get(browser_identity) else {
            log::warn!(
                "[WebRTC] No channel for browser {} when sending message",
                &browser_identity[..browser_identity.len().min(8)]
            );
            return;
        };

        // Wrap data with subscriptionId for routing on browser side
        let message = serde_json::json!({
            "subscriptionId": subscription_id,
            "data": data
        });

        let payload = match serde_json::to_vec(&message) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("[WebRTC] Failed to serialize message: {e}");
                return;
            }
        };

        // Send via WebRTC DataChannel with Signal Protocol E2E encryption
        let peer = crate::channel::PeerId(browser_identity.to_string());
        let _guard = self.tokio_runtime.enter();
        if let Err(e) = self.tokio_runtime.block_on(channel.send_to(&payload, &peer)) {
            log::warn!("[WebRTC] Failed to send message: {e}");
        }
    }

    /// Send agent list to a WebRTC subscription.
    fn send_webrtc_agent_list(&self, subscription_id: &str, browser_identity: &str) {
        use crate::relay::TerminalMessage;

        let handles = self.handle_cache.get_all_agents();
        let hub_id = self.server_hub_id();

        let agents: Vec<crate::relay::AgentInfo> = handles
            .iter()
            .map(|h| {
                let mut info = h.info().clone();
                if info.hub_identifier.is_none() {
                    info.hub_identifier = Some(hub_id.to_string());
                }
                info
            })
            .collect();

        log::info!(
            "[WebRTC] Sending agent list ({} agents) to subscription {}",
            agents.len(),
            &subscription_id[..subscription_id.len().min(16)]
        );

        let message = TerminalMessage::Agents { agents };
        let json = match serde_json::to_value(&message) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("[WebRTC] Failed to serialize agent list: {e}");
                return;
            }
        };

        self.send_webrtc_message(subscription_id, browser_identity, json);
    }

    /// Send worktree list to a WebRTC subscription.
    fn send_webrtc_worktree_list(&self, subscription_id: &str, browser_identity: &str) {
        use crate::relay::{state::build_worktree_info, TerminalMessage};

        // HandleCache stores worktrees as (path, branch) tuples
        let worktrees_raw = self.handle_cache.get_worktrees();

        let worktrees: Vec<_> = worktrees_raw
            .iter()
            .map(|(path, branch)| build_worktree_info(path, branch))
            .collect();

        let repo = crate::git::WorktreeManager::detect_current_repo()
            .map(|(_, name)| name)
            .ok();

        log::info!(
            "[WebRTC] Sending worktree list ({} worktrees) to subscription {}",
            worktrees.len(),
            &subscription_id[..subscription_id.len().min(16)]
        );

        let message = TerminalMessage::Worktrees { worktrees, repo };
        let json = match serde_json::to_value(&message) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("[WebRTC] Failed to serialize worktree list: {e}");
                return;
            }
        };

        self.send_webrtc_message(subscription_id, browser_identity, json);
    }

    /// Send raw bytes to a WebRTC subscription (for PTY output).
    ///
    /// Unlike `send_webrtc_message`, this sends raw bytes without JSON wrapping.
    /// The browser distinguishes raw terminal data by the 0x01 prefix byte.
    fn send_webrtc_raw(
        &self,
        subscription_id: &str,
        browser_identity: &str,
        data: Vec<u8>,
    ) {
        let Some(channel) = self.webrtc_channels.get(browser_identity) else {
            log::warn!(
                "[WebRTC] No channel for browser {} when sending raw data",
                &browser_identity[..browser_identity.len().min(8)]
            );
            return;
        };

        // Wrap raw data with subscriptionId for routing on browser side
        // Format: { "subscriptionId": "...", "raw": <base64 encoded bytes> }
        // Browser detects "raw" key and decodes base64 to pass to xterm
        let message = serde_json::json!({
            "subscriptionId": subscription_id,
            "raw": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data)
        });

        let payload = match serde_json::to_vec(&message) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("[WebRTC] Failed to serialize raw message: {e}");
                return;
            }
        };

        // Send via WebRTC DataChannel with Signal Protocol E2E encryption
        let peer = crate::channel::PeerId(browser_identity.to_string());
        let _guard = self.tokio_runtime.enter();
        if let Err(e) = self.tokio_runtime.block_on(channel.send_to(&payload, &peer)) {
            log::warn!("[WebRTC] Failed to send raw data: {e}");
        }
    }

    /// Poll for queued PTY output and send via WebRTC.
    ///
    /// Forwarder tasks queue `WebRtcPtyOutput` messages; this drains and sends them.
    fn poll_webrtc_pty_output(&mut self) {
        // Drain all pending PTY output messages
        while let Ok(msg) = self.webrtc_pty_output_rx.try_recv() {
            self.send_webrtc_raw(&msg.subscription_id, &msg.browser_identity, msg.data);
        }
    }

    // === WebRTC Signaling ===

    /// Handle incoming WebRTC offer from browser.
    ///
    /// Creates or reuses a WebRTC peer connection for the browser, processes
    /// the SDP offer, and sends the answer back via the signaling endpoint.
    fn handle_webrtc_offer(&mut self, payload: &serde_json::Value) {
        use crate::channel::{ChannelConfig, WebRtcChannel};

        let sdp = match payload.get("sdp").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                log::warn!("[WebRTC] Offer missing SDP");
                return;
            }
        };

        let browser_identity = match payload.get("browser_identity").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                log::warn!("[WebRTC] Offer missing browser_identity");
                return;
            }
        };

        let hub_id = self.server_hub_id().to_string();
        let server_url = self.config.server_url.clone();
        let api_key = self.config.get_api_key().to_string();

        log::info!(
            "[WebRTC] Received offer from browser {}",
            &browser_identity[..browser_identity.len().min(8)]
        );

        // Get or create WebRTC channel for this browser
        if !self.webrtc_channels.contains_key(&browser_identity) {
            let mut channel = WebRtcChannel::builder()
                .server_url(&server_url)
                .api_key(&api_key)
                .crypto_service(self.browser.crypto_service.clone().expect("crypto service required"))
                .build();

            // Configure the channel with hub_id
            let config = ChannelConfig {
                channel_name: "WebRtcChannel".to_string(),
                hub_id: hub_id.clone(),
                agent_index: None,
                pty_index: None,
                browser_identity: Some(browser_identity.clone()),
                encrypt: true,
                compression_threshold: Some(4096),
                cli_subscription: false,
            };

            // Connect the channel (sets up config, this is sync-safe)
            let _guard = self.tokio_runtime.enter();
            if let Err(e) = self.tokio_runtime.block_on(channel.connect(config)) {
                log::error!("[WebRTC] Failed to configure channel: {e}");
                return;
            }

            self.webrtc_channels.insert(browser_identity.clone(), channel);
        }

        // Handle the offer and get the answer
        let channel = self.webrtc_channels.get(&browser_identity).unwrap();
        let _guard = self.tokio_runtime.enter();

        match self.tokio_runtime.block_on(channel.handle_sdp_offer(&sdp, &browser_identity)) {
            Ok(answer_sdp) => {
                log::info!(
                    "[WebRTC] Created answer for browser {}",
                    &browser_identity[..browser_identity.len().min(8)]
                );

                // Send answer back via signaling endpoint
                let url = format!("{}/hubs/{}/webrtc_signals", server_url, hub_id);
                let client = reqwest::blocking::Client::new();

                let body = serde_json::json!({
                    "signal_type": "answer",
                    "browser_identity": browser_identity,
                    "sdp": answer_sdp,
                });

                match client.post(&url).bearer_auth(&api_key).json(&body).send() {
                    Ok(resp) if resp.status().is_success() => {
                        log::info!("[WebRTC] Answer sent successfully");
                        // Spawn background task to poll for browser's ICE candidates
                        self.spawn_ice_candidate_poller(
                            browser_identity.clone(),
                            hub_id.clone(),
                            server_url.clone(),
                            api_key.clone(),
                        );
                    }
                    Ok(resp) => {
                        log::error!("[WebRTC] Failed to send answer: {}", resp.status());
                    }
                    Err(e) => {
                        log::error!("[WebRTC] Failed to send answer: {e}");
                    }
                }
            }
            Err(e) => {
                log::error!("[WebRTC] Failed to handle offer: {e}");
            }
        }
    }

    /// Spawn a background task to poll for ICE candidates from the browser.
    ///
    /// Polls the signaling endpoint periodically to collect trickle ICE candidates.
    /// The task runs for a limited time (enough for typical ICE gathering).
    fn spawn_ice_candidate_poller(
        &self,
        browser_identity: String,
        hub_id: String,
        server_url: String,
        api_key: String,
    ) {
        // Get reference to the WebRTC channel
        let Some(channel) = self.webrtc_channels.get(&browser_identity) else {
            log::warn!("[WebRTC] No channel for browser {} during ICE polling", &browser_identity[..8]);
            return;
        };

        // Clone necessary references for the async task
        // We need to be careful here - WebRtcChannel has async methods
        // For now, we'll use a separate HTTP client and call handle_ice_candidate

        // Since WebRtcChannel is not Clone, we spawn a task that:
        // 1. Polls for ICE candidates via HTTP
        // 2. The candidates will be applied when we call handle_ice_candidate
        // For this initial implementation, we'll do synchronous polling

        log::info!("[WebRTC] Starting ICE candidate polling for browser {}", &browser_identity[..8]);

        let url = format!(
            "{}/hubs/{}/webrtc_signals?browser_identity={}",
            server_url, hub_id, browser_identity
        );

        // Poll a few times with delays (blocking, but quick)
        let client = reqwest::blocking::Client::new();
        for i in 0..10 {
            std::thread::sleep(std::time::Duration::from_millis(200));

            let response = match client.get(&url).bearer_auth(&api_key).send() {
                Ok(r) => r,
                Err(e) => {
                    log::warn!("[WebRTC] ICE poll request failed: {e}");
                    continue;
                }
            };

            if !response.status().is_success() {
                continue;
            }

            #[derive(serde::Deserialize)]
            struct SignalResponse {
                signals: Vec<serde_json::Value>,
            }

            let signals: SignalResponse = match response.json() {
                Ok(s) => s,
                Err(_) => continue,
            };

            for signal in &signals.signals {
                if signal.get("type").and_then(|t| t.as_str()) == Some("ice") {
                    if let Some(candidate_obj) = signal.get("candidate") {
                        let candidate = candidate_obj.get("candidate")
                            .and_then(|c| c.as_str())
                            .unwrap_or("");
                        let sdp_mid = candidate_obj.get("sdpMid")
                            .and_then(|m| m.as_str());
                        let sdp_mline_index = candidate_obj.get("sdpMLineIndex")
                            .and_then(|i| i.as_u64())
                            .map(|i| i as u16);

                        log::debug!("[WebRTC] Received ICE candidate from browser (poll {})", i);

                        // Add candidate to peer connection
                        let _guard = self.tokio_runtime.enter();
                        if let Err(e) = self.tokio_runtime.block_on(
                            channel.handle_ice_candidate(candidate, sdp_mid, sdp_mline_index)
                        ) {
                            log::warn!("[WebRTC] Failed to add ICE candidate: {e}");
                        }
                    }
                }
            }

            // If no signals for a while, stop polling
            if signals.signals.is_empty() && i > 3 {
                log::debug!("[WebRTC] No more ICE candidates, stopping poll");
                break;
            }
        }

        log::info!("[WebRTC] ICE candidate polling complete for browser {}", &browser_identity[..8]);
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

/// Background task that forwards PTY output to WebRTC via the output queue.
///
/// Subscribes to PTY events and sends raw bytes (with 0x01 prefix) to the
/// output queue. The main loop drains and sends via WebRTC DataChannel.
///
/// Exits when the PTY closes or the task is aborted on unsubscribe.
async fn spawn_webrtc_pty_forwarder(
    mut pty_rx: tokio::sync::broadcast::Receiver<crate::agent::pty::PtyEvent>,
    output_tx: tokio::sync::mpsc::UnboundedSender<crate::hub::WebRtcPtyOutput>,
    subscription_id: String,
    browser_identity: String,
    agent_index: usize,
    pty_index: usize,
) {
    use crate::agent::pty::PtyEvent;
    use crate::relay::TerminalMessage;

    log::info!(
        "[WebRTC] Started PTY forwarder for browser {} agent {} pty {}",
        &browser_identity[..browser_identity.len().min(8)],
        agent_index,
        pty_index
    );

    loop {
        match pty_rx.recv().await {
            Ok(PtyEvent::Output(data)) => {
                // Send raw bytes with 0x01 prefix (no JSON, no UTF-8 conversion).
                // Browser detects prefix and passes bytes directly to xterm.
                let mut raw_message = Vec::with_capacity(1 + data.len());
                raw_message.push(0x01); // Raw terminal data prefix
                raw_message.extend(&data);

                // Queue for main loop to send
                if output_tx
                    .send(crate::hub::WebRtcPtyOutput {
                        subscription_id: subscription_id.clone(),
                        browser_identity: browser_identity.clone(),
                        data: raw_message,
                    })
                    .is_err()
                {
                    log::debug!("[WebRTC] PTY output queue closed, stopping forwarder");
                    break;
                }
            }
            Ok(PtyEvent::ProcessExited { exit_code }) => {
                log::info!(
                    "[WebRTC] PTY process exited (code={:?}) for agent {} pty {}",
                    exit_code,
                    agent_index,
                    pty_index
                );
                // Send exit notification
                let message = TerminalMessage::ProcessExited { exit_code };
                if let Ok(json) = serde_json::to_string(&message) {
                    let _ = output_tx.send(crate::hub::WebRtcPtyOutput {
                        subscription_id: subscription_id.clone(),
                        browser_identity: browser_identity.clone(),
                        data: json.into_bytes(),
                    });
                }
            }
            Ok(_other_event) => {
                // Ignore other events (Resized, OwnerChanged).
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                log::warn!(
                    "[WebRTC] PTY forwarder lagged by {} events for agent {} pty {}",
                    n,
                    agent_index,
                    pty_index
                );
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                log::info!(
                    "[WebRTC] PTY channel closed for agent {} pty {}",
                    agent_index,
                    pty_index
                );
                break;
            }
        }
    }

    log::info!(
        "[WebRTC] Stopped PTY forwarder for browser {} agent {} pty {}",
        &browser_identity[..browser_identity.len().min(8)],
        agent_index,
        pty_index
    );
}
