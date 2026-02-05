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

// Rust guideline compliant 2026-02

use std::time::{Duration, Instant};

use crate::agent::AgentNotification;
use crate::channel::Channel;
use crate::hub::actions::{self, HubAction};
use crate::hub::{command_channel, registration, workers, Hub};
use crate::server::messages::ParsedMessage;

impl Hub {
    /// Perform periodic tasks (command channel polling, heartbeat, notifications).
    ///
    /// Call this from your event loop to handle time-based operations.
    /// This method is **non-blocking** - all network I/O is handled via
    /// the WebSocket command channel and background notification worker.
    pub fn tick(&mut self) {
        self.poll_command_channel();
        self.poll_signal_channel();
        self.poll_outgoing_signals();
        self.poll_webrtc_channels();
        self.poll_webrtc_pty_output();
        self.poll_tui_requests();
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
    pub(crate) fn flush_lua_queues(&mut self) {
        self.process_lua_webrtc_sends();
        self.process_lua_tui_sends();
        self.process_lua_pty_requests();
        self.process_lua_hub_requests();
        self.process_lua_connection_requests();
        self.process_lua_worktree_requests();
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
                "terminal_connected" | "terminal_disconnected" | "browser_wants_preview" => {
                    // Legacy event types -- browsers now connect via WebRTC DataChannel directly.
                    log::debug!("Ignoring legacy command channel event: {}", msg.event_type);
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
    /// - `agent_cleanup` messages are handled directly in Rust via `HubAction::CloseAgent`.
    /// - All other messages (including `issue_comment`, `pull_request`, and unknown types)
    ///   are delegated to Lua via `fire_command_message()`. Lua's `handlers/agents.lua`
    ///   listens for `"command_message"` events and handles agent creation routing.
    fn process_command_channel_message(&mut self, msg: &command_channel::CommandMessage) {
        use crate::server::types::MessageData;

        // Convert CommandMessage to MessageData for compatibility with existing parsing
        let message_data = MessageData {
            id: msg.id,
            event_type: msg.event_type.clone(),
            payload: msg.payload.clone(),
        };

        let parsed = ParsedMessage::from_message_data(&message_data);

        // Detect repo for context
        let repo_name = if let Ok(repo) = std::env::var("BOTSTER_REPO") {
            repo
        } else {
            match crate::git::WorktreeManager::detect_current_repo() {
                Ok((_path, name)) => name,
                Err(_) if crate::env::is_test_mode() => "test/repo".to_string(),
                Err(e) => {
                    log::warn!("Not in a git repository, skipping message processing: {e}");
                    return;
                }
            }
        };

        // Try to notify existing agent first (before Lua or action dispatch)
        if self.try_notify_existing_agent(&parsed, &repo_name) {
            return;
        }

        // Handle cleanup directly in Rust (still needs Rust-side agent removal)
        if parsed.is_cleanup() {
            if let (Some(issue_number), Some(repo)) = (parsed.issue_number, &parsed.repo) {
                let repo_safe = repo.replace('/', "-");
                let session_key = format!("{repo_safe}-{issue_number}");
                self.handle_action(HubAction::CloseAgent {
                    session_key,
                    delete_worktree: false,
                });
            } else {
                log::warn!(
                    "Cleanup message {} missing repo or issue_number, skipping",
                    msg.id
                );
            }
            return;
        }

        // Skip WebRTC offers (handled by signal channel)
        if parsed.is_webrtc_offer() {
            return;
        }

        // Everything else goes to Lua
        let lua_message = serde_json::json!({
            "type": "create_agent",
            "issue_or_branch": parsed.issue_number.map(|n| n.to_string()),
            "prompt": parsed.task_description(),
            "repo": parsed.repo,
            "invocation_url": parsed.invocation_url,
        });

        if let Err(e) = self.lua.fire_command_message(&lua_message) {
            log::error!(
                "Lua command_message error for message {}: {e}",
                msg.id
            );
        }
        self.flush_lua_queues();
    }

    /// Send heartbeat via command channel (non-blocking).
    ///
    /// Builds minimal agent data from Rust-owned state (session keys,
    /// invocation URLs, PTY alive status).
    fn send_command_channel_heartbeat(&mut self) {
        let Some(ref channel) = self.command_channel else {
            return;
        };

        /// Heartbeat interval in seconds. Aligned with Rails
        /// `HubCommandChannel::HEARTBEAT_TIMEOUT` (90s) -- sending at 30s
        /// gives three chances before the server considers the hub offline.
        const HEARTBEAT_INTERVAL_SECS: u64 = 30;

        if self.last_heartbeat.elapsed() < Duration::from_secs(HEARTBEAT_INTERVAL_SECS) {
            return;
        }
        self.last_heartbeat = Instant::now();

        let state = self.state.read()
            .expect("HubState RwLock poisoned in heartbeat");
        let agents: Vec<serde_json::Value> = state
            .agent_keys_ordered
            .iter()
            .filter_map(|key| {
                state.agents.get(key).map(|agent| {
                    serde_json::json!({
                        "session_key": key,
                        "last_invocation_url": agent.last_invocation_url
                    })
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

        let state = self.state.read()
            .expect("HubState RwLock poisoned in poll_agent_notifications_async");

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

    /// Try to send a notification to an existing agent for this issue.
    ///
    /// Returns true if an agent was found and notified, false otherwise.
    /// Does NOT apply to cleanup messages -- those go through action dispatch.
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

        let mut state = self.state.write()
            .expect("HubState RwLock poisoned in try_notify_existing_agent");
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
    fn process_lua_webrtc_sends(&mut self) {
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
    fn process_lua_pty_requests(&mut self) {
        use crate::lua::PtyRequest;

        for request in self.lua.drain_pty_requests() {
            match request {
                PtyRequest::CreateForwarder(req) => {
                    self.create_lua_pty_forwarder(req);
                }
                PtyRequest::CreateTuiForwarder(req) => {
                    self.create_lua_tui_pty_forwarder(req);
                }
                PtyRequest::CreateTuiForwarderDirect(req) => {
                    self.create_lua_tui_pty_forwarder_direct(req);
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
    /// Called after any Lua callback that might queue Hub-level operations.
    fn process_lua_hub_requests(&mut self) {
        use crate::lua::primitives::HubRequest;

        for request in self.lua.drain_hub_requests() {
            match request {
                HubRequest::Quit => {
                    log::info!("[Lua] Processing quit request");
                    self.quit = true;
                }
            }
        }
    }

    /// Process connection requests queued by Lua callbacks.
    ///
    /// Drains the Lua connection request queue and processes each request.
    /// Called after any Lua callback that might queue connection operations.
    fn process_lua_connection_requests(&mut self) {
        use crate::lua::primitives::ConnectionRequest;

        for request in self.lua.drain_connection_requests() {
            match request {
                ConnectionRequest::Generate => {
                    log::debug!("[Lua] Processing connection.generate() request");
                    match self.generate_connection_url() {
                        Ok(ref url) => {
                            if let Err(e) = self.lua.fire_connection_code_ready(url) {
                                log::error!("Failed to fire connection_code_ready: {e}");
                            }
                        }
                        Err(ref e) => {
                            log::warn!("Connection URL generation failed: {e}");
                            if let Err(fire_err) = self.lua.fire_connection_code_error(e) {
                                log::error!("Failed to fire connection_code_error: {fire_err}");
                            }
                        }
                    }
                }
                ConnectionRequest::Regenerate => {
                    log::info!("[Lua] Processing connection.regenerate() request");
                    actions::dispatch(self, HubAction::RegenerateConnectionCode);
                }
                ConnectionRequest::CopyToClipboard => {
                    log::debug!("[Lua] Processing connection.copy_to_clipboard() request");
                    actions::dispatch(self, HubAction::CopyConnectionUrl);
                }
            }
        }
    }

    /// Process worktree requests queued by Lua callbacks.
    ///
    /// Drains the Lua worktree request queue and processes each request.
    /// Called after any Lua callback that might queue worktree operations.
    fn process_lua_worktree_requests(&mut self) {
        use crate::git::WorktreeManager;
        use crate::lua::primitives::WorktreeRequest;

        for request in self.lua.drain_worktree_requests() {
            match request {
                WorktreeRequest::Delete { path, branch } => {
                    log::info!("[Lua] Processing worktree.delete({}, {})", path, branch);
                    let manager = WorktreeManager::new(self.config.worktree_base.clone());
                    if let Err(e) = manager.delete_worktree_by_path(
                        std::path::Path::new(&path),
                        &branch,
                    ) {
                        log::error!("[Lua] Failed to delete worktree: {e}");
                    } else {
                        // Refresh worktrees after deletion
                        if let Err(e) = self.load_available_worktrees() {
                            log::warn!("Failed to refresh worktrees after deletion: {e}");
                        }
                    }
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
            *req.active_flag.lock().expect("Forwarder active_flag mutex poisoned") = false;
            return;
        };

        let Some(pty_handle) = agent_handle.get_pty(req.pty_index) else {
            log::warn!(
                "[Lua] Cannot create forwarder: no PTY at index {} for agent {}",
                req.pty_index,
                req.agent_index
            );
            *req.active_flag.lock().expect("Forwarder active_flag mutex poisoned") = false;
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
                    let active = active_flag.lock().expect("Forwarder active_flag mutex poisoned");
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
            *active_flag.lock().expect("Forwarder active_flag mutex poisoned") = false;

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
            *req.active_flag.lock().expect("Forwarder active_flag mutex poisoned") = false;
            return;
        };

        let Some(pty_handle) = agent_handle.get_pty(req.pty_index) else {
            log::warn!(
                "[Lua-TUI] Cannot create forwarder: no PTY at index {} for agent {}",
                req.pty_index, req.agent_index
            );
            *req.active_flag.lock().expect("Forwarder active_flag mutex poisoned") = false;
            return;
        };

        let Some(ref output_tx) = self.tui_output_tx else {
            log::warn!("[Lua-TUI] Cannot create forwarder: no TUI output channel");
            *req.active_flag.lock().expect("Forwarder active_flag mutex poisoned") = false;
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
                    let active = active_flag.lock().expect("Forwarder active_flag mutex poisoned");
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
            *active_flag.lock().expect("Forwarder active_flag mutex poisoned") = false;

            log::info!(
                "[Lua-TUI] Stopped PTY forwarder for agent {} pty {}",
                agent_index, pty_index
            );
        });

        self.webrtc_pty_forwarders.insert(forwarder_key, task);
    }

    /// Create a TUI PTY forwarder with direct session access.
    ///
    /// This variant receives the PTY event sender directly from Lua's PtySessionHandle,
    /// avoiding the need to look up agents in HandleCache.
    fn create_lua_tui_pty_forwarder_direct(
        &mut self,
        req: crate::lua::primitives::CreateTuiForwarderDirectRequest,
    ) {
        use crate::client::TuiOutput;

        let forwarder_key = format!("tui:{}:{}", req.agent_key, req.session_name);

        let Some(ref output_tx) = self.tui_output_tx else {
            log::warn!("[Lua-TUI-Direct] Cannot create forwarder: no TUI output channel");
            *req.active_flag.lock().expect("Forwarder active_flag mutex poisoned") = false;
            return;
        };

        // Abort any existing forwarder for this key
        if let Some(old_task) = self.webrtc_pty_forwarders.remove(&forwarder_key) {
            old_task.abort();
            log::debug!("[Lua-TUI-Direct] Aborted existing PTY forwarder for {}", forwarder_key);
        }

        // Subscribe to PTY events directly from the session handle's event_tx
        let pty_rx = req.event_tx.subscribe();

        // Get scrollback buffer
        let scrollback: Vec<u8> = {
            let buffer = req.scrollback_buffer.lock().expect("Scrollback buffer mutex poisoned");
            buffer.iter().copied().collect()
        };

        let sink = output_tx.clone();
        let agent_key = req.agent_key.clone();
        let session_name = req.session_name.clone();
        let active_flag = req.active_flag;

        let _guard = self.tokio_runtime.enter();
        let task = tokio::spawn(async move {
            use crate::agent::pty::PtyEvent;

            log::info!(
                "[Lua-TUI-Direct] Started PTY forwarder for {}:{}",
                agent_key, session_name
            );

            // Send scrollback buffer first (if any)
            if !scrollback.is_empty() {
                log::debug!(
                    "[Lua-TUI-Direct] Sending {} bytes of scrollback for {}:{}",
                    scrollback.len(), agent_key, session_name
                );
                if sink.send(TuiOutput::Scrollback(scrollback)).is_err() {
                    log::trace!("[Lua-TUI-Direct] Output channel closed before scrollback sent");
                    return;
                }
            }

            let mut pty_rx = pty_rx;
            loop {
                // Check if forwarder was stopped by Lua
                {
                    let active = active_flag.lock().expect("Forwarder active_flag mutex poisoned");
                    if !*active {
                        log::debug!("[Lua-TUI-Direct] PTY forwarder stopped by Lua");
                        break;
                    }
                }

                match pty_rx.recv().await {
                    Ok(PtyEvent::Output(data)) => {
                        if sink.send(TuiOutput::Output(data)).is_err() {
                            log::trace!("[Lua-TUI-Direct] Output channel closed, stopping forwarder");
                            break;
                        }
                    }
                    Ok(PtyEvent::ProcessExited { exit_code }) => {
                        log::info!(
                            "[Lua-TUI-Direct] PTY process exited (code={:?}) for {}:{}",
                            exit_code, agent_key, session_name
                        );
                        let _ = sink.send(TuiOutput::ProcessExited { exit_code });
                        break;
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!(
                            "[Lua-TUI-Direct] PTY forwarder lagged by {} events for {}:{}",
                            n, agent_key, session_name
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        log::info!(
                            "[Lua-TUI-Direct] PTY channel closed for {}:{}",
                            agent_key, session_name
                        );
                        break;
                    }
                }
            }

            // Mark forwarder as inactive
            *active_flag.lock().expect("Forwarder active_flag mutex poisoned") = false;

            log::info!(
                "[Lua-TUI-Direct] Stopped PTY forwarder for {}:{}",
                agent_key, session_name
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
    /// All TUI messages are JSON routed through Lua `client.lua` — the same
    /// path as browser clients. Each message goes to `lua.call_tui_message()`
    /// which routes through `Client:on_message()` in Lua.
    fn poll_tui_requests(&mut self) {
        let Some(ref mut rx) = self.tui_request_rx else {
            return;
        };

        // Drain into Vec to release the mutable borrow on self before
        // calling lua.call_tui_message() and flush_lua_queues().
        let messages: Vec<serde_json::Value> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        for msg in messages {
            if let Err(e) = self.lua.call_tui_message(msg) {
                log::error!("[TUI] Lua message handling error: {}", e);
            }
            self.flush_lua_queues();
        }
    }

    /// Process TUI send requests queued by Lua callbacks.
    ///
    /// Drains JSON and binary messages queued by `tui.send()` in Lua.
    /// JSON messages carry agent lifecycle events (`agent_created`,
    /// `agent_deleted`, `worktree_list`, etc.) and are forwarded as
    /// `TuiOutput::Message`. Binary messages are forwarded as
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
                    let _ = tx.send(TuiOutput::Message(data));
                }
                TuiSendRequest::Binary { data } => {
                    // Binary data = raw terminal output, forward to TuiRunner
                    let _ = tx.send(TuiOutput::Output(data));
                }
            }
        }
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
        let channel = self.webrtc_channels.get(browser_identity)
            .expect("WebRTC channel must exist after offer handling");
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
    pub(crate) fn register_device(&mut self) {
        registration::register_device(&mut self.device, &self.client, &self.config);
    }

    /// Register the hub with the server and store the server-assigned ID.
    ///
    /// The server-assigned `botster_id` is used for all URLs and WebSocket subscriptions
    /// to guarantee uniqueness (no collision between different CLI instances).
    /// The local `hub_identifier` is kept for config directories.
    pub(crate) fn register_hub_with_server(&mut self) {
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
    pub(crate) fn init_signal_protocol(&mut self) {
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
    pub(crate) fn get_or_generate_connection_url(&mut self) -> Result<String, String> {
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
