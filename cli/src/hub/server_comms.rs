//! Server communication for Hub.
//!
//! This module handles all communication with the Rails server, including:
//!
//! - WebSocket command channel for real-time message delivery
//! - Heartbeat sending via command channel
//! - WebRTC signaling via ActionCable (encrypted with Signal Protocol)
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
        self.poll_signal_channel();
        self.poll_outgoing_signals();
        self.poll_webrtc_channels();
        self.poll_webrtc_pty_output();
        self.poll_tui_requests();
        self.poll_tui_hub_events();
        self.send_command_channel_heartbeat();
        self.poll_agent_notifications_async();
        self.poll_lua_file_changes();
        // Flush any Lua-queued operations (WebRTC sends, TUI sends, PTY requests, Hub requests)
        // This catches any events fired outside the normal message flow
        self.flush_lua_queues();
    }

    /// Flush all Lua-queued operations.
    ///
    /// Processes WebRTC sends, TUI sends, PTY requests, and Hub requests that Lua
    /// callbacks may have queued. Called automatically in `tick()` to ensure all
    /// queued operations are processed without requiring manual calls after each
    /// Lua event.
    pub fn flush_lua_queues(&mut self) {
        self.process_lua_webrtc_sends();
        self.process_lua_tui_sends();
        self.process_lua_pty_requests();
        self.process_lua_hub_requests();
        self.process_lua_connection_requests();
    }

    /// Poll for Lua file changes and hot-reload modified modules.
    ///
    /// This is a no-op if file watching is not enabled.
    fn poll_lua_file_changes(&self) {
        let reloaded = self.lua.poll_and_reload();
        if reloaded > 0 {
            log::info!("Hot-reloaded {} Lua module(s)", reloaded);
        }
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
                // Note: "browser_connected" and "browser_disconnected" events are no longer
                // sent by Rails since HubChannel was deleted. Browser communication now
                // happens directly via WebRTC (see handle_webrtc_* methods below).
                // These event types remain in Bot::Message validation for legacy compatibility.
                "terminal_connected" => {
                    // Browser wants terminal I/O for a specific PTY (legacy notification path).
                    // WebRTC browsers now subscribe directly via DataChannel.
                    let agent_index = msg.payload.get("agent_index").and_then(|v| v.as_u64());
                    let pty_index = msg.payload.get("pty_index").and_then(|v| v.as_u64());
                    let browser_identity = msg.payload
                        .get("browser_identity")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");

                    log::debug!(
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

                        // Broadcast event so WebRTC can set up PTY forwarding.
                        let client_id = ClientId::Browser(browser_identity.to_string());
                        log::debug!(
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
                    // Browser no longer wants terminal I/O (legacy notification path).
                    // WebRTC browsers unsubscribe directly via DataChannel.
                    let agent_index = msg.payload.get("agent_index").and_then(|v| v.as_u64());
                    let pty_index = msg.payload.get("pty_index").and_then(|v| v.as_u64());
                    let browser_identity = msg.payload
                        .get("browser_identity")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");

                    log::debug!(
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
                    // Browser subscribed to PreviewChannel - notify to create HttpChannel.
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

                    log::debug!(
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

        // Broadcast progress event to all subscribers (WebRTC, TUI)
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
                        log::error!("Failed to spawn agent for {}: {}", pending.client_id, e);
                    }
                }
            }
            Err(e) => {
                log::error!(
                    "Background agent creation failed for {:?}: {}",
                    pending.client_id,
                    e
                );
                // Error already logged above
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

        // Refresh worktree cache BEFORE firing events so Lua sees updated state
        if let Err(e) = self.load_available_worktrees() {
            log::warn!("Failed to refresh worktree cache after agent creation: {}", e);
        }

        // Broadcast AgentCreated event to all subscribers (WebRTC, TUI)
        // Get info and release lock before calling methods that need &mut self
        let info = self.state.read().unwrap().get_agent_info(&session_key);

        if let Some(info) = info {
            self.broadcast(HubEvent::agent_created(session_key.clone(), info.clone()));

            // Fire Lua event for agent_created
            // Note: Queued WebRTC sends are flushed automatically in tick()
            if let Err(e) = self.lua.fire_agent_created(&session_key, &info) {
                log::warn!("Lua agent_created event error: {}", e);
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
            log::trace!("[WebRTC-POLL] Polling {} channels", browser_ids.len());
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
                        self.handle_webrtc_message(&browser_identity, &m.payload);
                    }
                    None => break,
                }
            }

            // Check for repeated decryption failures (session desync)
            if let Some(channel) = self.webrtc_channels.get(&browser_identity) {
                let failures = channel.decrypt_failure_count();
                if failures >= 3 {
                    log::warn!(
                        "[WebRTC] {} consecutive decryption failures for {}, sending session_invalid",
                        failures,
                        &browser_identity[..browser_identity.len().min(8)]
                    );
                    channel.reset_decrypt_failures();

                    let msg = serde_json::json!({
                        "type": "session_invalid",
                        "reason": "decryption_failed",
                        "message": "Signal session out of sync. Please re-pair.",
                    });
                    if let Ok(payload) = serde_json::to_vec(&msg) {
                        let _guard = self.tokio_runtime.enter();
                        if let Err(e) = self.tokio_runtime.block_on(channel.send_plaintext(&payload)) {
                            log::warn!("[WebRTC] Failed to send session_invalid: {e}");
                        }
                    }
                }
            }
        }
    }

    /// Handle a message received from a WebRTC DataChannel.
    ///
    /// All message handling is delegated to Lua. The message is passed to Lua's
    /// on_message callback which routes to the appropriate handler (subscribe,
    /// unsubscribe, terminal data, hub commands, etc.).
    ///
    /// Note: Signal envelope decryption happens inside WebRtcChannel.try_recv(),
    /// so we receive plaintext JSON here.
    fn handle_webrtc_message(&mut self, browser_identity: &str, payload: &[u8]) {
        let msg: serde_json::Value = match serde_json::from_slice::<serde_json::Value>(payload) {
            Ok(v) => v,
            Err(e) => {
                log::error!("[WebRTC-MSG] JSON parse failed: {e}");
                return;
            }
        };

        // Delegate all message handling to Lua
        self.call_lua_webrtc_message(browser_identity, msg);
    }

    /// Call Lua WebRTC message handler and process any queued sends.
    ///
    /// Passes the decrypted message to Lua's on_message callback (if registered).
    /// After the callback returns, drains any messages that Lua queued via
    /// webrtc.send() and sends them to the appropriate peers, and also processes
    /// any PTY requests that Lua queued.
    fn call_lua_webrtc_message(&mut self, browser_identity: &str, msg: serde_json::Value) {
        // Call Lua callback
        if let Err(e) = self.lua.call_webrtc_message(browser_identity, msg) {
            log::error!("[WebRTC-LUA] Lua callback error: {e}");
        }

        // Process any sends, PTY requests, and Hub requests that Lua queued
        self.process_lua_webrtc_sends();
        self.process_lua_pty_requests();
        self.process_lua_hub_requests();
    }

    /// Process WebRTC send requests queued by Lua callbacks.
    ///
    /// Drains the Lua send queue and sends each message to the target peer.
    /// Called after any Lua callback that might queue messages.
    pub fn process_lua_webrtc_sends(&mut self) {
        use crate::lua::primitives::WebRtcSendRequest;

        for send_req in self.lua.drain_webrtc_sends() {
            match send_req {
                WebRtcSendRequest::Json { peer_id, data } => {
                    // Find the HubChannel subscription for this peer (if any)
                    // For Lua sends, we send directly without subscription wrapping
                    // since Lua handles its own message framing.
                    if let Some(channel) = self.webrtc_channels.get(&peer_id) {
                        let payload = match serde_json::to_vec(&data) {
                            Ok(p) => p,
                            Err(e) => {
                                log::warn!("[WebRTC] Lua send failed to serialize: {e}");
                                continue;
                            }
                        };

                        let peer = crate::channel::PeerId(peer_id.clone());
                        let _guard = self.tokio_runtime.enter();
                        if let Err(e) = self.tokio_runtime.block_on(channel.send_to(&payload, &peer)) {
                            log::warn!("[WebRTC] Lua send failed: {e}");
                        }
                    } else {
                        log::debug!("[WebRTC] Lua send to unknown peer: {}", &peer_id[..peer_id.len().min(8)]);
                    }
                }
                WebRtcSendRequest::Binary { peer_id, data } => {
                    if let Some(channel) = self.webrtc_channels.get(&peer_id) {
                        let peer = crate::channel::PeerId(peer_id.clone());
                        let _guard = self.tokio_runtime.enter();
                        if let Err(e) = self.tokio_runtime.block_on(channel.send_to(&data, &peer)) {
                            log::warn!("[WebRTC] Lua binary send failed: {e}");
                        }
                    } else {
                        log::debug!("[WebRTC] Lua binary send to unknown peer: {}", &peer_id[..peer_id.len().min(8)]);
                    }
                }
            }
        }
    }

    /// Process PTY requests queued by Lua callbacks.
    ///
    /// Drains the Lua PTY request queue and processes each request.
    /// Called after any Lua callback that might queue PTY operations.
    pub fn process_lua_pty_requests(&mut self) {
        use crate::lua::PtyRequest;

        for request in self.lua.drain_pty_requests() {
            match request {
                PtyRequest::CreateForwarder(req) => {
                    self.create_lua_pty_forwarder(req);
                }
                PtyRequest::CreateTuiForwarder(req) => {
                    self.create_lua_tui_pty_forwarder(req);
                }
                PtyRequest::StopForwarder { forwarder_id } => {
                    self.stop_lua_pty_forwarder(&forwarder_id);
                }
                PtyRequest::WritePty {
                    agent_index,
                    pty_index,
                    data,
                } => {
                    if let Some(agent_handle) = self.handle_cache.get_agent(agent_index) {
                        if let Some(pty_handle) = agent_handle.get_pty(pty_index) {
                            if let Err(e) = pty_handle.write_input_direct(&data) {
                                log::error!("[PTY-WRITE] Write failed: {e}");
                            }
                        } else {
                            log::warn!("[PTY-WRITE] No PTY at index {} for agent {}", pty_index, agent_index);
                        }
                    } else {
                        log::warn!("[PTY-WRITE] No agent at index {}", agent_index);
                    }
                }
                PtyRequest::ResizePty {
                    agent_index,
                    pty_index,
                    rows,
                    cols,
                } => {
                    if let Some(agent_handle) = self.handle_cache.get_agent(agent_index) {
                        if let Some(pty_handle) = agent_handle.get_pty(pty_index) {
                            // For Lua-initiated resize, use a synthetic client ID
                            pty_handle.resize_direct(crate::client::ClientId::Internal, rows, cols);
                        } else {
                            log::debug!("[Lua] No PTY at index {} for agent {}", pty_index, agent_index);
                        }
                    } else {
                        log::debug!("[Lua] No agent at index {}", agent_index);
                    }
                }
                PtyRequest::GetScrollback {
                    agent_index,
                    pty_index,
                    response_key,
                } => {
                    let scrollback = if let Some(agent_handle) = self.handle_cache.get_agent(agent_index) {
                        if let Some(pty_handle) = agent_handle.get_pty(pty_index) {
                            pty_handle.get_scrollback()
                        } else {
                            Vec::new()
                        }
                    } else {
                        Vec::new()
                    };
                    // Store response for Lua to retrieve
                    self.lua.set_scrollback_response(&response_key, scrollback);
                }
            }
        }
    }

    /// Process Hub requests queued by Lua callbacks.
    ///
    /// Drains the Lua Hub request queue and processes each request.
    /// Called after any Lua callback that might queue agent lifecycle operations.
    pub fn process_lua_hub_requests(&mut self) {
        use crate::client::{CreateAgentRequest, DeleteAgentRequest};
        use crate::lua::primitives::HubRequest;

        for request in self.lua.drain_hub_requests() {
            match request {
                HubRequest::CreateAgent {
                    issue_or_branch,
                    prompt,
                    from_worktree,
                    response_key,
                } => {
                    log::info!(
                        "[Lua] Processing create_agent request: {} (key: {})",
                        issue_or_branch,
                        &response_key[..response_key.len().min(24)]
                    );

                    let request = CreateAgentRequest {
                        issue_or_branch,
                        prompt,
                        from_worktree: from_worktree.map(std::path::PathBuf::from),
                        dims: None, // Lua doesn't provide dims currently
                    };

                    // Use Internal client ID since this is from Lua
                    actions::dispatch(
                        self,
                        HubAction::CreateAgentForClient {
                            client_id: ClientId::Internal,
                            request,
                        },
                    );
                }
                HubRequest::DeleteAgent {
                    agent_id,
                    delete_worktree,
                } => {
                    log::info!(
                        "[Lua] Processing delete_agent request: {} (delete_worktree: {})",
                        agent_id,
                        delete_worktree
                    );

                    let request = DeleteAgentRequest {
                        agent_id,
                        delete_worktree,
                    };

                    actions::dispatch(
                        self,
                        HubAction::DeleteAgentForClient {
                            client_id: ClientId::Internal,
                            request,
                        },
                    );
                }
            }
        }
    }

    /// Process connection requests queued by Lua callbacks.
    ///
    /// Drains the Lua connection request queue and processes each request.
    /// Called after any Lua callback that might queue connection operations.
    pub fn process_lua_connection_requests(&mut self) {
        use crate::lua::primitives::ConnectionRequest;

        for request in self.lua.drain_connection_requests() {
            match request {
                ConnectionRequest::Regenerate => {
                    log::info!("[Lua] Processing connection.regenerate() request");
                    actions::dispatch(self, HubAction::RegenerateConnectionCode);
                }
            }
        }
    }

    /// Create a PTY forwarder requested by Lua.
    ///
    /// Spawns a new forwarder task that streams PTY output to WebRTC.
    fn create_lua_pty_forwarder(&mut self, req: crate::lua::CreateForwarderRequest) {
        let forwarder_key = format!("{}:{}:{}", req.peer_id, req.agent_index, req.pty_index);

        // Check if agent and PTY exist
        let Some(agent_handle) = self.handle_cache.get_agent(req.agent_index) else {
            log::warn!("[Lua] Cannot create forwarder: no agent at index {}", req.agent_index);
            // Mark forwarder as inactive
            *req.active_flag.lock().unwrap() = false;
            return;
        };

        let Some(pty_handle) = agent_handle.get_pty(req.pty_index) else {
            log::warn!(
                "[Lua] Cannot create forwarder: no PTY at index {} for agent {}",
                req.pty_index,
                req.agent_index
            );
            *req.active_flag.lock().unwrap() = false;
            return;
        };

        // Abort any existing forwarder for this key
        if let Some(old_task) = self.webrtc_pty_forwarders.remove(&forwarder_key) {
            old_task.abort();
            log::debug!("[Lua] Aborted existing PTY forwarder for {}", forwarder_key);
        }

        // Subscribe to PTY events
        let pty_rx = pty_handle.subscribe();

        // Get scrollback buffer to send initially
        let scrollback = pty_handle.get_scrollback();

        // Spawn forwarder task
        let output_tx = self.webrtc_pty_output_tx.clone();
        let peer_id = req.peer_id.clone();
        let agent_index = req.agent_index;
        let pty_index = req.pty_index;
        let prefix = req.prefix.unwrap_or_else(|| vec![0x01]);
        let active_flag = req.active_flag;

        // Use browser-provided subscription ID for message routing
        let subscription_id = req.subscription_id;

        let _guard = self.tokio_runtime.enter();
        let task = tokio::spawn(async move {
            use crate::agent::pty::PtyEvent;
            use crate::hub::WebRtcPtyOutput;

            log::info!(
                "[Lua] Started PTY forwarder for peer {} agent {} pty {}",
                &peer_id[..peer_id.len().min(8)],
                agent_index,
                pty_index
            );

            // Send scrollback buffer first (if any)
            if !scrollback.is_empty() {
                let mut raw_message = Vec::with_capacity(prefix.len() + scrollback.len());
                raw_message.extend(&prefix);
                raw_message.extend(&scrollback);

                log::debug!(
                    "[Lua] Sending {} bytes of scrollback for agent {} pty {}",
                    scrollback.len(),
                    agent_index,
                    pty_index
                );

                if output_tx
                    .send(WebRtcPtyOutput {
                        subscription_id: subscription_id.clone(),
                        browser_identity: peer_id.clone(),
                        data: raw_message,
                    })
                    .is_err()
                {
                    log::trace!("[Lua] PTY output queue closed before scrollback sent");
                    return;
                }
            }

            let mut pty_rx = pty_rx;
            loop {
                // Check if forwarder was stopped by Lua
                {
                    let active = active_flag.lock().unwrap();
                    if !*active {
                        log::debug!("[Lua] PTY forwarder stopped by Lua");
                        break;
                    }
                }

                match pty_rx.recv().await {
                    Ok(PtyEvent::Output(data)) => {
                        // Send raw bytes with prefix
                        let mut raw_message = Vec::with_capacity(prefix.len() + data.len());
                        raw_message.extend(&prefix);
                        raw_message.extend(&data);

                        if output_tx
                            .send(WebRtcPtyOutput {
                                subscription_id: subscription_id.clone(),
                                browser_identity: peer_id.clone(),
                                data: raw_message,
                            })
                            .is_err()
                        {
                            log::trace!("[Lua] PTY output queue closed, stopping forwarder");
                            break;
                        }
                    }
                    Ok(PtyEvent::ProcessExited { exit_code }) => {
                        log::info!(
                            "[Lua] PTY process exited (code={:?}) for agent {} pty {}",
                            exit_code,
                            agent_index,
                            pty_index
                        );
                        // Could send exit notification here
                        break;
                    }
                    Ok(_other_event) => {
                        // Ignore other events
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!(
                            "[Lua] PTY forwarder lagged by {} events for agent {} pty {}",
                            n,
                            agent_index,
                            pty_index
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        log::info!(
                            "[Lua] PTY channel closed for agent {} pty {}",
                            agent_index,
                            pty_index
                        );
                        break;
                    }
                }
            }

            // Mark forwarder as inactive
            *active_flag.lock().unwrap() = false;

            log::info!(
                "[Lua] Stopped PTY forwarder for peer {} agent {} pty {}",
                &peer_id[..peer_id.len().min(8)],
                agent_index,
                pty_index
            );
        });

        self.webrtc_pty_forwarders.insert(forwarder_key, task);
    }

    /// Create a TUI PTY forwarder requested by Lua.
    ///
    /// Spawns a forwarder task that streams PTY output to TUI via `tui_output_tx`.
    /// Unlike the WebRTC forwarder, no encryption or subscription wrapping is needed.
    fn create_lua_tui_pty_forwarder(&mut self, req: crate::lua::primitives::CreateTuiForwarderRequest) {
        use crate::client::TuiOutput;

        let forwarder_key = format!("tui:{}:{}", req.agent_index, req.pty_index);

        // Check if agent and PTY exist
        let Some(agent_handle) = self.handle_cache.get_agent(req.agent_index) else {
            log::warn!("[Lua-TUI] Cannot create forwarder: no agent at index {}", req.agent_index);
            *req.active_flag.lock().unwrap() = false;
            return;
        };

        let Some(pty_handle) = agent_handle.get_pty(req.pty_index) else {
            log::warn!(
                "[Lua-TUI] Cannot create forwarder: no PTY at index {} for agent {}",
                req.pty_index, req.agent_index
            );
            *req.active_flag.lock().unwrap() = false;
            return;
        };

        let Some(ref output_tx) = self.tui_output_tx else {
            log::warn!("[Lua-TUI] Cannot create forwarder: no TUI output channel");
            *req.active_flag.lock().unwrap() = false;
            return;
        };

        // Abort any existing forwarder for this key
        if let Some(old_task) = self.webrtc_pty_forwarders.remove(&forwarder_key) {
            old_task.abort();
            log::debug!("[Lua-TUI] Aborted existing PTY forwarder for {}", forwarder_key);
        }

        // Subscribe to PTY events
        let pty_rx = pty_handle.subscribe();

        // Get scrollback buffer to send initially
        let scrollback = pty_handle.get_scrollback();

        let sink = output_tx.clone();
        let agent_index = req.agent_index;
        let pty_index = req.pty_index;
        let active_flag = req.active_flag;

        let _guard = self.tokio_runtime.enter();
        let task = tokio::spawn(async move {
            use crate::agent::pty::PtyEvent;

            log::info!(
                "[Lua-TUI] Started PTY forwarder for agent {} pty {}",
                agent_index, pty_index
            );

            // Send scrollback buffer first (if any)
            if !scrollback.is_empty() {
                log::debug!(
                    "[Lua-TUI] Sending {} bytes of scrollback for agent {} pty {}",
                    scrollback.len(), agent_index, pty_index
                );
                if sink.send(TuiOutput::Scrollback(scrollback)).is_err() {
                    log::trace!("[Lua-TUI] Output channel closed before scrollback sent");
                    return;
                }
            }

            let mut pty_rx = pty_rx;
            loop {
                // Check if forwarder was stopped by Lua
                {
                    let active = active_flag.lock().unwrap();
                    if !*active {
                        log::debug!("[Lua-TUI] PTY forwarder stopped by Lua");
                        break;
                    }
                }

                match pty_rx.recv().await {
                    Ok(PtyEvent::Output(data)) => {
                        if sink.send(TuiOutput::Output(data)).is_err() {
                            log::trace!("[Lua-TUI] Output channel closed, stopping forwarder");
                            break;
                        }
                    }
                    Ok(PtyEvent::ProcessExited { exit_code }) => {
                        log::info!(
                            "[Lua-TUI] PTY process exited (code={:?}) for agent {} pty {}",
                            exit_code, agent_index, pty_index
                        );
                        let _ = sink.send(TuiOutput::ProcessExited { exit_code });
                        break;
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!(
                            "[Lua-TUI] PTY forwarder lagged by {} events for agent {} pty {}",
                            n, agent_index, pty_index
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        log::info!(
                            "[Lua-TUI] PTY channel closed for agent {} pty {}",
                            agent_index, pty_index
                        );
                        break;
                    }
                }
            }

            // Mark forwarder as inactive
            *active_flag.lock().unwrap() = false;

            log::info!(
                "[Lua-TUI] Stopped PTY forwarder for agent {} pty {}",
                agent_index, pty_index
            );
        });

        self.webrtc_pty_forwarders.insert(forwarder_key, task);
    }

    /// Stop a PTY forwarder by ID.
    fn stop_lua_pty_forwarder(&mut self, forwarder_id: &str) {
        if let Some(task) = self.webrtc_pty_forwarders.remove(forwarder_id) {
            task.abort();
            log::debug!("[Lua] Stopped PTY forwarder {}", forwarder_id);
        }
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

    // === TUI via Lua (Hub-side Processing) ===

    /// Poll TUI requests from TuiRunner (non-blocking).
    ///
    /// Drains all pending `TuiRequest` messages and handles each one directly.
    /// Hub processes TUI requests synchronously instead of delegating to a
    /// `TuiClient` async task, allowing Lua to participate in the pipeline.
    fn poll_tui_requests(&mut self) {
        let Some(ref mut rx) = self.tui_request_rx else {
            return;
        };

        let mut requests = Vec::new();
        while let Ok(req) = rx.try_recv() {
            requests.push(req);
        }

        for request in requests {
            self.handle_tui_request(request);
        }
    }

    /// Poll Hub events and forward to TuiRunner (non-blocking).
    ///
    /// Receives Hub events via broadcast channel and sends them to TuiRunner
    /// as `TuiOutput::HubEvent`. Filters browser-specific and Shutdown events
    /// (matching `TuiClient::handle_hub_event` behavior).
    ///
    /// Events are drained into a Vec first to release the mutable borrow on
    /// `tui_hub_event_rx` before calling `handle_tui_disconnect_from_pty`.
    fn poll_tui_hub_events(&mut self) {
        use crate::client::TuiOutput;

        // Drain events into a Vec to release the mutable borrow
        let events: Vec<HubEvent> = {
            let Some(ref mut rx) = self.tui_hub_event_rx else {
                return;
            };

            let mut events = Vec::new();
            loop {
                match rx.try_recv() {
                    Ok(event) => events.push(event),
                    Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => {
                        log::warn!("[TUI] Hub event receiver lagged by {} events", n);
                    }
                    Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
                }
            }
            events
        };

        if events.is_empty() {
            return;
        }

        // Clone the sender to avoid borrow conflict with &mut self methods
        let tx = match self.tui_output_tx.clone() {
            Some(tx) => tx,
            None => return,
        };

        for event in events {
            match &event {
                HubEvent::AgentDeleted { agent_id } => {
                    // Disconnect from deleted agent's PTYs before forwarding
                    let agent_index = self
                        .handle_cache
                        .get_all_agents()
                        .iter()
                        .position(|h| h.agent_id() == agent_id);

                    if let Some(idx) = agent_index {
                        self.handle_tui_disconnect_from_pty(idx, 0);
                        self.handle_tui_disconnect_from_pty(idx, 1);
                    }
                    let _ = tx.send(TuiOutput::HubEvent(event));
                }
                HubEvent::Shutdown => {
                    // Don't forward - TuiRunner detects shutdown via its own mechanisms
                }
                HubEvent::PtyConnectionRequested { .. }
                | HubEvent::PtyDisconnectionRequested { .. }
                | HubEvent::HttpConnectionRequested { .. } => {
                    // Browser-specific events, TUI ignores
                }
                _ => {
                    let _ = tx.send(TuiOutput::HubEvent(event));
                }
            }
        }
    }

    /// Process TUI send requests queued by Lua callbacks.
    ///
    /// Drains JSON and binary messages queued by `tui.send()` in Lua.
    /// JSON messages are logged and discarded (TuiRunner speaks `TuiOutput`,
    /// not JSON subscription protocol). Binary messages are forwarded as
    /// `TuiOutput::Output` (raw terminal data).
    fn process_lua_tui_sends(&mut self) {
        use crate::client::TuiOutput;
        use crate::lua::primitives::TuiSendRequest;

        let Some(ref tx) = self.tui_output_tx else {
            // No TUI connected, drain and discard
            let _ = self.lua.drain_tui_sends();
            return;
        };

        for send_req in self.lua.drain_tui_sends() {
            match send_req {
                TuiSendRequest::Json { data } => {
                    // TuiRunner doesn't speak JSON subscription protocol.
                    // These are subscription confirmations and Lua hub broadcasts.
                    log::trace!("[TUI-LUA] Discarding JSON send: {}", data);
                }
                TuiSendRequest::Binary { data } => {
                    // Binary data = raw terminal output, forward to TuiRunner
                    let _ = tx.send(TuiOutput::Output(data));
                }
            }
        }
    }

    /// Handle a single TUI request from TuiRunner.
    ///
    /// Processes the request synchronously on the Hub main thread. PTY operations
    /// use `HandleCache` for direct access. Agent lifecycle operations dispatch
    /// `HubAction`s through the standard action pipeline.
    fn handle_tui_request(&mut self, request: crate::client::TuiRequest) {
        use crate::client::TuiRequest;

        match request {
            // === PTY I/O (direct HandleCache access) ===
            TuiRequest::SendInput { agent_index, pty_index, data } => {
                if let Some(agent) = self.handle_cache.get_agent(agent_index) {
                    if let Some(pty) = agent.get_pty(pty_index) {
                        if let Err(e) = pty.write_input_direct(&data) {
                            log::error!("[TUI] Failed to send input: {}", e);
                        }
                    }
                }
            }
            TuiRequest::SetDims { agent_index, pty_index, cols, rows } => {
                self.tui_dims = (cols, rows);
                if let Some(agent) = self.handle_cache.get_agent(agent_index) {
                    if let Some(pty) = agent.get_pty(pty_index) {
                        pty.resize_direct(ClientId::Tui, rows, cols);
                    }
                }
            }

            // === Agent Selection / PTY Connection ===
            TuiRequest::SelectAgent { index, response_tx } => {
                let result = self.handle_tui_select_agent(index);
                let _ = response_tx.send(result);
            }
            TuiRequest::ConnectToPty { agent_index, pty_index } => {
                self.handle_tui_connect_to_pty(agent_index, pty_index);
            }
            TuiRequest::DisconnectFromPty { agent_index, pty_index } => {
                self.handle_tui_disconnect_from_pty(agent_index, pty_index);
            }

            // === Hub Lifecycle ===
            TuiRequest::Quit => {
                self.quit = true;
                self.broadcast(HubEvent::shutdown());
            }
            TuiRequest::ListWorktrees { response_tx } => {
                if let Err(e) = self.load_available_worktrees() {
                    log::error!("[TUI] Failed to load worktrees: {}", e);
                }
                let worktrees = self.state.read().unwrap().available_worktrees.clone();
                let _ = response_tx.send(worktrees);
            }
            TuiRequest::GetConnectionCodeWithQr { response_tx } => {
                let result = self.generate_connection_url().and_then(|url| {
                    // Cache the URL so connection.get_url() works from Lua
                    self.handle_cache.set_connection_url(Ok(url.clone()));
                    crate::tui::generate_qr_png(&url, 4)
                        .map(|qr_png| crate::tui::ConnectionCodeData { url, qr_png })
                });
                let _ = response_tx.send(result);
            }
            TuiRequest::CreateAgent { request } => {
                actions::dispatch(
                    self,
                    HubAction::CreateAgentForClient {
                        client_id: ClientId::Tui,
                        request,
                    },
                );
            }
            TuiRequest::DeleteAgent { request } => {
                actions::dispatch(
                    self,
                    HubAction::DeleteAgentForClient {
                        client_id: ClientId::Tui,
                        request,
                    },
                );
            }
            TuiRequest::RegenerateConnectionCode => {
                actions::dispatch(self, HubAction::RegenerateConnectionCode);
            }
            TuiRequest::CopyConnectionUrl => {
                actions::dispatch(self, HubAction::CopyConnectionUrl);
            }
        }
    }

    /// Select an agent for TUI and connect to its CLI PTY.
    ///
    /// Dispatches `SelectAgentForClient` action and connects to the CLI PTY
    /// (index 0). Returns metadata for TuiRunner, or `None` if the agent
    /// doesn't exist at the given index.
    fn handle_tui_select_agent(
        &mut self,
        index: usize,
    ) -> Option<crate::client::TuiAgentMetadata> {
        let agent = self.handle_cache.get_agent(index)?;
        let agent_id = agent.agent_id().to_string();
        let has_server_pty = agent.get_pty(1).is_some();

        // Notify Hub of selection
        actions::dispatch(
            self,
            HubAction::SelectAgentForClient {
                client_id: ClientId::Tui,
                agent_key: agent_id.clone(),
            },
        );

        // Connect to CLI PTY (index 0)
        self.handle_tui_connect_to_pty(index, 0);

        Some(crate::client::TuiAgentMetadata {
            agent_id,
            agent_index: index,
            has_server_pty,
        })
    }

    /// Connect TUI to a specific PTY and start output forwarding.
    ///
    /// Aborts any existing forwarder task, connects to the PTY (getting scrollback),
    /// subscribes to PTY events, and spawns a new forwarder task that routes
    /// `PtyEvent::Output` to `TuiOutput::Output`.
    fn handle_tui_connect_to_pty(&mut self, agent_index: usize, pty_index: usize) {
        use crate::client::TuiOutput;

        // Abort existing forwarder
        if let Some(task) = self.tui_output_task.take() {
            task.abort();
        }

        let Some(agent) = self.handle_cache.get_agent(agent_index) else {
            log::warn!("[TUI] No agent at index {}", agent_index);
            return;
        };

        let Some(pty) = agent.get_pty(pty_index) else {
            log::warn!("[TUI] No PTY at index {} for agent {}", pty_index, agent_index);
            return;
        };

        // Connect and get scrollback
        let scrollback = match pty.connect_direct(ClientId::Tui, self.tui_dims) {
            Ok(sb) => sb,
            Err(e) => {
                log::error!("[TUI] Failed to connect to PTY: {}", e);
                return;
            }
        };

        let Some(ref output_tx) = self.tui_output_tx else {
            log::warn!("[TUI] No output channel for PTY connection");
            return;
        };

        // Send scrollback
        if !scrollback.is_empty() {
            let _ = output_tx.send(TuiOutput::Scrollback(scrollback));
        }

        // Subscribe and spawn forwarder
        let pty_rx = pty.subscribe();
        let sink = output_tx.clone();

        let _guard = self.tokio_runtime.enter();
        self.tui_output_task = Some(tokio::spawn(async move {
            use crate::agent::pty::PtyEvent;

            let mut pty_rx = pty_rx;
            loop {
                match pty_rx.recv().await {
                    Ok(PtyEvent::Output(data)) => {
                        if sink.send(TuiOutput::Output(data)).is_err() {
                            break;
                        }
                    }
                    Ok(PtyEvent::ProcessExited { exit_code }) => {
                        let _ = sink.send(TuiOutput::ProcessExited { exit_code });
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!("[TUI] Output forwarder lagged by {} events", n);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
        }));

        log::info!("[TUI] Connected to PTY ({}, {})", agent_index, pty_index);
    }

    /// Disconnect TUI from a specific PTY.
    ///
    /// Aborts the output forwarder task and notifies the PTY of disconnection.
    fn handle_tui_disconnect_from_pty(&mut self, agent_index: usize, pty_index: usize) {
        // Abort forwarder
        if let Some(task) = self.tui_output_task.take() {
            task.abort();
        }

        // Notify PTY
        if let Some(agent) = self.handle_cache.get_agent(agent_index) {
            if let Some(pty) = agent.get_pty(pty_index) {
                pty.disconnect_direct(ClientId::Tui);
            }
        }

        log::info!("[TUI] Disconnected from PTY ({}, {})", agent_index, pty_index);
    }

    // === WebRTC Signaling (ActionCable + Signal Protocol) ===

    /// Poll command channel for incoming encrypted signal envelopes (non-blocking).
    ///
    /// Signals arrive via ActionCable (`HubSignalingChannel` -> `HubCommandChannel`
    /// relay). Each envelope is encrypted with Signal Protocol -- Rails never sees
    /// the plaintext. After decryption, routes by `type` field:
    /// - `"offer"` -> create peer connection + encrypted answer
    /// - `"ice"` -> add ICE candidate to existing peer connection
    fn poll_signal_channel(&mut self) {
        use crate::relay::signal::SignalEnvelope;

        let signals: Vec<command_channel::SignalMessage> = {
            let Some(ref mut channel) = self.command_channel else {
                return;
            };
            let mut sigs = Vec::new();
            while let Some(sig) = channel.try_recv_signal() {
                sigs.push(sig);
            }
            sigs
        };

        if signals.is_empty() {
            return;
        }

        let crypto = match self.browser.crypto_service.clone() {
            Some(cs) => cs,
            None => {
                log::warn!("[Signal] No crypto service -- cannot decrypt signal envelopes");
                return;
            }
        };

        let _guard = self.tokio_runtime.enter();

        for signal in signals {
            // Deserialize envelope to SignalEnvelope for decryption
            let envelope: SignalEnvelope = match serde_json::from_value(signal.envelope.clone()) {
                Ok(e) => e,
                Err(e) => {
                    log::warn!("[Signal] Failed to parse signal envelope: {e}");
                    continue;
                }
            };

            // Decrypt the envelope
            let plaintext = match self.tokio_runtime.block_on(crypto.decrypt(&envelope)) {
                Ok(pt) => pt,
                Err(e) => {
                    log::error!(
                        "[Signal] Failed to decrypt signal from {}: {e}",
                        &signal.browser_identity[..signal.browser_identity.len().min(8)]
                    );
                    continue;
                }
            };

            // Parse decrypted plaintext as JSON
            let payload: serde_json::Value = match serde_json::from_slice(&plaintext) {
                Ok(v) => v,
                Err(e) => {
                    log::warn!("[Signal] Failed to parse decrypted signal payload: {e}");
                    continue;
                }
            };

            let signal_type = payload.get("type").and_then(|t| t.as_str());

            match signal_type {
                Some("offer") => {
                    let sdp = payload.get("sdp").and_then(|v| v.as_str()).unwrap_or("");
                    if sdp.is_empty() {
                        log::warn!("[Signal] Offer missing SDP");
                        continue;
                    }
                    self.handle_webrtc_offer(sdp, &signal.browser_identity);
                }
                Some("ice") => {
                    if let Some(candidate_obj) = payload.get("candidate") {
                        let candidate = candidate_obj
                            .get("candidate")
                            .and_then(|c| c.as_str())
                            .unwrap_or("");
                        let sdp_mid = candidate_obj.get("sdpMid").and_then(|m| m.as_str());
                        let sdp_mline_index = candidate_obj
                            .get("sdpMLineIndex")
                            .and_then(|i| i.as_u64())
                            .map(|i| i as u16);

                        if let Some(channel) =
                            self.webrtc_channels.get(&signal.browser_identity)
                        {
                            if let Err(e) = self.tokio_runtime.block_on(
                                channel.handle_ice_candidate(candidate, sdp_mid, sdp_mline_index),
                            ) {
                                log::warn!("[Signal] Failed to add ICE candidate: {e}");
                            }
                        } else {
                            log::warn!(
                                "[Signal] ICE candidate for unknown browser {}",
                                &signal.browser_identity
                                    [..signal.browser_identity.len().min(8)]
                            );
                        }
                    }
                }
                other => {
                    log::trace!("[Signal] Unknown signal type: {:?}", other);
                }
            }
        }
    }

    /// Drain outgoing signals (encrypted ICE candidates) and relay via ActionCable.
    ///
    /// WebRTC `on_ice_candidate` callbacks encrypt candidates and push them to
    /// `webrtc_outgoing_signal_rx`. This method drains them and sends each via
    /// `CommandChannelHandle::perform("signal", ...)` for ActionCable relay to browser.
    fn poll_outgoing_signals(&mut self) {
        use crate::channel::webrtc::OutgoingSignal;

        let Some(ref command_channel) = self.command_channel else {
            // Drain and discard if no command channel
            while self.webrtc_outgoing_signal_rx.try_recv().is_ok() {}
            return;
        };

        while let Ok(signal) = self.webrtc_outgoing_signal_rx.try_recv() {
            match signal {
                OutgoingSignal::Ice {
                    browser_identity,
                    envelope,
                } => {
                    command_channel.perform(
                        "signal",
                        serde_json::json!({
                            "browser_identity": browser_identity,
                            "envelope": envelope,
                        }),
                    );
                    log::debug!(
                        "[Signal] Relayed ICE candidate to browser {}",
                        &browser_identity[..browser_identity.len().min(8)]
                    );
                }
            }
        }
    }

    /// Handle incoming WebRTC offer from browser (decrypted).
    ///
    /// Creates or reuses a WebRTC peer connection for the browser, processes
    /// the SDP offer, encrypts the answer, and sends it back via ActionCable.
    fn handle_webrtc_offer(&mut self, sdp: &str, browser_identity: &str) {
        use crate::channel::{ChannelConfig, WebRtcChannel};

        let hub_id = self.server_hub_id().to_string();
        let server_url = self.config.server_url.clone();
        let api_key = self.config.get_api_key().to_string();

        log::info!(
            "[WebRTC] Received offer from browser {}",
            &browser_identity[..browser_identity.len().min(8)]
        );

        // Get or create WebRTC channel for this browser
        let is_new_connection = !self.webrtc_channels.contains_key(browser_identity);

        if is_new_connection {
            let mut channel = WebRtcChannel::builder()
                .server_url(&server_url)
                .api_key(&api_key)
                .crypto_service(
                    self.browser
                        .crypto_service
                        .clone()
                        .expect("crypto service required"),
                )
                .signal_tx(self.webrtc_outgoing_signal_tx.clone())
                .build();

            // Configure the channel with hub_id
            let config = ChannelConfig {
                channel_name: "WebRtcChannel".to_string(),
                hub_id: hub_id.clone(),
                agent_index: None,
                pty_index: None,
                browser_identity: Some(browser_identity.to_string()),
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

            self.webrtc_channels
                .insert(browser_identity.to_string(), channel);

            // Notify Lua of peer connection
            if let Err(e) = self.lua.call_peer_connected(browser_identity) {
                log::warn!("[WebRTC] Lua peer_connected callback error: {e}");
            }
            self.process_lua_webrtc_sends();
            self.process_lua_pty_requests();
            self.process_lua_hub_requests();
        }

        // Handle the offer and get the answer
        let channel = self.webrtc_channels.get(browser_identity).unwrap();
        let _guard = self.tokio_runtime.enter();

        match self
            .tokio_runtime
            .block_on(channel.handle_sdp_offer(sdp, browser_identity))
        {
            Ok(answer_sdp) => {
                log::info!(
                    "[WebRTC] Created answer for browser {}",
                    &browser_identity[..browser_identity.len().min(8)]
                );

                // Encrypt the answer with Signal Protocol
                let crypto = self
                    .browser
                    .crypto_service
                    .clone()
                    .expect("crypto service required");

                let answer_payload = serde_json::json!({
                    "type": "answer",
                    "sdp": answer_sdp,
                });
                let plaintext = serde_json::to_vec(&answer_payload).unwrap_or_default();

                // Extract identity key from browser_identity ("identityKey:tabId")
                let identity_key = browser_identity
                    .split(':')
                    .next()
                    .unwrap_or(browser_identity);

                match self
                    .tokio_runtime
                    .block_on(crypto.encrypt(&plaintext, identity_key))
                {
                    Ok(envelope) => {
                        let envelope_value = match serde_json::to_value(&envelope) {
                            Ok(v) => v,
                            Err(e) => {
                                log::error!(
                                    "[WebRTC] Failed to serialize answer envelope: {e}"
                                );
                                return;
                            }
                        };

                        // Send via ActionCable (CommandChannel perform)
                        if let Some(ref cmd_channel) = self.command_channel {
                            cmd_channel.perform(
                                "signal",
                                serde_json::json!({
                                    "browser_identity": browser_identity,
                                    "envelope": envelope_value,
                                }),
                            );
                            log::info!("[WebRTC] Encrypted answer sent via ActionCable");
                        } else {
                            log::error!("[WebRTC] No command channel for answer relay");
                        }
                    }
                    Err(e) => {
                        log::error!("[WebRTC] Failed to encrypt answer: {e}");
                    }
                }
            }
            Err(e) => {
                log::error!("[WebRTC] Failed to handle offer: {e}");
            }
        }
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
