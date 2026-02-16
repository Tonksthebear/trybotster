//! Server communication for Hub.
//!
//! This module handles all communication with the Rails server, including:
//!
//! - WebRTC peer connections and signaling (E2E encrypted with vodozemac Olm)
//! - Agent notification delivery via background worker
//! - Device and hub registration
//! - Lua plugin event processing (ActionCable, WebSocket, timers, HTTP, etc.)
//!
//! # Architecture
//!
//! ActionCable channels and heartbeat are now managed by Lua plugins.
//! The Rust side handles WebRTC peer connections, agent notifications,
//! and Lua event processing in the tick loop.

// Rust guideline compliant 2026-02

use std::time::{Duration, Instant};

use base64::Engine;
use crate::channel::Channel;
use crate::hub::actions::{self, HubAction};
use crate::hub::{registration, Hub, WebRtcPtyOutput};

impl Hub {
    /// Perform periodic tasks (Lua event processing, WebRTC, notifications).
    ///
    /// Call this from your event loop to handle time-based operations.
    /// This method is **non-blocking** - all network I/O is handled via
    /// Lua plugins (ActionCable, WebSocket) and background workers.
    pub fn tick(&mut self) {
        self.poll_lua_websocket_events();
        self.process_lua_action_cable_requests();
        self.poll_lua_action_cable_channels();

        self.poll_outgoing_webrtc_signals();
        self.poll_webrtc_dc_opens();
        self.poll_webrtc_channels();
        self.cleanup_disconnected_webrtc_channels();
        self.poll_webrtc_pty_output();
        self.poll_pty_input();
        self.poll_stream_frames_incoming();
        self.poll_stream_frames_outgoing();
        self.poll_pty_observers();
        self.poll_tui_requests();
        self.poll_pty_notifications();
        self.poll_lua_file_changes();
        self.poll_user_file_watches();
        self.poll_lua_timers();
        self.poll_lua_http_responses();
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
        self.process_lua_action_cable_requests();
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

    /// Poll user file watches created by `watch.directory()` in Lua.
    ///
    /// Fires registered Lua callbacks for any file events detected.
    fn poll_user_file_watches(&self) {
        let fired = self.lua.poll_user_file_watches();
        if fired > 0 {
            log::debug!("Fired {} user file watch event(s)", fired);
        }
    }

    /// Poll Lua timers and fire callbacks for expired timers.
    ///
    /// Checks all registered timers, fires callbacks for expired ones,
    /// reschedules repeating timers, and removes completed entries.
    fn poll_lua_timers(&self) {
        let fired = self.lua.poll_timers();
        if fired > 0 {
            log::debug!("Fired {} Lua timer callback(s)", fired);
        }
    }

    /// Poll for completed async HTTP responses and fire Lua callbacks.
    ///
    /// Background threads spawned by `http.request()` push completed
    /// responses to the registry. This drains them and fires callbacks.
    fn poll_lua_http_responses(&self) {
        let fired = self.lua.poll_http_responses();
        if fired > 0 {
            log::debug!("Fired {} Lua HTTP callback(s)", fired);
        }
    }

    /// Spawn a notification watcher task for a PTY session.
    ///
    /// Subscribes to the PTY's broadcast channel, filters for
    /// `PtyEvent::Notification`, and pushes events to the Hub's
    /// `pty_notification_queue` for processing in the tick loop.
    fn spawn_notification_watcher(
        &mut self,
        watcher_key: String,
        agent_key: String,
        session_name: String,
        event_tx: tokio::sync::broadcast::Sender<crate::agent::pty::PtyEvent>,
    ) {
        // Abort any existing watcher for this key
        if let Some(old) = self.notification_watcher_handles.remove(&watcher_key) {
            old.abort();
            log::debug!("[NotifWatcher] Aborted existing watcher for {}", watcher_key);
        }

        let queue = std::sync::Arc::clone(&self.pty_notification_queue);
        let mut rx = event_tx.subscribe();
        let key = watcher_key.clone();

        let _guard = self.tokio_runtime.enter();
        let task = tokio::spawn(async move {
            use crate::agent::pty::PtyEvent;

            log::info!("[NotifWatcher] Started for {}", key);

            loop {
                match rx.recv().await {
                    Ok(PtyEvent::Notification(notif)) => {
                        log::info!("[NotifWatcher] Got notification for {}: {:?}", key, notif);
                        let mut q = queue.lock().expect("pty_notification_queue lock poisoned");
                        q.push(super::PtyNotificationEvent {
                            agent_key: agent_key.clone(),
                            session_name: session_name.clone(),
                            notification: notif,
                        });
                    }
                    Ok(_) => {
                        // Ignore non-notification events
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!("[NotifWatcher] Lagged by {} events for {}", n, key);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        log::info!("[NotifWatcher] Channel closed for {}", key);
                        break;
                    }
                }
            }
        });

        self.notification_watcher_handles.insert(watcher_key, task);
    }

    // === PTY Notification Watcher ===

    /// Poll queued PTY notifications and fire the `pty_notification` Lua hook.
    ///
    /// Watcher tasks push `PtyNotificationEvent` into `pty_notification_queue`.
    /// This method drains the queue and fires the Lua hook for each event.
    fn poll_pty_notifications(&self) {
        let events: Vec<super::PtyNotificationEvent> = {
            let mut queue = self
                .pty_notification_queue
                .lock()
                .expect("pty_notification_queue lock poisoned");
            std::mem::take(&mut *queue)
        };

        if events.is_empty() {
            return;
        }

        for event in events {
            self.lua.notify_pty_notification(
                &event.agent_key,
                &event.session_name,
                &event.notification,
            );
        }
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

            // Check for repeated decryption failures (session desync) —
            // initiate ratchet restart by sending a fresh bundle (type 2).
            if let Some(channel) = self.webrtc_channels.get(&browser_identity) {
                let failures = channel.decrypt_failure_count();
                if failures >= 3 {
                    log::warn!(
                        "[WebRTC] {} consecutive decryption failures for {}, initiating ratchet restart",
                        failures,
                        &browser_identity[..browser_identity.len().min(8)]
                    );
                    channel.reset_decrypt_failures();
                    self.send_ratchet_restart(&browser_identity);
                }
            }
        }
    }

    /// Check for WebRTC DataChannels that have just opened and fire `peer_connected`.
    ///
    /// This is the correct place to notify Lua — the DC is usable, so PTY forwarders
    /// started by the Lua callback can actually send data.
    fn poll_webrtc_dc_opens(&mut self) {
        let browser_ids: Vec<String> = self.webrtc_channels.keys().cloned().collect();
        for browser_identity in browser_ids {
            if let Some(channel) = self.webrtc_channels.get(&browser_identity) {
                if channel.take_dc_opened() {
                    log::info!(
                        "[WebRTC] DataChannel opened for {}, firing peer_connected",
                        &browser_identity[..browser_identity.len().min(8)]
                    );
                    if let Err(e) = self.lua.call_peer_connected(&browser_identity) {
                        log::warn!("[WebRTC] Lua peer_connected callback error: {e}");
                    }
                    self.flush_lua_queues();
                }
            }
        }
    }

    /// Send a fresh Olm bundle (type 2) to a browser peer via both DataChannel and ActionCable.
    ///
    /// Generates a new OTK, builds a 161-byte `DeviceKeyBundle`, removes the stale Olm session,
    /// and delivers the bundle over both transport paths (belt and suspenders).
    fn send_ratchet_restart(&mut self, browser_identity: &str) {
        let peer_olm_key = crate::relay::extract_olm_key(browser_identity).to_string();
        let Some(ref cs) = self.browser.crypto_service else {
            log::warn!("[RatchetRestart] No crypto service available");
            return;
        };

        let bundle_bytes = match cs.lock() {
            Ok(mut guard) => match guard.refresh_bundle_for_peer(&peer_olm_key) {
                Ok(bytes) => {
                    if let Err(e) = guard.persist() {
                        log::warn!("[RatchetRestart] Failed to persist after refresh: {e}");
                    }
                    bytes
                }
                Err(e) => {
                    log::error!("[RatchetRestart] Failed to generate refresh bundle: {e}");
                    return;
                }
            },
            Err(e) => {
                log::error!("[RatchetRestart] Crypto mutex poisoned: {e}");
                return;
            }
        };

        // Send type 2 via DataChannel (if available)
        if let Some(channel) = self.webrtc_channels.get(browser_identity) {
            let _guard = self.tokio_runtime.enter();
            if let Err(e) = self
                .tokio_runtime
                .block_on(channel.send_bundle_refresh(&bundle_bytes))
            {
                log::warn!("[RatchetRestart] Failed to send bundle refresh via DC: {e}");
            }
        }

        // Also send via ActionCable
        let envelope = serde_json::json!({
            "t": 2,
            "b": base64::engine::general_purpose::STANDARD_NO_PAD
                .encode(&bundle_bytes),
        });
        let data = serde_json::json!({
            "browser_identity": browser_identity,
            "envelope": envelope,
        });
        if let Err(e) = self.lua.fire_json_event("outgoing_signal", &data) {
            log::warn!("[RatchetRestart] Failed to send bundle refresh via AC: {e}");
        }

        log::info!(
            "[RatchetRestart] Sent fresh bundle to {}",
            &browser_identity[..browser_identity.len().min(8)]
        );
    }

    /// Clean up WebRTC channels that have disconnected or timed out.
    ///
    /// When a WebRTC connection fails (ICE failure, network change, etc.),
    /// the channel transitions to Disconnected state but remains in the map.
    /// This leaks file descriptors (UDP sockets from ICE gathering) and
    /// prevents new connections.
    ///
    /// Also cleans up connections stuck in "Connecting" state for too long
    /// (e.g., ICE negotiation that never completes due to network issues).
    ///
    /// This function removes stale channels and properly closes them
    /// to release resources, including aborting any associated PTY forwarders.
    fn cleanup_disconnected_webrtc_channels(&mut self) {
        use crate::channel::ConnectionState;

        // Enter tokio runtime for channel state() calls
        let _guard = self.tokio_runtime.enter();

        // Timeout for connections stuck in "Connecting" state (30 seconds)
        const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
        let now = Instant::now();

        // Collect IDs of channels that need cleanup:
        // 1. Disconnected channels
        // 2. Channels stuck in Connecting state for too long
        let to_cleanup: Vec<(String, &'static str)> = self
            .webrtc_channels
            .iter()
            .filter_map(|(id, ch)| {
                let state = ch.state();
                if state == ConnectionState::Disconnected {
                    Some((id.clone(), "disconnected"))
                } else if state == ConnectionState::Connecting {
                    // Check if connection has timed out
                    if let Some(started) = self.webrtc_connection_started.get(id) {
                        if now.duration_since(*started) > CONNECTION_TIMEOUT {
                            return Some((id.clone(), "timeout"));
                        }
                    }
                    None
                } else {
                    // Connection is Connected - remove from start tracking
                    None
                }
            })
            .collect();

        // Also clean up tracking for connections that reached Connected state
        let connected: Vec<String> = self
            .webrtc_channels
            .iter()
            .filter(|(_, ch)| ch.state() == ConnectionState::Connected)
            .map(|(id, _)| id.clone())
            .collect();
        for id in connected {
            self.webrtc_connection_started.remove(&id);
        }

        // Clean up stale channels
        for (browser_identity, reason) in to_cleanup {
            self.cleanup_webrtc_channel(&browser_identity, reason);
        }
    }

    /// Clean up a single WebRTC channel and its associated resources.
    ///
    /// This is the centralized cleanup point that:
    /// 1. Removes and disconnects the WebRTC channel
    /// 2. Removes connection start time tracking
    /// 3. Aborts any PTY forwarder tasks for this browser
    /// 4. Notifies Lua of peer disconnection
    fn cleanup_webrtc_channel(&mut self, browser_identity: &str, reason: &str) {
        log::info!(
            "[WebRTC] Cleaning up {} channel: {}",
            reason,
            &browser_identity[..browser_identity.len().min(8)]
        );

        // Remove the channel and track its close notification.
        // The state handler (Disconnected/Failed) already takes pc/dc and spawns
        // a close task — disconnect() here is belt-and-suspenders. We store the
        // close_complete Notify so the offer handler can await socket release
        // before creating a replacement channel (prevents fd exhaustion).
        if let Some(mut channel) = self.webrtc_channels.remove(browser_identity) {
            let close_rx = channel.close_receiver();
            let olm_key = crate::relay::extract_olm_key(browser_identity).to_string();
            self.webrtc_pending_closes.insert(olm_key, close_rx);

            self.tokio_runtime.spawn(async move {
                channel.disconnect().await;
                log::debug!("[WebRTC] Channel disconnect completed");
            });
        }

        // Remove connection start time tracking
        self.webrtc_connection_started.remove(browser_identity);

        // Close and remove stream multiplexer for this browser
        if let Some(mut mux) = self.stream_muxes.remove(browser_identity) {
            mux.close_all();
            log::debug!("[WebRTC] Closed stream multiplexer for {}", &browser_identity[..browser_identity.len().min(8)]);
        }

        // Abort any PTY forwarders for this browser.
        // Forwarder keys are "{peer_id}:{agent_index}:{pty_index}" where peer_id = browser_identity
        self.webrtc_pty_forwarders.retain(|key, task| {
            if key.starts_with(browser_identity) {
                task.abort();
                log::debug!("[WebRTC] Aborted PTY forwarder: {}", key);
                false
            } else {
                true
            }
        });

        // Notify Lua of peer disconnection (Lua handles subscription cleanup)
        if let Err(e) = self.lua.call_peer_disconnected(browser_identity) {
            log::warn!("[WebRTC] Lua peer_disconnected callback error: {e}");
        }
    }

    /// Handle a message received from a WebRTC DataChannel.
    ///
    /// All message handling is delegated to Lua. The message is passed to Lua's
    /// on_message callback which routes to the appropriate handler (subscribe,
    /// unsubscribe, terminal data, hub commands, etc.).
    ///
    /// Note: Crypto envelope decryption happens inside WebRtcChannel.try_recv(),
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

        /// Send timeout to prevent SCTP congestion from blocking the tick loop.
        const SEND_TIMEOUT: Duration = Duration::from_secs(2);

        for send_req in self.lua.drain_webrtc_sends() {
            match send_req {
                WebRtcSendRequest::Json { peer_id, data } => {
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
                        match self.tokio_runtime.block_on(
                            tokio::time::timeout(SEND_TIMEOUT, channel.send_to(&payload, &peer))
                        ) {
                            Ok(Err(e)) => log::warn!("[WebRTC] Lua send failed: {e}"),
                            Err(_) => log::warn!("[WebRTC] Lua send timed out for {}", &peer_id[..peer_id.len().min(8)]),
                            Ok(Ok(())) => {}
                        }
                    } else {
                        log::debug!("[WebRTC] Lua send to unknown peer: {}", &peer_id[..peer_id.len().min(8)]);
                    }
                }
                WebRtcSendRequest::Binary { peer_id, data } => {
                    if let Some(channel) = self.webrtc_channels.get(&peer_id) {
                        let peer = crate::channel::PeerId(peer_id.clone());
                        let _guard = self.tokio_runtime.enter();
                        match self.tokio_runtime.block_on(
                            tokio::time::timeout(SEND_TIMEOUT, channel.send_to(&data, &peer))
                        ) {
                            Ok(Err(e)) => log::warn!("[WebRTC] Lua binary send failed: {e}"),
                            Err(_) => log::warn!("[WebRTC] Lua binary send timed out for {}", &peer_id[..peer_id.len().min(8)]),
                            Ok(Ok(())) => {}
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
                            pty_handle.resize_direct(rows, cols);
                        } else {
                            log::debug!("[Lua] No PTY at index {} for agent {}", pty_index, agent_index);
                        }
                    } else {
                        log::debug!("[Lua] No agent at index {}", agent_index);
                    }
                }
                PtyRequest::SpawnNotificationWatcher {
                    watcher_key,
                    agent_key,
                    session_name,
                    event_tx,
                } => {
                    self.spawn_notification_watcher(
                        watcher_key,
                        agent_key,
                        session_name,
                        event_tx,
                    );
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

        // Track peers that already received a ratchet restart this batch
        // to avoid burning multiple OTKs for the same decrypt failure storm.
        let mut restarted_peers = std::collections::HashSet::<String>::new();

        for request in self.lua.drain_hub_requests() {
            match request {
                HubRequest::Quit => {
                    log::info!("[Lua] Processing quit request");
                    self.quit = true;
                }
                HubRequest::HandleWebrtcOffer {
                    browser_identity,
                    sdp,
                } => {
                    log::info!(
                        "[Lua] Processing WebRTC offer from {}",
                        &browser_identity[..browser_identity.len().min(8)]
                    );
                    self.handle_webrtc_offer(&sdp, &browser_identity);
                }
                HubRequest::HandleIceCandidate {
                    browser_identity,
                    candidate,
                } => {
                    let candidate_str = candidate
                        .get("candidate")
                        .and_then(|c| c.as_str())
                        .unwrap_or("");
                    let sdp_mid = candidate.get("sdpMid").and_then(|m| m.as_str());
                    let sdp_mline_index = candidate
                        .get("sdpMLineIndex")
                        .and_then(|i| i.as_u64())
                        .map(|i| i as u16);

                    if let Some(channel) = self.webrtc_channels.get(&browser_identity) {
                        let _guard = self.tokio_runtime.enter();
                        if let Err(e) = self.tokio_runtime.block_on(
                            channel.handle_ice_candidate(candidate_str, sdp_mid, sdp_mline_index),
                        ) {
                            log::warn!("[Lua] Failed to add ICE candidate: {e}");
                        }
                    } else {
                        log::warn!(
                            "[Lua] ICE candidate for unknown browser {}",
                            &browser_identity[..browser_identity.len().min(8)]
                        );
                    }
                }
                HubRequest::RatchetRestart { browser_identity } => {
                    // Deduplicate: only restart once per Olm key per batch.
                    // Multiple queued signals from the same peer all fail decryption,
                    // but we only need one fresh OTK per restart.
                    let olm_key = crate::relay::extract_olm_key(&browser_identity).to_string();
                    if !restarted_peers.contains(&olm_key) {
                        log::warn!(
                            "[Lua] Signaling decrypt failed for {}, initiating ratchet restart",
                            &browser_identity[..browser_identity.len().min(8)]
                        );
                        self.send_ratchet_restart(&browser_identity);
                        restarted_peers.insert(olm_key);
                    }
                }
            }
        }
    }

    /// Poll WebSocket connections for events and fire Lua callbacks.
    fn poll_lua_websocket_events(&mut self) {
        let count = self.lua.poll_websocket_events();
        if count > 0 {
            self.flush_lua_queues();
        }
    }

    /// Process ActionCable requests queued by Lua callbacks.
    ///
    /// Delegates to the primitive's processing function which drains the
    /// shared queue and handles connect/subscribe/perform/unsubscribe/close.
    fn process_lua_action_cable_requests(&mut self) {
        use crate::lua::primitives::action_cable;

        let handle = self.tokio_runtime.handle().clone();
        action_cable::process_lua_action_cable_requests(
            self.lua.action_cable_queue_ref(),
            &mut self.lua_ac_connections,
            &mut self.lua_ac_channels,
            &self.config.server_url,
            self.config.get_api_key(),
            &handle,
        );
    }

    /// Poll Lua ActionCable channels for incoming messages and fire callbacks.
    fn poll_lua_action_cable_channels(&mut self) {
        use crate::lua::primitives::action_cable;

        let crypto = self.browser.crypto_service.as_ref();
        let count = action_cable::poll_lua_action_cable_channels(
            self.lua.lua_ref(),
            &mut self.lua_ac_channels,
            &self.lua_ac_connections,
            crypto,
        );
        if count > 0 {
            self.flush_lua_queues();
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

        // Get snapshot BEFORE subscribing to avoid duplicate data.
        // If we subscribe first, PTY output between subscribe and snapshot
        // gets both captured in the snapshot AND buffered as a live event.
        let snapshot = pty_handle.get_snapshot();
        let pty_rx = pty_handle.subscribe();

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

            log::info!(
                "[Lua] Started PTY forwarder for peer {} agent {} pty {}",
                &peer_id[..peer_id.len().min(8)],
                agent_index,
                pty_index
            );

            // Send terminal snapshot first (if any), chunked to fit within
            // SCTP max message size (DataChannel limit ~256KB, we use 64KB chunks
            // to leave room for encryption overhead and OlmEnvelope framing).
            //
            // Snapshot chunks use prefix 0x02 with an 8-byte header:
            //   [0x02][snapshot_id:4 LE][chunk_idx:2 LE][total_chunks:2 LE][data]
            // The browser buffers chunks and only feeds data to the terminal
            // when all chunks arrive, preventing garbled output from partial
            // delivery if the connection drops mid-snapshot.
            if !snapshot.is_empty() {
                const CHUNK_SIZE: usize = 64 * 1024;
                let num_chunks = (snapshot.len() + CHUNK_SIZE - 1) / CHUNK_SIZE;
                let snapshot_id: u32 = rand::random();
                log::debug!(
                    "[Lua] Sending {} bytes of snapshot in {} chunks (id={:#010x}) for agent {} pty {}",
                    snapshot.len(),
                    num_chunks,
                    snapshot_id,
                    agent_index,
                    pty_index
                );

                for (i, chunk) in snapshot.chunks(CHUNK_SIZE).enumerate() {
                    // 9-byte header: prefix + snapshot_id + chunk_idx + total_chunks
                    let mut raw_message = Vec::with_capacity(9 + chunk.len());
                    raw_message.push(0x02); // snapshot chunk prefix
                    raw_message.extend_from_slice(&snapshot_id.to_le_bytes());
                    raw_message.extend_from_slice(&(i as u16).to_le_bytes());
                    raw_message.extend_from_slice(&(num_chunks as u16).to_le_bytes());
                    raw_message.extend(chunk);

                    if output_tx
                        .send(WebRtcPtyOutput {
                            subscription_id: subscription_id.clone(),
                            browser_identity: peer_id.clone(),
                            data: raw_message,
                            agent_index,
                            pty_index,
                        })
                        .is_err()
                    {
                        log::trace!("[Lua] PTY output queue closed during snapshot send");
                        return;
                    }
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
                                agent_index,
                                pty_index,
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

        // Get snapshot BEFORE subscribing to avoid duplicate data.
        let snapshot = pty_handle.get_snapshot();
        let pty_rx = pty_handle.subscribe();

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

            // Send terminal snapshot first (if any)
            if !snapshot.is_empty() {
                log::debug!(
                    "[Lua-TUI] Sending {} bytes of snapshot for agent {} pty {}",
                    snapshot.len(), agent_index, pty_index
                );
                if sink.send(TuiOutput::Scrollback {
                    agent_index: Some(agent_index),
                    pty_index: Some(pty_index),
                    data: snapshot,
                }).is_err() {
                    log::trace!("[Lua-TUI] Output channel closed before snapshot sent");
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
                        if sink.send(TuiOutput::Output {
                            agent_index: Some(agent_index),
                            pty_index: Some(pty_index),
                            data,
                        }).is_err() {
                            log::trace!("[Lua-TUI] Output channel closed, stopping forwarder");
                            break;
                        }
                    }
                    Ok(PtyEvent::ProcessExited { exit_code }) => {
                        log::info!(
                            "[Lua-TUI] PTY process exited (code={:?}) for agent {} pty {}",
                            exit_code, agent_index, pty_index
                        );
                        let _ = sink.send(TuiOutput::ProcessExited {
                            agent_index: Some(agent_index),
                            pty_index: Some(pty_index),
                            exit_code,
                        });
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

        // Get snapshot BEFORE subscribing to avoid duplicate data.
        let snapshot: Vec<u8> = {
            let mut parser = req.shadow_screen.lock().expect("shadow_screen mutex poisoned");
            crate::agent::pty::snapshot_with_scrollback(parser.screen_mut())
        };
        let pty_rx = req.event_tx.subscribe();

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

            // Send terminal snapshot first (if any)
            if !snapshot.is_empty() {
                log::debug!(
                    "[Lua-TUI-Direct] Sending {} bytes of snapshot for {}:{}",
                    snapshot.len(), agent_key, session_name
                );
                if sink.send(TuiOutput::Scrollback {
                    agent_index: None,
                    pty_index: None,
                    data: snapshot,
                }).is_err() {
                    log::trace!("[Lua-TUI-Direct] Output channel closed before snapshot sent");
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
                        if sink.send(TuiOutput::Output {
                            agent_index: None,
                            pty_index: None,
                            data,
                        }).is_err() {
                            log::trace!("[Lua-TUI-Direct] Output channel closed, stopping forwarder");
                            break;
                        }
                    }
                    Ok(PtyEvent::ProcessExited { exit_code }) => {
                        log::info!(
                            "[Lua-TUI-Direct] PTY process exited (code={:?}) for {}:{}",
                            exit_code, agent_key, session_name
                        );
                        let _ = sink.send(TuiOutput::ProcessExited {
                            agent_index: None,
                            pty_index: None,
                            exit_code,
                        });
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

    // === Stream Multiplexer ===

    /// Poll for incoming stream frames from WebRTC DataChannels.
    ///
    /// Drain binary PTY input from browser (bypasses JSON/Lua).
    ///
    /// Handles `CONTENT_PTY` frames with the input direction flag set.
    /// Writes directly to PTY handles for zero-overhead keystroke delivery.
    fn poll_pty_input(&mut self) {
        while let Ok(input) = self.pty_input_rx.try_recv() {
            if let Some(agent_handle) = self.handle_cache.get_agent(input.agent_index) {
                if let Some(pty_handle) = agent_handle.get_pty(input.pty_index) {
                    if let Err(e) = pty_handle.write_input_direct(&input.data) {
                        log::error!("[PTY-INPUT] Write failed: {e}");
                    }
                }
            }
        }
    }

    /// Drains `stream_frame_rx`, gets or creates a `StreamMultiplexer` per
    /// browser identity, and dispatches each frame.
    fn poll_stream_frames_incoming(&mut self) {
        use crate::relay::stream_mux::StreamMultiplexer;

        let frames: Vec<crate::channel::webrtc::StreamIncoming> =
            std::iter::from_fn(|| self.stream_frame_rx.try_recv().ok()).collect();

        if frames.is_empty() {
            return;
        }

        // handle_frame may call tokio::spawn, so we need a runtime context
        let _guard = self.tokio_runtime.enter();

        for frame in frames {
            let mux = self
                .stream_muxes
                .entry(frame.browser_identity.clone())
                .or_insert_with(StreamMultiplexer::new);

            mux.handle_frame(frame.frame_type, frame.stream_id, frame.payload);
        }
    }

    /// Poll stream multiplexers for outgoing frames and send via WebRTC.
    ///
    /// Iterates all active multiplexers, drains their output queues, and sends
    /// each frame via the corresponding WebRTC channel's `send_stream_raw`.
    fn poll_stream_frames_outgoing(&mut self) {
        let browser_ids: Vec<String> = self.stream_muxes.keys().cloned().collect();

        for browser_identity in browser_ids {
            let frames = {
                let Some(mux) = self.stream_muxes.get_mut(&browser_identity) else {
                    continue;
                };
                mux.drain_output()
            };

            if frames.is_empty() {
                continue;
            }

            let Some(channel) = self.webrtc_channels.get(&browser_identity) else {
                log::warn!(
                    "[StreamMux] No WebRTC channel for browser {} when sending frames",
                    &browser_identity[..browser_identity.len().min(8)]
                );
                continue;
            };

            let peer = crate::channel::PeerId(browser_identity.clone());
            let _guard = self.tokio_runtime.enter();

            for frame in frames {
                if let Err(e) = self.tokio_runtime.block_on(
                    channel.send_stream_raw(frame.frame_type, frame.stream_id, &frame.payload, &peer),
                ) {
                    log::warn!("[StreamMux] Failed to send frame: {e}");
                }
            }
        }
    }

    /// Send raw PTY bytes to a WebRTC subscription via Olm-encrypted m.botster.pty.
    ///
    /// Uses the hot path: compress → base64 → Olm encrypt → binary DataChannel.
    /// The browser decrypts and routes by subscription ID + msgtype.
    /// Returns `false` if the DataChannel is not open (circuit breaker signal).
    fn send_webrtc_raw(
        &self,
        subscription_id: &str,
        browser_identity: &str,
        data: Vec<u8>,
    ) -> bool {
        /// Send timeout to prevent SCTP congestion from blocking the tick loop.
        /// Dead peers cause SCTP retransmit backpressure that can block `dc.send()`
        /// for 60+ seconds. This timeout ensures the tick loop stays responsive.
        const SEND_TIMEOUT: Duration = Duration::from_secs(2);

        let Some(channel) = self.webrtc_channels.get(browser_identity) else {
            return false;
        };

        let peer = crate::channel::PeerId(browser_identity.to_string());
        let _guard = self.tokio_runtime.enter();
        let send_future = channel.send_pty_raw(subscription_id, &data, &peer);
        match self.tokio_runtime.block_on(tokio::time::timeout(SEND_TIMEOUT, send_future)) {
            Ok(Ok(())) => true,
            Ok(Err(e)) => {
                let msg = e.to_string();
                if msg.contains("not opened") || msg.contains("No data channel") {
                    // DataChannel dead — caller should stop sending to this peer
                    false
                } else {
                    log::warn!("[WebRTC] Failed to send PTY data: {e}");
                    true // Transient error, keep trying
                }
            }
            Err(_elapsed) => {
                // Send timed out — SCTP is congested or peer is dead
                log::warn!(
                    "[WebRTC] Send timed out for {} (SCTP congestion), treating as dead",
                    &browser_identity[..browser_identity.len().min(8)]
                );
                false
            }
        }
    }

    /// Poll for queued PTY output and send via WebRTC.
    ///
    /// Forwarder tasks queue [`WebRtcPtyOutput`] messages; this drains and
    /// sends them. If interceptors are registered, they run synchronously
    /// (opt-in blocking). If observers are registered, notifications are
    /// queued for [`Self::poll_pty_observers`] — never inline.
    ///
    /// Uses a circuit breaker: if a send fails because the DataChannel is not
    /// open, all remaining messages for that peer are skipped (prevents the
    /// tick loop from being starved by hundreds of failed `block_on` calls).
    fn poll_webrtc_pty_output(&mut self) {
        use crate::hub::PtyObserverNotification;
        use crate::lua::primitives::PtyOutputContext;

        /// Max messages to process per tick to keep the event loop responsive.
        const DRAIN_BUDGET: usize = 256;

        // Drain pending PTY output messages (budget-limited)
        let messages: Vec<WebRtcPtyOutput> = std::iter::from_fn(|| {
            self.webrtc_pty_output_rx.try_recv().ok()
        })
        .take(DRAIN_BUDGET)
        .collect();

        let has_interceptors = self.lua.has_interceptors("pty_output");
        let has_observers = self.lua.has_observers("pty_output");

        // Circuit breaker: peers whose DataChannel is dead (skip further sends)
        let mut dead_peers: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for msg in messages {
            // Skip peers with dead DataChannels
            if dead_peers.contains(&msg.browser_identity) {
                continue;
            }

            let ctx = PtyOutputContext {
                agent_index: msg.agent_index,
                pty_index: msg.pty_index,
                peer_id: msg.browser_identity.clone(),
            };

            // Interceptors: sync, opt-in blocking, can transform or drop
            let final_data = if has_interceptors {
                match self.lua.call_pty_output_interceptors(&ctx, &msg.data) {
                    Ok(Some(transformed)) => transformed,
                    Ok(None) => continue, // Dropped by interceptor
                    Err(e) => {
                        log::warn!("PTY interceptor error: {}", e);
                        msg.data // Fallback to original on error
                    }
                }
            } else {
                msg.data
            };

            // Fast path: send to browser immediately
            if !self.send_webrtc_raw(&msg.subscription_id, &msg.browser_identity, final_data.clone()) {
                log::warn!(
                    "[WebRTC] DataChannel not open for {}, skipping remaining PTY output this tick",
                    &msg.browser_identity[..msg.browser_identity.len().min(8)]
                );
                dead_peers.insert(msg.browser_identity.clone());
                continue;
            }

            // Observers: queue for async processing, never block here
            if has_observers {
                // Drop oldest if queue is full
                if self.pty_observer_queue.len() >= super::PTY_OBSERVER_QUEUE_CAPACITY {
                    self.pty_observer_queue.pop_front();
                }
                self.pty_observer_queue.push_back(PtyObserverNotification {
                    ctx,
                    data: final_data,
                });
            }
        }
    }

    /// Drain pending PTY observer notifications (budget-limited).
    ///
    /// Called separately from [`Self::poll_webrtc_pty_output`] so slow
    /// observers never block the WebRTC send path. Processes up to
    /// `OBSERVER_BUDGET_PER_TICK` notifications per tick to keep the
    /// main loop responsive.
    fn poll_pty_observers(&mut self) {
        /// Max observer callbacks per tick to prevent stalling the event loop.
        const OBSERVER_BUDGET_PER_TICK: usize = 64;

        if self.pty_observer_queue.is_empty() {
            return;
        }

        let budget = OBSERVER_BUDGET_PER_TICK.min(self.pty_observer_queue.len());
        for _ in 0..budget {
            let Some(notification) = self.pty_observer_queue.pop_front() else {
                break;
            };
            self.lua.notify_pty_output_observers(&notification.ctx, &notification.data);
        }
    }

    // === TUI via Lua (Hub-side Processing) ===

    /// Poll TUI requests from TuiRunner (non-blocking).
    ///
    /// JSON control messages go through Lua `client.lua` — the same path as
    /// browser clients. Raw PTY input bytes are written directly to the PTY,
    /// bypassing Lua entirely.
    fn poll_tui_requests(&mut self) {
        use crate::client::TuiRequest;

        let Some(ref mut rx) = self.tui_request_rx else {
            return;
        };

        // Drain into Vec to release the mutable borrow on self before
        // calling lua.call_tui_message() and flush_lua_queues().
        let requests: Vec<TuiRequest> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        for request in requests {
            match request {
                TuiRequest::LuaMessage(msg) => {
                    if let Err(e) = self.lua.call_tui_message(msg) {
                        log::error!("[TUI] Lua message handling error: {}", e);
                    }
                    self.flush_lua_queues();
                }
                TuiRequest::PtyInput {
                    agent_index,
                    pty_index,
                    data,
                } => {
                    if let Some(agent_handle) = self.handle_cache.get_agent(agent_index) {
                        if let Some(pty_handle) = agent_handle.get_pty(pty_index) {
                            if let Err(e) = pty_handle.write_input_direct(&data) {
                                log::error!("[PTY-INPUT] Write failed: {e}");
                            }
                        } else {
                            log::warn!(
                                "[PTY-INPUT] No PTY at index {} for agent {}",
                                pty_index,
                                agent_index
                            );
                        }
                    } else {
                        log::warn!("[PTY-INPUT] No agent at index {}", agent_index);
                    }
                }
            }
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
                    // Binary data = raw terminal output, forward to active parser
                    let _ = tx.send(TuiOutput::Output {
                        agent_index: None,
                        pty_index: None,
                        data,
                    });
                }
            }
        }
    }

    /// Drain outgoing WebRTC signals and fire Lua events for relay.
    ///
    /// Pre-encrypted ICE candidates from `webrtc_outgoing_signal_rx` are
    /// dispatched as `"outgoing_signal"` Lua events. The `hub_commands.lua`
    /// handler picks these up and relays them via the ActionCable primitive.
    fn poll_outgoing_webrtc_signals(&mut self) {
        use crate::channel::webrtc::OutgoingSignal;

        while let Ok(signal) = self.webrtc_outgoing_signal_rx.try_recv() {
            match signal {
                OutgoingSignal::Ice {
                    browser_identity,
                    envelope,
                } => {
                    let data = serde_json::json!({
                        "browser_identity": browser_identity,
                        "envelope": envelope,
                    });
                    if let Err(e) = self.lua.fire_json_event("outgoing_signal", &data) {
                        log::error!("Failed to fire outgoing_signal event: {e}");
                    }
                    log::debug!(
                        "[Crypto] Relayed ICE candidate to browser {}",
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
            // Clean up stale channels from the same device (same Olm key, different tab UUID).
            // When a browser refreshes, it generates a new tab UUID but keeps its Olm identity key.
            // The old channel's SCTP association may still be alive and sends to it will block
            // the tick loop for 60+ seconds waiting for retransmit timeouts.
            let olm_key = crate::relay::extract_olm_key(browser_identity);
            let stale: Vec<String> = self
                .webrtc_channels
                .keys()
                .filter(|id| {
                    *id != browser_identity
                        && crate::relay::extract_olm_key(id) == olm_key
                })
                .cloned()
                .collect();
            for stale_id in stale {
                log::info!(
                    "[WebRTC] Replacing stale channel for same device: {}",
                    &stale_id[..stale_id.len().min(8)]
                );
                self.cleanup_webrtc_channel(&stale_id, "replaced");
            }

            // Wait for the previous connection's sockets to be released before
            // creating a replacement. Each WebRTC connection opens ~15 UDP sockets
            // for ICE gathering; without this, rapid reconnection cycles (e.g. phone
            // lock/unlock) accumulate sockets and exhaust the fd limit.
            // 500ms timeout: enough for the common case where close just completed,
            // but not long enough to block signaling. If the old connection hasn't
            // detected the disconnect yet (ICE timeout takes 30-60s), waiting longer
            // won't help — proceed and let cleanup happen in the background.
            if let Some(mut close_rx) = self.webrtc_pending_closes.remove(olm_key) {
                if *close_rx.borrow() {
                    log::debug!("[WebRTC] Previous connection already closed");
                } else {
                    let _guard = self.tokio_runtime.enter();
                    match self.tokio_runtime.block_on(
                        tokio::time::timeout(
                            std::time::Duration::from_millis(500),
                            close_rx.wait_for(|v| *v),
                        )
                    ) {
                        Ok(Ok(_)) => log::debug!("[WebRTC] Previous connection sockets released"),
                        Ok(Err(_)) => log::debug!("[WebRTC] Close channel dropped, proceeding"),
                        Err(_) => log::debug!("[WebRTC] Previous connection still closing, proceeding anyway"),
                    }
                }
            }

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
                .stream_frame_tx(self.stream_frame_tx.clone())
                .pty_input_tx(self.pty_input_tx.clone())
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

            // Track connection start time for timeout detection
            self.webrtc_connection_started
                .insert(browser_identity.to_string(), Instant::now());

            // NOTE: peer_connected is NOT fired here — it fires when the
            // DataChannel actually opens (polled in poll_webrtc_dc_opens).
            // This prevents PTY forwarders from starting before the DC is usable.
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

                // Encrypt the answer with E2E encryption (synchronous via mutex)
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

                match crypto.lock() {
                    Ok(mut guard) => match guard.encrypt(&plaintext, crate::relay::extract_olm_key(browser_identity)) {
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

                            // Relay encrypted answer through Lua → ActionCable
                            let data = serde_json::json!({
                                "browser_identity": browser_identity,
                                "envelope": envelope_value,
                            });
                            if let Err(e) = self.lua.fire_json_event("outgoing_signal", &data) {
                                log::error!("[WebRTC] Failed to fire outgoing_signal for answer: {e}");
                            } else {
                                log::info!("[WebRTC] Encrypted answer sent via Lua relay");
                            }
                        }
                        Err(e) => {
                            log::error!("[WebRTC] Failed to encrypt answer: {e}");
                        }
                    },
                    Err(e) => {
                        log::error!("[WebRTC] Crypto mutex poisoned: {e}");
                    }
                };
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
            self.config.hub_name.as_deref(),
        );
        // Store server-assigned ID (used for all server communication)
        self.botster_id = Some(botster_id.clone());
        // Sync to shared copy for Lua primitives
        *self.shared_server_id.lock().expect("SharedServerId mutex poisoned") = Some(botster_id);
    }

    /// Initialize CryptoService for E2E encryption (vodozemac Olm).
    ///
    /// Creates the CryptoService only. DeviceKeyBundle generation is deferred
    /// until the connection URL is first requested (lazy initialization via
    /// `get_or_generate_connection_url()`).
    pub(crate) fn init_crypto_service(&mut self) {
        registration::init_crypto_service(&mut self.browser, &self.hub_identifier);
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
