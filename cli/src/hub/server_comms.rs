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

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::hub::actions::{self, HubAction};
use crate::hub::{registration, Hub, PendingTerminalAttach, PendingTerminalAttachRequest};
use crate::notifications::push::send_push_direct;
use base64::Engine;

mod client_worker_adapters;
mod fixtures;
mod lua_bridge;
mod metrics_guardrails;
mod push_notifications;
mod server_lifecycle;
mod session_io_bridge;
mod session_reconnect;
mod terminal_profile;
mod terminal_runtime;
mod webrtc_transport;

#[derive(Debug, Clone, PartialEq, Eq)]
enum CargoBuildProfile {
    Debug,
    Release,
    Named(String),
}

fn detect_running_cargo_profile(current_exe: &Path) -> Option<CargoBuildProfile> {
    let components: Vec<_> = current_exe.components().collect();

    for window in components.windows(2) {
        let target = window[0].as_os_str();
        let profile = window[1].as_os_str();
        if target == "target" {
            let profile = profile.to_string_lossy();
            return match profile.as_ref() {
                "debug" => Some(CargoBuildProfile::Debug),
                "release" => Some(CargoBuildProfile::Release),
                "" => None,
                other => Some(CargoBuildProfile::Named(other.to_string())),
            };
        }
    }

    None
}

/// Infer Cargo target dir from the running executable path.
///
/// For paths like `<...>/target/<profile>/<bin>`, returns `<...>/target`.
fn detect_running_target_dir(current_exe: &Path) -> Option<std::path::PathBuf> {
    let profile_dir = current_exe.parent()?;
    let target_dir = profile_dir.parent()?;
    (target_dir.file_name()? == "target").then(|| target_dir.to_path_buf())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotAttachState {
    Ready,
    Reconnecting,
    Exited,
}

enum TerminalStreamFilter {
    None,
    StripOscQueriesWhenInactive {
        active_terminal_peers: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
        peer_id: String,
    },
}

impl TerminalStreamFilter {
    fn filter_chunk(
        &self,
        session_uuid: &str,
        query_filter_buffer: &mut Vec<u8>,
        data: Vec<u8>,
    ) -> Vec<u8> {
        match self {
            Self::None => data,
            Self::StripOscQueriesWhenInactive {
                active_terminal_peers,
                peer_id,
            } => {
                if active_terminal_peers
                    .lock()
                    .ok()
                    .and_then(|active| active.get(session_uuid).cloned())
                    .is_some_and(|active_peer| active_peer != peer_id.as_str())
                {
                    crate::hub::terminal_profile::strip_osc_queries_from_output(
                        query_filter_buffer,
                        &data,
                    )
                } else {
                    query_filter_buffer.clear();
                    data
                }
            }
        }
    }
}

impl Hub {
    /// How long a terminal attach intent can stay pending before `not_found`.
    const TERMINAL_ATTACH_NOT_FOUND_TIMEOUT: Duration = Duration::from_secs(10);
    const RESTTY_FIXTURE_LIVE_CHUNK_LIMIT: usize = 8;
    const HOT_SUBHANDLER_SLOW: Duration = Duration::from_millis(50);
    const SNAPSHOT_SLOW: Duration = Duration::from_millis(100);
    const CLEANUP_SCAN_SLOW: Duration = Duration::from_millis(50);
    const CLOSED_AFTER_CONNECT_WINDOW: Duration = Duration::from_secs(10);

    /// Spawn a background task to reconnect to a session process.
    ///
    /// The blocking `connect_and_seed` handshake runs in `spawn_blocking`.
    /// On success, a `SessionReconnectReady` event is sent back to the hub
    /// loop for reader installation and state seeding.
    /// Build a single-line preview for ICE candidate logging.
    /// Legacy polling entrypoint — calls all poll functions + flush.
    ///
    /// Only available in tests. Production uses `run_event_loop()` which drives
    /// individual handlers via `tokio::select!` with zero polling.
    #[cfg(test)]
    pub fn tick(&mut self) {
        self.poll_tui_requests();
        self.poll_pty_input();
        self.poll_outgoing_webrtc_signals();
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
        self.poll_webrtc_peer_payloads_for_tests();
        self.poll_user_file_watches();
        self.poll_hub_events();
        self.process_pending_terminal_attaches();
    }

    #[cfg(test)]
    fn poll_hub_events(&mut self) {
        let Some(ref mut rx) = self.hub_event_rx else {
            return;
        };
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        for event in events {
            self.handle_hub_event(event);
        }
    }

    /// Legacy periodic maintenance (test-only fallback).
    ///
    /// Production uses `HubEvent::CleanupTick` from a spawned interval task.
    #[cfg(test)]
    fn tick_periodic(&mut self) {
        self.cleanup_webrtc_peer_registry();
        self.poll_stream_frames_outgoing();
        self.process_pending_terminal_attaches();
        self.dispatch_webrtc_recovery_snapshot_requests();
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
                    &notif.session_uuid,
                    &notif.session_name,
                    &notif.notification,
                );
            }
            HubEvent::PtyOscEvent {
                session_uuid,
                session_name,
                event,
            } => {
                let counter = match &event {
                    crate::agent::pty::PtyEvent::TitleChanged(_) => "pty_osc.title",
                    crate::agent::pty::PtyEvent::CwdChanged(_) => "pty_osc.cwd",
                    crate::agent::pty::PtyEvent::PromptMark(_) => "pty_osc.prompt",
                    crate::agent::pty::PtyEvent::CursorVisibilityChanged(_) => "pty_osc.cursor",
                    _ => "pty_osc.other",
                };
                self.record_volume_guardrail(counter, "pty_osc.volume_burst");
                self.lua
                    .notify_pty_osc_event(&session_uuid, &session_name, &event);
            }
            HubEvent::PtyProcessExited {
                session_uuid,
                session_name,
                exit_code,
            } => {
                log::info!(
                    "[Hub] PTY process exited for {}:{} (code={:?})",
                    session_uuid,
                    session_name,
                    exit_code
                );
                let data = serde_json::json!({
                    "session_uuid": session_uuid,
                    "session_name": session_name,
                    "exit_code": exit_code,
                });
                if let Err(e) = self.lua.fire_json_event("process_exited", &data) {
                    log::error!("Failed to fire process_exited event: {e}");
                }
            }
            HubEvent::PtyOutputObserved { session_uuid, data } => {
                self.handle_observed_pty_output(session_uuid, data);
            }
            HubEvent::SessionIoBatch(batch) => {
                if let Some(output) = batch.output {
                    self.handle_observed_pty_output(batch.session_uuid, output);
                }
            }
            HubEvent::SessionIo(event) => {
                self.handle_session_io_event(event);
            }
            HubEvent::DropPendingSessionIoSnapshot { request_id } => {
                self.pending_session_io_snapshots.remove(&request_id);
            }
            HubEvent::ClientWorkerControl(message) => {
                self.handle_client_worker_control(message);
            }
            HubEvent::TimerFired { timer_id } => {
                self.record_volume_guardrail("timer_fired.count", "timer_fired.volume_burst");
                self.lua.fire_timer_callback(&timer_id);
            }
            HubEvent::AcChannelMessage {
                channel_id,
                message,
            } => {
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
            HubEvent::LuaHubClientRequest(request) => {
                self.process_hub_client_request(request);
            }
            HubEvent::HubClientMessage {
                connection_id,
                message,
            } => {
                use crate::lua::primitives::hub_client;
                hub_client::fire_hub_client_message(
                    self.lua.lua_ref(),
                    self.lua.hub_client_callback_registry(),
                    self.lua.hub_client_pending_requests(),
                    &connection_id,
                    message,
                );
            }
            HubEvent::HubClientDisconnected { connection_id } => {
                if self
                    .lua_hub_client_connections
                    .remove(&connection_id)
                    .is_some()
                {
                    // Clean up the callback registry entry and release the RegistryKey.
                    if let Ok(mut reg) = self.lua.hub_client_callback_registry().lock() {
                        if let Some(key) = reg.remove(&connection_id) {
                            let _ = self.lua.lua_ref().remove_registry_value(key);
                        }
                    }
                    // Remove the direct frame sender (used by hub_client.request()).
                    if let Ok(mut senders) = self.lua.hub_client_frame_senders().lock() {
                        senders.remove(&connection_id);
                    }
                    log::info!(
                        "[HubClient] Connection '{}' disconnected (remote EOF)",
                        connection_id
                    );
                }
            }
            HubEvent::LuaPushRequest { payload } => {
                self.handle_lua_push_request(payload);
            }
            HubEvent::BrowserPushControl {
                browser_identity,
                payload,
            } => {
                self.handle_browser_push_control(&browser_identity, &payload);
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
                    if let Err(e) =
                        crate::relay::persistence::save_push_subscriptions(&self.push_subscriptions)
                    {
                        log::error!(
                            "[WebPush] Failed to save push subscriptions after cleanup: {e}"
                        );
                    }
                }
            }
            HubEvent::WebRtcMessage {
                browser_identity,
                payload,
            } => {
                let started = Instant::now();
                self.process_webrtc_plaintext_payload(&browser_identity, &payload);
                self.record_hot_span(
                    "webrtc_message.total",
                    started,
                    payload.len(),
                    &browser_identity,
                );
                // Check for decrypt failure threshold (ratchet restart).
                for restart_peer in self.webrtc.drain_decrypt_failure_triggers() {
                    self.request_transport_ratchet_restart(&restart_peer);
                }
            }
            HubEvent::WebRtcPtyInput(input) => {
                self.handle_pty_input(input);
            }
            HubEvent::WebRtcFileInput(file) => {
                self.handle_file_input(file);
            }
            HubEvent::WebRtcOutgoingSignal(signal) => {
                self.handle_webrtc_signal(signal);
            }
            HubEvent::WebRtcClientWorkerEgress {
                browser_identity,
                egress,
            } => {
                self.process_webrtc_client_worker_egress(&browser_identity, egress);
            }
            HubEvent::WebRtcStreamFrame(frame) => {
                self.handle_stream_frame(frame);
                self.poll_stream_frames_outgoing();
            }
            HubEvent::UserFileWatch { watch_id, events } => {
                let fired = self.lua.fire_user_file_watch(&watch_id, events);
                if fired > 0 {
                    log::debug!("Fired {} user file watch event(s)", fired);
                }
            }
            // LuaFileChange removed — hot-reload now handled by Lua's module_watcher
            HubEvent::CleanupTick => {
                self.repair_missing_socket_path();
                self.cleanup_webrtc_peer_registry();
                self.cleanup_stale_session_io_snapshots();
                self.poll_stream_frames_outgoing();
                self.dispatch_webrtc_recovery_snapshot_requests();
                self.webrtc.clear_ratchet_restart_dedupe();
                if self.hub_event_metrics_last_log.elapsed() >= std::time::Duration::from_secs(30) {
                    let m = self.hub_event_metrics.snapshot();
                    let by_type = m
                        .by_type
                        .iter()
                        .filter(|(_, s)| s.enqueue_ok > 0 || s.pending > 0)
                        .map(|(kind, s)| {
                            let avg_us = if s.dequeue > 0 {
                                s.handler_time_total_ns / s.dequeue / 1_000
                            } else {
                                0
                            };
                            let max_us = s.handler_time_max_ns / 1_000;
                            format!(
                                "{kind}:ok={} fail={} deq={} pend={} hwm={} bytes={} bytes_hwm={} avg_us={} max_us={}",
                                s.enqueue_ok,
                                s.enqueue_failed,
                                s.dequeue,
                                s.pending,
                                s.pending_high_water,
                                s.bytes_pending,
                                s.bytes_high_water,
                                avg_us,
                                max_us
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("; ");
                    let avg_us = if m.dequeue_total > 0 {
                        m.handler_time_total_ns / m.dequeue_total / 1_000
                    } else {
                        0
                    };
                    let max_us = m.handler_time_max_ns / 1_000;
                    let spans = Self::format_metrics_spans(&m.spans);
                    let counters = Self::format_metrics_counters(&m.counters);
                    let slow_samples = Self::format_metrics_slow_samples(&m.slow_samples);
                    log::info!(
                        "[HubEventMetrics] enqueue_ok={} dequeue={} failed={} pending={} pending_hwm={} bytes_pending={} bytes_hwm={} avg_us={} max_us={} by_type=[{}] counters=[{}] spans=[{}] slow_samples=[{}]",
                        m.enqueue_ok_total,
                        m.dequeue_total,
                        m.enqueue_failed_total,
                        m.pending_total,
                        m.pending_high_water_total,
                        m.bytes_pending_total,
                        m.bytes_high_water_total,
                        avg_us,
                        max_us,
                        by_type,
                        counters,
                        spans,
                        slow_samples
                    );
                    self.hub_event_metrics_last_log = std::time::Instant::now();
                }

                // Retry pending session reconnects.
                if !self.pending_reconnects.is_empty() {
                    self.hub_event_metrics.record_high_water(
                        "reconnect.pending",
                        self.pending_reconnects.len() as u64,
                    );
                    let now = Instant::now();
                    let reconnect_deadline = Duration::from_secs(110);
                    let in_flight_timeout = Duration::from_secs(10);

                    // Phase 1: categorize entries (expired, retryable, in-flight-stale).
                    let mut expired = Vec::new();
                    let mut retryable = Vec::new();

                    for (uuid, state) in &mut self.pending_reconnects {
                        if now.duration_since(state.started_at) > reconnect_deadline {
                            expired.push(uuid.clone());
                        } else if state.in_flight
                            && state
                                .attempt_started_at
                                .is_some_and(|t| now.duration_since(t) > in_flight_timeout)
                        {
                            // Background task likely failed silently — reset.
                            state.in_flight = false;
                            state.attempt_started_at = None;
                            retryable.push((uuid.clone(), state.generation));
                        } else if !state.in_flight {
                            retryable.push((uuid.clone(), state.generation));
                        }
                    }

                    // Phase 2: handle expired entries.
                    for uuid in expired {
                        log::warn!(
                            "[Session] Reconnect expired for '{}'",
                            &uuid[..uuid.len().min(16)]
                        );
                        self.hub_event_metrics
                            .record_counter("reconnect.expired", 1);
                        self.pending_reconnects.remove(&uuid);
                        if let Some(sh) = self.handle_cache.get_session(&uuid) {
                            sh.pty().notify_process_exited(None);
                        }
                        let data = serde_json::json!({
                            "session_uuid": uuid,
                            "exit_code": null,
                        });
                        let _ = self.lua.fire_json_event("session_process_exited", &data);
                    }

                    // Phase 3: retry non-in-flight entries.
                    for (uuid, generation) in retryable {
                        self.spawn_session_reconnect(uuid, generation);
                        self.hub_event_metrics.record_counter("reconnect.retry", 1);
                    }
                }
            }
            HubEvent::DcOpened { browser_identity } => {
                let generation = self.webrtc.current_offer_generation(&browser_identity);
                let Some(peer_state) = self
                    .webrtc
                    .mark_data_channel_open(&browser_identity, generation)
                else {
                    log::warn!(
                        "[WebRTC] DcOpened for unknown peer {}, ignoring stale open event",
                        &browser_identity[..browser_identity.len().min(8)]
                    );
                    return;
                };
                self.handle_transport_control_message(peer_state);
                log::info!(
                    "[WebRTC] DataChannel opened for {}, firing peer_connected",
                    &browser_identity[..browser_identity.len().min(8)],
                );

                if self.webrtc.start_recv_forwarder(
                    &browser_identity,
                    &self.tokio_runtime,
                    self.hub_event_tx.clone(),
                ) {
                    // Spawn per-peer send task so DataChannel sends run off the event loop.
                    self.spawn_webrtc_peer_sender(&browser_identity);
                    self.queue_webrtc_peer_command(
                        &browser_identity,
                        crate::worker::webrtc::WebRtcAdapterCommand::Json {
                            data: serde_json::to_vec(&serde_json::json!({
                                "type": "dc_ready",
                            }))
                            .expect("static JSON serialization cannot fail"),
                        },
                    );
                    let worker = self.spawn_webrtc_client_worker_adapter(browser_identity.clone());
                    self.browser_client_workers
                        .insert(browser_identity.clone(), worker);

                    self.spawn_dc_ping_task(&browser_identity);
                    if let Err(e) = self.lua.call_peer_connected(&browser_identity) {
                        log::warn!("[WebRTC] Lua peer_connected callback error: {e}");
                    }
                }
            }
            HubEvent::WebRtcIngressBackpressure {
                browser_identity,
                source,
            } => {
                log::warn!(
                    "[WebRTC] Ingress backpressure from {} for {}; cleaning up peer",
                    source,
                    &browser_identity[..browser_identity.len().min(8)]
                );
                self.cleanup_webrtc_peer(&browser_identity, source);
            }
            HubEvent::WebRtcSend(send_req) => {
                use crate::lua::primitives::WebRtcSendRequest;

                match send_req {
                    WebRtcSendRequest::Json { peer_id, data } => {
                        let payload = match serde_json::to_vec(&data) {
                            Ok(p) => p,
                            Err(e) => {
                                log::warn!("[WebRTC] Lua send failed to serialize: {e}");
                                return;
                            }
                        };
                        self.queue_webrtc_peer_command(
                            &peer_id,
                            crate::worker::webrtc::WebRtcAdapterCommand::Json { data: payload },
                        );
                    }
                    WebRtcSendRequest::Binary { peer_id, data } => {
                        self.queue_webrtc_peer_command(
                            &peer_id,
                            crate::worker::webrtc::WebRtcAdapterCommand::Binary { data },
                        );
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
                        let _ = tx.send(TuiOutput::Binary(data));
                    }
                }
                self.wake_tui();
            }
            HubEvent::SocketClientConnected { client_id, conn } => {
                log::info!("[Socket] Registering client: {}", client_id);
                self.socket_clients.insert(client_id.clone(), conn);
                if let Err(e) = self.lua.call_socket_client_connected(&client_id) {
                    log::warn!("[Socket] Lua client_connected callback error: {e}");
                }
            }
            HubEvent::SocketClientDisconnected { client_id } => {
                log::info!("[Socket] Unregistering client: {}", client_id);
                if let Some(conn) = self.socket_clients.remove(&client_id) {
                    conn.disconnect();
                }
                self.unregister_terminal_client_peer(&client_id, true);
                let client_prefix = format!("{client_id}:");
                let worker_keys: Vec<String> = self
                    .terminal_client_workers
                    .keys()
                    .filter(|key| key.starts_with(&client_prefix))
                    .cloned()
                    .collect();
                for key in worker_keys {
                    if let Some(session_uuid) = key.strip_prefix(&client_prefix).map(str::to_owned)
                    {
                        self.remove_terminal_client_worker(&key, &session_uuid, "Socket");
                    }
                }
                self.pty_forwarders.retain(|key, task| {
                    if key.starts_with(&client_prefix) {
                        task.abort();
                        log::debug!("[Socket] Aborted PTY forwarder: {}", key);
                        false
                    } else {
                        true
                    }
                });
                self.pending_terminal_attaches.retain(|key, intent| {
                    if key.starts_with(&client_prefix) {
                        intent.request.deactivate();
                        log::debug!("[Socket] Dropped pending terminal attach intent: {}", key);
                        false
                    } else {
                        true
                    }
                });
                if let Err(e) = self.lua.call_socket_client_disconnected(&client_id) {
                    log::warn!("[Socket] Lua client_disconnected callback error: {e}");
                }
            }
            HubEvent::SocketMessage { client_id, msg } => {
                let bytes = serde_json::to_vec(&msg).map_or(0, |v| v.len());
                // Intercept focus_changed before Lua — it updates pty_clients
                // focus state for notification suppression, independent of
                // whether the child PTY requested focus reporting.
                if msg.get("type").and_then(|v| v.as_str()) == Some("focus_changed") {
                    let started = Instant::now();
                    if let Some(session_uuid) = msg.get("session_uuid").and_then(|v| v.as_str()) {
                        let focused = msg
                            .get("focused")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        self.set_active_terminal_peer(session_uuid, &client_id, focused);
                        self.lua.set_pty_focused(session_uuid, &client_id, focused);
                    }
                    self.record_hot_span(
                        "socket_message.focus_changed",
                        started,
                        bytes,
                        &client_id,
                    );
                } else {
                    let started = Instant::now();
                    if self.handle_terminal_color_profile_message(&client_id, &msg) {
                        self.record_hot_span(
                            "socket_message.terminal_color_profile",
                            started,
                            bytes,
                            &client_id,
                        );
                        // Handled above — do not route client profile updates through Lua.
                    } else if let Err(e) = self.lua.call_socket_message(&client_id, msg) {
                        self.hub_event_metrics
                            .record_counter("socket_message.error", 1);
                        log::error!("[Socket] Lua message handling error for {}: {e}", client_id);
                        self.record_hot_span("socket_message.lua", started, bytes, &client_id);
                    } else {
                        self.record_hot_span("socket_message.lua", started, bytes, &client_id);
                    }
                }
            }
            HubEvent::SocketPtyInput {
                client_id,
                session_uuid,
                data,
            } => {
                if data == b"\x1b[I" {
                    self.set_active_terminal_peer(&session_uuid, &client_id, true);
                    self.lua.set_pty_focused(&session_uuid, &client_id, true);
                } else if data == b"\x1b[O" {
                    self.set_active_terminal_peer(&session_uuid, &client_id, false);
                    self.lua.set_pty_focused(&session_uuid, &client_id, false);
                }
                self.learn_terminal_probe_replies(&session_uuid, &client_id, &data);
                self.lua.notify_pty_input(&session_uuid);

                let forwarder_key = format!("{client_id}:{session_uuid}");
                if let Some(worker) = self.terminal_client_workers.get(&forwarder_key) {
                    let ingress = crate::worker::transport::SocketFrameAdapter::frame_to_ingress(
                        crate::socket::framing::Frame::PtyInput {
                            session_uuid: session_uuid.clone(),
                            data,
                        },
                    )
                    .expect("PtyInput frame maps to worker ingress");
                    let adapter = crate::worker::transport::SocketFrameAdapter::new(client_id);
                    let message = crate::worker::transport::TransportAdapter::ingress_to_client(
                        &adapter, ingress,
                    );
                    if let Err(e) = worker.try_send(message) {
                        log::warn!("[Socket] Worker input queue rejected {forwarder_key}: {e}");
                    }
                } else {
                    log::warn!("[Socket] No workerized terminal subscription for {forwarder_key}");
                }
            }
            HubEvent::SocketSend(send_req) => {
                use crate::lua::primitives::SocketSendRequest;
                use crate::socket::framing::Frame;

                match send_req {
                    SocketSendRequest::Json { client_id, data } => {
                        if let Some(conn) = self.socket_clients.get(&client_id) {
                            conn.send_frame(&Frame::Json(data));
                        } else {
                            log::debug!("[Socket] Send to unknown client: {}", client_id);
                        }
                    }
                    SocketSendRequest::Binary { client_id, data } => {
                        if let Some(conn) = self.socket_clients.get(&client_id) {
                            conn.send_frame(&Frame::Binary(data));
                        } else {
                            log::debug!("[Socket] Binary send to unknown client: {}", client_id);
                        }
                    }
                }
            }
            HubEvent::LuaPtyRequest(request) => {
                use crate::lua::PtyRequest;

                match request {
                    PtyRequest::CreateForwarder(req) => {
                        self.create_lua_pty_forwarder(req);
                    }
                    PtyRequest::RefreshSnapshot(req) => {
                        self.refresh_lua_terminal_snapshot(req);
                    }
                    PtyRequest::CreateTuiForwarder(req) => {
                        self.create_lua_tui_pty_forwarder(req);
                    }
                    PtyRequest::CreateSocketForwarder(req) => {
                        self.create_lua_socket_pty_forwarder(req);
                    }
                    PtyRequest::StopForwarder { forwarder_id } => {
                        self.stop_lua_pty_forwarder(&forwarder_id);
                    }
                    PtyRequest::WritePty { session_uuid, data } => {
                        if let Some(session_handle) = self.handle_cache.get_session(&session_uuid) {
                            if let Err(e) = session_handle.pty().enqueue_session_io_request(
                                crate::worker::session_io::SessionIoRequest::PtyInput { data },
                            ) {
                                log::error!("[PTY-WRITE] Session I/O enqueue failed: {e:?}");
                            }
                        } else {
                            log::warn!("[PTY-WRITE] No session '{}'", session_uuid);
                        }
                    }
                    PtyRequest::ResizePty {
                        session_uuid,
                        rows,
                        cols,
                    } => {
                        if let Some(session_handle) = self.handle_cache.get_session(&session_uuid) {
                            if let Err(e) = session_handle.pty().enqueue_session_io_request(
                                crate::worker::session_io::SessionIoRequest::Resize { rows, cols },
                            ) {
                                log::error!("[PTY-RESIZE] Session I/O enqueue failed: {e:?}");
                            }
                        } else {
                            log::debug!("[Lua] No session '{}'", session_uuid);
                        }
                    }
                    PtyRequest::SpawnNotificationWatcher {
                        watcher_key,
                        session_uuid,
                        session_name,
                        observe_output,
                        event_tx,
                    } => {
                        self.spawn_notification_watcher(
                            watcher_key,
                            session_uuid,
                            session_name,
                            observe_output,
                            event_tx,
                        );
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
                    HubRequest::ExecRestart => {
                        log::info!("[Lua] Processing exec-restart request (self-update)");
                        self.exec_restart = true;
                        self.quit = true;
                    }
                    HubRequest::GracefulRestart => {
                        log::info!(
                            "[Lua] Processing graceful-restart request — agents will survive"
                        );
                        self.quit = true;
                    }
                    HubRequest::DevRebuild => {
                        // Run `cargo build` in the background using the same Cargo profile as
                        // the currently running executable when we can infer it from `current_exe()`.
                        // On success, fire ExecRestart so the Hub exec-replaces itself with the
                        // freshly built binary while session processes survive.
                        //
                        // On failure the Hub logs the error and keeps running — no agents
                        // are disrupted.
                        let current_exe = std::env::current_exe().ok();
                        let profile = current_exe
                            .as_deref()
                            .and_then(detect_running_cargo_profile);
                        let target_dir = current_exe.as_deref().and_then(detect_running_target_dir);
                        match &profile {
                            Some(CargoBuildProfile::Debug) => {
                                log::info!(
                                    "[Dev] Starting cargo build (debug profile) — Hub will exec-restart on success"
                                );
                            }
                            Some(CargoBuildProfile::Release) => {
                                log::info!(
                                    "[Dev] Starting cargo build (--release) — Hub will exec-restart on success"
                                );
                            }
                            Some(CargoBuildProfile::Named(name)) => {
                                log::info!(
                                    "[Dev] Starting cargo build (--profile {}) — Hub will exec-restart on success",
                                    name
                                );
                            }
                            None => {
                                log::info!(
                                    "[Dev] Starting cargo build (default profile: debug) — Hub will exec-restart on success"
                                );
                            }
                        }
                        let tx = self.hub_event_tx.clone();
                        // manifest_dir is the `cli/` directory, embedded at compile time.
                        let manifest_dir = env!("CARGO_MANIFEST_DIR");
                        let profile_for_build = profile.clone();
                        let target_dir_for_build = target_dir.clone();
                        if let Some(exe) = current_exe.as_ref() {
                            log::info!("[Dev] Running executable: {}", exe.display());
                        }
                        if let Some(td) = target_dir.as_ref() {
                            log::info!("[Dev] Using Cargo target-dir: {}", td.display());
                        }
                        self.tokio_runtime.spawn(async move {
                            let result = tokio::task::spawn_blocking(move || {
                                let mut cmd = std::process::Command::new("cargo");
                                cmd.arg("build")
                                    .arg("--manifest-path")
                                    .arg(format!("{manifest_dir}/Cargo.toml"))
                                    .current_dir(manifest_dir)
                                    .stdin(std::process::Stdio::null());
                                if let Some(target_dir) = target_dir_for_build {
                                    cmd.arg("--target-dir").arg(target_dir);
                                }
                                match profile_for_build {
                                    Some(CargoBuildProfile::Debug) | None => {}
                                    Some(CargoBuildProfile::Release) => {
                                        cmd.arg("--release");
                                    }
                                    Some(CargoBuildProfile::Named(name)) => {
                                        cmd.arg("--profile").arg(name);
                                    }
                                }
                                cmd.status()
                            })
                            .await;

                            match result {
                                Ok(Ok(status)) if status.success() => {
                                    log::info!(
                                        "[Dev] cargo build succeeded — triggering exec-restart"
                                    );
                                    let _ =
                                        tx.send(HubEvent::LuaHubRequest(HubRequest::ExecRestart));
                                }
                                Ok(Ok(status)) => {
                                    log::error!(
                                        "[Dev] cargo build failed with exit status: {status}"
                                    );
                                }
                                Ok(Err(e)) => {
                                    log::error!("[Dev] cargo build failed to launch: {e}");
                                }
                                Err(e) => {
                                    log::error!("[Dev] cargo build task panicked: {e}");
                                }
                            }
                        });
                    }
                    HubRequest::ProbeUrlReady {
                        connector_session_uuid,
                        parent_session_uuid,
                        url,
                        hostname,
                        timeout_secs,
                    } => {
                        log::info!(
                            "[UrlReadyProbe] Probe start connector={} parent={} url={} hostname={} timeout_secs={:.1}",
                            connector_session_uuid,
                            parent_session_uuid,
                            url,
                            hostname,
                            timeout_secs
                        );
                        let event_tx = self.hub_event_tx.clone();
                        self.tokio_runtime.spawn(async move {
                            let result = crate::plugin_helpers::wait_until_url_ready(
                                &hostname,
                                &url,
                                std::time::Duration::from_secs_f64(timeout_secs.max(0.1)),
                            )
                            .await;
                            let (ready, error) = match result {
                                Ok(()) => {
                                    log::info!(
                                        "[UrlReadyProbe] Probe success connector={} parent={} url={}",
                                        connector_session_uuid,
                                        parent_session_uuid,
                                        url
                                    );
                                    (true, None)
                                }
                                Err(e) => {
                                    log::warn!(
                                        "[UrlReadyProbe] Probe failure connector={} parent={} url={} reason={}",
                                        connector_session_uuid,
                                        parent_session_uuid,
                                        url,
                                        e
                                    );
                                    (false, Some(e))
                                }
                            };
                            let _ = event_tx.send(
                                crate::hub::events::HubEvent::UrlProbeReady {
                                    connector_session_uuid,
                                    parent_session_uuid,
                                    url,
                                    ready,
                                    error,
                                },
                            );
                        });
                    }
                    HubRequest::PreparePluginCommand {
                        request_id,
                        command,
                        config_path,
                        config_contents,
                        context,
                    } => {
                        let event_tx = self.hub_event_tx.clone();
                        self.tokio_runtime.spawn(async move {
                            let request_id_for_task = request_id.clone();
                            let config_path_for_task = config_path.clone();
                            let result = tokio::task::spawn_blocking(move || {
                                let config_path_ref =
                                    config_path_for_task.as_deref().map(std::path::Path::new);
                                crate::plugin_helpers::prepare_plugin_command(
                                    &command,
                                    config_path_ref,
                                    config_contents.as_deref(),
                                )
                            })
                            .await;

                            let event = match result {
                                Ok(Ok(prepared)) => {
                                    crate::hub::events::HubEvent::PluginCommandPrepared {
                                        request_id: request_id_for_task,
                                        command: Some(
                                            prepared.command.to_string_lossy().into_owned(),
                                        ),
                                        config_path: prepared
                                            .config_path
                                            .map(|path| path.to_string_lossy().into_owned()),
                                        context,
                                        error_kind: None,
                                        error: None,
                                    }
                                }
                                Ok(Err(error)) => {
                                    crate::hub::events::HubEvent::PluginCommandPrepared {
                                        request_id: request_id_for_task,
                                        command: None,
                                        config_path,
                                        context,
                                        error_kind: Some(error.kind.as_str().to_string()),
                                        error: Some(error.to_string()),
                                    }
                                }
                                Err(error) => crate::hub::events::HubEvent::PluginCommandPrepared {
                                    request_id: request_id_for_task,
                                    command: None,
                                    config_path,
                                    context,
                                    error_kind: Some("task_failed".to_string()),
                                    error: Some(format!(
                                        "Plugin command preparation task failed: {error}"
                                    )),
                                },
                            };
                            let _ = event_tx.send(event);
                        });
                    }
                    HubRequest::RunCommandGate {
                        request_id,
                        command,
                        cwd,
                        timeout_secs,
                        env,
                        config_path,
                        config_contents,
                        metadata,
                        context,
                    } => {
                        let event_tx = self.hub_event_tx.clone();
                        self.tokio_runtime.spawn(async move {
                            let request_id_for_task = request_id.clone();
                            let result = tokio::task::spawn_blocking(move || {
                                crate::plugin_helpers::run_command_gate(
                                    crate::plugin_helpers::CommandGateRequest {
                                        command,
                                        cwd: std::path::PathBuf::from(cwd),
                                        timeout: if timeout_secs > 0.0 {
                                            std::time::Duration::from_secs_f64(timeout_secs)
                                        } else {
                                            std::time::Duration::ZERO
                                        },
                                        env,
                                        config_path: config_path.map(std::path::PathBuf::from),
                                        config_contents,
                                    },
                                )
                            })
                            .await;

                            let event = match result {
                                Ok(completion) => {
                                    crate::hub::events::HubEvent::CommandGateCompleted {
                                        request_id: request_id_for_task,
                                        metadata,
                                        context,
                                        success: completion.success,
                                        exit_status: completion.exit_status,
                                        stdout_tail: completion.output_summary.stdout_tail,
                                        stderr_tail: completion.output_summary.stderr_tail,
                                        output_truncated: completion.output_summary.truncated,
                                        error_kind: completion.error_kind,
                                        error: completion.error,
                                        duration_ms: completion.duration_ms,
                                    }
                                }
                                Err(error) => crate::hub::events::HubEvent::CommandGateCompleted {
                                    request_id: request_id_for_task,
                                    metadata,
                                    context,
                                    success: false,
                                    exit_status: None,
                                    stdout_tail: String::new(),
                                    stderr_tail: String::new(),
                                    output_truncated: false,
                                    error_kind: Some("task_failed".to_string()),
                                    error: Some(format!("Command gate task failed: {error}")),
                                    duration_ms: 0,
                                },
                            };
                            let _ = event_tx.send(event);
                        });
                    }
                    HubRequest::HandleSignalingMessage { message } => {
                        self.handle_signaling_message(message);
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
                        label,
                        branch,
                        repo_root,
                        metadata,
                        prompt,
                        agent_name,
                        client_rows,
                        client_cols,
                    } => {
                        log::info!(
                            "[Lua] Dispatching async worktree.create({}) for {}",
                            branch,
                            label
                        );
                        let worktree_base = self.config.worktree_base.clone();
                        let result_tx = self.worktree_result_tx.clone();
                        let branch_clone = branch.clone();
                        let label_clone = label.clone();
                        let repo_root_clone = repo_root.clone();

                        self.tokio_runtime.spawn(async move {
                            let result = tokio::task::spawn_blocking(move || {
                                let manager = WorktreeManager::new(worktree_base);
                                if let Some(repo_root) = repo_root_clone {
                                    let repo_path = std::path::Path::new(&repo_root);
                                    if crate::lua::primitives::worktree::branch_is_repo_head(
                                        repo_path,
                                        &branch_clone,
                                    ) {
                                        Ok(repo_path.to_path_buf())
                                    } else if let Some(path) = manager
                                        .find_worktree_for_branch(repo_path, &branch_clone)?
                                    {
                                        Ok(path)
                                    } else {
                                        manager
                                            .create_worktree_for_repo_root(repo_path, &branch_clone)
                                    }
                                } else {
                                    manager.create_worktree_with_branch(&branch_clone)
                                }
                            })
                            .await;

                            let outcome = match result {
                                Ok(Ok(path)) => Ok(path),
                                Ok(Err(e)) => Err(e.to_string()),
                                Err(e) => Err(format!("spawn_blocking panicked: {e}")),
                            };

                            if result_tx
                                .try_send(WorktreeCreateResult {
                                    label: label_clone,
                                    branch,
                                    repo_root,
                                    result: outcome,
                                    metadata,
                                    prompt,
                                    agent_name,
                                    client_rows,
                                    client_cols,
                                })
                                .is_err()
                            {
                                log::warn!(
                                    "[Worktree] Result queue full/closed; dropping async result"
                                );
                            }
                        });
                    }
                    WorktreeRequest::Delete { path, branch } => {
                        log::info!(
                            "[Lua] Dispatching async worktree.delete({}, {})",
                            path,
                            branch
                        );
                        let worktree_base = self.config.worktree_base.clone();
                        let event_tx = self.hub_event_tx.clone();
                        let path_clone = path.clone();
                        let branch_clone = branch.clone();

                        self.tokio_runtime.spawn(async move {
                            let result = tokio::task::spawn_blocking(move || {
                                let manager = WorktreeManager::new(worktree_base);
                                manager.delete_worktree_by_path(
                                    std::path::Path::new(&path_clone),
                                    &branch_clone,
                                )
                            })
                            .await;

                            let outcome = match result {
                                Ok(Ok(())) => Ok(()),
                                Ok(Err(e)) => Err(e.to_string()),
                                Err(e) => Err(format!("spawn_blocking panicked: {e}")),
                            };

                            let _ =
                                event_tx.send(super::events::HubEvent::WorktreeDeleteCompleted {
                                    path,
                                    branch,
                                    result: outcome,
                                });
                        });
                    }
                }
            }
            HubEvent::WorktreeDeleteCompleted {
                path,
                branch,
                result,
            } => match result {
                Ok(()) => {
                    log::info!("[Worktree] Async deletion complete: {} ({})", branch, path);
                    self.handle_cache.remove_worktree_by_branch(&branch);
                }
                Err(e) => {
                    log::error!("[Worktree] Async deletion failed for {}: {}", branch, e);
                }
            },
            HubEvent::UrlProbeReady {
                connector_session_uuid,
                parent_session_uuid,
                url,
                ready,
                error,
            } => {
                let payload = serde_json::json!({
                    "connector_session_uuid": connector_session_uuid,
                    "parent_session_uuid": parent_session_uuid,
                    "url": url,
                    "ready": ready,
                    "error": error,
                });
                if let Err(e) = self.lua.fire_json_event("url_probe_ready", &payload) {
                    log::error!("Failed to fire url_probe_ready: {e}");
                }
            }
            HubEvent::PluginCommandPrepared {
                request_id,
                command,
                config_path,
                context,
                error_kind,
                error,
            } => {
                let payload = serde_json::json!({
                    "request_id": request_id,
                    "command": command,
                    "config_path": config_path,
                    "context": context,
                    "error_kind": error_kind,
                    "error": error,
                });
                if let Err(e) = self
                    .lua
                    .fire_json_event("plugin_command_prepared", &payload)
                {
                    log::error!("Failed to fire plugin_command_prepared: {e}");
                }
            }
            HubEvent::CommandGateCompleted {
                request_id,
                metadata,
                context,
                success,
                exit_status,
                stdout_tail,
                stderr_tail,
                output_truncated,
                error_kind,
                error,
                duration_ms,
            } => {
                let payload = serde_json::json!({
                    "request_id": request_id,
                    "metadata": metadata,
                    "context": context,
                    "success": success,
                    "exit_status": exit_status,
                    "stdout_tail": stdout_tail,
                    "stderr_tail": stderr_tail,
                    "output_truncated": output_truncated,
                    "error_kind": error_kind,
                    "error": error,
                    "duration_ms": duration_ms,
                });
                if let Err(e) = self.lua.fire_json_event("command_gate_completed", &payload) {
                    log::error!("Failed to fire command_gate_completed: {e}");
                }
            }
            HubEvent::MessageDelivered { message_len } => {
                log::info!("[MessageDelivery] Delivered message ({message_len} bytes)");
            }
            // Per-session process exited or disconnected.
            // The reader thread already broadcasts PtyEvent directly, so we
            // just need to notify Lua for cleanup — unless this is a reader
            // death (exit_code=None) on a session-backed handle, in which case
            // we attempt to reconnect before declaring the session dead.
            HubEvent::SessionProcessExited {
                session_uuid,
                exit_code,
            } => {
                log::info!(
                    "[Session] ProcessExited uuid='{}' exit={:?}",
                    session_uuid,
                    exit_code
                );

                // Reader death on a session-backed handle: attempt reconnect
                // instead of immediately declaring the session dead.
                if exit_code.is_none() {
                    if let Some(session_handle) = self.handle_cache.get_session(&session_uuid) {
                        let pty = session_handle.pty();
                        if pty.is_session_backed() {
                            // Drop old connection so session process sees EOF
                            // and enters wait_for_reconnect().
                            let cleared = pty.clear_session_connection();
                            log::info!(
                                "[Session] Reader died for '{}', cleared old connection={}, initiating reconnect",
                                &session_uuid[..session_uuid.len().min(16)],
                                cleared
                            );

                            // Allocate a generation and insert pending entry.
                            self.reconnect_generation += 1;
                            let generation = self.reconnect_generation;
                            self.pending_reconnects.insert(
                                session_uuid.clone(),
                                super::ReconnectState {
                                    started_at: Instant::now(),
                                    attempt_started_at: None,
                                    generation,
                                    in_flight: false,
                                },
                            );

                            // Immediately spawn background reconnect.
                            self.hub_event_metrics.record_counter("reconnect.retry", 1);
                            self.spawn_session_reconnect(session_uuid, generation);
                            return;
                        }
                    }
                }

                // Real process exit or non-session-backed: normal handling.
                self.cleanup_pending_session_io_snapshots_for_session(&session_uuid);
                self.cleanup_paste_files(&session_uuid);
                if let Some(session_handle) = self.handle_cache.get_session(&session_uuid) {
                    session_handle.pty().notify_process_exited(exit_code);
                }
                let data = serde_json::json!({
                    "session_uuid": session_uuid,
                    "exit_code": exit_code,
                });
                if let Err(e) = self.lua.fire_json_event("session_process_exited", &data) {
                    log::error!("[Session] Failed to fire session_process_exited event: {e}");
                }
            }

            // Background reconnect completed — validate generation, install
            // reader, publish connection, seed state.
            HubEvent::SessionReconnectReady {
                session_uuid,
                generation,
                mut conn,
                mode_flags,
            } => {
                // Validate the pending entry still exists and generation matches.
                let valid = self
                    .pending_reconnects
                    .get(&session_uuid)
                    .is_some_and(|s| s.generation == generation);

                if !valid {
                    log::info!(
                        "[Session] Dropping stale reconnect for '{}' gen={}",
                        &session_uuid[..session_uuid.len().min(16)],
                        generation
                    );
                    self.hub_event_metrics
                        .record_counter("reconnect.stale_generation", 1);
                    drop(conn);
                    return;
                }

                // Look up the session handle to access PtyHandle fields.
                let Some(session_handle) = self.handle_cache.get_session(&session_uuid) else {
                    log::warn!(
                        "[Session] Session '{}' disappeared during reconnect",
                        &session_uuid[..session_uuid.len().min(16)]
                    );
                    self.hub_event_metrics.record_counter("reconnect.failed", 1);
                    self.pending_reconnects.remove(&session_uuid);
                    return;
                };
                let pty = session_handle.pty();

                // Install reader on the new connection (cheap: stream clone + thread spawn).
                if let Err(e) = conn.install_reader(
                    session_uuid.clone(),
                    pty.event_tx_clone(),
                    pty.kitty_enabled_arc(),
                    pty.cursor_visible_arc(),
                    pty.resize_pending_arc(),
                    Arc::clone(pty.last_output_at_atomic()),
                    pty.last_human_input_atomic(),
                    self.hub_event_tx.clone(),
                ) {
                    log::error!(
                        "[Session] Failed to install reader after reconnect for '{}': {e}",
                        &session_uuid[..session_uuid.len().min(16)]
                    );
                    self.hub_event_metrics.record_counter("reconnect.failed", 1);
                    self.pending_reconnects.remove(&session_uuid);
                    // Fire deferred exit.
                    pty.notify_process_exited(None);
                    let data = serde_json::json!({
                        "session_uuid": session_uuid,
                        "exit_code": null,
                    });
                    let _ = self.lua.fire_json_event("session_process_exited", &data);
                    return;
                }

                // Store the new connection in the shared mutex.
                // This propagates to all holders (SessionConnectionWriter, etc.).
                if let Some(shared) = pty.shared_session_connection() {
                    if let Ok(mut guard) = shared.lock() {
                        *guard = Some(conn);
                    }
                }

                // Seed hub-visible state from mode flags.
                if let Some(flags) = mode_flags {
                    pty.kitty_enabled_arc()
                        .store(flags.kitty_enabled, std::sync::atomic::Ordering::Relaxed);
                    pty.cursor_visible_arc()
                        .store(flags.cursor_visible, std::sync::atomic::Ordering::Relaxed);
                }

                // Clean up pending entry.
                self.pending_reconnects.remove(&session_uuid);
                self.hub_event_metrics.record_counter("reconnect.ready", 1);

                log::info!(
                    "[Session] Reconnected to '{}' successfully",
                    &session_uuid[..session_uuid.len().min(16)]
                );

                // Fire Lua event for observability.
                let data = serde_json::json!({ "session_uuid": session_uuid });
                if let Err(e) = self.lua.fire_json_event("session_reconnected", &data) {
                    log::error!("[Session] Failed to fire session_reconnected: {e}");
                }
            }

            HubEvent::SessionUnregistered { session_uuid } => {
                self.cleanup_pending_session_io_snapshots_for_session(&session_uuid);
                self.cleanup_paste_files(&session_uuid);
                self.terminal_profiles.clear_session(&session_uuid);
                self.terminal_session_peers.remove(&session_uuid);
                self.terminal_forwarder_peers
                    .retain(|_, (tracked_session, _)| tracked_session != &session_uuid);
                let suffix = format!(":{session_uuid}");
                let worker_keys: Vec<String> = self
                    .terminal_client_workers
                    .keys()
                    .filter(|key| key.ends_with(&suffix))
                    .cloned()
                    .collect();
                for key in worker_keys {
                    self.remove_terminal_client_worker(&key, &session_uuid, "Session");
                }
                for worker in self.browser_client_workers.values() {
                    Self::unregister_worker_session_io_sender(worker, &session_uuid, "Session");
                }
                let suffix = format!(":{session_uuid}");
                self.browser_terminal_attach_sizes
                    .retain(|key, _| !key.ends_with(&suffix));
                if let Ok(mut active) = self.active_terminal_peers.lock() {
                    active.remove(&session_uuid);
                }

                log::debug!("[Session] Unregistered '{}'", session_uuid);
            }
            HubEvent::WebRtcOfferNegotiated(completion) => {
                match self.webrtc.complete_offer(completion, &self.tokio_runtime) {
                    crate::worker::webrtc::WebRtcOfferCompletionOutcome::AnswerReady {
                        browser_identity,
                        generation,
                        envelope,
                        queued_ice,
                    } => {
                        // Send the answer first. Queued browser ICE can be applied
                        // afterward; invalid or slow candidates must not delay the
                        // browser receiving the answer and beginning ICE checks.
                        self.handle_transport_control_message(
                            crate::worker::hub_control::HubControlMessage::TransportSignalReady {
                                client_id: crate::client::ClientId::browser(
                                    browser_identity.clone(),
                                ),
                                signal: crate::worker::hub_control::TransportSignal::Answer {
                                    browser_identity: browser_identity.clone(),
                                    envelope,
                                },
                            },
                        );

                        let browser_id_short =
                            browser_identity[..browser_identity.len().min(8)].to_string();
                        self.webrtc.apply_queued_ice_for_offer(
                            &browser_identity,
                            generation,
                            queued_ice,
                            &self.tokio_runtime,
                            |gen, candidate_str, sdp_mid, sdp_mline_index, e| {
                                log::warn!(
                                    "[WebRTC] Failed to apply queued ICE candidate for {}: {} (gen={}, mid={:?}, mline={:?}, candidate='{}')",
                                    browser_id_short,
                                    e,
                                    gen,
                                    sdp_mid,
                                    sdp_mline_index,
                                    Self::ice_candidate_preview(candidate_str),
                                );
                            },
                        );
                    }
                    crate::worker::webrtc::WebRtcOfferCompletionOutcome::StaleDropped {
                        browser_identity,
                        completed_generation,
                        current_generation,
                    } => {
                        log::info!(
                            "[WebRTC] Discarding stale offer completion for {} (got gen {}, current gen {})",
                            &browser_identity[..browser_identity.len().min(8)],
                            completed_generation,
                            current_generation
                        );
                    }
                    crate::worker::webrtc::WebRtcOfferCompletionOutcome::FailedCleaned {
                        browser_identity,
                        generation,
                    } => {
                        log::warn!(
                            "[WebRTC] Offer handling failed for {} at generation {} — registry discarded channel so the next retry can start cleanly",
                            &browser_identity[..browser_identity.len().min(8)],
                            generation
                        );
                    }
                }
            }
            HubEvent::WebRtcRecoverySnapshotReady { request, result } => {
                let _ = self.webrtc.complete_recovery_snapshot(
                    request,
                    result,
                    &self.hub_event_metrics,
                );
            }
        }

        // Resolve attach intents after every event so session registration and
        // subscribe handling converge immediately without client-side retry loops.
        self.process_pending_terminal_attaches();
    }
}

#[cfg(test)]
mod cargo_profile_tests;
#[cfg(test)]
mod tests;
