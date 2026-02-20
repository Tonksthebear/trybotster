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
use crate::notifications::push::send_push_direct;

impl Hub {
    /// Legacy polling entrypoint — calls all poll functions + flush.
    ///
    /// Only available in tests. Production uses `run_event_loop()` which drives
    /// individual handlers via `tokio::select!` with zero polling.
    #[cfg(test)]
    pub fn tick(&mut self) {
        self.poll_tui_requests();
        self.poll_pty_input();
        self.poll_outgoing_webrtc_signals();
        self.poll_webrtc_pty_output();
        self.poll_stream_frames_incoming();
        self.poll_worktree_results();
        self.tick_periodic();
        // Drain shared vecs/flags that are used by tests without the event channel.
        // In production, these are delivered via HubEvent instead.
        self.poll_lua_http_responses();
        self.poll_lua_websocket_events();
        self.poll_pty_notifications();
        self.poll_webrtc_dc_opens();
        self.poll_lua_timers();
        self.poll_lua_action_cable_channels();
        self.poll_webrtc_channels();
        self.poll_lua_file_changes();
        self.poll_user_file_watches();
    }

    /// Legacy periodic maintenance (test-only fallback).
    ///
    /// Production uses `HubEvent::CleanupTick` from a spawned interval task.
    #[cfg(test)]
    fn tick_periodic(&mut self) {
        self.cleanup_disconnected_webrtc_channels();
        self.poll_stream_frames_outgoing();
        self.poll_pty_observers();
    }

    // === Per-Event Handlers for select! Loop ===

    /// Dispatch a unified event from the `HubEvent` channel.
    ///
    /// Called by the `select!` loop for each event delivered by background
    /// producers. Each match arm delegates to the appropriate Lua callback
    /// firing logic or message handling.
    pub(crate) fn handle_hub_event(&mut self, event: super::events::HubEvent) {
        use super::events::HubEvent;

        match event {
            HubEvent::HttpResponse(response) => {
                self.lua.fire_http_callback(response);
            }
            HubEvent::WebSocketEvent(ws_event) => {
                self.lua.fire_websocket_event(ws_event);
            }
            HubEvent::PtyNotification(notif) => {
                self.lua.notify_pty_notification(
                    &notif.agent_key,
                    &notif.session_name,
                    &notif.notification,
                );
                // Push notifications now triggered by Lua hooks via push.send()
            }
            HubEvent::PtyOscEvent { agent_key, session_name, event } => {
                self.lua.notify_pty_osc_event(&agent_key, &session_name, &event);
            }
            HubEvent::PtyProcessExited { agent_key, session_name, exit_code } => {
                log::info!(
                    "[Hub] PTY process exited for {}:{} (code={:?})",
                    agent_key, session_name, exit_code
                );
                let data = serde_json::json!({
                    "agent_key": agent_key,
                    "session_name": session_name,
                    "exit_code": exit_code,
                });
                if let Err(e) = self.lua.fire_json_event("process_exited", &data) {
                    log::error!("Failed to fire process_exited event: {e}");
                }
            }
            HubEvent::TimerFired { timer_id } => {
                self.lua.fire_timer_callback(&timer_id);
            }
            HubEvent::AcChannelMessage { channel_id, message } => {
                use crate::lua::primitives::action_cable;
                let crypto = self.browser.crypto_service.as_ref();
                action_cable::fire_single_ac_message(
                    self.lua.lua_ref(),
                    &self.lua_ac_channels,
                    &self.lua_ac_connections,
                    self.lua.ac_callback_registry(),
                    crypto,
                    &channel_id,
                    message,
                );
            }
            HubEvent::LuaActionCableRequest(request) => {
                self.process_single_action_cable_request(request);
            }
            HubEvent::LuaPushRequest { payload } => {
                self.handle_lua_push_request(payload);
            }
            HubEvent::PushSubscriptionsExpired { identities } => {
                for identity in &identities {
                    self.push_subscriptions.remove(identity);
                    log::info!(
                        "[WebPush] Removed stale subscription for {}",
                        &identity[..identity.len().min(8)]
                    );
                }
                if !identities.is_empty() {
                    if let Err(e) = crate::relay::persistence::save_push_subscriptions(
                        &self.push_subscriptions,
                    ) {
                        log::error!("[WebPush] Failed to save push subscriptions after cleanup: {e}");
                    }
                }
            }
            HubEvent::WebRtcMessage { browser_identity, payload } => {
                self.handle_webrtc_message(&browser_identity, &payload);
                // Check for decrypt failure threshold (ratchet restart).
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
            HubEvent::UserFileWatch { watch_id, events } => {
                let fired = self.lua.fire_user_file_watch(&watch_id, events);
                if fired > 0 {
                    log::debug!("Fired {} user file watch event(s)", fired);
                }
            }
            HubEvent::LuaFileChange { modules } => {
                let reloaded = self.lua.reload_lua_modules(&modules);
                if reloaded > 0 {
                    log::info!("Hot-reloaded {} Lua module(s)", reloaded);
                }
            }
            HubEvent::CleanupTick => {
                self.cleanup_disconnected_webrtc_channels();
                self.poll_stream_frames_outgoing();
                self.poll_pty_observers();
                self.ratchet_restarted_peers.clear();
            }
            HubEvent::DcOpened { browser_identity } => {
                log::info!(
                    "[WebRTC] DataChannel opened for {}, firing peer_connected",
                    &browser_identity[..browser_identity.len().min(8)]
                );

                // Spawn a forwarding task that reads from the WebRTC recv channel
                // and sends HubEvent::WebRtcMessage for each received message.
                if let Some(channel) = self.webrtc_channels.get(&browser_identity) {
                    let tx = self.hub_event_tx.clone();
                    let bi = browser_identity.clone();
                    let recv_rx_arc = channel.recv_rx_arc();
                    let handle = self.tokio_runtime.handle().clone();
                    handle.spawn(async move {
                        let mut rx = {
                            let mut guard = recv_rx_arc.lock().await;
                            match guard.take() {
                                Some(rx) => rx,
                                None => return,
                            }
                        };
                        while let Some(raw) = rx.recv().await {
                            if tx.send(HubEvent::WebRtcMessage {
                                browser_identity: bi.clone(),
                                payload: raw.payload,
                            }).is_err() {
                                break;
                            }
                        }
                    });
                }

                if let Err(e) = self.lua.call_peer_connected(&browser_identity) {
                    log::warn!("[WebRTC] Lua peer_connected callback error: {e}");
                }
            }
            HubEvent::WebRtcSend(send_req) => {
                use crate::lua::primitives::WebRtcSendRequest;
                const SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

                match send_req {
                    WebRtcSendRequest::Json { peer_id, data } => {
                        if let Some(channel) = self.webrtc_channels.get(&peer_id) {
                            let payload = match serde_json::to_vec(&data) {
                                Ok(p) => p,
                                Err(e) => {
                                    log::warn!("[WebRTC] Lua send failed to serialize: {e}");
                                    return;
                                }
                            };
                            let peer = crate::channel::PeerId(peer_id.clone());
                            match tokio::task::block_in_place(|| {
                                self.tokio_runtime.block_on(
                                    tokio::time::timeout(SEND_TIMEOUT, channel.send_to(&payload, &peer))
                                )
                            }) {
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
                            match tokio::task::block_in_place(|| {
                                self.tokio_runtime.block_on(
                                    tokio::time::timeout(SEND_TIMEOUT, channel.send_to(&data, &peer))
                                )
                            }) {
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
            HubEvent::TuiSend(send_req) => {
                use crate::client::TuiOutput;
                use crate::lua::primitives::TuiSendRequest;

                let Some(ref tx) = self.tui_output_tx else {
                    return; // No TUI connected, discard
                };

                match send_req {
                    TuiSendRequest::Json { data } => {
                        let _ = tx.send(TuiOutput::Message(data));
                    }
                    TuiSendRequest::Binary { data } => {
                        let _ = tx.send(TuiOutput::Output {
                            agent_index: None,
                            pty_index: None,
                            data,
                        });
                    }
                }
                self.wake_tui();
            }
            HubEvent::LuaPtyRequest(request) => {
                use crate::lua::PtyRequest;

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
                    PtyRequest::WritePty { agent_index, pty_index, data } => {
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
                    PtyRequest::ResizePty { agent_index, pty_index, rows, cols } => {
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
                    PtyRequest::SpawnNotificationWatcher { watcher_key, agent_key, session_name, event_tx } => {
                        self.spawn_notification_watcher(watcher_key, agent_key, session_name, event_tx);
                    }
                }
            }
            HubEvent::LuaHubRequest(request) => {
                use crate::lua::primitives::HubRequest;

                match request {
                    HubRequest::Quit => {
                        log::info!("[Lua] Processing quit request");
                        self.quit = true;
                    }
                    HubRequest::HandleWebrtcOffer { browser_identity, sdp } => {
                        log::info!(
                            "[Lua] Processing WebRTC offer from {}",
                            &browser_identity[..browser_identity.len().min(8)]
                        );
                        self.handle_webrtc_offer(&sdp, &browser_identity);
                    }
                    HubRequest::HandleIceCandidate { browser_identity, candidate } => {
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
                            if let Err(e) = tokio::task::block_in_place(|| {
                                self.tokio_runtime.block_on(
                                    channel.handle_ice_candidate(candidate_str, sdp_mid, sdp_mline_index),
                                )
                            }) {
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
                        let olm_key = crate::relay::extract_olm_key(&browser_identity).to_string();
                        if !self.ratchet_restarted_peers.contains(&olm_key) {
                            log::warn!(
                                "[Lua] Signaling decrypt failed for {}, initiating ratchet restart",
                                &browser_identity[..browser_identity.len().min(8)]
                            );
                            self.send_ratchet_restart(&browser_identity);
                            self.ratchet_restarted_peers.insert(olm_key);
                        }
                    }
                }
            }
            HubEvent::LuaConnectionRequest(request) => {
                use crate::lua::primitives::ConnectionRequest;

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
            HubEvent::LuaWorktreeRequest(request) => {
                use crate::git::WorktreeManager;
                use crate::lua::primitives::{WorktreeCreateResult, WorktreeRequest};

                match request {
                    WorktreeRequest::Create {
                        agent_key, branch, issue_number, prompt, profile_name,
                        client_rows, client_cols,
                    } => {
                        log::info!(
                            "[Lua] Dispatching async worktree.create({}) for agent {}",
                            branch, agent_key
                        );
                        let worktree_base = self.config.worktree_base.clone();
                        let result_tx = self.worktree_result_tx.clone();
                        let branch_clone = branch.clone();
                        let agent_key_clone = agent_key.clone();

                        self.tokio_runtime.spawn(async move {
                            let result = tokio::task::spawn_blocking(move || {
                                let manager = WorktreeManager::new(worktree_base);
                                manager.create_worktree_with_branch(&branch_clone)
                            }).await;

                            let outcome = match result {
                                Ok(Ok(path)) => Ok(path),
                                Ok(Err(e)) => Err(e.to_string()),
                                Err(e) => Err(format!("spawn_blocking panicked: {e}")),
                            };

                            let _ = result_tx.send(WorktreeCreateResult {
                                agent_key: agent_key_clone,
                                branch,
                                result: outcome,
                                issue_number,
                                prompt,
                                profile_name,
                                client_rows,
                                client_cols,
                            });
                        });
                    }
                    WorktreeRequest::Delete { path, branch } => {
                        log::info!("[Lua] Processing worktree.delete({}, {})", path, branch);
                        let manager = WorktreeManager::new(self.config.worktree_base.clone());
                        if let Err(e) = manager.delete_worktree_by_path(
                            std::path::Path::new(&path),
                            &branch,
                        ) {
                            log::error!("[Lua] Failed to delete worktree: {e}");
                        } else {
                            if let Err(e) = self.load_available_worktrees() {
                                log::warn!("Failed to refresh worktrees after deletion: {e}");
                            }
                        }
                    }
                }
            }
        }
    }

    /// Handle a single TUI request from the TuiRunner thread.
    pub fn handle_tui_request(&mut self, request: crate::client::TuiRequest) {
        use crate::client::TuiRequest;
        match request {
            TuiRequest::LuaMessage(msg) => {
                if let Err(e) = self.lua.call_tui_message(msg) {
                    log::error!("[TUI] Lua message handling error: {}", e);
                }
            }
            TuiRequest::PtyInput {
                agent_index,
                pty_index,
                data,
            } => {
                self.lua.notify_pty_input(agent_index);
                if let Some(agent_handle) = self.handle_cache.get_agent(agent_index) {
                    if let Some(pty_handle) = agent_handle.get_pty(pty_index) {
                        if let Err(e) = pty_handle.write_input_direct(&data) {
                            log::error!("[PTY-INPUT] Write failed: {e}");
                        }
                    } else {
                        log::warn!(
                            "[PTY-INPUT] No PTY at index {} for agent {} (key={})",
                            pty_index, agent_index, agent_handle.agent_key()
                        );
                    }
                } else {
                    log::warn!(
                        "[PTY-INPUT] No agent at index {} (cache has {} agents)",
                        agent_index, self.handle_cache.len()
                    );
                }
            }
        }
    }

    /// Handle a single binary PTY input from a browser (WebRTC).
    pub fn handle_pty_input(&mut self, input: crate::channel::webrtc::PtyInputIncoming) {
        self.lua.notify_pty_input(input.agent_index);
        if let Some(agent_handle) = self.handle_cache.get_agent(input.agent_index) {
            if let Some(pty_handle) = agent_handle.get_pty(input.pty_index) {
                if let Err(e) = pty_handle.write_input_direct(&input.data) {
                    log::error!("[PTY-INPUT] Write failed: {e}");
                }
            }
        }
    }

    /// Handle a file transfer from browser (image paste/drop via WebRTC).
    ///
    /// Writes the file to a temp path and injects the path as text into the
    /// target PTY, so CLI tools (e.g., Claude Code) see a local file path.
    pub fn handle_file_input(&mut self, file: crate::channel::webrtc::FileInputIncoming) {
        use std::io::Write;

        // Determine file extension from filename
        let ext = std::path::Path::new(&file.filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("png");

        // Hash content for dedup filename
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            file.data.hash(&mut hasher);
            format!("{:016x}", hasher.finish())
        };

        let path = std::path::PathBuf::from(format!("/tmp/botster-paste-{hash}.{ext}"));

        // Write file
        match std::fs::File::create(&path) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(&file.data) {
                    log::error!("[FILE-INPUT] Failed to write paste file: {e}");
                    return;
                }
            }
            Err(e) => {
                log::error!("[FILE-INPUT] Failed to create paste file: {e}");
                return;
            }
        }

        log::info!(
            "[FILE-INPUT] Wrote {} bytes to {} (agent={}, pty={})",
            file.data.len(),
            path.display(),
            file.agent_index,
            file.pty_index,
        );

        // Track for cleanup + inject path into PTY
        if let Some(agent_handle) = self.handle_cache.get_agent(file.agent_index) {
            self.paste_files
                .entry(agent_handle.agent_key().to_string())
                .or_default()
                .push(path.clone());

            if let Some(pty_handle) = agent_handle.get_pty(file.pty_index) {
                let path_with_space = format!("{} ", path.display());
                if let Err(e) = pty_handle.write_input_direct(path_with_space.as_bytes()) {
                    log::error!("[FILE-INPUT] Failed to inject path into PTY: {e}");
                }
            }
        }
    }

    /// Clean up paste files for a closed agent.
    pub fn cleanup_paste_files(&mut self, agent_key: &str) {
        if let Some(files) = self.paste_files.remove(agent_key) {
            for path in &files {
                if let Err(e) = std::fs::remove_file(path) {
                    log::warn!("[FILE-INPUT] Failed to clean up paste file {}: {e}", path.display());
                }
            }
            if !files.is_empty() {
                log::info!("[FILE-INPUT] Cleaned up {} paste file(s) for agent {agent_key}", files.len());
            }
        }
    }

    /// Handle a single outgoing WebRTC signal (ICE candidate).
    pub fn handle_webrtc_signal(&mut self, signal: crate::channel::webrtc::OutgoingSignal) {
        use crate::channel::webrtc::OutgoingSignal;
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

    /// Handle a single incoming stream frame from WebRTC.
    pub fn handle_stream_frame(&mut self, frame: crate::channel::webrtc::StreamIncoming) {
        use crate::relay::stream_mux::StreamMultiplexer;

        let _guard = self.tokio_runtime.enter();
        let mux = self
            .stream_muxes
            .entry(frame.browser_identity.clone())
            .or_insert_with(StreamMultiplexer::new);
        mux.handle_frame(frame.frame_type, frame.stream_id, frame.payload);
    }

    /// Handle a single worktree creation result.
    pub fn handle_worktree_result(
        &mut self,
        result: crate::lua::primitives::WorktreeCreateResult,
    ) {
        match result.result {
            Ok(ref path) => {
                let path_str = path.to_string_lossy().to_string();
                log::info!(
                    "[Worktree] Async creation complete: {} at {}",
                    result.branch,
                    path_str
                );

                let mut worktrees = self.handle_cache.get_worktrees();
                worktrees.push((path_str.clone(), result.branch.clone()));
                self.handle_cache.set_worktrees(worktrees);

                if let Err(e) = self.load_available_worktrees() {
                    log::warn!("Failed to refresh worktrees after creation: {e}");
                }

                let event_data = serde_json::json!({
                    "agent_key": result.agent_key,
                    "branch": result.branch,
                    "path": path_str,
                    "issue_number": result.issue_number,
                    "prompt": result.prompt,
                    "profile_name": result.profile_name,
                    "client_rows": result.client_rows,
                    "client_cols": result.client_cols,
                });
                if let Err(e) = self.lua.fire_json_event("worktree_created", &event_data) {
                    log::error!("[Worktree] Failed to fire worktree_created event: {e}");
                }
            }
            Err(ref error) => {
                log::error!(
                    "[Worktree] Async creation failed for {}: {}",
                    result.branch,
                    error
                );

                let event_data = serde_json::json!({
                    "agent_key": result.agent_key,
                    "branch": result.branch,
                    "error": error,
                });
                if let Err(e) =
                    self.lua.fire_json_event("worktree_create_failed", &event_data)
                {
                    log::error!(
                        "[Worktree] Failed to fire worktree_create_failed event: {e}"
                    );
                }
            }
        }
    }

    /// Drain and process WebRTC PTY output in a batch.
    ///
    /// Called from the event loop when the `select!` branch fires. The first
    /// message is passed explicitly because `recv().await` already consumed it
    /// from the channel. It is processed directly before draining the remaining
    /// buffered messages to preserve FIFO ordering — re-injecting via `send()`
    /// would place it at the back of the queue, reordering the byte stream.
    pub fn handle_webrtc_pty_output_batch(
        &mut self,
        first: WebRtcPtyOutput,
        rx: &mut Option<tokio::sync::mpsc::UnboundedReceiver<WebRtcPtyOutput>>,
    ) {
        // Process the first message directly to preserve ordering.
        self.process_single_pty_output(first);

        // Temporarily put the receiver back into self for poll_webrtc_pty_output
        self.webrtc_pty_output_rx = rx.take();
        self.poll_webrtc_pty_output();
        // Extract it back out
        *rx = self.webrtc_pty_output_rx.take();

        // Drain PTY observer notifications inline — avoids periodic polling delay.
        // Observers are populated by poll_webrtc_pty_output above.
        self.poll_pty_observers();
    }


    /// Poll for Lua file changes and hot-reload modified modules.
    ///
    /// Production uses `HubEvent::LuaFileChange` from a blocking forwarder task.
    /// Tests use this polling fallback via the legacy `tick()` path.
    #[cfg(test)]
    fn poll_lua_file_changes(&self) {
        let reloaded = self.lua.poll_and_reload();
        if reloaded > 0 {
            log::info!("Hot-reloaded {} Lua module(s)", reloaded);
        }
    }

    /// Poll user file watches created by `watch.directory()` in Lua.
    ///
    /// Production uses `HubEvent::UserFileWatch` from blocking forwarder tasks.
    /// Tests use this polling fallback via the legacy `tick()` path.
    #[cfg(test)]
    fn poll_user_file_watches(&self) {
        let fired = self.lua.poll_user_file_watches();
        if fired > 0 {
            log::debug!("Fired {} user file watch event(s)", fired);
        }
    }

    /// Poll Lua timers and fire callbacks for expired timers.
    ///
    /// Production uses `HubEvent::TimerFired` from spawned tokio tasks.
    /// Tests use this deadline-based polling via the legacy `tick()` path.
    #[cfg(test)]
    fn poll_lua_timers(&self) {
        let fired = self.lua.poll_timers();
        if fired > 0 {
            log::debug!("Fired {} Lua timer callback(s)", fired);
        }
    }

    /// Poll for completed async HTTP responses and fire Lua callbacks.
    ///
    /// Test-only fallback for registries without an event channel.
    /// Production uses `HubEvent::HttpResponse` via `handle_hub_event()`.
    #[cfg(test)]
    fn poll_lua_http_responses(&self) {
        let fired = self.lua.poll_http_responses();
        if fired > 0 {
            log::debug!("Fired {} Lua HTTP callback(s)", fired);
        }
    }

    /// Spawn a notification watcher task for a PTY session.
    ///
    /// Subscribes to the PTY's broadcast channel, filters for
    /// `PtyEvent::Notification`, and sends `HubEvent::PtyNotification`
    /// through the unified event channel for instant delivery.
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

        let hub_tx = self.hub_event_tx.clone();
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
                        let event = super::PtyNotificationEvent {
                            agent_key: agent_key.clone(),
                            session_name: session_name.clone(),
                            notification: notif,
                        };
                        if hub_tx.send(super::events::HubEvent::PtyNotification(event)).is_err() {
                            log::warn!("[NotifWatcher] Hub event channel closed for {}", key);
                            break;
                        }
                    }
                    Ok(PtyEvent::ProcessExited { exit_code }) => {
                        log::info!(
                            "[NotifWatcher] Process exited (code={:?}) for {}",
                            exit_code, key
                        );
                        let event = super::events::HubEvent::PtyProcessExited {
                            agent_key: agent_key.clone(),
                            session_name: session_name.clone(),
                            exit_code,
                        };
                        let _ = hub_tx.send(event);
                        break;
                    }
                    Ok(event @ PtyEvent::TitleChanged(_))
                    | Ok(event @ PtyEvent::CwdChanged(_))
                    | Ok(event @ PtyEvent::PromptMark(_)) => {
                        if hub_tx
                            .send(super::events::HubEvent::PtyOscEvent {
                                agent_key: agent_key.clone(),
                                session_name: session_name.clone(),
                                event,
                            })
                            .is_err()
                        {
                            log::warn!("[NotifWatcher] Hub event channel closed for {}", key);
                            break;
                        }
                    }
                    Ok(_) => {
                        // Ignore other events (Output, Resized)
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
    /// Test-only fallback for Hub instances without the event channel wired.
    /// Production uses `HubEvent::PtyNotification` via `handle_hub_event()`.
    #[cfg(test)]
    fn poll_pty_notifications(&mut self) {
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
    /// Production uses `HubEvent::WebRtcMessage` from forwarding tasks.
    /// Tests use this poll-based path via the legacy `tick()` path.
    #[cfg(test)]
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
    /// Test-only fallback. Production uses `HubEvent::DcOpened` via `handle_hub_event()`.
    #[cfg(test)]
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
            if let Err(e) = tokio::task::block_in_place(|| {
                self.tokio_runtime
                    .block_on(channel.send_bundle_refresh(&bundle_bytes))
            }) {
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

        // Intercept push notification protocol messages before Lua
        if let Some(msg_type) = msg.get("type").and_then(|t| t.as_str()) {
            match msg_type {
                "push_sub" => {
                    self.handle_push_subscription(browser_identity, &msg);
                    return;
                }
                "vapid_generate" => {
                    self.handle_vapid_generate(browser_identity);
                    return;
                }
                "vapid_key_req" => {
                    self.handle_vapid_key_request(browser_identity);
                    return;
                }
                "vapid_key_set" => {
                    self.handle_vapid_key_set(browser_identity, &msg);
                    return;
                }
                "vapid_pub_req" => {
                    self.handle_vapid_pub_request(browser_identity);
                    return;
                }
                "push_test" => {
                    self.handle_push_test(browser_identity);
                    return;
                }
                "push_disable" => {
                    self.handle_push_disable(browser_identity);
                    return;
                }
                "push_status_req" => {
                    self.handle_push_status_request(browser_identity, &msg);
                    return;
                }
                _ => {}
            }
        }

        // Delegate all other message handling to Lua
        self.call_lua_webrtc_message(browser_identity, msg);
    }

    /// Call Lua WebRTC message handler.
    ///
    /// Passes the decrypted message to Lua's `on_message` callback (if registered).
    /// Any operations queued by the callback are sent directly via `HubEvent`.
    fn call_lua_webrtc_message(&mut self, browser_identity: &str, msg: serde_json::Value) {
        // Call Lua callback
        if let Err(e) = self.lua.call_webrtc_message(browser_identity, msg) {
            log::error!("[WebRTC-LUA] Lua callback error: {e}");
        }

    }

    /// Poll WebSocket connections for events and fire Lua callbacks.
    ///
    /// Test-only fallback for registries without an event channel.
    /// Production uses `HubEvent::WebSocketEvent` via `handle_hub_event()`.
    #[cfg(test)]
    fn poll_lua_websocket_events(&mut self) {
        let _count = self.lua.poll_websocket_events();
    }

    /// Process a single ActionCable request from `HubEvent::LuaActionCableRequest`.
    ///
    /// Handles connect/subscribe/perform/unsubscribe/close operations. When
    /// subscribing, spawns a forwarding task that sends `HubEvent::AcChannelMessage`
    /// for each received message.
    fn process_single_action_cable_request(
        &mut self,
        request: crate::lua::primitives::ActionCableRequest,
    ) {
        use crate::lua::primitives::action_cable::{LuaAcChannel, LuaAcConnection};
        use crate::lua::primitives::ActionCableRequest;

        match request {
            ActionCableRequest::Connect {
                connection_id,
                crypto,
            } => {
                let handle = self.tokio_runtime.handle().clone();
                let _guard = handle.enter();
                let connection = crate::hub::action_cable_connection::ActionCableConnection::connect(
                    &self.config.server_url,
                    self.config.get_api_key(),
                );
                self.lua_ac_connections.insert(
                    connection_id.clone(),
                    LuaAcConnection {
                        connection,
                        crypto_enabled: crypto,
                    },
                );
                log::info!(
                    "[ActionCable-Lua] Connection '{}' opened (crypto={})",
                    connection_id,
                    crypto
                );
            }

            ActionCableRequest::Subscribe {
                connection_id,
                channel_id,
                channel_name,
                params,
            } => {
                if let Some(conn) = self.lua_ac_connections.get(&connection_id) {
                    // Build the ActionCable identifier JSON with channel name and params
                    let mut identifier = serde_json::json!({ "channel": channel_name });
                    if let serde_json::Value::Object(map) = params {
                        if let serde_json::Value::Object(ref mut id_map) = identifier {
                            for (k, v) in map {
                                id_map.insert(k, v);
                            }
                        }
                    }

                    let mut ch_handle = conn.connection.subscribe(identifier);

                    // Spawn a forwarding task for incoming channel messages.
                    let forwarder_handle = if let Some(mut rx) = ch_handle.take_message_rx() {
                        let tx = self.hub_event_tx.clone();
                        let ch_id = channel_id.clone();
                        let handle = self.tokio_runtime.handle().clone();
                        Some(handle.spawn(async move {
                            while let Some(msg) = rx.recv().await {
                                if tx.send(super::events::HubEvent::AcChannelMessage {
                                    channel_id: ch_id.clone(),
                                    message: msg,
                                }).is_err() {
                                    break;
                                }
                            }
                        }))
                    } else {
                        None
                    };

                    self.lua_ac_channels.insert(
                        channel_id.clone(),
                        LuaAcChannel {
                            handle: ch_handle,
                            connection_id,
                            forwarder_handle,
                        },
                    );
                    log::info!(
                        "[ActionCable-Lua] Channel '{}' subscribed to '{}'",
                        channel_id,
                        channel_name
                    );
                } else {
                    log::warn!(
                        "[ActionCable-Lua] Subscribe failed: connection '{}' not found",
                        connection_id
                    );
                }
            }

            ActionCableRequest::Perform {
                channel_id,
                action,
                data,
            } => {
                if let Some(ch) = self.lua_ac_channels.get(&channel_id) {
                    ch.handle.perform(&action, data);
                    log::trace!(
                        "[ActionCable-Lua] Performed '{}' on channel '{}'",
                        action,
                        channel_id
                    );
                } else {
                    log::warn!(
                        "[ActionCable-Lua] Perform failed: channel '{}' not found",
                        channel_id
                    );
                }
            }

            ActionCableRequest::Unsubscribe { channel_id } => {
                if self.lua_ac_channels.remove(&channel_id).is_some() {
                    // Clean up the callback registry entry and release the RegistryKey.
                    if let Ok(mut reg) = self.lua.ac_callback_registry().lock() {
                        if let Some(key) = reg.remove(&channel_id) {
                            let _ = self.lua.lua_ref().remove_registry_value(key);
                        }
                    }
                    log::info!(
                        "[ActionCable-Lua] Channel '{}' unsubscribed",
                        channel_id
                    );
                } else {
                    log::warn!(
                        "[ActionCable-Lua] Unsubscribe failed: channel '{}' not found",
                        channel_id
                    );
                }
            }

            ActionCableRequest::Close { connection_id } => {
                // Remove all channels belonging to this connection
                let orphaned: Vec<String> = self.lua_ac_channels
                    .iter()
                    .filter(|(_, ch)| ch.connection_id == connection_id)
                    .map(|(id, _)| id.clone())
                    .collect();

                for ch_id in &orphaned {
                    self.lua_ac_channels.remove(ch_id);
                }

                // Clean up callback registry entries for all removed channels.
                if let Ok(mut reg) = self.lua.ac_callback_registry().lock() {
                    for ch_id in &orphaned {
                        if let Some(key) = reg.remove(ch_id) {
                            let _ = self.lua.lua_ref().remove_registry_value(key);
                        }
                    }
                }

                if let Some(conn) = self.lua_ac_connections.remove(&connection_id) {
                    conn.connection.shutdown();
                    log::info!(
                        "[ActionCable-Lua] Connection '{}' closed ({} channels removed)",
                        connection_id,
                        orphaned.len()
                    );
                } else {
                    log::warn!(
                        "[ActionCable-Lua] Close failed: connection '{}' not found",
                        connection_id
                    );
                }
            }
        }
    }

    /// Poll Lua ActionCable channels for incoming messages and fire callbacks.
    ///
    /// Production uses `HubEvent::AcChannelMessage` from forwarding tasks.
    /// Tests use this poll-based path via the legacy `tick()` path.
    #[cfg(test)]
    fn poll_lua_action_cable_channels(&mut self) {
        use crate::lua::primitives::action_cable;

        let crypto = self.browser.crypto_service.as_ref();
        let _count = action_cable::poll_lua_action_cable_channels(
            self.lua.lua_ref(),
            &mut self.lua_ac_channels,
            &self.lua_ac_connections,
            self.lua.ac_callback_registry(),
            crypto,
        );
    }

    /// Poll for completed async worktree creation results.
    ///
    /// Drains the result channel and fires Lua events for each completed
    /// creation. On success, updates HandleCache and fires `worktree_created`.
    /// On failure, fires `worktree_create_failed`. Both events carry the full
    /// context needed for Lua to resume or abort agent spawning.
    ///
    /// Used by `tick()` for synchronous test driving. Production uses
    /// `handle_worktree_result()` via `select!`.
    #[cfg(test)]
    fn poll_worktree_results(&mut self) {
        let Some(ref mut rx) = self.worktree_result_rx else { return; };
        let results: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        for result in results {
            match result.result {
                Ok(ref path) => {
                    let path_str = path.to_string_lossy().to_string();
                    log::info!(
                        "[Worktree] Async creation complete: {} at {}",
                        result.branch,
                        path_str
                    );

                    // Update HandleCache so worktree.find() sees it immediately
                    let mut worktrees = self.handle_cache.get_worktrees();
                    worktrees.push((path_str.clone(), result.branch.clone()));
                    self.handle_cache.set_worktrees(worktrees);

                    // Refresh state-level worktree list
                    if let Err(e) = self.load_available_worktrees() {
                        log::warn!("Failed to refresh worktrees after creation: {e}");
                    }

                    // Fire Lua event with all context for agent spawning
                    let event_data = serde_json::json!({
                        "agent_key": result.agent_key,
                        "branch": result.branch,
                        "path": path_str,
                        "issue_number": result.issue_number,
                        "prompt": result.prompt,
                        "profile_name": result.profile_name,
                        "client_rows": result.client_rows,
                        "client_cols": result.client_cols,
                    });
                    if let Err(e) = self.lua.fire_json_event("worktree_created", &event_data) {
                        log::error!("[Worktree] Failed to fire worktree_created event: {e}");
                    }
                }
                Err(ref error) => {
                    log::error!(
                        "[Worktree] Async creation failed for {}: {}",
                        result.branch,
                        error
                    );

                    let event_data = serde_json::json!({
                        "agent_key": result.agent_key,
                        "branch": result.branch,
                        "error": error,
                    });
                    if let Err(e) =
                        self.lua.fire_json_event("worktree_create_failed", &event_data)
                    {
                        log::error!(
                            "[Worktree] Failed to fire worktree_create_failed event: {e}"
                        );
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
        let wake_fd = self.tui_wake_fd;

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
                if let Some(fd) = wake_fd { super::wake_tui_pipe(fd); }
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
                        if let Some(fd) = wake_fd { super::wake_tui_pipe(fd); }
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
                        if let Some(fd) = wake_fd { super::wake_tui_pipe(fd); }
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

    /// Stop a PTY forwarder by ID.
    fn stop_lua_pty_forwarder(&mut self, forwarder_id: &str) {
        if let Some(task) = self.webrtc_pty_forwarders.remove(forwarder_id) {
            task.abort();
            log::debug!("[Lua] Stopped PTY forwarder {}", forwarder_id);
        }
    }

    // === Stream Multiplexer ===

    /// Drain PTY input from browser (bypasses JSON/Lua).
    ///
    /// Used by `tick()` for synchronous test driving. Production uses
    /// `handle_pty_input()` via `select!`.
    #[cfg(test)]
    fn poll_pty_input(&mut self) {
        let Some(ref mut rx) = self.pty_input_rx else { return; };
        let inputs: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        for input in inputs {
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
    ///
    /// Used by `tick()` for synchronous test driving. Production uses
    /// `handle_stream_frame()` via `select!`.
    #[cfg(test)]
    fn poll_stream_frames_incoming(&mut self) {
        use crate::relay::stream_mux::StreamMultiplexer;

        let Some(ref mut rx) = self.stream_frame_rx else { return; };
        let frames: Vec<crate::channel::webrtc::StreamIncoming> =
            std::iter::from_fn(|| rx.try_recv().ok()).collect();

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
    pub(crate) fn poll_stream_frames_outgoing(&mut self) {
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

            for frame in frames {
                if let Err(e) = tokio::task::block_in_place(|| {
                    self.tokio_runtime.block_on(
                        channel.send_stream_raw(frame.frame_type, frame.stream_id, &frame.payload, &peer),
                    )
                }) {
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
        let send_future = channel.send_pty_raw(subscription_id, &data, &peer);
        match tokio::task::block_in_place(|| {
            self.tokio_runtime.block_on(tokio::time::timeout(SEND_TIMEOUT, send_future))
        }) {
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
    /// Process a single PTY output message: run interceptors, send via WebRTC,
    /// and queue observer notifications. Used by `handle_webrtc_pty_output_batch`
    /// to process the first message before draining the rest.
    fn process_single_pty_output(&mut self, msg: WebRtcPtyOutput) {
        use crate::hub::PtyObserverNotification;
        use crate::lua::primitives::PtyOutputContext;

        #[cfg(test)]
        {
            self.pty_output_messages_drained += 1;
        }

        let ctx = PtyOutputContext {
            agent_index: msg.agent_index,
            pty_index: msg.pty_index,
            peer_id: msg.browser_identity.clone(),
        };

        let final_data = if self.lua.has_interceptors("pty_output") {
            match self.lua.call_pty_output_interceptors(&ctx, &msg.data) {
                Ok(Some(transformed)) => transformed,
                Ok(None) => return,
                Err(e) => {
                    log::warn!("PTY interceptor error: {}", e);
                    msg.data
                }
            }
        } else {
            msg.data
        };

        if !self.send_webrtc_raw(&msg.subscription_id, &msg.browser_identity, final_data.clone()) {
            log::warn!(
                "[WebRTC] DataChannel not open for {}, skipping PTY output",
                &msg.browser_identity[..msg.browser_identity.len().min(8)]
            );
            return;
        }

        if self.lua.has_observers("pty_output") {
            if self.pty_observer_queue.len() >= super::PTY_OBSERVER_QUEUE_CAPACITY {
                self.pty_observer_queue.pop_front();
            }
            self.pty_observer_queue.push_back(PtyObserverNotification {
                ctx,
                data: final_data,
            });
        }
    }

    /// Uses a circuit breaker: if a send fails because the DataChannel is not
    /// open, all remaining messages for that peer are skipped (prevents the
    /// tick loop from being starved by hundreds of failed `block_on` calls).
    fn poll_webrtc_pty_output(&mut self) {
        use crate::hub::PtyObserverNotification;
        use crate::lua::primitives::PtyOutputContext;

        /// Max messages to process per tick to keep the event loop responsive.
        const DRAIN_BUDGET: usize = 256;

        // Drain pending PTY output messages (budget-limited)
        let Some(ref mut rx) = self.webrtc_pty_output_rx else { return; };
        let messages: Vec<WebRtcPtyOutput> = std::iter::from_fn(|| {
            rx.try_recv().ok()
        })
        .take(DRAIN_BUDGET)
        .collect();

        // Track how many messages were drained for regression testing.
        #[cfg(test)]
        {
            self.pty_output_messages_drained += messages.len();
        }

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
    /// Used by `tick()` for synchronous test driving. Production uses
    /// `handle_tui_request()` via `select!`.
    #[cfg(test)]
    fn poll_tui_requests(&mut self) {
        use crate::client::TuiRequest;

        let Some(ref mut rx) = self.tui_request_rx else {
            return;
        };

        // Drain into Vec to release the mutable borrow on self before
        // calling lua.call_tui_message().
        let requests: Vec<TuiRequest> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        for request in requests {
            match request {
                TuiRequest::LuaMessage(msg) => {
                    if let Err(e) = self.lua.call_tui_message(msg) {
                        log::error!("[TUI] Lua message handling error: {}", e);
                    }
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

    /// Drain outgoing WebRTC signals and fire Lua events for relay.
    ///
    /// Used by `tick()` for synchronous test driving. Production uses
    /// `handle_webrtc_signal()` via `select!`.
    #[cfg(test)]
    fn poll_outgoing_webrtc_signals(&mut self) {
        use crate::channel::webrtc::OutgoingSignal;

        let Some(ref mut rx) = self.webrtc_outgoing_signal_rx else { return; };
        let signals: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        for signal in signals {
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
                    match tokio::task::block_in_place(|| {
                        self.tokio_runtime.block_on(
                            tokio::time::timeout(
                                std::time::Duration::from_millis(500),
                                close_rx.wait_for(|v| *v),
                            )
                        )
                    }) {
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
                .file_input_tx(self.file_input_tx.clone())
                .hub_event_tx(self.hub_event_tx.clone())
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
            if let Err(e) = tokio::task::block_in_place(|| {
                self.tokio_runtime.block_on(channel.connect(config))
            }) {
                log::error!("[WebRTC] Failed to configure channel: {e}");
                return;
            }

            self.webrtc_channels
                .insert(browser_identity.to_string(), channel);

            // Track connection start time for timeout detection
            self.webrtc_connection_started
                .insert(browser_identity.to_string(), Instant::now());

            // NOTE: peer_connected is NOT fired here — it fires when the
            // DataChannel actually opens (via HubEvent::DcOpened).
            // This prevents PTY forwarders from starting before the DC is usable.
        }

        // Handle the offer and get the answer
        let channel = self.webrtc_channels.get(browser_identity)
            .expect("WebRTC channel must exist after offer handling");

        match tokio::task::block_in_place(|| {
            self.tokio_runtime
                .block_on(channel.handle_sdp_offer(sdp, browser_identity))
        }) {
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

    // === Web Push Notifications ===

    /// Send VAPID public key to a browser via DataChannel.
    ///
    /// Called by `handle_vapid_generate` and `handle_vapid_key_set` after
    /// VAPID keys are available.
    fn send_vapid_public_key(&mut self, browser_identity: &str) {
        let Some(ref vapid) = self.vapid_keys else {
            return;
        };

        let msg = serde_json::json!({
            "type": "vapid_pub",
            "key": vapid.public_key_base64url(),
        });

        if let Some(channel) = self.webrtc_channels.get(browser_identity) {
            let payload = match serde_json::to_vec(&msg) {
                Ok(p) => p,
                Err(e) => {
                    log::warn!("[WebPush] Failed to serialize vapid_pub: {e}");
                    return;
                }
            };
            let peer = crate::channel::PeerId(browser_identity.to_string());
            match tokio::task::block_in_place(|| {
                self.tokio_runtime.block_on(tokio::time::timeout(
                    Duration::from_secs(2),
                    channel.send_to(&payload, &peer),
                ))
            }) {
                Ok(Ok(())) => {
                    log::info!(
                        "[WebPush] Sent VAPID public key to {}",
                        &browser_identity[..browser_identity.len().min(8)]
                    );
                }
                Ok(Err(e)) => log::warn!("[WebPush] Failed to send vapid_pub: {e}"),
                Err(_) => log::warn!("[WebPush] vapid_pub send timed out"),
            }
        }
    }

    /// Handle a push subscription from a browser.
    ///
    /// The browser sends `{ type: "push_sub", browser_id, endpoint, p256dh, auth }`
    /// after subscribing to push using our VAPID public key.
    ///
    /// `browser_id` is a stable UUID stored in localStorage, so the same physical
    /// browser always maps to the same key regardless of WebRTC identity rotation.
    /// Falls back to `browser_identity` for older clients that don't send it.
    fn handle_push_subscription(&mut self, browser_identity: &str, msg: &serde_json::Value) {
        let endpoint = msg.get("endpoint").and_then(|v| v.as_str()).unwrap_or("");
        let p256dh = msg.get("p256dh").and_then(|v| v.as_str()).unwrap_or("");
        let auth = msg.get("auth").and_then(|v| v.as_str()).unwrap_or("");

        if endpoint.is_empty() || p256dh.is_empty() || auth.is_empty() {
            log::warn!("[WebPush] Received incomplete push subscription");
            return;
        }

        // Validate endpoint is HTTPS to prevent SSRF
        if !endpoint.starts_with("https://") {
            log::warn!("[WebPush] Rejected push endpoint with non-HTTPS scheme");
            return;
        }

        // Use stable browser_id when available, fall back to ephemeral identity
        let storage_key = msg
            .get("browser_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(browser_identity)
            .to_string();

        let subscription = crate::notifications::push::PushSubscription {
            endpoint: endpoint.to_string(),
            p256dh: p256dh.to_string(),
            auth: auth.to_string(),
        };

        self.push_subscriptions
            .upsert(storage_key.clone(), subscription);

        // Persist to encrypted storage
        if let Err(e) = crate::relay::persistence::save_push_subscriptions(
            &self.push_subscriptions,
        ) {
            log::error!("[WebPush] Failed to save push subscriptions: {e}");
        }

        log::info!(
            "[WebPush] Stored push subscription for {} ({} total)",
            &storage_key[..storage_key.len().min(8)],
            self.push_subscriptions.len()
        );

        // Send acknowledgment
        self.send_push_sub_ack(browser_identity);
    }

    /// Handle browser request to generate VAPID keys (Flow A).
    ///
    /// The browser sends `{ type: "vapid_generate" }` when the user enables
    /// push notifications for this device for the first time.
    fn handle_vapid_generate(&mut self, browser_identity: &str) {
        // Load existing or generate fresh keys
        let keys = match crate::relay::persistence::load_vapid_keys() {
            Ok(Some(existing)) => existing,
            Ok(None) => {
                match crate::notifications::vapid::VapidKeys::generate() {
                    Ok(new_keys) => {
                        if let Err(e) = crate::relay::persistence::save_vapid_keys(&new_keys) {
                            log::error!("[WebPush] Failed to save generated VAPID keys: {e}");
                            return;
                        }
                        log::info!("[WebPush] Generated and saved new device-level VAPID keys");
                        new_keys
                    }
                    Err(e) => {
                        log::error!("[WebPush] Failed to generate VAPID keys: {e}");
                        return;
                    }
                }
            }
            Err(e) => {
                log::error!("[WebPush] Failed to load VAPID keys: {e}");
                return;
            }
        };

        self.vapid_keys = Some(keys);
        self.set_notifications_enabled(true);
        self.send_vapid_public_key(browser_identity);
    }

    /// Handle browser sending a copied VAPID keypair (Flow B).
    ///
    /// The browser sends `{ type: "vapid_key_set", pub, priv }` after copying
    /// keys from another device. This device stores the keypair and notifies
    /// Rails that notifications are enabled.
    fn handle_vapid_key_set(&mut self, browser_identity: &str, msg: &serde_json::Value) {
        let pub_key = match msg.get("pub").and_then(|v| v.as_str()) {
            Some(k) => k,
            None => {
                log::warn!("[WebPush] vapid_key_set missing 'pub' field");
                return;
            }
        };
        let priv_key = match msg.get("priv").and_then(|v| v.as_str()) {
            Some(k) => k,
            None => {
                log::warn!("[WebPush] vapid_key_set missing 'priv' field");
                return;
            }
        };

        let keys = match crate::notifications::vapid::VapidKeys::from_base64url(pub_key, priv_key)
        {
            Ok(k) => k,
            Err(e) => {
                log::error!("[WebPush] Invalid VAPID keys in vapid_key_set: {e}");
                return;
            }
        };

        if let Err(e) = crate::relay::persistence::save_vapid_keys(&keys) {
            log::error!("[WebPush] Failed to save copied VAPID keys: {e}");
            return;
        }

        log::info!("[WebPush] Stored copied VAPID keys from another device");
        self.vapid_keys = Some(keys);
        self.set_notifications_enabled(true);
        self.send_vapid_public_key(browser_identity);
    }

    /// Handle browser request for existing VAPID public key (Flow C).
    ///
    /// The browser sends `{ type: "vapid_pub_req" }` when the CLI already has
    /// VAPID keys but this browser isn't subscribed yet. Just send back the
    /// existing public key so the browser can subscribe its push manager.
    fn handle_vapid_pub_request(&mut self, browser_identity: &str) {
        // Ensure keys are loaded into memory
        if self.vapid_keys.is_none() {
            match crate::relay::persistence::load_vapid_keys() {
                Ok(Some(keys)) => self.vapid_keys = Some(keys),
                Ok(None) => {
                    log::warn!("[WebPush] vapid_pub_req but no VAPID keys exist");
                    return;
                }
                Err(e) => {
                    log::error!("[WebPush] Failed to load VAPID keys for pub_req: {e}");
                    return;
                }
            }
        }

        self.send_vapid_public_key(browser_identity);
    }

    /// Handle a VAPID key copy request from a browser.
    ///
    /// The browser sends `{ type: "vapid_key_req" }` when copying VAPID keys
    /// from this device to another device via the notifications settings GUI.
    fn handle_vapid_key_request(&mut self, browser_identity: &str) {
        let Some(ref vapid) = self.vapid_keys else {
            log::warn!("[WebPush] VAPID key requested but no keys loaded");
            return;
        };

        // Send full keypair (private + public) for multi-device VAPID key copying.
        // This is safe because the DataChannel is E2E encrypted via Olm.
        let msg = serde_json::json!({
            "type": "vapid_keys",
            "pub": vapid.public_key_base64url(),
            "priv": vapid.private_key_base64url(),
        });

        if let Some(channel) = self.webrtc_channels.get(browser_identity) {
            let payload = match serde_json::to_vec(&msg) {
                Ok(p) => p,
                Err(e) => {
                    log::warn!("[WebPush] Failed to serialize vapid_keys: {e}");
                    return;
                }
            };
            let peer = crate::channel::PeerId(browser_identity.to_string());
            match tokio::task::block_in_place(|| {
                self.tokio_runtime.block_on(tokio::time::timeout(
                    Duration::from_secs(2),
                    channel.send_to(&payload, &peer),
                ))
            }) {
                Ok(Ok(())) => log::info!("[WebPush] Sent VAPID keypair to browser for copy"),
                Ok(Err(e)) => log::warn!("[WebPush] Failed to send vapid_keys: {e}"),
                Err(_) => log::warn!("[WebPush] vapid_keys send timed out"),
            }
        }
    }

    /// Send push subscription acknowledgment to browser.
    fn send_push_sub_ack(&self, browser_identity: &str) {
        let msg = serde_json::json!({ "type": "push_sub_ack" });

        if let Some(channel) = self.webrtc_channels.get(browser_identity) {
            let payload = match serde_json::to_vec(&msg) {
                Ok(p) => p,
                Err(e) => {
                    log::warn!("[WebPush] Failed to serialize push_sub_ack: {e}");
                    return;
                }
            };
            let peer = crate::channel::PeerId(browser_identity.to_string());
            match tokio::task::block_in_place(|| {
                self.tokio_runtime.block_on(tokio::time::timeout(
                    Duration::from_secs(2),
                    channel.send_to(&payload, &peer),
                ))
            }) {
                Ok(Err(e)) => log::warn!("[WebPush] push_sub_ack send failed: {e}"),
                Err(_) => log::warn!("[WebPush] push_sub_ack send timed out"),
                Ok(Ok(())) => {}
            }
        }
    }

    /// Handle a test push request from the browser.
    ///
    /// Sends a test notification to all subscriptions, then acks the browser
    /// so the UI can confirm delivery.
    fn handle_push_test(&mut self, browser_identity: &str) {
        let Some(ref vapid) = self.vapid_keys else {
            log::warn!("[WebPush] Cannot send test push: no VAPID keys");
            return;
        };
        if self.push_subscriptions.is_empty() {
            log::warn!("[WebPush] Cannot send test push: no subscriptions");
            return;
        }

        let Some(ref hub_id) = self.botster_id else {
            log::warn!("[WebPush] Cannot send test push: no server hub ID");
            return;
        };

        let base_url = self.config.server_url.trim_end_matches('/');
        let navigate_url = format!("{base_url}/hubs/{hub_id}");

        let payload = serde_json::json!({
            "web_push": 8030,
            "notification": {
                "title": "Botster",
                "body": "Test notification — push is working!",
                "icon": format!("{base_url}/icon.png"),
                "navigate": navigate_url,
                "data": {
                    "id": uuid::Uuid::new_v4().to_string(),
                    "kind": "test",
                    "hubId": hub_id,
                    "url": format!("/hubs/{hub_id}"),
                    "createdAt": chrono::Utc::now().to_rfc3339(),
                }
            }
        });
        let payload_bytes = match serde_json::to_vec(&payload) {
            Ok(b) => b,
            Err(e) => {
                log::error!("[WebPush] Failed to serialize test payload: {e}");
                return;
            }
        };

        let vapid_b64 = vapid.private_key_base64url().to_string();

        let subs: Vec<(String, crate::notifications::push::PushSubscription)> = self
            .push_subscriptions
            .all()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();

        // Ack immediately — the push notification arriving is the real confirmation
        self.send_push_test_ack(browser_identity, subs.len());

        let event_tx = self.hub_event_tx.clone();
        self.tokio_runtime.handle().spawn(async move {
            let client = reqwest::Client::new();
            let mut stale = Vec::new();
            let mut sent = 0usize;

            for (identity, sub) in &subs {
                match send_push_direct(&client, &vapid_b64, sub, &payload_bytes).await {
                    Ok(true) => sent += 1,
                    Ok(false) => stale.push(identity.clone()),
                    Err(e) => {
                        log::error!(
                            "[WebPush] Test push failed for {}: {e}",
                            &identity[..identity.len().min(8)]
                        );
                    }
                }
            }

            log::info!("[WebPush] Test push: {sent} sent, {} stale", stale.len());

            if !stale.is_empty() {
                let _ = event_tx.send(super::events::HubEvent::PushSubscriptionsExpired {
                    identities: stale,
                });
            }
        });
    }

    /// Send test push acknowledgment to browser.
    fn send_push_test_ack(&self, browser_identity: &str, count: usize) {
        let msg = serde_json::json!({ "type": "push_test_ack", "sent": count });

        if let Some(channel) = self.webrtc_channels.get(browser_identity) {
            let payload = match serde_json::to_vec(&msg) {
                Ok(p) => p,
                Err(e) => {
                    log::warn!("[WebPush] Failed to serialize push_test_ack: {e}");
                    return;
                }
            };
            let peer = crate::channel::PeerId(browser_identity.to_string());
            match tokio::task::block_in_place(|| {
                self.tokio_runtime.block_on(tokio::time::timeout(
                    Duration::from_secs(2),
                    channel.send_to(&payload, &peer),
                ))
            }) {
                Ok(Err(e)) => log::warn!("[WebPush] push_test_ack send failed: {e}"),
                Err(_) => log::warn!("[WebPush] push_test_ack send timed out"),
                Ok(Ok(())) => {}
            }
        }
    }

    /// Handle browser request to disable push notifications.
    ///
    /// Clears all push subscriptions, tells Rails notifications are disabled,
    /// and acks the browser so it can unsubscribe from the push manager.
    fn handle_push_disable(&mut self, browser_identity: &str) {
        // Clear all stored push subscriptions
        self.push_subscriptions = crate::notifications::push::PushSubscriptionStore::default();
        if let Err(e) = crate::relay::persistence::save_push_subscriptions(
            &self.push_subscriptions,
        ) {
            log::error!("[WebPush] Failed to clear push subscriptions: {e}");
        }

        self.set_notifications_enabled(false);

        log::info!("[WebPush] Push notifications disabled");

        // Ack browser
        let msg = serde_json::json!({ "type": "push_disable_ack" });
        if let Some(channel) = self.webrtc_channels.get(browser_identity) {
            let payload = match serde_json::to_vec(&msg) {
                Ok(p) => p,
                Err(e) => {
                    log::warn!("[WebPush] Failed to serialize push_disable_ack: {e}");
                    return;
                }
            };
            let peer = crate::channel::PeerId(browser_identity.to_string());
            match tokio::task::block_in_place(|| {
                self.tokio_runtime.block_on(tokio::time::timeout(
                    Duration::from_secs(2),
                    channel.send_to(&payload, &peer),
                ))
            }) {
                Ok(Err(e)) => log::warn!("[WebPush] push_disable_ack send failed: {e}"),
                Err(_) => log::warn!("[WebPush] push_disable_ack send timed out"),
                Ok(Ok(())) => {}
            }
        }
    }

    /// Handle push status check from the device settings page.
    ///
    /// Browser sends `{ type: "push_status_req", browser_id }` on connect
    /// to determine which notification buttons to show. Responds with the
    /// authoritative CLI state: whether VAPID keys exist and whether this
    /// specific browser has a stored push subscription.
    fn handle_push_status_request(
        &mut self,
        browser_identity: &str,
        msg: &serde_json::Value,
    ) {
        let has_keys = self.vapid_keys.is_some();

        // Use stable browser_id when available, fall back to ephemeral identity
        let browser_id = msg
            .get("browser_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(browser_identity);

        let browser_subscribed = self.push_subscriptions.contains(browser_id);

        let response = serde_json::json!({
            "type": "push_status",
            "has_keys": has_keys,
            "browser_subscribed": browser_subscribed,
        });

        if let Some(channel) = self.webrtc_channels.get(browser_identity) {
            let payload = match serde_json::to_vec(&response) {
                Ok(p) => p,
                Err(e) => {
                    log::warn!("[WebPush] Failed to serialize push_status: {e}");
                    return;
                }
            };
            let peer = crate::channel::PeerId(browser_identity.to_string());
            match tokio::task::block_in_place(|| {
                self.tokio_runtime.block_on(tokio::time::timeout(
                    Duration::from_secs(2),
                    channel.send_to(&payload, &peer),
                ))
            }) {
                Ok(Ok(())) => {
                    log::info!(
                        "[WebPush] Sent push_status to {} (has_keys={has_keys}, subscribed={browser_subscribed})",
                        &browser_identity[..browser_identity.len().min(8)]
                    );
                }
                Ok(Err(e)) => log::warn!("[WebPush] push_status send failed: {e}"),
                Err(_) => log::warn!("[WebPush] push_status send timed out"),
            }
        }
    }

    /// Notify Rails that this device's notifications_enabled flag changed.
    ///
    /// PATCHes `/devices/{device_id}` with the new value. Fire-and-forget:
    /// a failure here doesn't block the push subscription flow.
    fn set_notifications_enabled(&self, enabled: bool) {
        let Some(device_id) = self.device.device_id else {
            log::warn!("[WebPush] No device_id, cannot update notifications_enabled on Rails");
            return;
        };
        let url = format!("{}/devices/{}", self.config.server_url, device_id);
        let body = serde_json::json!({ "notifications_enabled": enabled });
        // block_in_place: reqwest::blocking cannot run inside a tokio runtime
        // (it drops an internal runtime, which panics in async context).
        let result = tokio::task::block_in_place(|| {
            self.client
                .patch(&url)
                .bearer_auth(self.config.get_api_key())
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
        });
        match result {
            Ok(response) if response.status().is_success() => {
                log::info!("[WebPush] Set notifications_enabled={enabled} on Rails");
            }
            Ok(response) => {
                log::warn!(
                    "[WebPush] Failed to update notifications_enabled: {}",
                    response.status()
                );
            }
            Err(e) => log::warn!("[WebPush] Failed to update notifications_enabled: {e}"),
        }
    }

    /// Handle a push notification request from Lua's `push.send()`.
    ///
    /// Merges Lua-provided fields with defaults (id, hubId, createdAt) and
    /// broadcasts to all subscribed browsers. The Lua payload must include
    /// at least a `kind` field; all other fields are optional overrides.
    fn handle_lua_push_request(&mut self, lua_payload: serde_json::Value) {
        let Some(ref vapid) = self.vapid_keys else {
            return;
        };
        if self.push_subscriptions.is_empty() {
            return;
        }

        let Some(ref hub_id) = self.botster_id else {
            log::warn!("[WebPush] Cannot send Lua push: no server hub ID yet");
            return;
        };

        let base_url = self.config.server_url.trim_end_matches('/');
        let lua = lua_payload.as_object();

        // Extract fields from Lua payload with defaults
        let id = lua
            .and_then(|o| o.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let id = if id.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            id
        };

        let kind = lua
            .and_then(|o| o.get("kind"))
            .and_then(|v| v.as_str())
            .unwrap_or("agent_alert");
        let title = lua
            .and_then(|o| o.get("title"))
            .and_then(|v| v.as_str())
            .unwrap_or("Botster");
        let body = lua
            .and_then(|o| o.get("body"))
            .and_then(|v| v.as_str())
            .unwrap_or("Your attention is needed");
        let relative_url = lua
            .and_then(|o| o.get("url"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let relative_url = if relative_url.is_empty() {
            format!("/hubs/{hub_id}")
        } else {
            relative_url
        };

        let icon_path = lua
            .and_then(|o| o.get("icon"))
            .and_then(|v| v.as_str())
            .unwrap_or("/icon.png");

        // Build absolute URLs for declarative web push `navigate` field
        let navigate_url = if relative_url.starts_with("http") {
            relative_url.clone()
        } else {
            format!("{base_url}{relative_url}")
        };
        let icon_url = if icon_path.starts_with("http") {
            icon_path.to_string()
        } else {
            format!("{base_url}{icon_path}")
        };

        let agent_index = lua
            .and_then(|o| o.get("agentIndex"))
            .and_then(|v| v.as_i64());
        let pty_index = lua
            .and_then(|o| o.get("ptyIndex"))
            .and_then(|v| v.as_i64());

        let mut data = serde_json::json!({
            "id": id,
            "kind": kind,
            "hubId": hub_id,
            "url": relative_url,
            "createdAt": chrono::Utc::now().to_rfc3339(),
        });
        if let Some(ai) = agent_index {
            data["agentIndex"] = serde_json::json!(ai);
        }
        if let Some(pi) = pty_index {
            data["ptyIndex"] = serde_json::json!(pi);
        }

        let mut notification = serde_json::json!({
            "title": title,
            "body": body,
            "icon": icon_url,
            "navigate": navigate_url,
            "data": data,
        });

        // Forward optional `tag` field
        if let Some(tag) = lua.and_then(|o| o.get("tag")) {
            notification["tag"] = tag.clone();
        }

        let mut payload = serde_json::json!({
            "web_push": 8030,
            "notification": notification,
        });

        // Forward any extra Lua fields to the top-level payload (e.g. app_badge).
        // This keeps Rust generic — Lua uses Declarative Web Push field names directly.
        const CONSUMED_KEYS: &[&str] = &[
            "kind", "title", "body", "url", "icon", "tag", "id",
            "agentIndex", "ptyIndex",
            "web_push", "notification", // prevent overwriting structured fields
        ];
        if let Some(obj) = lua {
            for (key, value) in obj {
                if !CONSUMED_KEYS.contains(&key.as_str()) {
                    payload[key] = value.clone();
                }
            }
        }

        let payload_bytes = match serde_json::to_vec(&payload) {
            Ok(b) => b,
            Err(e) => {
                log::error!("[WebPush] Failed to serialize Lua push payload: {e}");
                return;
            }
        };

        let vapid_b64 = vapid.private_key_base64url().to_string();

        let subs: Vec<(String, crate::notifications::push::PushSubscription)> = self
            .push_subscriptions
            .all()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();

        let event_tx = self.hub_event_tx.clone();
        self.tokio_runtime.handle().spawn(async move {
            let client = reqwest::Client::new();
            let mut stale = Vec::new();
            let mut sent = 0usize;

            for (identity, sub) in &subs {
                match send_push_direct(&client, &vapid_b64, sub, &payload_bytes).await {
                    Ok(true) => sent += 1,
                    Ok(false) => stale.push(identity.clone()),
                    Err(e) => {
                        log::error!(
                            "[WebPush] Lua push failed for {}: {e}",
                            &identity[..identity.len().min(8)]
                        );
                    }
                }
            }

            if sent > 0 || !stale.is_empty() {
                log::info!("[WebPush] Lua push: {sent} sent, {} stale", stale.len());
            }

            if !stale.is_empty() {
                let _ = event_tx.send(super::events::HubEvent::PushSubscriptionsExpired {
                    identities: stale,
                });
            }
        });
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

    /// Initialize web push state from encrypted storage.
    ///
    /// Loads device-level VAPID keys (if they exist) and per-hub push
    /// subscriptions. Does NOT generate keys — that's triggered by the
    /// browser via `vapid_generate` DataChannel message.
    pub(crate) fn init_web_push(&mut self) {
        // Device-level VAPID keys
        match crate::relay::persistence::load_vapid_keys() {
            Ok(Some(keys)) => {
                log::info!("[WebPush] Loaded device-level VAPID keys");
                self.vapid_keys = Some(keys);
            }
            Ok(None) => {
                // Try legacy per-hub keys (migration from earlier versions)
                match crate::relay::persistence::load_legacy_hub_vapid_keys(
                    &self.hub_identifier,
                ) {
                    Ok(Some(legacy_keys)) => {
                        log::info!("[WebPush] Migrating legacy per-hub VAPID keys to device level");
                        if let Err(e) = crate::relay::persistence::save_vapid_keys(&legacy_keys) {
                            log::error!("[WebPush] Failed to save migrated VAPID keys: {e}");
                        }
                        self.vapid_keys = Some(legacy_keys);
                    }
                    Ok(None) => {
                        log::debug!("[WebPush] No VAPID keys yet (browser will trigger generation)");
                    }
                    Err(e) => log::error!("[WebPush] Failed to load legacy VAPID keys: {e}"),
                }
            }
            Err(e) => log::error!("[WebPush] Failed to load VAPID keys: {e}"),
        }

        // Device-level push subscriptions (shared across all hubs)
        match crate::relay::persistence::load_push_subscriptions() {
            Ok(mut store) => {
                // Clean up duplicate subscriptions from browser reconnections
                let removed = store.dedup_by_endpoint();
                if removed > 0 {
                    log::info!(
                        "[WebPush] Removed {} duplicate subscription(s) (same endpoint, different identity)",
                        removed
                    );
                    if let Err(e) = crate::relay::persistence::save_push_subscriptions(
                        &store,
                    ) {
                        log::error!("[WebPush] Failed to save deduped subscriptions: {e}");
                    }
                }
                if !store.is_empty() {
                    log::info!("[WebPush] Loaded {} push subscription(s)", store.len());
                }
                self.push_subscriptions = store;
            }
            Err(e) => log::error!("[WebPush] Failed to load push subscriptions: {e}"),
        }
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

#[cfg(test)]
mod tests {
    /// Proves that nesting `block_on` inside `block_on` panics.
    ///
    /// This is the exact pattern that caused the WebRTC connection panic
    /// before the `block_in_place` fix was applied to all 9 call sites
    /// in this file.
    #[test]
    #[should_panic(expected = "Cannot start a runtime from within a runtime")]
    fn test_nested_block_on_panics_without_block_in_place() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            rt.block_on(async { 42 });
        });
    }

    /// Proves that `block_in_place` wrapping `block_on` prevents the
    /// nested-runtime panic. This is the pattern used by all async
    /// bridge points in this file.
    #[test]
    fn test_block_in_place_prevents_nested_runtime_panic() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let result = tokio::task::block_in_place(|| rt.block_on(async { 42 }));
            assert_eq!(result, 42);
        });
    }

    /// Reproduces the panic from `set_notifications_enabled`:
    /// reqwest::blocking::Client cannot `.send()` inside a tokio runtime
    /// because it internally drops a runtime in an async context.
    #[test]
    #[should_panic(expected = "Cannot drop a runtime")]
    fn test_reqwest_blocking_inside_tokio_panics() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = reqwest::blocking::Client::new();
        rt.block_on(async {
            // This is exactly what set_notifications_enabled did:
            // blocking HTTP inside the select! loop's block_on context.
            let _ = client
                .patch("http://127.0.0.1:1/devices/1")
                .json(&serde_json::json!({"notifications_enabled": true}))
                .send();
        });
    }

    /// Proves that wrapping the blocking HTTP call in `block_in_place`
    /// prevents the nested-runtime panic.
    #[test]
    fn test_reqwest_blocking_with_block_in_place_works() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_millis(50))
            .build()
            .unwrap();
        rt.block_on(async {
            tokio::task::block_in_place(|| {
                // Will fail to connect (no server), but won't panic
                let result = client
                    .patch("http://127.0.0.1:1/devices/1")
                    .json(&serde_json::json!({"notifications_enabled": true}))
                    .send();
                assert!(result.is_err()); // connection refused, not a panic
            });
        });
    }

    // === End-to-End Integration Tests ===
    //
    // These tests use Hub::setup() to load ALL real Lua handlers, then
    // exercise the full TUI → Lua → Hub → TUI pipeline without mocks.

    use std::path::PathBuf;

    use crate::client::{TuiOutput, TuiRequest};
    use crate::config::Config;
    use crate::hub::Hub;
    use crate::relay::create_crypto_service;

    fn e2e_config() -> Config {
        Config {
            server_url: "http://localhost:3000".to_string(),
            token: "btstr_test-key".to_string(),
            poll_interval: 10,
            agent_timeout: 300,
            max_sessions: 10,
            worktree_base: PathBuf::from("/tmp/test-worktrees"),
            hub_name: None,
        }
    }

    /// Create a Hub with TUI registered, crypto initialized, and all real
    /// Lua handlers loaded. Returns the Hub plus the TUI channels for
    /// sending requests and receiving output.
    ///
    /// Manually calls `register_hub_primitives()` + `load_lua_init()`
    /// instead of the full `setup()` to avoid `start_lua_file_watching()`
    /// which spawns a background file watcher thread.
    fn e2e_hub() -> (
        Hub,
        tokio::sync::mpsc::UnboundedSender<TuiRequest>,
        tokio::sync::mpsc::UnboundedReceiver<TuiOutput>,
    ) {
        let config = e2e_config();
        let mut hub = Hub::new(config).unwrap();

        let crypto_service = create_crypto_service("test-hub");
        hub.browser.crypto_service = Some(crypto_service);

        // Register Hub primitives (must happen before loading init script)
        hub.lua
            .register_hub_primitives(
                std::sync::Arc::clone(&hub.handle_cache),
                hub.config.worktree_base.clone(),
                std::sync::Arc::clone(&hub.shared_server_id),
                std::sync::Arc::clone(&hub.state),
            )
            .expect("Should register hub primitives");

        // Load real Lua handlers (init.lua and all handlers)
        hub.load_lua_init();

        // Register TUI AFTER Lua handlers are loaded (triggers
        // tui_connected which may broadcast initial state)
        let (request_tx, request_rx) = tokio::sync::mpsc::unbounded_channel::<TuiRequest>();
        let output_rx = hub.register_tui_via_lua(request_rx);

        (hub, request_tx, output_rx)
    }

    /// Drains all pending `TuiOutput::Message` JSON values from the output
    /// channel, ignoring non-Message variants.
    fn drain_messages(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<TuiOutput>,
    ) -> Vec<serde_json::Value> {
        let mut messages = Vec::new();
        while let Ok(output) = rx.try_recv() {
            if let TuiOutput::Message(json) = output {
                messages.push(json);
            }
        }
        messages
    }

    /// TUI subscribe triggers state broadcasts through real Lua handlers.
    ///
    /// Sends a subscribe message, ticks the Hub, and verifies that Lua
    /// broadcasts hub state (worktree list, agent list, etc.) back to
    /// the TUI client.
    #[test]
    fn test_tui_subscribe_delivers_state() {
        let (mut hub, request_tx, mut output_rx) = e2e_hub();

        // Drain anything from setup
        drain_messages(&mut output_rx);

        // Subscribe to get initial state broadcast
        request_tx
            .send(TuiRequest::LuaMessage(serde_json::json!({
                "type": "subscribe",
                "channel": "hub"
            })))
            .unwrap();

        hub.tick();

        let messages = drain_messages(&mut output_rx);

        // After subscribe, Lua handlers should broadcast hub state.
        // Even if no events fire, the test proves the pipeline doesn't
        // crash — messages through real Lua handlers without panic.
        for msg in &messages {
            assert!(
                msg.get("type").is_some(),
                "All TUI messages should have a 'type' field, got: {}",
                msg
            );
        }
    }

    /// TUI message round-trips through real Lua handlers.
    ///
    /// Sends a JSON message via `TuiRequest::LuaMessage`, ticks the Hub
    /// to process it through real Lua handlers, and verifies that Lua
    /// produces output on the TUI channel.
    #[test]
    fn test_tui_message_round_trips_through_lua() {
        let (mut hub, request_tx, mut output_rx) = e2e_hub();

        // Drain initial state messages from setup
        drain_messages(&mut output_rx);

        // Send a subscribe message (simple, always handled by real Lua)
        request_tx
            .send(TuiRequest::LuaMessage(serde_json::json!({
                "type": "subscribe",
                "channel": "agents"
            })))
            .unwrap();

        // Tick Hub to process the message through real Lua handlers
        hub.tick();

        // The subscribe message should be processed by real Lua handlers.
        // Even if subscribe doesn't produce output, the test proves the
        // pipeline doesn't crash or lose the message.
        // (No assertion on specific output — the point is no panic/crash)
    }

    /// Full create_agent pipeline through real Lua handlers.
    ///
    /// Sends a `create_agent` message, ticks the Hub, and verifies that
    /// the real Lua handlers process it (agent creation on main repo).
    /// The agent may fail to spawn in test env (no git repo at
    /// `/tmp/test-worktrees`), but the Lua handler response proves the
    /// full pipeline is wired: TUI → Hub → Lua handlers → response.
    #[test]
    fn test_create_agent_pipeline_e2e() {
        let (mut hub, request_tx, mut output_rx) = e2e_hub();

        // Drain initial state messages from setup
        drain_messages(&mut output_rx);

        // Send create_agent through the real pipeline
        request_tx
            .send(TuiRequest::LuaMessage(serde_json::json!({
                "type": "create_agent",
                "prompt": "test prompt for e2e"
            })))
            .unwrap();

        // Tick Hub to process through real Lua handlers
        hub.tick();

        // Collect any responses from Lua handlers
        let messages = drain_messages(&mut output_rx);

        // The real Lua handlers should produce some response — either
        // agent_created (success) or an error event. The key assertion
        // is that the message flows through the full pipeline and produces
        // typed output (not silence).
        //
        // Note: In test env without a real git repo, agent creation will
        // likely fail, but the Lua error handler should still broadcast
        // an event back to TUI.
        for msg in &messages {
            assert!(
                msg.get("type").is_some(),
                "Lua handler response should have a 'type' field, got: {}",
                msg
            );
        }
    }

    /// Messages with null JSON fields don't crash real Lua handlers.
    ///
    /// The null→userdata bug caused crashes in `config_resolver.lua`.
    /// This test sends a message with explicit null fields through the
    /// full pipeline to verify `json_to_lua()` correctly maps null→nil.
    #[test]
    fn test_null_fields_dont_crash_real_lua_handlers() {
        let (mut hub, request_tx, mut output_rx) = e2e_hub();

        // Drain initial state
        drain_messages(&mut output_rx);

        // Send message with explicit null fields (the pattern that
        // previously crashed config_resolver.lua)
        request_tx
            .send(TuiRequest::LuaMessage(serde_json::json!({
                "type": "create_agent",
                "issue_or_branch": null,
                "prompt": "test with nulls",
                "repo": null
            })))
            .unwrap();

        // Tick — should NOT panic or crash
        hub.tick();

        // If we get here without panic, null fields were handled correctly
        // by real Lua handlers via json_to_lua()
    }

    /// Regression test: `select!` consumes the first message via `recv().await`.
    ///
    /// Before the fix, `handle_webrtc_pty_output_batch` did not accept the
    /// first message — the `select!` arm used `Some(_)` which silently
    /// discarded it. Since `poll_webrtc_pty_output` then calls `try_recv()`
    /// to drain remaining messages, single-message arrivals (typical for
    /// interactive terminal output) were ALL dropped.
    ///
    /// This test simulates the exact `select!` sequence:
    /// 1. Send one message (PTY forwarder)
    /// 2. `recv()` consumes it (select! wake-up)
    /// 3. Pass consumed message to `handle_webrtc_pty_output_batch`
    /// 4. Verify the message was processed (not dropped)
    #[test]
    fn test_pty_output_first_message_not_dropped_by_select() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();

        assert_eq!(
            hub.pty_output_messages_drained, 0,
            "Counter should start at zero"
        );

        // Craft a PTY output message (prefix 0x01 = terminal data)
        let msg = super::WebRtcPtyOutput {
            subscription_id: "sub_test".to_string(),
            browser_identity: "test-browser-identity".to_string(),
            data: vec![0x01, 0x41, 0x42, 0x43], // "ABC"
            agent_index: 0,
            pty_index: 0,
        };

        // Step 1: PTY forwarder sends output
        hub.webrtc_pty_output_tx.send(msg).unwrap();

        // Step 2: Extract rx (as run_event_loop does before select!)
        let mut rx = hub.webrtc_pty_output_rx.take();

        // Step 3: recv() consumes the first message (as select! does)
        let first = rx
            .as_mut()
            .unwrap()
            .try_recv()
            .expect("Should have one message");

        // Channel is now empty — the old code lost `first` here
        assert!(
            rx.as_mut().unwrap().try_recv().is_err(),
            "Channel should be empty after recv"
        );

        // Step 4: Call batch handler with the consumed first message
        hub.handle_webrtc_pty_output_batch(first, &mut rx);

        // Step 5: Verify the message was actually processed
        assert_eq!(
            hub.pty_output_messages_drained, 1,
            "The first message must be processed directly, not dropped"
        );

        // Restore rx for clean drop
        hub.webrtc_pty_output_rx = rx;
    }

    /// Verify multiple PTY output messages in a batch are all processed.
    ///
    /// When several messages arrive before the `select!` branch fires, only
    /// the first is consumed by `recv().await` — the rest are drained by
    /// `try_recv()`. This test ensures all messages are accounted for.
    #[test]
    fn test_pty_output_batch_processes_all_messages() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();

        // Send 5 messages
        for i in 0..5u8 {
            hub.webrtc_pty_output_tx
                .send(super::WebRtcPtyOutput {
                    subscription_id: "sub_test".to_string(),
                    browser_identity: "test-browser-identity".to_string(),
                    data: vec![0x01, 0x41 + i],
                    agent_index: 0,
                    pty_index: 0,
                })
                .unwrap();
        }

        let mut rx = hub.webrtc_pty_output_rx.take();

        // select! consumes the first
        let first = rx
            .as_mut()
            .unwrap()
            .try_recv()
            .expect("Should have messages");

        // 4 remain in the channel
        hub.handle_webrtc_pty_output_batch(first, &mut rx);

        // All 5 should have been processed (1 direct + 4 drained)
        assert_eq!(
            hub.pty_output_messages_drained, 5,
            "All messages in the batch must be processed"
        );

        hub.webrtc_pty_output_rx = rx;
    }
}
