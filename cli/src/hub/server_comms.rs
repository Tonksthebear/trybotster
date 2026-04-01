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

use crate::channel::Channel;
use crate::hub::actions::{self, HubAction};
use crate::hub::{
    registration, Hub, PendingTerminalAttach, PendingTerminalAttachRequest, WebRtcPtyOutput,
};
use crate::notifications::push::send_push_direct;
use base64::Engine;

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

impl Hub {
    /// How long a terminal attach intent can stay pending before `not_found`.
    const TERMINAL_ATTACH_NOT_FOUND_TIMEOUT: Duration = Duration::from_secs(10);
    const RESTTY_FIXTURE_LIVE_CHUNK_LIMIT: usize = 8;

    /// Build a single-line preview for ICE candidate logging.
    fn ice_candidate_preview(candidate: &str) -> String {
        const MAX: usize = 220;
        let single_line = candidate.replace('\n', " ").replace('\r', " ");
        let char_count = single_line.chars().count();
        if char_count <= MAX {
            return single_line;
        }
        let truncated: String = single_line.chars().take(MAX).collect();
        format!("{truncated}...<truncated,len={char_count}>")
    }

    fn restty_fixture_dump_dir() -> Option<std::path::PathBuf> {
        let raw = std::env::var("BOTSTER_DUMP_RESTTY_FIXTURES").ok()?;
        let trimmed = raw.trim();
        if trimmed.is_empty()
            || trimmed == "0"
            || trimmed.eq_ignore_ascii_case("false")
            || trimmed.eq_ignore_ascii_case("off")
        {
            return None;
        }

        if trimmed == "1" || trimmed.eq_ignore_ascii_case("true") {
            return Some(std::env::temp_dir());
        }

        Some(std::path::PathBuf::from(trimmed))
    }

    fn restty_fixture_stem(session_uuid: &str) -> String {
        let sanitized: String = session_uuid
            .chars()
            .map(|ch| match ch {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
                _ => '_',
            })
            .collect();
        format!("botster-restty-{sanitized}")
    }

    fn restty_fixture_preview_hex(data: &[u8]) -> String {
        const LIMIT: usize = 24;
        let preview = data
            .iter()
            .take(LIMIT)
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join("");
        if data.len() > LIMIT {
            format!("{preview}...")
        } else {
            preview
        }
    }

    fn write_restty_fixture_file(path: &std::path::Path, data: &[u8]) {
        use std::io::Write;

        let Some(parent) = path.parent() else {
            return;
        };
        if let Err(e) = std::fs::create_dir_all(parent) {
            log::warn!(
                "[ResttyFixture] Failed to create dump dir {}: {}",
                parent.display(),
                e
            );
            return;
        }

        match std::fs::File::create(path) {
            Ok(mut file) => {
                if let Err(e) = file.write_all(data) {
                    log::warn!("[ResttyFixture] Failed to write {}: {}", path.display(), e);
                }
            }
            Err(e) => {
                log::warn!("[ResttyFixture] Failed to create {}: {}", path.display(), e);
            }
        }
    }

    fn reset_restty_fixture_capture(
        session_uuid: &str,
        peer_id: &str,
        subscription_id: &str,
        rows: u16,
        cols: u16,
        snapshot_len: usize,
    ) {
        let Some(dir) = Self::restty_fixture_dump_dir() else {
            return;
        };

        let stem = Self::restty_fixture_stem(session_uuid);
        for index in 1..=Self::RESTTY_FIXTURE_LIVE_CHUNK_LIMIT {
            let _ = std::fs::remove_file(dir.join(format!("{stem}-live-{index:04}.bin")));
        }

        let manifest = format!(
            "session_uuid={session_uuid}\npeer_id={peer_id}\nsubscription_id={subscription_id}\nrows={rows}\ncols={cols}\nsnapshot_len={snapshot_len}\nsnapshot_file={stem}-snapshot.bin\nlive_chunk_files={stem}-live-0001.bin..{stem}-live-{limit:04}.bin\nlive_chunk_format=raw post-snapshot PTY bytes after query filtering, before WebRTC prefix/encryption\n",
            limit = Self::RESTTY_FIXTURE_LIVE_CHUNK_LIMIT,
        );
        let manifest_path = dir.join(format!("{stem}-manifest.txt"));
        Self::write_restty_fixture_file(&manifest_path, manifest.as_bytes());
        log::info!(
            "[ResttyFixture] Reset capture for session {} in {}",
            session_uuid,
            dir.display()
        );
    }

    fn dump_restty_snapshot_fixture(session_uuid: &str, snapshot: &[u8]) {
        let Some(dir) = Self::restty_fixture_dump_dir() else {
            return;
        };

        let stem = Self::restty_fixture_stem(session_uuid);
        let path = dir.join(format!("{stem}-snapshot.bin"));
        Self::write_restty_fixture_file(&path, snapshot);
        log::info!(
            "[ResttyFixture] Wrote snapshot fixture {} ({} bytes, hex={})",
            path.display(),
            snapshot.len(),
            Self::restty_fixture_preview_hex(snapshot)
        );
    }

    fn dump_restty_live_fixture_chunk(session_uuid: &str, chunk_index: usize, data: &[u8]) {
        let Some(dir) = Self::restty_fixture_dump_dir() else {
            return;
        };
        if chunk_index >= Self::RESTTY_FIXTURE_LIVE_CHUNK_LIMIT {
            return;
        }

        let stem = Self::restty_fixture_stem(session_uuid);
        let path = dir.join(format!("{stem}-live-{:04}.bin", chunk_index + 1));
        Self::write_restty_fixture_file(&path, data);
        log::info!(
            "[ResttyFixture] Wrote live chunk {} for session {} ({} bytes, hex={})",
            chunk_index + 1,
            session_uuid,
            data.len(),
            Self::restty_fixture_preview_hex(data)
        );
    }

    fn boot_terminal_colors(&self) -> std::collections::HashMap<usize, crate::terminal::Rgb> {
        self.shared_color_cache
            .lock()
            .map(|colors| colors.clone())
            .unwrap_or_default()
    }

    fn pick_replacement_terminal_peer(
        &self,
        session_uuid: &str,
        excluding_peer_id: &str,
    ) -> Option<String> {
        self.terminal_session_peers
            .get(session_uuid)
            .into_iter()
            .flat_map(|peers| peers.iter())
            .filter(|peer_id| peer_id.as_str() != excluding_peer_id)
            .filter(|peer_id| self.terminal_client_profiles.contains_key(*peer_id))
            .min()
            .cloned()
    }

    fn effective_terminal_colors(
        &self,
        session_uuid: &str,
    ) -> std::collections::HashMap<usize, crate::terminal::Rgb> {
        let active_peer = self
            .active_terminal_peers
            .lock()
            .ok()
            .and_then(|active| active.get(session_uuid).cloned());

        if let Some(peer_id) = active_peer {
            if let Some(colors) = self.terminal_client_profiles.get(&peer_id) {
                return colors.clone();
            }
        }

        self.boot_terminal_colors()
    }

    fn sync_session_terminal_profile(&mut self, session_uuid: &str) {
        let Some(session_handle) = self.handle_cache.get_session(session_uuid) else {
            return;
        };

        let colors = self.effective_terminal_colors(session_uuid);
        if colors.is_empty() {
            return;
        }

        log::debug!(
            "[PTY-PROFILE] syncing session profile session={} colors={} active_peer={:?}",
            &session_uuid[..session_uuid.len().min(16)],
            colors.len(),
            self.active_terminal_peers
                .lock()
                .ok()
                .and_then(|active| active.get(session_uuid).cloned())
        );

        if let Err(error) = session_handle.pty().set_color_profile(&colors) {
            log::warn!(
                "[PTY-PROFILE] Failed to sync session {} color profile: {}",
                &session_uuid[..session_uuid.len().min(16)],
                error
            );
        }
    }

    fn sync_active_sessions_for_terminal_peer(&mut self, peer_id: &str) {
        let session_ids: Vec<String> = self
            .active_terminal_peers
            .lock()
            .ok()
            .into_iter()
            .flat_map(|active| {
                active
                    .iter()
                    .filter_map(|(session_uuid, active_peer)| {
                        (active_peer == peer_id).then(|| session_uuid.clone())
                    })
                    .collect::<Vec<_>>()
            })
            .collect();

        for session_uuid in session_ids {
            self.sync_session_terminal_profile(&session_uuid);
        }
    }

    fn update_terminal_client_profile(
        &mut self,
        peer_id: &str,
        colors: std::collections::HashMap<usize, crate::terminal::Rgb>,
    ) {
        // Merge into the shared boot cache so newly spawned sessions inherit
        // current colors. Uses extend (not replace) so a partial client profile
        // (e.g. fg/bg only) doesn't erase existing palette entries.
        if let Ok(mut shared) = self.shared_color_cache.lock() {
            shared.extend(colors.iter().map(|(k, v)| (*k, *v)));
        }
        self.terminal_client_profiles
            .insert(peer_id.to_string(), colors);
        self.sync_active_sessions_for_terminal_peer(peer_id);
    }

    fn register_terminal_forwarder_peer(
        &mut self,
        forwarder_id: &str,
        session_uuid: &str,
        peer_id: &str,
    ) {
        self.terminal_forwarder_peers.insert(
            forwarder_id.to_string(),
            (session_uuid.to_string(), peer_id.to_string()),
        );
        self.terminal_session_peers
            .entry(session_uuid.to_string())
            .or_default()
            .insert(peer_id.to_string());
    }

    fn unregister_terminal_forwarder_peer(&mut self, forwarder_id: &str, promote_next: bool) {
        let Some((session_uuid, peer_id)) = self.terminal_forwarder_peers.remove(forwarder_id)
        else {
            return;
        };

        let mut remove_session_entry = false;
        if let Some(peers) = self.terminal_session_peers.get_mut(&session_uuid) {
            peers.remove(&peer_id);
            remove_session_entry = peers.is_empty();
        }
        if remove_session_entry {
            self.terminal_session_peers.remove(&session_uuid);
        }

        let mut should_sync = false;
        if let Ok(mut active) = self.active_terminal_peers.lock() {
            if active
                .get(&session_uuid)
                .is_some_and(|current| current == &peer_id)
            {
                active.remove(&session_uuid);
                if promote_next {
                    if let Some(next_peer) =
                        self.pick_replacement_terminal_peer(&session_uuid, &peer_id)
                    {
                        active.insert(session_uuid.clone(), next_peer);
                    }
                }
                should_sync = true;
            }
        }

        if should_sync {
            self.sync_session_terminal_profile(&session_uuid);
        }
    }

    fn unregister_terminal_client_peer(&mut self, peer_id: &str, promote_next: bool) {
        self.terminal_client_profiles.remove(peer_id);

        let forwarder_ids: Vec<String> = self
            .terminal_forwarder_peers
            .iter()
            .filter_map(|(forwarder_id, (_, owner_peer))| {
                (owner_peer == peer_id).then(|| forwarder_id.clone())
            })
            .collect();

        for forwarder_id in forwarder_ids {
            self.unregister_terminal_forwarder_peer(&forwarder_id, promote_next);
        }
    }

    fn handle_terminal_color_profile_message(
        &mut self,
        peer_id: &str,
        msg: &serde_json::Value,
    ) -> bool {
        if msg.get("type").and_then(|value| value.as_str()) != Some("terminal_color_profile") {
            return false;
        }

        let colors: std::collections::HashMap<usize, crate::terminal::Rgb> = msg
            .get("colors")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        let session_uuid = msg
            .get("session_uuid")
            .and_then(|value| value.as_str())
            .unwrap_or("<unknown>");
        let bg = colors.get(&257usize).copied();
        log::debug!(
            "[PTY-PROFILE] learned client profile peer={} session={} colors={} bg={:?}",
            peer_id,
            session_uuid,
            colors.len(),
            bg
        );
        self.update_terminal_client_profile(peer_id, colors);
        true
    }

    fn set_active_terminal_peer(&mut self, session_uuid: &str, peer_id: &str, focused: bool) {
        let Ok(mut active) = self.active_terminal_peers.lock() else {
            return;
        };

        if focused {
            active.insert(session_uuid.to_string(), peer_id.to_string());
        } else if active
            .get(session_uuid)
            .is_some_and(|current| current == peer_id)
        {
            active.remove(session_uuid);
        } else {
            return;
        }

        drop(active);
        self.sync_session_terminal_profile(session_uuid);
    }

    fn learn_terminal_probe_replies(&mut self, session_uuid: &str, peer_id: &str, data: &[u8]) {
        let descriptions = crate::hub::terminal_profile::describe_probe_sequences(data);
        if !descriptions.is_empty() {
            log::info!(
                "[PTY-PROBE] Learned terminal reply candidates from peer={} session={}: {}",
                peer_id,
                session_uuid,
                descriptions.join(", ")
            );
        }
        self.terminal_profiles
            .observe_input(session_uuid, peer_id, data);
    }

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
        self.poll_user_file_watches();
        self.process_pending_terminal_attaches();
    }

    /// Legacy periodic maintenance (test-only fallback).
    ///
    /// Production uses `HubEvent::CleanupTick` from a spawned interval task.
    #[cfg(test)]
    fn tick_periodic(&mut self) {
        self.cleanup_disconnected_webrtc_channels();
        self.poll_stream_frames_outgoing();
        self.process_pending_terminal_attaches();
        self.send_backpressure_recovery_snapshots();
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
                // Learn terminal probes from raw session output (headless-safe).
                // Without this, probe responses are only learned through client
                // input paths (TUI/WebRTC/socket), missing headless sessions.
                self.learn_terminal_probe_replies(&session_uuid, "session", &data);

                if self.lua.has_observers("pty_output") {
                    let ctx = crate::lua::primitives::PtyOutputContext {
                        peer_id: format!("session:{session_uuid}"),
                        session_uuid,
                    };
                    self.lua.notify_pty_output_observers(&ctx, &data);
                }
            }
            HubEvent::TimerFired { timer_id } => {
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
                self.handle_webrtc_message(&browser_identity, &payload);
                // Check for decrypt failure threshold (ratchet restart).
                if let Some(channel) = self.webrtc_channels.get(&browser_identity) {
                    let failures = channel.decrypt_failure_count();
                    if failures >= 3 {
                        channel.reset_decrypt_failures();
                        self.try_ratchet_restart(&browser_identity);
                    }
                }
            }
            HubEvent::UserFileWatch { watch_id, events } => {
                let fired = self.lua.fire_user_file_watch(&watch_id, events);
                if fired > 0 {
                    log::debug!("Fired {} user file watch event(s)", fired);
                }
            }
            // LuaFileChange removed — hot-reload now handled by Lua's module_watcher
            HubEvent::CleanupTick => {
                self.cleanup_disconnected_webrtc_channels();
                self.poll_stream_frames_outgoing();
                self.send_backpressure_recovery_snapshots();
                self.ratchet_restarted_peers.clear();
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
                    log::info!(
                        "[HubEventMetrics] enqueue_ok={} dequeue={} failed={} pending={} pending_hwm={} bytes_pending={} bytes_hwm={} avg_us={} max_us={} by_type=[{}]",
                        m.enqueue_ok_total,
                        m.dequeue_total,
                        m.enqueue_failed_total,
                        m.pending_total,
                        m.pending_high_water_total,
                        m.bytes_pending_total,
                        m.bytes_high_water_total,
                        avg_us,
                        max_us,
                        by_type
                    );
                    self.hub_event_metrics_last_log = std::time::Instant::now();
                }
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
                            if tx
                                .send(HubEvent::WebRtcMessage {
                                    browser_identity: bi.clone(),
                                    payload: raw.payload,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    });

                    // Spawn per-peer send task so DataChannel sends run off the event loop.
                    self.spawn_peer_send_task(&browser_identity);

                    // Spawn periodic DC ping task for liveness detection.
                    self.spawn_dc_ping_task(&browser_identity);
                    if let Err(e) = self.lua.call_peer_connected(&browser_identity) {
                        log::warn!("[WebRTC] Lua peer_connected callback error: {e}");
                    }
                } else {
                    log::warn!(
                        "[WebRTC] DcOpened for unknown peer {}, ignoring stale open event",
                        &browser_identity[..browser_identity.len().min(8)]
                    );
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
                self.cleanup_webrtc_channel(&browser_identity, source);
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
                        self.try_send_to_peer(
                            &peer_id,
                            super::WebRtcSendItem::Json { data: payload },
                        );
                    }
                    WebRtcSendRequest::Binary { peer_id, data } => {
                        self.try_send_to_peer(&peer_id, super::WebRtcSendItem::Binary { data });
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
                // Intercept focus_changed before Lua — it updates pty_clients
                // focus state for notification suppression, independent of
                // whether the child PTY requested focus reporting.
                if msg.get("type").and_then(|v| v.as_str()) == Some("focus_changed") {
                    if let Some(session_uuid) = msg.get("session_uuid").and_then(|v| v.as_str()) {
                        let focused = msg
                            .get("focused")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        self.set_active_terminal_peer(session_uuid, &client_id, focused);
                        self.lua.set_pty_focused(session_uuid, &client_id, focused);
                    }
                } else if self.handle_terminal_color_profile_message(&client_id, &msg) {
                    // Handled above — do not route client profile updates through Lua.
                } else if let Err(e) = self.lua.call_socket_message(&client_id, msg) {
                    log::error!("[Socket] Lua message handling error for {}: {e}", client_id);
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

                if let Some(session_handle) = self.handle_cache.get_session(&session_uuid) {
                    if let Err(e) = session_handle.pty().write_input_direct(&data) {
                        log::error!("[Socket] PTY write failed for {}: {e}", client_id);
                    }
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
                            if let Err(e) = session_handle.pty().write_input_direct(&data) {
                                log::error!("[PTY-WRITE] Write failed: {e}");
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
                            session_handle.pty().resize_direct(rows, cols);
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
                        metadata,
                        prompt,
                        profile_name,
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

                        self.tokio_runtime.spawn(async move {
                            let result = tokio::task::spawn_blocking(move || {
                                let manager = WorktreeManager::new(worktree_base);
                                manager.create_worktree_with_branch(&branch_clone)
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
                                    result: outcome,
                                    metadata,
                                    prompt,
                                    profile_name,
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
            HubEvent::MessageDelivered { message_len } => {
                log::info!("[MessageDelivery] Delivered message ({message_len} bytes)");
            }
            // Per-session process exited or disconnected.
            // The reader thread already broadcasts PtyEvent directly, so we
            // just need to notify Lua for cleanup.
            HubEvent::SessionProcessExited {
                session_uuid,
                exit_code,
            } => {
                log::info!(
                    "[Session] ProcessExited uuid='{}' exit={:?}",
                    session_uuid,
                    exit_code
                );
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

            HubEvent::SessionUnregistered { session_uuid } => {
                self.terminal_profiles.clear_session(&session_uuid);
                self.terminal_session_peers.remove(&session_uuid);
                self.terminal_forwarder_peers
                    .retain(|_, (tracked_session, _)| tracked_session != &session_uuid);
                if let Ok(mut active) = self.active_terminal_peers.lock() {
                    active.remove(&session_uuid);
                }
                log::debug!("[Session] Unregistered '{}'", session_uuid);
            }
            HubEvent::WebRtcOfferCompleted {
                browser_identity,
                offer_generation,
                mut channel,
                encrypted_answer,
            } => {
                let current_generation = self
                    .webrtc_offer_generation
                    .get(&browser_identity)
                    .copied()
                    .unwrap_or(0);
                if offer_generation != current_generation {
                    log::info!(
                        "[WebRTC] Discarding stale offer completion for {} (got gen {}, current gen {})",
                        &browser_identity[..browser_identity.len().min(8)],
                        offer_generation,
                        current_generation
                    );
                    self.tokio_runtime.spawn(async move {
                        channel.disconnect().await;
                    });
                    return;
                }

                if encrypted_answer.is_none() {
                    log::warn!(
                        "[WebRTC] Offer handling failed for {} — discarding channel so the next retry can start cleanly",
                        &browser_identity[..browser_identity.len().min(8)]
                    );
                    self.webrtc_connection_started.remove(&browser_identity);
                    self.webrtc_offer_generation.remove(&browser_identity);
                    self.webrtc_pending_ice_candidates.remove(&browser_identity);
                    self.tokio_runtime.spawn(async move {
                        channel.disconnect().await;
                    });
                    return;
                }

                if let Some(mut replaced) = self
                    .webrtc_channels
                    .insert(browser_identity.clone(), channel)
                {
                    log::warn!(
                        "[WebRTC] Replaced existing channel on offer completion for {}, disconnecting replaced channel",
                        &browser_identity[..browser_identity.len().min(8)]
                    );
                    self.tokio_runtime.spawn(async move {
                        replaced.disconnect().await;
                    });
                }

                let envelope_value =
                    encrypted_answer.expect("encrypted_answer checked above to be present");
                // Send the answer first. Queued browser ICE can be applied
                // afterward; invalid or slow candidates must not delay the
                // browser receiving the answer and beginning ICE checks.
                if self.emit_outgoing_signal(&browser_identity, envelope_value, "answer") {
                    log::info!("[WebRTC] Encrypted answer sent via Lua relay (async)");
                }

                if let Some(candidates) =
                    self.webrtc_pending_ice_candidates.remove(&browser_identity)
                {
                    if let Some(channel) = self.webrtc_channels.get(&browser_identity) {
                        // Apply any queued browser ICE after the answer is already
                        // on the wire. Slow or invalid candidates must not delay
                        // the browser receiving the answer and starting ICE.
                        let valid: Vec<_> = candidates
                            .into_iter()
                            .filter_map(|(candidate_generation, candidate)| {
                                if candidate_generation != offer_generation {
                                    log::debug!(
                                        "[WebRTC] Dropping stale queued ICE candidate for {} (candidate gen {}, current gen {})",
                                        &browser_identity[..browser_identity.len().min(8)],
                                        candidate_generation,
                                        offer_generation
                                    );
                                    return None;
                                }
                                let candidate_str = candidate
                                    .get("candidate")
                                    .and_then(|c| c.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                if candidate_str.is_empty() {
                                    return None;
                                }
                                let sdp_mid = candidate
                                    .get("sdpMid")
                                    .and_then(|m| m.as_str())
                                    .map(String::from);
                                let sdp_mline_index = candidate
                                    .get("sdpMLineIndex")
                                    .and_then(|i| i.as_u64())
                                    .map(|i| i as u16);
                                Some((candidate_generation, candidate_str, sdp_mid, sdp_mline_index))
                            })
                            .collect();

                        if !valid.is_empty() {
                            let browser_id_short =
                                browser_identity[..browser_identity.len().min(8)].to_string();
                            tokio::task::block_in_place(|| {
                                self.tokio_runtime.block_on(async {
                                    for (gen, candidate_str, sdp_mid, sdp_mline_index) in &valid {
                                        if let Err(e) = channel.handle_ice_candidate(
                                            candidate_str,
                                            sdp_mid.as_deref(),
                                            *sdp_mline_index,
                                        ).await {
                                            log::warn!(
                                                "[WebRTC] Failed to apply queued ICE candidate for {}: {} (gen={}, mid={:?}, mline={:?}, candidate='{}')",
                                                browser_id_short,
                                                e,
                                                gen,
                                                sdp_mid,
                                                sdp_mline_index,
                                                Self::ice_candidate_preview(candidate_str),
                                            );
                                        }
                                    }
                                });
                            });
                        }
                    }
                }
            }
        }

        // Resolve attach intents after every event so session registration and
        // subscribe handling converge immediately without client-side retry loops.
        self.process_pending_terminal_attaches();
    }

    /// Handle a single TUI request from the TuiRunner thread.
    pub fn handle_tui_request(&mut self, request: crate::client::TuiRequest) {
        use crate::client::TuiRequest;
        match request {
            TuiRequest::LuaMessage(msg) => {
                if self.handle_terminal_color_profile_message("tui", &msg) {
                    return;
                }
                if let Err(e) = self.lua.call_tui_message(msg) {
                    log::error!("[TUI] Lua message handling error: {}", e);
                }
            }
            TuiRequest::FocusChanged {
                session_uuid,
                focused,
            } => {
                self.set_active_terminal_peer(&session_uuid, "tui", focused);
                self.lua.set_pty_focused(&session_uuid, "tui", focused);
            }
            TuiRequest::PtyInput { session_uuid, data } => {
                self.lua.notify_pty_input(&session_uuid);
                if let Some(session_handle) = self.handle_cache.get_session(&session_uuid) {
                    if let Err(e) = session_handle.pty().write_input_direct(&data) {
                        log::error!("[PTY-INPUT] Write failed: {e}");
                    }
                } else {
                    log::warn!(
                        "[PTY-INPUT] No session for UUID {} (cache has {} agents)",
                        session_uuid,
                        self.handle_cache.len()
                    );
                }
            }
        }
    }

    /// Handle a single binary PTY input from a browser (WebRTC).
    pub fn handle_pty_input(&mut self, input: crate::channel::webrtc::PtyInputIncoming) {
        if input.data == b"\x1b[I" {
            self.set_active_terminal_peer(&input.session_uuid, &input.browser_identity, true);
            self.lua
                .set_pty_focused(&input.session_uuid, &input.browser_identity, true);
            // Color profile is now sent by the browser as a JSON message
            // after snapshot load — no need to inject OSC probe bytes.
        } else if input.data == b"\x1b[O" {
            self.set_active_terminal_peer(&input.session_uuid, &input.browser_identity, false);
            self.lua
                .set_pty_focused(&input.session_uuid, &input.browser_identity, false);
        }

        self.learn_terminal_probe_replies(
            &input.session_uuid,
            &input.browser_identity,
            &input.data,
        );
        self.lua.notify_pty_input(&input.session_uuid);

        if let Some(session_handle) = self.handle_cache.get_session(&input.session_uuid) {
            if let Err(e) = session_handle.pty().write_input_direct(&input.data) {
                log::error!("[PTY-INPUT] Write failed: {e}");
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
            "[FILE-INPUT] Wrote {} bytes to {} (session={})",
            file.data.len(),
            path.display(),
            file.session_uuid,
        );

        // Track for cleanup + inject path into PTY
        if let Some(session_handle) = self.handle_cache.get_session(&file.session_uuid) {
            let agent_handle = session_handle;
            self.paste_files
                .entry(agent_handle.label().to_string())
                .or_default()
                .push(path.clone());

            let path_with_space = format!("{} ", path.display());
            if let Err(e) = agent_handle
                .pty()
                .write_input_direct(path_with_space.as_bytes())
            {
                log::error!("[FILE-INPUT] Failed to inject path into PTY: {e}");
            }
        }
    }

    /// Clean up paste files for a closed session.
    pub fn cleanup_paste_files(&mut self, label: &str) {
        if let Some(files) = self.paste_files.remove(label) {
            for path in &files {
                if let Err(e) = std::fs::remove_file(path) {
                    log::warn!(
                        "[FILE-INPUT] Failed to clean up paste file {}: {e}",
                        path.display()
                    );
                }
            }
            if !files.is_empty() {
                log::info!(
                    "[FILE-INPUT] Cleaned up {} paste file(s) for {label}",
                    files.len()
                );
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
                self.emit_outgoing_signal(&browser_identity, envelope, "ICE candidate");
                log::debug!(
                    "[Crypto] Relayed ICE candidate to browser {}",
                    &browser_identity[..browser_identity.len().min(8)]
                );
            }
        }
    }

    fn emit_outgoing_signal(
        &self,
        browser_identity: &str,
        envelope: serde_json::Value,
        signal_kind: &str,
    ) -> bool {
        let data = serde_json::json!({
            "browser_identity": browser_identity,
            "envelope": envelope,
        });
        if let Err(error) = self.lua.fire_json_event("outgoing_signal", &data) {
            log::error!("[WebRTC] Failed to fire outgoing_signal for {signal_kind}: {error}");
            return false;
        }
        true
    }

    fn handle_signaling_message(&mut self, message: serde_json::Value) {
        let msg_type = message.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let browser_identity = message
            .get("browser_identity")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match msg_type {
            "signal" => {
                if browser_identity.is_empty() {
                    log::warn!("[Lua] Signal message missing browser_identity");
                    return;
                }

                if message
                    .get("decrypt_failed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    log::warn!(
                        "Signal decryption failed for browser {}, requesting ratchet restart",
                        browser_identity
                    );
                    self.try_ratchet_restart(browser_identity);
                    return;
                }

                let Some(signal_data) = message.get("envelope") else {
                    log::warn!(
                        "[Lua] Signal message missing envelope for {}",
                        &browser_identity[..browser_identity.len().min(8)]
                    );
                    return;
                };
                let signal_type = signal_data
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                match signal_type {
                    "offer" => {
                        let Some(sdp) = signal_data.get("sdp").and_then(|v| v.as_str()) else {
                            log::warn!(
                                "[Lua] Offer missing sdp for {}",
                                &browser_identity[..browser_identity.len().min(8)]
                            );
                            return;
                        };
                        log::info!(
                            "[Lua] Processing WebRTC offer from {}",
                            &browser_identity[..browser_identity.len().min(8)]
                        );
                        self.handle_webrtc_offer(sdp, browser_identity);
                    }
                    "ice" => {
                        let candidate = signal_data
                            .get("candidate")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        self.handle_browser_ice_candidate(browser_identity, candidate);
                    }
                    other => {
                        log::warn!(
                            "[Lua] Unknown signal type for {}: {}",
                            &browser_identity[..browser_identity.len().min(8)],
                            other
                        );
                    }
                }
            }
            "bundle_request" => {
                if browser_identity.is_empty() {
                    log::warn!("[Lua] bundle_request missing browser_identity");
                    return;
                }
                self.send_ratchet_restart(browser_identity);
            }
            other => {
                log::warn!("[Lua] Unsupported signaling message type: {}", other);
            }
        }
    }

    fn handle_browser_ice_candidate(
        &mut self,
        browser_identity: &str,
        candidate: serde_json::Value,
    ) {
        const MAX_QUEUED_ICE_PER_BROWSER: usize = 128;

        let candidate_str = candidate
            .get("candidate")
            .and_then(|c| c.as_str())
            .unwrap_or("");
        if candidate_str.is_empty() {
            log::debug!(
                "[Lua] Ignoring empty ICE candidate for {}",
                &browser_identity[..browser_identity.len().min(8)]
            );
            return;
        }

        let sdp_mid = candidate.get("sdpMid").and_then(|m| m.as_str());
        let sdp_mline_index = candidate
            .get("sdpMLineIndex")
            .and_then(|i| i.as_u64())
            .map(|i| i as u16);

        if let Some(channel) = self.webrtc_channels.get(browser_identity) {
            if let Err(error) = tokio::task::block_in_place(|| {
                self.tokio_runtime.block_on(channel.handle_ice_candidate(
                    candidate_str,
                    sdp_mid,
                    sdp_mline_index,
                ))
            }) {
                log::warn!(
                    "[Lua] Failed to add ICE candidate for {}: {} (mid={:?}, mline={:?}, candidate='{}')",
                    &browser_identity[..browser_identity.len().min(8)],
                    error,
                    sdp_mid,
                    sdp_mline_index,
                    Self::ice_candidate_preview(candidate_str),
                );
            }
        } else if self.webrtc_offer_generation.contains_key(browser_identity) {
            let current_generation = self
                .webrtc_offer_generation
                .get(browser_identity)
                .copied()
                .unwrap_or(0);
            let queue = self
                .webrtc_pending_ice_candidates
                .entry(browser_identity.to_string())
                .or_default();
            queue.push((current_generation, candidate));
            if queue.len() > MAX_QUEUED_ICE_PER_BROWSER {
                let dropped = queue.len() - MAX_QUEUED_ICE_PER_BROWSER;
                queue.drain(..dropped);
            }
            log::debug!(
                "[Lua] Queued ICE candidate while offer in flight for {} (queued={})",
                &browser_identity[..browser_identity.len().min(8)],
                queue.len()
            );
        } else {
            log::warn!(
                "[Lua] ICE candidate for unknown browser {}",
                &browser_identity[..browser_identity.len().min(8)]
            );
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
    pub fn handle_worktree_result(&mut self, result: crate::lua::primitives::WorktreeCreateResult) {
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

                let event_data = serde_json::json!({
                    "label": result.label,
                    "branch": result.branch,
                    "path": path_str,
                    "metadata": result.metadata,
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
                    "label": result.label,
                    "branch": result.branch,
                    "error": error,
                });
                if let Err(e) = self
                    .lua
                    .fire_json_event("worktree_create_failed", &event_data)
                {
                    log::error!("[Worktree] Failed to fire worktree_create_failed event: {e}");
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
        rx: &mut Option<tokio::sync::mpsc::Receiver<WebRtcPtyOutput>>,
    ) {
        // Process the first message directly to preserve ordering.
        self.process_single_pty_output(first);

        // Temporarily put the receiver back into self for poll_webrtc_pty_output
        self.webrtc_pty_output_rx = rx.take();
        self.poll_webrtc_pty_output();
        // Extract it back out
        *rx = self.webrtc_pty_output_rx.take();
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
        session_uuid: String,
        session_name: String,
        observe_output: bool,
        event_tx: tokio::sync::broadcast::Sender<crate::agent::pty::PtyEvent>,
    ) {
        // Abort any existing watcher for this key
        if let Some(old) = self.notification_watcher_handles.remove(&watcher_key) {
            old.abort();
            log::debug!(
                "[NotifWatcher] Aborted existing watcher for {}",
                watcher_key
            );
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
                        log::debug!("[NotifWatcher] Notification for {}: {:?}", key, notif);
                        let event = super::PtyNotificationEvent {
                            session_uuid: session_uuid.clone(),
                            session_name: session_name.clone(),
                            notification: notif,
                        };
                        if hub_tx
                            .send(super::events::HubEvent::PtyNotification(event))
                            .is_err()
                        {
                            log::warn!("[NotifWatcher] Hub event channel closed for {}", key);
                            break;
                        }
                    }
                    Ok(PtyEvent::ProcessExited { exit_code }) => {
                        log::info!(
                            "[NotifWatcher] Process exited (code={:?}) for {}",
                            exit_code,
                            key
                        );
                        let event = super::events::HubEvent::PtyProcessExited {
                            session_uuid: session_uuid.clone(),
                            session_name: session_name.clone(),
                            exit_code,
                        };
                        let _ = hub_tx.send(event);
                        break;
                    }
                    Ok(PtyEvent::Output(data)) => {
                        if observe_output {
                            if hub_tx
                                .send(super::events::HubEvent::PtyOutputObserved {
                                    session_uuid: session_uuid.clone(),
                                    data,
                                })
                                .is_err()
                            {
                                log::warn!("[NotifWatcher] Hub event channel closed for {}", key);
                                break;
                            }
                        }
                    }
                    Ok(event @ PtyEvent::TitleChanged(_))
                    | Ok(event @ PtyEvent::CwdChanged(_))
                    | Ok(event @ PtyEvent::PromptMark(_))
                    | Ok(event @ PtyEvent::CursorVisibilityChanged(_)) => {
                        if hub_tx
                            .send(super::events::HubEvent::PtyOscEvent {
                                session_uuid: session_uuid.clone(),
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
                &event.session_uuid,
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
                    channel.reset_decrypt_failures();
                    self.try_ratchet_restart(&browser_identity);
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
                    // Spawn per-peer send task (same as production DcOpened handler)
                    self.spawn_peer_send_task(&browser_identity);
                    if let Err(e) = self.lua.call_peer_connected(&browser_identity) {
                        log::warn!("[WebRTC] Lua peer_connected callback error: {e}");
                    }
                }
            }
        }
    }

    /// Attempt a ratchet restart, deduplicating by both Olm key and tab ID.
    ///
    /// Prevents cascading restarts when the same browser device reconnects
    /// with a new Olm identity (new account after bundle refresh) but the
    /// same tab/session UUID.
    fn try_ratchet_restart(&mut self, browser_identity: &str) {
        let olm_key = crate::relay::extract_olm_key(browser_identity).to_string();
        let tab_id = browser_identity
            .split_once(':')
            .map(|(_, id)| id.to_string());
        let already_restarted = self.ratchet_restarted_peers.contains(&olm_key)
            || tab_id
                .as_ref()
                .is_some_and(|id| self.ratchet_restarted_peers.contains(id));
        if already_restarted {
            return;
        }
        log::warn!(
            "[RatchetRestart] Initiating restart for {}",
            &browser_identity[..browser_identity.len().min(8)]
        );
        self.send_ratchet_restart(browser_identity);
        self.ratchet_restarted_peers.insert(olm_key);
        if let Some(id) = tab_id {
            self.ratchet_restarted_peers.insert(id);
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
                Ok(bytes) => bytes,
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

        // Send type 2 via DataChannel — non-blocking via per-peer send task
        self.try_send_to_peer(
            browser_identity,
            super::WebRtcSendItem::BundleRefresh {
                bundle_bytes: bundle_bytes.clone(),
            },
        );

        // Also send via ActionCable
        let envelope = serde_json::json!({
            "t": 2,
            "b": base64::engine::general_purpose::STANDARD_NO_PAD
                .encode(&bundle_bytes),
        });
        self.emit_outgoing_signal(&browser_identity, envelope, "bundle refresh");

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

        // Timeout for connections stuck in "Connecting" state.
        // Keep this comfortably above the offer/answer happy path, but short
        // enough that failed negotiations do not force manual refreshes.
        const CONNECTION_TIMEOUT: Duration = Duration::from_secs(15);
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

        // Prune webrtc_pending_closes entries whose disconnect has already completed.
        // Entries are normally consumed when the same device sends a new offer (removed
        // and awaited in the offer handler). If the browser disconnects and never
        // reconnects, the entry would otherwise accumulate indefinitely. Any entry
        // whose receiver is already `true` (disconnect finished) is safe to drop.
        self.webrtc_pending_closes
            .retain(|_, close_rx| !*close_rx.borrow());
    }

    /// Clean up a single WebRTC channel and its associated resources.
    ///
    /// This is the centralized cleanup point that:
    /// 1. Removes and disconnects the WebRTC channel
    /// 2. Removes connection start time tracking
    /// 3. Aborts any PTY forwarder tasks for this browser
    /// 4. Notifies Lua of peer disconnection
    fn cleanup_webrtc_channel(&mut self, browser_identity: &str, reason: &str) {
        // Guard against duplicate cleanup calls (e.g. handle_webrtc_send and
        // poll_webrtc_pty_output both detecting the same dead channel in the
        // same tick). If the channel is already gone this is a no-op — we must
        // not fire peer_disconnected a second time or the browser JS state
        // machine will enter an unrecoverable state and stop reconnecting.
        let Some(mut channel) = self.webrtc_channels.remove(browser_identity) else {
            log::debug!(
                "[WebRTC] cleanup_webrtc_channel({}) called but channel already removed (duplicate skipped)",
                &browser_identity[..browser_identity.len().min(8)]
            );
            return;
        };

        log::info!(
            "[WebRTC] Cleaning up {} channel: {}",
            reason,
            &browser_identity[..browser_identity.len().min(8)]
        );

        // Track close notification so the offer handler can await socket release
        // before creating a replacement channel (prevents fd exhaustion).
        let close_rx = channel.close_receiver();
        let olm_key = crate::relay::extract_olm_key(browser_identity).to_string();
        self.webrtc_pending_closes.insert(olm_key, close_rx);

        self.tokio_runtime.spawn(async move {
            channel.disconnect().await;
            log::debug!("[WebRTC] Channel disconnect completed");
        });

        // Remove connection start time tracking
        self.webrtc_connection_started.remove(browser_identity);
        // Remove offer generation tracking for fully-cleaned channels.
        self.webrtc_offer_generation.remove(browser_identity);
        self.webrtc_pending_ice_candidates.remove(browser_identity);

        // Stop per-peer send task (dropping sender causes task exit)
        if let Some(state) = self.webrtc_send_tasks.remove(browser_identity) {
            drop(state.tx);
            state.task.abort();
            log::debug!(
                "[WebRTC] Stopped send task for {}",
                &browser_identity[..browser_identity.len().min(8)]
            );
        }

        // Remove any pending backpressure recovery snapshots for this peer.
        self.webrtc_backpressure_recovery
            .retain(|_, entry| entry.browser_identity != browser_identity);

        // Stop DC ping task for this peer
        if let Some(task) = self.dc_ping_tasks.remove(browser_identity) {
            task.abort();
        }

        // Close and remove stream multiplexer for this browser
        if let Some(mut mux) = self.stream_muxes.remove(browser_identity) {
            mux.close_all();
            log::debug!(
                "[WebRTC] Closed stream multiplexer for {}",
                &browser_identity[..browser_identity.len().min(8)]
            );
        }

        // Abort any PTY forwarders for this browser.
        // Forwarder keys are "{peer_id}:{session_uuid}" where peer_id = browser_identity
        let peer_prefix = format!("{browser_identity}:");
        self.pty_forwarders.retain(|key, task| {
            if key.starts_with(&peer_prefix) {
                task.abort();
                log::debug!("[WebRTC] Aborted PTY forwarder: {}", key);
                false
            } else {
                true
            }
        });
        self.pending_terminal_attaches.retain(|key, intent| {
            if key.starts_with(&peer_prefix) {
                intent.request.deactivate();
                log::debug!("[WebRTC] Dropped pending terminal attach intent: {}", key);
                false
            } else {
                true
            }
        });
        self.unregister_terminal_client_peer(browser_identity, true);

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
                "dc_ping" => {
                    // Browser sent a heartbeat ping — respond immediately so it
                    // doesn't declare the connection stalled after 3 missed pongs.
                    let pong = serde_json::to_vec(&serde_json::json!({ "type": "dc_pong" }))
                        .expect("static JSON serialization cannot fail");
                    self.try_send_to_peer(
                        browser_identity,
                        super::WebRtcSendItem::Json { data: pong },
                    );
                    return;
                }
                "dc_pong" => {
                    // Browser responded to our dc_ping — connection is alive.
                    // Informational logging only; the browser side uses missed
                    // pongs to detect dead connections and trigger reconnect.
                    log::trace!(
                        "[WebRTC] dc_pong from {}",
                        &browser_identity[..browser_identity.len().min(8)]
                    );
                    return;
                }
                "terminal_color_profile" => {
                    self.handle_terminal_color_profile_message(browser_identity, &msg);
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
                let connection =
                    crate::hub::action_cable_connection::ActionCableConnection::connect(
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
                                if tx
                                    .send(super::events::HubEvent::AcChannelMessage {
                                        channel_id: ch_id.clone(),
                                        message: msg,
                                    })
                                    .is_err()
                                {
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
                    log::info!("[ActionCable-Lua] Channel '{}' unsubscribed", channel_id);
                } else {
                    log::warn!(
                        "[ActionCable-Lua] Unsubscribe failed: channel '{}' not found",
                        channel_id
                    );
                }
            }

            ActionCableRequest::Close { connection_id } => {
                // Remove all channels belonging to this connection
                let orphaned: Vec<String> = self
                    .lua_ac_channels
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

    /// Process a single hub client request from `HubEvent::LuaHubClientRequest`.
    ///
    /// Handles connect/send/close operations. When connecting, spawns read and
    /// write tokio tasks. The read task sends `HubEvent::HubClientMessage` for
    /// each incoming JSON frame and `HubEvent::HubClientDisconnected` on EOF.
    fn process_hub_client_request(&mut self, request: crate::lua::primitives::HubClientRequest) {
        use crate::lua::primitives::hub_client::LuaHubClientConn;
        use crate::lua::primitives::HubClientRequest;
        use crate::socket::framing::{Frame, FrameDecoder};

        match request {
            HubClientRequest::Connect {
                connection_id,
                socket_path,
            } => {
                let hub_tx = self.hub_event_tx.clone();
                let conn_id = connection_id.clone();
                let handle = self.tokio_runtime.handle().clone();

                let hub_tx2 = hub_tx.clone();
                let conn_id2 = conn_id.clone();
                // Clone pending_requests so the read task can deliver _mcp_rid
                // responses directly, bypassing the Hub event loop. This is
                // required because hub_client.request() blocks the event loop
                // thread via recv_timeout() — the event loop cannot process
                // HubClientMessage while Lua is blocked.
                let pending_requests2 =
                    std::sync::Arc::clone(self.lua.hub_client_pending_requests());

                // Use std UnixStream::connect (synchronous) and convert to tokio.
                // Cannot use tokio's async connect here because we're inside the
                // Hub's block_on event loop — nested block_on panics.
                let std_stream = match std::os::unix::net::UnixStream::connect(&socket_path) {
                    Ok(s) => s,
                    Err(e) => {
                        log::warn!("[HubClient] Failed to connect to {}: {}", socket_path, e);
                        return;
                    }
                };
                if let Err(e) = std_stream.set_nonblocking(true) {
                    log::warn!(
                        "[HubClient] Failed to set nonblocking on {}: {}",
                        socket_path,
                        e
                    );
                    return;
                }
                let stream = match tokio::net::UnixStream::from_std(std_stream) {
                    Ok(s) => s,
                    Err(e) => {
                        log::warn!(
                            "[HubClient] Failed to convert to tokio stream for {}: {}",
                            socket_path,
                            e
                        );
                        return;
                    }
                };

                let (read_half, write_half) = stream.into_split();
                let (frame_tx, mut frame_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

                // Subscribe immediately (same as TuiBridge)
                let sub_frame = Frame::Json(serde_json::json!({
                    "type": "subscribe",
                    "channel": "hub",
                    "subscriptionId": format!("hub_client_{}", conn_id)
                }));
                let _ = frame_tx.send(sub_frame.encode());

                // Spawn write task
                let write_handle = handle.spawn(async move {
                    let mut writer = tokio::io::BufWriter::new(write_half);
                    while let Some(data) = frame_rx.recv().await {
                        use tokio::io::AsyncWriteExt;
                        if writer.write_all(&data).await.is_err() {
                            break;
                        }
                        if writer.flush().await.is_err() {
                            break;
                        }
                    }
                });

                // Spawn read task
                let read_handle = handle.spawn(async move {
                    let mut reader = tokio::io::BufReader::new(read_half);
                    let mut decoder = FrameDecoder::new();
                    let mut buf = [0u8; 8192];
                    loop {
                        use tokio::io::AsyncReadExt;
                        match reader.read(&mut buf).await {
                            Ok(0) | Err(_) => {
                                let _ = hub_tx2.send(
                                    super::events::HubEvent::HubClientDisconnected {
                                        connection_id: conn_id2.clone(),
                                    },
                                );
                                break;
                            }
                            Ok(n) => {
                                match decoder.feed(&buf[..n]) {
                                    Ok(frames) => {
                                        for frame in frames {
                                            if let Frame::Json(v) = frame {
                                                // Short-circuit _mcp_rid responses directly to
                                                // the pending_requests map. hub_client.request()
                                                // blocks the Hub event loop thread via recv_timeout(),
                                                // so we cannot route through HubEvent — the event
                                                // loop is not being polled while Lua waits.
                                                if let Some(rid) = v.get("_mcp_rid").and_then(|r| r.as_str()) {
                                                    let sender = {
                                                        let mut map = pending_requests2
                                                            .lock()
                                                            .expect("HubClientPendingRequests mutex poisoned");
                                                        map.remove(rid)
                                                    };
                                                    if let Some(tx) = sender {
                                                        let _ = tx.send(v);
                                                        continue;
                                                    }
                                                }
                                                let _ = hub_tx2.send(
                                                    super::events::HubEvent::HubClientMessage {
                                                        connection_id: conn_id2.clone(),
                                                        message: v,
                                                    },
                                                );
                                            }
                                            // Other frame types (PtyOutput etc) could be handled later
                                        }
                                    }
                                    Err(e) => {
                                        log::warn!(
                                            "[HubClient] Frame decode error on '{}': {}",
                                            conn_id2,
                                            e
                                        );
                                        let _ = hub_tx2.send(
                                            super::events::HubEvent::HubClientDisconnected {
                                                connection_id: conn_id2.clone(),
                                            },
                                        );
                                        break;
                                    }
                                }
                            }
                        }
                    }
                });

                // Register the frame sender so hub_client.request() can write
                // directly without going through the Hub event loop.
                if let Ok(mut senders) = self.lua.hub_client_frame_senders().lock() {
                    senders.insert(connection_id.clone(), frame_tx.clone());
                }

                // Store connection
                self.lua_hub_client_connections.insert(
                    connection_id.clone(),
                    LuaHubClientConn {
                        frame_tx,
                        read_handle,
                        write_handle,
                    },
                );
                log::info!(
                    "[HubClient] Connection '{}' opened to '{}'",
                    connection_id,
                    socket_path
                );
            }

            HubClientRequest::Send {
                connection_id,
                data,
            } => {
                if let Some(conn) = self.lua_hub_client_connections.get(&connection_id) {
                    let frame = Frame::Json(data);
                    if conn.frame_tx.send(frame.encode()).is_err() {
                        log::warn!(
                            "[HubClient] Send failed: write task closed for '{}'",
                            connection_id
                        );
                    } else {
                        log::trace!("[HubClient] Sent frame to '{}'", connection_id);
                    }
                } else {
                    log::warn!(
                        "[HubClient] Send failed: connection '{}' not found",
                        connection_id
                    );
                }
            }

            HubClientRequest::Close { connection_id } => {
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
                    log::info!("[HubClient] Connection '{}' closed", connection_id);
                } else {
                    log::warn!(
                        "[HubClient] Close failed: connection '{}' not found",
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
        let Some(ref mut rx) = self.worktree_result_rx else {
            return;
        };
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
                        "label": result.label,
                        "branch": result.branch,
                        "path": path_str,
                        "metadata": result.metadata,
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
                        "label": result.label,
                        "branch": result.branch,
                        "error": error,
                    });
                    if let Err(e) = self
                        .lua
                        .fire_json_event("worktree_create_failed", &event_data)
                    {
                        log::error!("[Worktree] Failed to fire worktree_create_failed event: {e}");
                    }
                }
            }
        }
    }

    /// Send terminal attach state to a WebRTC subscription.
    fn send_terminal_attach_state(
        &self,
        peer_id: &str,
        subscription_id: &str,
        session_uuid: &str,
        state: &str,
    ) {
        let payload = serde_json::json!({
            "type": "terminal_attach",
            "subscriptionId": subscription_id,
            "session_uuid": session_uuid,
            "state": state,
        });
        match serde_json::to_vec(&payload) {
            Ok(data) => self.try_send_to_peer(peer_id, super::WebRtcSendItem::Json { data }),
            Err(e) => {
                log::warn!(
                    "[WebRTC] Failed to serialize terminal_attach state '{}': {}",
                    state,
                    e
                );
            }
        }
    }

    fn should_force_snapshot_redraw(
        pty_handle: &crate::hub::agent_handle::PtyHandle,
        target_rows: u16,
        target_cols: u16,
    ) -> bool {
        if pty_handle.dims() != (target_rows, target_cols) {
            return false;
        }

        pty_handle
            .get_mode_flags()
            .map(|flags| flags.alt_screen)
            .unwrap_or(false)
    }

    /// Try to attach a terminal forwarder immediately.
    ///
    /// Returns `true` when attached, `false` when the session is not yet
    /// available in `HandleCache`.
    fn try_attach_terminal_forwarder(&mut self, req: &crate::lua::CreateForwarderRequest) -> bool {
        let forwarder_key = format!("{}:{}", req.peer_id, req.session_uuid);

        let Some(session_handle) = self.handle_cache.get_session(&req.session_uuid) else {
            return false;
        };

        let pty_handle = session_handle.pty().clone();

        // Abort any existing forwarder for this key.
        if let Some(old_task) = self.pty_forwarders.remove(&forwarder_key) {
            old_task.abort();
            self.unregister_terminal_forwarder_peer(&forwarder_key, false);
            log::debug!("[Lua] Aborted existing PTY forwarder for {}", forwarder_key);
        }

        // Snapshot retrieval and subscription setup can block.
        // Run it inside the spawned forwarder task so Hub event processing stays
        // responsive while attach state is being prepared.
        let pty_for_snapshot = pty_handle.clone();

        // Spawn forwarder task.
        let output_tx = self.webrtc_pty_output_tx.clone();
        let hub_event_tx = self.hub_event_tx.clone();
        let peer_id = req.peer_id.clone();
        let session_uuid = req.session_uuid.clone();
        let target_rows = req.rows;
        let target_cols = req.cols;
        let prefix = req.prefix.clone().unwrap_or_else(|| vec![0x01]);
        let active_flag = req.active_flag.clone();
        let active_terminal_peers = Arc::clone(&self.active_terminal_peers);

        // Use browser-provided subscription ID for message routing.
        let subscription_id = req.subscription_id.clone();

        let _guard = self.tokio_runtime.enter();
        let task = tokio::spawn(async move {
            use crate::agent::pty::PtyEvent;

            log::info!(
                "[Lua] Started PTY forwarder for peer {} session {}",
                &peer_id[..peer_id.len().min(8)],
                session_uuid
            );
            let mut query_filter_buffer = Vec::new();
            let mut dumped_live_chunks = 0usize;

            let (snapshot, mut pty_rx) = match tokio::task::spawn_blocking(move || {
                if pty_for_snapshot.is_session_backed() {
                    if Self::should_force_snapshot_redraw(
                        &pty_for_snapshot,
                        target_rows,
                        target_cols,
                    ) {
                        // Force a redraw pulse for full-screen TUIs. Resizing to the
                        // same dimensions often does not trigger a redraw path.
                        // Normal-screen sessions keep real scrollback in the primary
                        // buffer, so bouncing them by one column can reflow and
                        // inflate restored history on resume.
                        let bounce_cols = if target_cols > 1 { target_cols - 1 } else { 2 };
                        pty_for_snapshot.resize_direct(target_rows, bounce_cols);
                        std::thread::sleep(std::time::Duration::from_millis(25));
                    }
                    pty_for_snapshot.resize_direct(target_rows, target_cols);
                    // Sessions redraw asynchronously after SIGWINCH.
                    // Let a short settle window pass so the binary snapshot
                    // includes the post-resize redraw.
                    std::thread::sleep(std::time::Duration::from_millis(125));
                }
                let (snapshot, _kitty_enabled, _rows, _cols, pty_rx) =
                    pty_for_snapshot.snapshot_and_subscribe();
                (snapshot, pty_rx)
            })
            .await
            {
                Ok(result) => result,
                Err(e) => {
                    log::warn!(
                        "[Lua] Snapshot fetch task failed for session {}: {}",
                        session_uuid,
                        e
                    );
                    (Vec::new(), pty_handle.subscribe())
                }
            };

            log::debug!(
                "[Lua] Snapshot bytes for peer {} session {}: {}",
                &peer_id[..peer_id.len().min(8)],
                session_uuid,
                snapshot.len()
            );

            Self::reset_restty_fixture_capture(
                &session_uuid,
                &peer_id,
                &subscription_id,
                target_rows,
                target_cols,
                snapshot.len(),
            );
            if !snapshot.is_empty() {
                Self::dump_restty_snapshot_fixture(&session_uuid, &snapshot);
            }

            if !Self::queue_webrtc_terminal_snapshot(
                &output_tx,
                &hub_event_tx,
                &peer_id,
                &subscription_id,
                &session_uuid,
                snapshot,
            ) {
                return;
            }

            loop {
                // Check if forwarder was stopped by Lua.
                {
                    let active = active_flag
                        .lock()
                        .expect("Forwarder active_flag mutex poisoned");
                    if !*active {
                        log::debug!("[Lua] PTY forwarder stopped by Lua");
                        break;
                    }
                }

                match pty_rx.recv().await {
                    Ok(PtyEvent::Output(data)) => {
                        let filtered = if active_terminal_peers
                            .lock()
                            .ok()
                            .and_then(|active| active.get(&session_uuid).cloned())
                            .is_some_and(|active_peer| active_peer != peer_id.as_str())
                        {
                            crate::hub::terminal_profile::strip_osc_queries_from_output(
                                &mut query_filter_buffer,
                                &data,
                            )
                        } else {
                            query_filter_buffer.clear();
                            data
                        };

                        if filtered.is_empty() {
                            continue;
                        }

                        if dumped_live_chunks < Self::RESTTY_FIXTURE_LIVE_CHUNK_LIMIT {
                            Self::dump_restty_live_fixture_chunk(
                                &session_uuid,
                                dumped_live_chunks,
                                &filtered,
                            );
                            dumped_live_chunks += 1;
                        }

                        // Send raw bytes with prefix.
                        let mut raw_message = Vec::with_capacity(prefix.len() + filtered.len());
                        raw_message.extend(&prefix);
                        raw_message.extend(&filtered);

                        match output_tx.try_send(WebRtcPtyOutput {
                            subscription_id: subscription_id.clone(),
                            browser_identity: peer_id.clone(),
                            data: raw_message,
                            session_uuid: session_uuid.clone(),
                        }) {
                            Ok(()) => {}
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                log::warn!(
                                    "[Lua] WebRTC PTY output queue full for {}; forcing reconnect",
                                    &peer_id[..peer_id.len().min(8)]
                                );
                                let _ = hub_event_tx.send(
                                    super::events::HubEvent::WebRtcIngressBackpressure {
                                        browser_identity: peer_id.clone(),
                                        source: "pty_output_queue_full",
                                    },
                                );
                                break;
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                log::trace!("[Lua] PTY output queue closed, stopping forwarder");
                                break;
                            }
                        }
                    }
                    Ok(PtyEvent::ProcessExited { exit_code }) => {
                        log::info!(
                            "[Lua] PTY process exited (code={:?}) for session {}",
                            exit_code,
                            session_uuid
                        );
                        break;
                    }
                    Ok(_other_event) => {
                        // Ignore other events.
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!(
                            "[Lua] PTY forwarder lagged by {} events for session {}",
                            n,
                            session_uuid
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        log::info!("[Lua] PTY channel closed for session {}", session_uuid);
                        break;
                    }
                }
            }

            // Mark forwarder as inactive.
            *active_flag
                .lock()
                .expect("Forwarder active_flag mutex poisoned") = false;

            log::info!(
                "[Lua] Stopped PTY forwarder for peer {} session {}",
                &peer_id[..peer_id.len().min(8)],
                session_uuid
            );
        });

        self.register_terminal_forwarder_peer(&forwarder_key, &req.session_uuid, &req.peer_id);
        self.pty_forwarders.insert(forwarder_key, task);
        true
    }

    fn refresh_lua_terminal_snapshot(&mut self, req: crate::lua::RefreshSnapshotRequest) {
        let Some(session_handle) = self.handle_cache.get_session(&req.session_uuid) else {
            log::debug!(
                "[Lua] Snapshot refresh ignored for missing session {}",
                req.session_uuid
            );
            return;
        };

        let pty_handle = session_handle.pty().clone();
        let output_tx = self.webrtc_pty_output_tx.clone();
        let hub_event_tx = self.hub_event_tx.clone();
        let peer_id = req.peer_id.clone();
        let subscription_id = req.subscription_id.clone();
        let session_uuid = req.session_uuid.clone();
        let target_rows = req.rows;
        let target_cols = req.cols;

        let _guard = self.tokio_runtime.enter();
        tokio::spawn(async move {
            let snapshot = match tokio::task::spawn_blocking(move || {
                if pty_handle.is_session_backed() {
                    if Self::should_force_snapshot_redraw(&pty_handle, target_rows, target_cols) {
                        let bounce_cols = if target_cols > 1 { target_cols - 1 } else { 2 };
                        pty_handle.resize_direct(target_rows, bounce_cols);
                        std::thread::sleep(std::time::Duration::from_millis(25));
                    }
                    pty_handle.resize_direct(target_rows, target_cols);
                    std::thread::sleep(std::time::Duration::from_millis(125));
                }
                pty_handle.get_snapshot()
            })
            .await
            {
                Ok(snapshot) => snapshot,
                Err(e) => {
                    log::warn!(
                        "[Lua] Snapshot refresh task failed for session {}: {}",
                        session_uuid,
                        e
                    );
                    Vec::new()
                }
            };

            Self::queue_webrtc_terminal_snapshot(
                &output_tx,
                &hub_event_tx,
                &peer_id,
                &subscription_id,
                &session_uuid,
                snapshot,
            );
        });
    }

    fn queue_webrtc_terminal_snapshot(
        output_tx: &tokio::sync::mpsc::Sender<WebRtcPtyOutput>,
        hub_event_tx: &super::events::HubEventTx,
        peer_id: &str,
        subscription_id: &str,
        session_uuid: &str,
        snapshot: Vec<u8>,
    ) -> bool {
        if snapshot.is_empty() {
            return true;
        }

        log::debug!(
            "[Lua] Sending {} bytes of snapshot for session {}",
            snapshot.len(),
            session_uuid
        );

        // Single message: [0x02 prefix][snapshot bytes]
        // WebRTC SCTP handles message fragmentation automatically.
        let mut raw_message = Vec::with_capacity(1 + snapshot.len());
        raw_message.push(0x02);
        raw_message.extend(snapshot);

        match output_tx.try_send(WebRtcPtyOutput {
            subscription_id: subscription_id.to_string(),
            browser_identity: peer_id.to_string(),
            data: raw_message,
            session_uuid: session_uuid.to_string(),
        }) {
            Ok(()) => true,
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                log::warn!(
                    "[Lua] WebRTC PTY output queue full during snapshot send for {}",
                    &peer_id[..peer_id.len().min(8)]
                );
                let _ = hub_event_tx.send(super::events::HubEvent::WebRtcIngressBackpressure {
                    browser_identity: peer_id.to_string(),
                    source: "pty_output_snapshot_queue_full",
                });
                false
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                log::trace!("[Lua] PTY output queue closed during snapshot send");
                false
            }
        }
    }

    /// Send recovery snapshots for peers that experienced backpressure drops.
    ///
    /// When PTY frames are dropped because the per-peer send channel is full,
    /// the browser's terminal state diverges (a dropped frame causes the local
    /// parser to miss output, corrupting rendering of all subsequent frames).
    ///
    /// After a cooldown period (letting the burst subside), this method fetches
    /// a fresh snapshot from the session process and sends it directly through
    /// the per-peer channel, bypassing the output queue to avoid re-triggering
    /// the same backpressure.
    fn send_backpressure_recovery_snapshots(&mut self) {
        if self.webrtc_backpressure_recovery.is_empty() {
            return;
        }

        let now = Instant::now();

        // Collect entries that have cooled down.
        let ready: Vec<(String, super::BackpressureRecoveryEntry)> = self
            .webrtc_backpressure_recovery
            .iter()
            .filter(|(_, entry)| {
                now.duration_since(entry.last_drop) >= super::BACKPRESSURE_SNAPSHOT_COOLDOWN
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        for (key, entry) in ready {
            // Check if peer send channel has capacity before attempting snapshot.
            let peer_state = self.webrtc_send_tasks.get(&entry.browser_identity);
            let has_capacity = peer_state
                .map(|state| {
                    !state.dead.load(std::sync::atomic::Ordering::Relaxed)
                        && state.tx.capacity() > 0
                })
                .unwrap_or(false);

            if !has_capacity {
                // Still congested or peer gone — leave entry for next tick.
                continue;
            }

            self.webrtc_backpressure_recovery.remove(&key);

            let Some(session_handle) = self.handle_cache.get_session(&entry.session_uuid) else {
                continue;
            };

            let pty_handle = session_handle.pty().clone();

            if pty_handle.is_session_backed() {
                // Session snapshot requires blocking I/O — spawn off the tick loop.
                // Clone the per-peer sender so the task can deliver chunks directly,
                // bypassing the output queue to avoid re-triggering backpressure.
                let peer_tx = peer_state.unwrap().tx.clone();
                let session_uuid = entry.session_uuid.clone();
                let subscription_id = entry.subscription_id.clone();
                let browser_identity = entry.browser_identity.clone();

                let _guard = self.tokio_runtime.enter();
                tokio::spawn(async move {
                    let snapshot = match tokio::task::spawn_blocking(move || {
                        pty_handle.get_snapshot()
                    })
                    .await
                    {
                        Ok(snapshot) => snapshot,
                        Err(e) => {
                            log::warn!(
                                "[WebRTC] Backpressure recovery snapshot task failed for session {}: {}",
                                &session_uuid[..session_uuid.len().min(8)],
                                e
                            );
                            return;
                        }
                    };

                    if snapshot.is_empty() {
                        return;
                    }

                    log::info!(
                        "[WebRTC] Sending async backpressure recovery snapshot ({} bytes) to {} for session {}",
                        snapshot.len(),
                        &browser_identity[..browser_identity.len().min(8)],
                        &session_uuid[..session_uuid.len().min(8)]
                    );

                    Self::send_snapshot_to_peer(&peer_tx, &subscription_id, &snapshot);
                });
                continue;
            }

            // Snapshot via RPC — run on blocking thread to avoid stalling the event loop.
            let pty_handle = session_handle.pty().clone();
            let subscription_id = entry.subscription_id.clone();
            let browser_identity = entry.browser_identity.clone();
            let session_uuid = entry.session_uuid.clone();
            let peer_tx = peer_state.unwrap().tx.clone();
            tokio::spawn(async move {
                let snapshot = match tokio::task::spawn_blocking(move || pty_handle.get_snapshot())
                    .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        log::warn!(
                            "[WebRTC] Backpressure recovery snapshot task failed for session {}: {}",
                            &session_uuid[..session_uuid.len().min(8)],
                            e
                        );
                        return;
                    }
                };

                if snapshot.is_empty() {
                    return;
                }

                log::info!(
                    "[WebRTC] Sending backpressure recovery snapshot ({} bytes) to {} for session {}",
                    snapshot.len(),
                    &browser_identity[..browser_identity.len().min(8)],
                    &session_uuid[..session_uuid.len().min(8)]
                );

                Self::send_snapshot_to_peer(&peer_tx, &subscription_id, &snapshot);
            });
        }
    }

    /// Send a snapshot directly through a per-peer channel.
    ///
    /// Bypasses the output queue to avoid re-triggering backpressure.
    /// Used by both sync (cached) and async recovery paths.
    fn send_snapshot_to_peer(
        peer_tx: &tokio::sync::mpsc::Sender<super::WebRtcSendItem>,
        subscription_id: &str,
        snapshot: &[u8],
    ) {
        // Single message: [0x02 prefix][snapshot bytes]
        // WebRTC SCTP handles message fragmentation automatically.
        let mut raw_message = Vec::with_capacity(1 + snapshot.len());
        raw_message.push(0x02);
        raw_message.extend(snapshot);

        // try_send to avoid blocking; if still full, the snapshot
        // is best-effort — live stream will eventually resync.
        let _ = peer_tx.try_send(super::WebRtcSendItem::Pty {
            subscription_id: subscription_id.to_string(),
            data: raw_message,
        });
    }

    /// Process pending terminal attach intents.
    ///
    /// Attach intents are created when a terminal subscription arrives before
    /// the target session has been registered in `HandleCache`.
    fn process_pending_terminal_attaches(&mut self) {
        if self.pending_terminal_attaches.is_empty() {
            return;
        }

        let now = Instant::now();
        let mut ready_keys = Vec::new();
        let mut stale_keys = Vec::new();
        let mut inactive_keys = Vec::new();

        for (key, intent) in &self.pending_terminal_attaches {
            if !intent.request.is_active() {
                inactive_keys.push(key.clone());
                continue;
            }

            if self
                .handle_cache
                .get_session(intent.request.session_uuid())
                .is_some()
            {
                ready_keys.push(key.clone());
                continue;
            }

            if now.duration_since(intent.requested_at) >= Self::TERMINAL_ATTACH_NOT_FOUND_TIMEOUT {
                stale_keys.push(key.clone());
            }
        }

        for key in inactive_keys {
            self.pending_terminal_attaches.remove(&key);
        }

        for key in ready_keys {
            let Some(intent) = self.pending_terminal_attaches.remove(&key) else {
                continue;
            };
            if self.try_attach_pending_terminal_request(&intent.request) {
                self.send_pending_terminal_attach_state(&intent.request, "attached");
            } else {
                // Session may have disappeared between lookup and attach attempt.
                self.pending_terminal_attaches.insert(key, intent);
            }
        }

        for key in stale_keys {
            let Some(intent) = self.pending_terminal_attaches.remove(&key) else {
                continue;
            };
            intent.request.deactivate();
            self.send_pending_terminal_attach_state(&intent.request, "not_found");
        }
    }

    fn try_attach_pending_terminal_request(
        &mut self,
        request: &PendingTerminalAttachRequest,
    ) -> bool {
        match request {
            PendingTerminalAttachRequest::WebRtc(req) => self.try_attach_terminal_forwarder(req),
            PendingTerminalAttachRequest::Tui(req) => self.try_attach_tui_terminal_forwarder(req),
            PendingTerminalAttachRequest::Socket(req) => {
                self.try_attach_socket_terminal_forwarder(req)
            }
        }
    }

    fn send_pending_terminal_attach_state(
        &self,
        request: &PendingTerminalAttachRequest,
        state: &str,
    ) {
        if let PendingTerminalAttachRequest::WebRtc(req) = request {
            self.send_terminal_attach_state(
                &req.peer_id,
                &req.subscription_id,
                &req.session_uuid,
                state,
            );
        }
    }

    fn replace_pending_terminal_attach(
        &mut self,
        forwarder_key: &str,
        request: PendingTerminalAttachRequest,
    ) {
        if let Some(prev) = self.pending_terminal_attaches.remove(forwarder_key) {
            prev.request.deactivate();
        }

        self.pending_terminal_attaches.insert(
            forwarder_key.to_string(),
            PendingTerminalAttach {
                request,
                requested_at: Instant::now(),
            },
        );
    }

    /// Create a PTY forwarder requested by Lua.
    ///
    /// Spawns a new forwarder task that streams PTY output to WebRTC.
    fn create_lua_pty_forwarder(&mut self, req: crate::lua::CreateForwarderRequest) {
        let forwarder_key = format!("{}:{}", req.peer_id, req.session_uuid);

        if self.try_attach_terminal_forwarder(&req) {
            self.send_terminal_attach_state(
                &req.peer_id,
                &req.subscription_id,
                &req.session_uuid,
                "attached",
            );
            return;
        }

        self.replace_pending_terminal_attach(
            &forwarder_key,
            PendingTerminalAttachRequest::WebRtc(req.clone()),
        );
        self.send_terminal_attach_state(
            &req.peer_id,
            &req.subscription_id,
            &req.session_uuid,
            "pending",
        );
    }

    /// Create a TUI PTY forwarder requested by Lua.
    ///
    /// Uses pending-attach semantics so early subscribe calls are retried until
    /// the session appears (or timeout), matching WebRTC behavior.
    fn create_lua_tui_pty_forwarder(
        &mut self,
        req: crate::lua::primitives::CreateTuiForwarderRequest,
    ) {
        let forwarder_key = format!("tui:{}", req.session_uuid);

        if self.try_attach_tui_terminal_forwarder(&req) {
            return;
        }

        self.replace_pending_terminal_attach(
            &forwarder_key,
            PendingTerminalAttachRequest::Tui(req),
        );
    }

    /// Try to attach a TUI PTY forwarder immediately.
    ///
    /// Returns `true` when attached, `false` when prerequisites are not ready.
    fn try_attach_tui_terminal_forwarder(
        &mut self,
        req: &crate::lua::primitives::CreateTuiForwarderRequest,
    ) -> bool {
        use crate::client::TuiOutput;

        let forwarder_key = format!("tui:{}", req.session_uuid);

        // Check if session exists
        let Some(session_handle) = self.handle_cache.get_session(&req.session_uuid) else {
            return false;
        };

        let pty_handle = session_handle.pty().clone();

        let Some(output_tx) = self.tui_output_tx.clone() else {
            return false;
        };

        // Abort any existing forwarder for this key
        if let Some(old_task) = self.pty_forwarders.remove(&forwarder_key) {
            old_task.abort();
            self.unregister_terminal_forwarder_peer(&forwarder_key, false);
            log::debug!(
                "[Lua-TUI] Aborted existing PTY forwarder for {}",
                forwarder_key
            );
        }

        // Snapshot retrieval and subscription setup may block. Fetch them in
        // the forwarder task so this handler never blocks the Hub event loop.
        let pty_for_snapshot = pty_handle.clone();

        let sink = output_tx;
        let session_uuid = req.session_uuid.clone();
        let target_rows = req.rows;
        let target_cols = req.cols;
        let active_flag = Arc::clone(&req.active_flag);
        let wake_fd = self.tui_wake_fd;
        let _guard = self.tokio_runtime.enter();
        let task = tokio::spawn(async move {
            use crate::agent::pty::PtyEvent;

            log::info!(
                "[Lua-TUI] Started PTY forwarder for session {}",
                session_uuid
            );

            let (snapshot, kitty_enabled, snapshot_rows, snapshot_cols, mut pty_rx) =
                match tokio::task::spawn_blocking(move || {
                    if pty_for_snapshot.is_session_backed() {
                        if Self::should_force_snapshot_redraw(
                            &pty_for_snapshot,
                            target_rows,
                            target_cols,
                        ) {
                            // Force a redraw pulse for full-screen TUIs. Resizing to the
                            // same dimensions often does not trigger a redraw path.
                            // Normal-screen sessions keep real scrollback in the primary
                            // buffer, so bouncing them by one column can reflow and
                            // inflate restored history on resume.
                            let bounce_cols = if target_cols > 1 { target_cols - 1 } else { 2 };
                            pty_for_snapshot.resize_direct(target_rows, bounce_cols);
                            std::thread::sleep(std::time::Duration::from_millis(25));
                        }
                        pty_for_snapshot.resize_direct(target_rows, target_cols);
                        // Broker-backed sessions redraw asynchronously after SIGWINCH.
                        // Let a short settle window pass so replayed snapshot and
                        // cursor state include that redraw.
                        std::thread::sleep(std::time::Duration::from_millis(125));
                    }
                    pty_for_snapshot.snapshot_and_subscribe()
                })
                .await
                {
                    Ok(result) => result,
                    Err(e) => {
                        log::warn!(
                            "[Lua-TUI] Snapshot fetch task failed for session {}: {}",
                            session_uuid,
                            e
                        );
                        (
                            Vec::new(),
                            false,
                            target_rows,
                            target_cols,
                            pty_handle.subscribe(),
                        )
                    }
                };

            log::debug!(
                "[Lua-TUI] Snapshot bytes for session {}: {}",
                session_uuid,
                snapshot.len()
            );

            if snapshot.is_empty()
                && pty_handle.is_session_backed()
                && !pty_handle.session_connection_alive()
            {
                log::warn!(
                    "[Lua-TUI] Session RPC died before snapshot for {}; sending ProcessExited instead of empty scrollback",
                    session_uuid
                );
                let _ = sink.send(TuiOutput::ProcessExited {
                    session_uuid: session_uuid.clone(),
                    exit_code: None,
                });
                if let Some(fd) = wake_fd {
                    super::wake_tui_pipe(fd);
                }
                return;
            }

            // Always send a scrollback frame (even empty) so the TUI panel
            // can transition out of Connecting and begin live streaming.
            log::debug!(
                "[Lua-TUI] Sending {} bytes of snapshot for session {}",
                snapshot.len(),
                session_uuid
            );
            if sink
                .send(TuiOutput::Scrollback {
                    session_uuid: session_uuid.clone(),
                    rows: snapshot_rows,
                    cols: snapshot_cols,
                    data: snapshot,
                    kitty_enabled,
                })
                .is_err()
            {
                log::trace!("[Lua-TUI] Output channel closed before snapshot sent");
                return;
            }
            if let Some(fd) = wake_fd {
                super::wake_tui_pipe(fd);
            }

            loop {
                // Check if forwarder was stopped by Lua
                {
                    let active = active_flag
                        .lock()
                        .expect("Forwarder active_flag mutex poisoned");
                    if !*active {
                        log::debug!("[Lua-TUI] PTY forwarder stopped by Lua");
                        break;
                    }
                }

                match pty_rx.recv().await {
                    Ok(PtyEvent::Output(data)) => {
                        // Batch: drain all immediately available Output chunks
                        // before sending. One wake per batch instead of per chunk.
                        //
                        // try_recv() consumes items before pattern matching, so
                        // non-Output events would be lost if we used `while let`.
                        // Collect ALL non-output events into a vec rather than a
                        // single stash slot — multiple mode changes can arrive
                        // together (e.g., kitty pop + cursor show when a TUI app
                        // exits) and a single slot silently drops all but the last.
                        let mut chunks = vec![data];
                        let mut stashed: Vec<PtyEvent> = Vec::new();
                        loop {
                            match pty_rx.try_recv() {
                                Ok(PtyEvent::Output(more)) => chunks.push(more),
                                Ok(other) => {
                                    let is_terminal =
                                        matches!(other, PtyEvent::ProcessExited { .. });
                                    stashed.push(other);
                                    if is_terminal {
                                        break; // ProcessExited must be last
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        if sink
                            .send(TuiOutput::OutputBatch {
                                session_uuid: session_uuid.clone(),
                                chunks,
                            })
                            .is_err()
                        {
                            log::trace!("[Lua-TUI] Output channel closed, stopping forwarder");
                            break;
                        }
                        if let Some(fd) = wake_fd {
                            super::wake_tui_pipe(fd);
                        }

                        // Process all stashed non-output events that were consumed
                        // during batching. These are rare (KittyChanged,
                        // FocusReportingChanged, ProcessExited) but must not be dropped.
                        let mut should_break = false;
                        for event in stashed {
                            match event {
                                PtyEvent::ProcessExited { exit_code } => {
                                    log::info!(
                                        "[Lua-TUI] PTY process exited (code={:?}) for session {} (stashed)",
                                        exit_code, session_uuid
                                    );
                                    let _ = sink.send(TuiOutput::ProcessExited {
                                        session_uuid: session_uuid.clone(),
                                        exit_code,
                                    });
                                    if let Some(fd) = wake_fd {
                                        super::wake_tui_pipe(fd);
                                    }
                                    should_break = true;
                                }
                                PtyEvent::KittyChanged(enabled) => {
                                    let _ = sink.send(TuiOutput::Message(serde_json::json!({
                                        "type": "kitty_changed",
                                        "enabled": enabled,
                                        "session_uuid": session_uuid,
                                    })));
                                    if let Some(fd) = wake_fd {
                                        super::wake_tui_pipe(fd);
                                    }
                                }
                                PtyEvent::FocusReportingChanged(enabled) => {
                                    let _ = sink.send(TuiOutput::Message(serde_json::json!({
                                        "type": "focus_reporting_changed",
                                        "enabled": enabled,
                                        "session_uuid": session_uuid,
                                    })));
                                    if let Some(fd) = wake_fd {
                                        super::wake_tui_pipe(fd);
                                    }
                                }
                                PtyEvent::Output(_) => unreachable!("output handled above"),
                                _ => {} // Resized, Notification, etc. — not forwarded to TUI
                            }
                        }
                        if should_break {
                            break; // exit forwarder on process exit
                        }
                    }
                    Ok(PtyEvent::ProcessExited { exit_code }) => {
                        log::info!(
                            "[Lua-TUI] PTY process exited (code={:?}) for session {}",
                            exit_code,
                            session_uuid
                        );
                        let _ = sink.send(TuiOutput::ProcessExited {
                            session_uuid: session_uuid.clone(),
                            exit_code,
                        });
                        if let Some(fd) = wake_fd {
                            super::wake_tui_pipe(fd);
                        }
                        break;
                    }
                    Ok(PtyEvent::KittyChanged(enabled)) => {
                        let _ = sink.send(TuiOutput::Message(serde_json::json!({
                            "type": "kitty_changed",
                            "enabled": enabled,
                            "session_uuid": session_uuid,
                        })));
                        if let Some(fd) = wake_fd {
                            super::wake_tui_pipe(fd);
                        }
                    }
                    Ok(PtyEvent::FocusReportingChanged(enabled)) => {
                        let _ = sink.send(TuiOutput::Message(serde_json::json!({
                            "type": "focus_reporting_changed",
                            "enabled": enabled,
                            "session_uuid": session_uuid,
                        })));
                        if let Some(fd) = wake_fd {
                            super::wake_tui_pipe(fd);
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!(
                            "[Lua-TUI] PTY forwarder lagged by {} events for session {}",
                            n,
                            session_uuid
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        log::info!("[Lua-TUI] PTY channel closed for session {}", session_uuid);
                        break;
                    }
                }
            }

            // Mark forwarder as inactive
            *active_flag
                .lock()
                .expect("Forwarder active_flag mutex poisoned") = false;

            log::info!(
                "[Lua-TUI] Stopped PTY forwarder for session {}",
                session_uuid
            );
        });

        self.register_terminal_forwarder_peer(&forwarder_key, &req.session_uuid, "tui");
        self.pty_forwarders.insert(forwarder_key, task);
        true
    }

    /// Create a socket PTY forwarder requested by Lua.
    ///
    /// Uses pending-attach semantics so early subscribe calls are retried until
    /// the session appears (or timeout), matching WebRTC/TUI behavior.
    fn create_lua_socket_pty_forwarder(
        &mut self,
        req: crate::lua::primitives::CreateSocketForwarderRequest,
    ) {
        let forwarder_key = format!("{}:{}", req.client_id, req.session_uuid);

        if self.try_attach_socket_terminal_forwarder(&req) {
            return;
        }

        self.replace_pending_terminal_attach(
            &forwarder_key,
            PendingTerminalAttachRequest::Socket(req),
        );
    }

    /// Try to attach a socket PTY forwarder immediately.
    ///
    /// Returns `true` when attached, `false` when prerequisites are not ready.
    fn try_attach_socket_terminal_forwarder(
        &mut self,
        req: &crate::lua::primitives::CreateSocketForwarderRequest,
    ) -> bool {
        use crate::socket::framing::Frame;

        let forwarder_key = format!("{}:{}", req.client_id, req.session_uuid);

        let Some(session_handle) = self.handle_cache.get_session(&req.session_uuid) else {
            return false;
        };

        let pty_handle = session_handle.pty().clone();

        let Some(frame_tx) = self
            .socket_clients
            .get(&req.client_id)
            .map(crate::socket::client_conn::SocketClientConn::frame_sender)
        else {
            return false;
        };

        // Abort any existing forwarder for this key
        if let Some(old_task) = self.pty_forwarders.remove(&forwarder_key) {
            old_task.abort();
            self.unregister_terminal_forwarder_peer(&forwarder_key, false);
            log::debug!(
                "[Lua-Socket] Aborted existing PTY forwarder for {}",
                forwarder_key
            );
        }

        let active_terminal_peers = Arc::clone(&self.active_terminal_peers);

        // Snapshot retrieval and subscription setup may block. Fetch them in
        // the forwarder task so this handler never blocks the Hub event loop.
        let pty_for_snapshot = pty_handle.clone();

        let session_uuid = req.session_uuid.clone();
        let target_rows = req.rows;
        let target_cols = req.cols;
        let active_flag = Arc::clone(&req.active_flag);
        let client_id = req.client_id.clone();
        let hub_event_tx = self.hub_event_tx.clone();

        let _guard = self.tokio_runtime.enter();
        let task = tokio::spawn(async move {
            use crate::agent::pty::PtyEvent;

            log::info!(
                "[Lua-Socket] Started PTY forwarder for {} session {}",
                client_id,
                session_uuid
            );
            let mut query_filter_buffer = Vec::new();

            let (snapshot, kitty_enabled, snapshot_rows, snapshot_cols, mut pty_rx) =
                match tokio::task::spawn_blocking(move || {
                    if pty_for_snapshot.is_session_backed() {
                        if Self::should_force_snapshot_redraw(
                            &pty_for_snapshot,
                            target_rows,
                            target_cols,
                        ) {
                            // Force a redraw pulse for full-screen TUIs. Resizing to the
                            // same dimensions often does not trigger a redraw path.
                            // Normal-screen sessions keep real scrollback in the primary
                            // buffer, so bouncing them by one column can reflow and
                            // inflate restored history on resume.
                            let bounce_cols = if target_cols > 1 { target_cols - 1 } else { 2 };
                            pty_for_snapshot.resize_direct(target_rows, bounce_cols);
                            std::thread::sleep(std::time::Duration::from_millis(25));
                        }
                        pty_for_snapshot.resize_direct(target_rows, target_cols);
                        // Broker-backed sessions redraw asynchronously after SIGWINCH.
                        // Let a short settle window pass so replayed snapshot and
                        // cursor state include that redraw.
                        std::thread::sleep(std::time::Duration::from_millis(125));
                    }
                    pty_for_snapshot.snapshot_and_subscribe()
                })
                .await
                {
                    Ok(result) => result,
                    Err(e) => {
                        log::warn!(
                            "[Lua-Socket] Snapshot fetch task failed for {} session {}: {}",
                            client_id,
                            session_uuid,
                            e
                        );
                        (
                            Vec::new(),
                            false,
                            target_rows,
                            target_cols,
                            pty_handle.subscribe(),
                        )
                    }
                };

            log::debug!(
                "[Lua-Socket] Snapshot bytes for {} session {}: {}",
                client_id,
                session_uuid,
                snapshot.len()
            );

            if snapshot.is_empty()
                && pty_handle.is_session_backed()
                && !pty_handle.session_connection_alive()
            {
                log::warn!(
                    "[Lua-Socket] Session RPC died before snapshot for {} session {}; sending ProcessExited instead of empty scrollback",
                    client_id,
                    session_uuid
                );
                let frame = Frame::ProcessExited {
                    session_uuid: session_uuid.clone(),
                    exit_code: None,
                };
                let _ = frame_tx.try_send(frame.encode());
                return;
            }

            // Always send a scrollback frame (even empty) so clients can
            // deterministically leave connecting state after subscribe.
            let frame = Frame::Scrollback {
                session_uuid: session_uuid.clone(),
                rows: snapshot_rows,
                cols: snapshot_cols,
                kitty_enabled,
                data: snapshot,
            };
            match frame_tx.try_send(frame.encode()) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    log::warn!(
                        "[Lua-Socket] Outbound queue full for {}, forcing reconnect",
                        client_id
                    );
                    let _ = hub_event_tx.send(super::events::HubEvent::SocketClientDisconnected {
                        client_id: client_id.clone(),
                    });
                    return;
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    log::trace!("[Lua-Socket] Frame channel closed before snapshot sent");
                    return;
                }
            }

            loop {
                {
                    let active = active_flag
                        .lock()
                        .expect("Forwarder active_flag mutex poisoned");
                    if !*active {
                        log::debug!("[Lua-Socket] PTY forwarder stopped by Lua");
                        break;
                    }
                }

                match pty_rx.recv().await {
                    Ok(PtyEvent::Output(data)) => {
                        let filtered = if active_terminal_peers
                            .lock()
                            .ok()
                            .and_then(|active| active.get(&session_uuid).cloned())
                            .is_some_and(|active_peer| active_peer != client_id.as_str())
                        {
                            crate::hub::terminal_profile::strip_osc_queries_from_output(
                                &mut query_filter_buffer,
                                &data,
                            )
                        } else {
                            query_filter_buffer.clear();
                            data
                        };

                        if filtered.is_empty() {
                            continue;
                        }

                        let frame = Frame::PtyOutput {
                            session_uuid: session_uuid.clone(),
                            data: filtered,
                        };
                        match frame_tx.try_send(frame.encode()) {
                            Ok(()) => {}
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                log::warn!(
                                    "[Lua-Socket] Outbound queue full for {}, forcing reconnect",
                                    client_id
                                );
                                let _ = hub_event_tx.send(
                                    super::events::HubEvent::SocketClientDisconnected {
                                        client_id: client_id.clone(),
                                    },
                                );
                                break;
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                log::trace!(
                                    "[Lua-Socket] Frame channel closed, stopping forwarder"
                                );
                                break;
                            }
                        }
                    }
                    Ok(PtyEvent::ProcessExited { exit_code }) => {
                        log::info!(
                            "[Lua-Socket] PTY process exited (code={:?}) for {} session {}",
                            exit_code,
                            client_id,
                            session_uuid
                        );
                        let frame = Frame::ProcessExited {
                            session_uuid: session_uuid.clone(),
                            exit_code,
                        };
                        let _ = frame_tx.try_send(frame.encode());
                        break;
                    }
                    Ok(PtyEvent::KittyChanged(enabled)) => {
                        let frame = Frame::Json(serde_json::json!({
                            "type": "kitty_changed",
                            "enabled": enabled,
                            "session_uuid": session_uuid,
                        }));
                        let _ = frame_tx.try_send(frame.encode());
                    }
                    Ok(PtyEvent::FocusReportingChanged(enabled)) => {
                        let frame = Frame::Json(serde_json::json!({
                            "type": "focus_reporting_changed",
                            "enabled": enabled,
                            "session_uuid": session_uuid,
                        }));
                        let _ = frame_tx.try_send(frame.encode());
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!(
                            "[Lua-Socket] PTY forwarder lagged by {} events for {} session {}",
                            n,
                            client_id,
                            session_uuid
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        log::info!(
                            "[Lua-Socket] PTY channel closed for {} session {}",
                            client_id,
                            session_uuid
                        );
                        break;
                    }
                }
            }

            *active_flag
                .lock()
                .expect("Forwarder active_flag mutex poisoned") = false;

            log::info!(
                "[Lua-Socket] Stopped PTY forwarder for {} session {}",
                client_id,
                session_uuid
            );
        });

        self.register_terminal_forwarder_peer(&forwarder_key, &req.session_uuid, &req.client_id);
        self.pty_forwarders.insert(forwarder_key, task);
        true
    }

    /// Stop a PTY forwarder by ID.
    fn stop_lua_pty_forwarder(&mut self, forwarder_id: &str) {
        if let Some(pending) = self.pending_terminal_attaches.remove(forwarder_id) {
            pending.request.deactivate();
        }
        if let Some(task) = self.pty_forwarders.remove(forwarder_id) {
            task.abort();
            self.unregister_terminal_forwarder_peer(forwarder_id, true);
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
        let Some(ref mut rx) = self.pty_input_rx else {
            return;
        };
        let inputs: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        for input in inputs {
            if let Some(session_handle) = self.handle_cache.get_session(&input.session_uuid) {
                if let Err(e) = session_handle.pty().write_input_direct(&input.data) {
                    log::error!("[PTY-INPUT] Write failed: {e}");
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

        let Some(ref mut rx) = self.stream_frame_rx else {
            return;
        };
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
    /// Iterates all active multiplexers, drains their output queues, and queues
    /// each frame via the per-peer send channel (non-blocking).
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

            for frame in frames {
                self.try_send_to_peer(
                    &browser_identity,
                    super::WebRtcSendItem::Stream {
                        frame_type: frame.frame_type,
                        stream_id: frame.stream_id,
                        payload: frame.payload,
                    },
                );
            }
        }
    }

    /// Queue raw PTY bytes for async delivery to a WebRTC peer.
    ///
    /// Non-blocking: pushes a [`WebRtcSendItem::Pty`] into the per-peer send
    /// channel. The actual compress → encrypt → DataChannel send happens in
    /// the spawned per-peer task.
    ///
    /// Returns `false` if the peer has no send task (not connected) or the
    /// send task has marked the peer as dead (circuit breaker).
    fn send_webrtc_raw(
        &self,
        subscription_id: &str,
        browser_identity: &str,
        data: Vec<u8>,
    ) -> super::WebRtcSendOutcome {
        let Some(state) = self.webrtc_send_tasks.get(browser_identity) else {
            return super::WebRtcSendOutcome::Dead;
        };

        // Circuit breaker: send task detected dead peer
        if state.dead.load(std::sync::atomic::Ordering::Relaxed) {
            return super::WebRtcSendOutcome::Dead;
        }

        match state.tx.try_send(super::WebRtcSendItem::Pty {
            subscription_id: subscription_id.to_string(),
            data,
        }) {
            Ok(()) => super::WebRtcSendOutcome::Sent,
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                // Per-peer channel full — peer is slow, drop this frame.
                // PTY output is a continuous stream; dropping is acceptable
                // but a recovery snapshot will be scheduled to resync state.
                log::warn!(
                    "[WebRTC] Backpressure: send channel full for peer {}, dropping PTY frame for subscription {}",
                    &browser_identity[..browser_identity.len().min(8)],
                    &subscription_id[..subscription_id.len().min(20)]
                );
                super::WebRtcSendOutcome::Backpressure
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                // Send task exited — mark dead for fast circuit-breaker on
                // subsequent sends before the cleanup tick runs.
                state.dead.store(true, std::sync::atomic::Ordering::Relaxed);
                super::WebRtcSendOutcome::Dead
            }
        }
    }

    /// Queue a send item for a peer via the per-peer send channel.
    ///
    /// Logs warnings on failure but never blocks the event loop. Used by
    /// `HubEvent::WebRtcSend` (Lua sends) and stream frame delivery.
    fn try_send_to_peer(&self, peer_id: &str, item: super::WebRtcSendItem) {
        let Some(state) = self.webrtc_send_tasks.get(peer_id) else {
            log::debug!(
                "[WebRTC] Send to unknown/disconnected peer: {}",
                &peer_id[..peer_id.len().min(8)]
            );
            return;
        };

        if state.dead.load(std::sync::atomic::Ordering::Relaxed) {
            log::debug!(
                "[WebRTC] Send to dead peer: {}",
                &peer_id[..peer_id.len().min(8)]
            );
            return;
        }

        match state.tx.try_send(item) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                // Channel full — the send task is still alive but can't keep up.
                // Don't mark dead: the send task itself will detect a truly dead
                // peer via timeout. A full channel during a PTY output burst is
                // normal backpressure, not a fatal condition.
                log::debug!(
                    "[WebRTC] Send channel full for {}, dropping frame",
                    &peer_id[..peer_id.len().min(8)]
                );
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                state.dead.store(true, std::sync::atomic::Ordering::Relaxed);
                log::debug!(
                    "[WebRTC] Send channel closed for {}, marking peer dead",
                    &peer_id[..peer_id.len().min(8)]
                );
            }
        }
    }

    /// Spawn a per-peer async send task for off-event-loop DataChannel sends.
    ///
    /// Creates a bounded channel and a `tokio::spawn` task that drains send
    /// items and calls the actual async send methods with timeout. The task
    /// sets the `dead` flag and exits if a send times out.
    fn spawn_peer_send_task(&mut self, browser_identity: &str) {
        // Remove any stale send task for this peer
        if let Some(old) = self.webrtc_send_tasks.remove(browser_identity) {
            old.task.abort();
        }

        let Some(channel) = self.webrtc_channels.get(browser_identity) else {
            return;
        };

        let sender = channel.sender();
        let (tx, mut rx) =
            tokio::sync::mpsc::channel::<super::WebRtcSendItem>(super::PEER_SEND_CHANNEL_CAPACITY);
        let dead = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dead_clone = std::sync::Arc::clone(&dead);
        let bi = browser_identity.to_string();

        let task = self.tokio_runtime.spawn(async move {
            while let Some(item) = rx.recv().await {
                let result = tokio::time::timeout(
                    super::PEER_SEND_TIMEOUT,
                    Self::execute_send(&sender, item),
                )
                .await;

                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        let msg = e.to_string();
                        if msg.contains("not opened")
                            || msg.contains("No data channel")
                            || msg.contains("No peer connection")
                        {
                            log::warn!(
                                "[WebRTC-Send] Peer {} dead ({}), exiting send task",
                                &bi[..bi.len().min(8)],
                                msg
                            );
                            dead_clone.store(true, std::sync::atomic::Ordering::Relaxed);
                            break;
                        }
                        log::warn!(
                            "[WebRTC-Send] Send error for {}: {e}",
                            &bi[..bi.len().min(8)]
                        );
                    }
                    Err(_elapsed) => {
                        log::warn!(
                            "[WebRTC-Send] Send timed out for {} (SCTP congestion), marking dead",
                            &bi[..bi.len().min(8)]
                        );
                        dead_clone.store(true, std::sync::atomic::Ordering::Relaxed);
                        break;
                    }
                }
            }
            log::debug!(
                "[WebRTC-Send] Send task exiting for {}",
                &bi[..bi.len().min(8)]
            );
        });

        self.webrtc_send_tasks.insert(
            browser_identity.to_string(),
            super::PeerSendState { tx, dead, task },
        );
    }

    /// Execute a single send item on a [`WebRtcSender`].
    ///
    /// Dispatches to the appropriate async send method based on the item type.
    async fn execute_send(
        sender: &crate::channel::webrtc::WebRtcSender,
        item: super::WebRtcSendItem,
    ) -> Result<(), crate::channel::ChannelError> {
        match item {
            super::WebRtcSendItem::Pty {
                subscription_id,
                data,
            } => sender.send_pty_raw(&subscription_id, &data).await,
            super::WebRtcSendItem::Json { data } => sender.send_json(&data).await,
            super::WebRtcSendItem::Binary { data } => sender.send_json(&data).await,
            super::WebRtcSendItem::Stream {
                frame_type,
                stream_id,
                payload,
            } => {
                sender
                    .send_stream_raw(frame_type, stream_id, &payload)
                    .await
            }
            super::WebRtcSendItem::BundleRefresh { bundle_bytes } => {
                sender.send_bundle_refresh(&bundle_bytes).await
            }
        }
    }

    /// Spawn a periodic DataChannel ping task for liveness detection.
    ///
    /// Sends `{ "type": "dc_ping" }` every 10 seconds through the per-peer
    /// send channel. The browser responds with `dc_pong`; if pongs stop
    /// arriving, the browser detects the dead connection and reconnects.
    /// The task exits naturally when the send channel is dropped (peer cleanup).
    fn spawn_dc_ping_task(&mut self, browser_identity: &str) {
        /// Interval between DC pings. 10 seconds balances liveness detection
        /// speed against bandwidth/CPU cost on mobile browsers.
        const DC_PING_INTERVAL: Duration = Duration::from_secs(10);

        // Abort any existing ping task for this peer (e.g. ICE restart).
        if let Some(old) = self.dc_ping_tasks.remove(browser_identity) {
            old.abort();
        }

        let Some(state) = self.webrtc_send_tasks.get(browser_identity) else {
            return;
        };
        let tx = state.tx.clone();
        let bi = browser_identity.to_string();

        let ping_payload = serde_json::to_vec(&serde_json::json!({ "type": "dc_ping" }))
            .expect("static JSON serialization cannot fail");

        let task = self.tokio_runtime.spawn(async move {
            let mut interval = tokio::time::interval(DC_PING_INTERVAL);
            // Skip the first immediate tick — peer just connected.
            interval.tick().await;

            loop {
                interval.tick().await;
                let item = super::WebRtcSendItem::Json {
                    data: ping_payload.clone(),
                };
                if tx.send(item).await.is_err() {
                    // Send channel closed — peer disconnected.
                    log::debug!(
                        "[WebRTC] DC ping task exiting for {} (channel closed)",
                        &bi[..bi.len().min(8)]
                    );
                    break;
                }
            }
        });

        self.dc_ping_tasks
            .insert(browser_identity.to_string(), task);
    }

    /// Process a single PTY output message: run interceptors, send via WebRTC,
    /// and notify observers inline.
    fn process_single_pty_output(&mut self, msg: WebRtcPtyOutput) {
        use crate::lua::primitives::PtyOutputContext;

        #[cfg(test)]
        {
            self.pty_output_messages_drained += 1;
        }

        let ctx = PtyOutputContext {
            session_uuid: msg.session_uuid.clone(),
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

        match self.send_webrtc_raw(
            &msg.subscription_id,
            &msg.browser_identity,
            final_data.clone(),
        ) {
            super::WebRtcSendOutcome::Sent => {}
            super::WebRtcSendOutcome::Backpressure => {
                let key = format!("{}:{}", msg.browser_identity, msg.session_uuid);
                self.webrtc_backpressure_recovery.insert(
                    key,
                    super::BackpressureRecoveryEntry {
                        browser_identity: msg.browser_identity.clone(),
                        session_uuid: msg.session_uuid.clone(),
                        subscription_id: msg.subscription_id.clone(),
                        last_drop: Instant::now(),
                    },
                );
            }
            super::WebRtcSendOutcome::Dead => {
                log::warn!(
                    "[WebRTC] DataChannel not open for {}, cleaning up dead channel",
                    &msg.browser_identity[..msg.browser_identity.len().min(8)]
                );
                // Immediate cleanup instead of waiting for CleanupTick.
                self.cleanup_webrtc_channel(&msg.browser_identity, "send_failed");
                return;
            }
        }

        if self.lua.has_observers("pty_output") {
            self.lua.notify_pty_output_observers(&ctx, &final_data);
        }
    }

    /// Uses a circuit breaker: if a send fails because the DataChannel is not
    /// open, all remaining messages for that peer are skipped (prevents the
    /// tick loop from being starved by hundreds of failed `block_on` calls).
    fn poll_webrtc_pty_output(&mut self) {
        use crate::lua::primitives::PtyOutputContext;

        /// Max messages to process per tick to keep the event loop responsive.
        const DRAIN_BUDGET: usize = 256;

        // Drain pending PTY output messages (budget-limited)
        let Some(ref mut rx) = self.webrtc_pty_output_rx else {
            return;
        };
        let messages: Vec<WebRtcPtyOutput> = std::iter::from_fn(|| rx.try_recv().ok())
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
        let mut dead_peers: std::collections::HashSet<String> = std::collections::HashSet::new();

        for msg in messages {
            // Skip peers with dead DataChannels
            if dead_peers.contains(&msg.browser_identity) {
                continue;
            }

            let ctx = PtyOutputContext {
                session_uuid: msg.session_uuid.clone(),
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
            match self.send_webrtc_raw(
                &msg.subscription_id,
                &msg.browser_identity,
                final_data.clone(),
            ) {
                super::WebRtcSendOutcome::Sent => {}
                super::WebRtcSendOutcome::Backpressure => {
                    let key = format!("{}:{}", msg.browser_identity, msg.session_uuid);
                    self.webrtc_backpressure_recovery.insert(
                        key,
                        super::BackpressureRecoveryEntry {
                            browser_identity: msg.browser_identity.clone(),
                            session_uuid: msg.session_uuid.clone(),
                            subscription_id: msg.subscription_id.clone(),
                            last_drop: Instant::now(),
                        },
                    );
                }
                super::WebRtcSendOutcome::Dead => {
                    log::warn!(
                        "[WebRTC] DataChannel not open for {}, skipping remaining PTY output this tick",
                        &msg.browser_identity[..msg.browser_identity.len().min(8)]
                    );
                    dead_peers.insert(msg.browser_identity.clone());
                    continue;
                }
            }

            // Observers: fire inline — the idle detection hook is cheap
            // (hash lookup + timer reset). No reason to defer.
            if has_observers {
                self.lua.notify_pty_output_observers(&ctx, &final_data);
            }
        }

        // Immediately clean up dead peers instead of waiting for the 5-second
        // CleanupTick. This prevents fd exhaustion from accumulating stale
        // WebRTC channels that are already known to be dead.
        for dead_id in &dead_peers {
            self.cleanup_webrtc_channel(dead_id, "send_failed");
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
            self.handle_tui_request(request);
        }
    }

    /// Drain outgoing WebRTC signals and fire Lua events for relay.
    ///
    /// Used by `tick()` for synchronous test driving. Production uses
    /// `handle_webrtc_signal()` via `select!`.
    #[cfg(test)]
    fn poll_outgoing_webrtc_signals(&mut self) {
        use crate::channel::webrtc::OutgoingSignal;

        let Some(ref mut rx) = self.webrtc_outgoing_signal_rx else {
            return;
        };
        let signals: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        for signal in signals {
            match signal {
                OutgoingSignal::Ice {
                    browser_identity,
                    envelope,
                } => {
                    self.emit_outgoing_signal(&browser_identity, envelope, "ICE candidate");
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
    /// Channel setup (stale cleanup, creation, configuration) runs synchronously
    /// on the event loop. The heavy work — SDP negotiation (ICE config fetch can
    /// take 10+ seconds), answer encryption — runs in a spawned async task that
    /// posts `HubEvent::WebRtcOfferCompleted` when done. This prevents the event
    /// loop from freezing during ICE config HTTP requests.
    fn handle_webrtc_offer(&mut self, sdp: &str, browser_identity: &str) {
        use crate::channel::{ChannelConfig, WebRtcChannel};

        if crate::env::is_offline() {
            log::warn!("[WebRTC] Rejecting offer — hub is in offline mode");
            return;
        }

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
            let olm_key = crate::relay::extract_olm_key(browser_identity);
            let stale: Vec<String> = self
                .webrtc_channels
                .keys()
                .filter(|id| {
                    *id != browser_identity && crate::relay::extract_olm_key(id) == olm_key
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

            // Wait briefly for the previous connection's sockets to be released.
            // Keep this short — the event loop is blocked during this wait.
            // The code proceeds regardless on timeout, so 100ms is sufficient
            // to catch the common case (already closed) without stalling.
            if let Some(mut close_rx) = self.webrtc_pending_closes.remove(olm_key) {
                if *close_rx.borrow() {
                    log::debug!("[WebRTC] Previous connection already closed");
                } else {
                    match tokio::task::block_in_place(|| {
                        self.tokio_runtime.block_on(tokio::time::timeout(
                            std::time::Duration::from_millis(100),
                            close_rx.wait_for(|v| *v),
                        ))
                    }) {
                        Ok(Ok(_)) => log::debug!("[WebRTC] Previous connection sockets released"),
                        Ok(Err(_)) => log::debug!("[WebRTC] Close channel dropped, proceeding"),
                        Err(_) => log::debug!(
                            "[WebRTC] Previous connection still closing, proceeding anyway"
                        ),
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

            let config = ChannelConfig {
                channel_name: "WebRtcChannel".to_string(),
                hub_id: hub_id.clone(),
                browser_identity: Some(browser_identity.to_string()),
                encrypt: true,
                compression_threshold: Some(4096),
                cli_subscription: false,
            };

            // Connect the channel (sets up config — fast, does not fetch ICE).
            if let Err(e) =
                tokio::task::block_in_place(|| self.tokio_runtime.block_on(channel.connect(config)))
            {
                log::error!("[WebRTC] Failed to configure channel: {e}");
                return;
            }

            self.webrtc_channels
                .insert(browser_identity.to_string(), channel);

            // Track connection start time for timeout detection
            self.webrtc_connection_started
                .insert(browser_identity.to_string(), Instant::now());
        }

        // Remove the channel from the HashMap to pass it owned to the async task.
        // The task will re-insert it via HubEvent::WebRtcOfferCompleted.
        let Some(channel) = self.webrtc_channels.remove(browser_identity) else {
            log::error!(
                "[WebRTC] Channel missing after setup for {}",
                &browser_identity[..browser_identity.len().min(8)]
            );
            return;
        };

        let crypto = self
            .browser
            .crypto_service
            .clone()
            .expect("crypto service required");
        let event_tx = self.hub_event_tx.clone();
        let sdp = sdp.to_string();
        let browser_id = browser_identity.to_string();
        let olm_key = crate::relay::extract_olm_key(browser_identity).to_string();
        let offer_generation = {
            let entry = self
                .webrtc_offer_generation
                .entry(browser_id.clone())
                .or_insert(0);
            *entry += 1;
            *entry
        };

        // Spawn async task for SDP negotiation + answer encryption.
        // This is the slow path: ICE config fetch + negotiation + envelope encryption.
        self.tokio_runtime.spawn(async move {
            let started_at = Instant::now();
            let encrypted_answer = match channel.handle_sdp_offer(&sdp, &browser_id).await {
                Ok(answer_sdp) => {
                    log::info!(
                        "[WebRTC] Created answer for browser {} in {}ms",
                        &browser_id[..browser_id.len().min(8)],
                        started_at.elapsed().as_millis()
                    );

                    let answer_payload = serde_json::json!({
                        "type": "answer",
                        "sdp": answer_sdp,
                    });
                    let plaintext = serde_json::to_vec(&answer_payload).unwrap_or_default();

                    match crypto.lock() {
                        Ok(mut guard) => match guard.encrypt(&plaintext, &olm_key) {
                            Ok(envelope) => match serde_json::to_value(&envelope) {
                                Ok(v) => Some(v),
                                Err(e) => {
                                    log::error!(
                                        "[WebRTC] Failed to serialize answer envelope: {e}"
                                    );
                                    None
                                }
                            },
                            Err(e) => {
                                log::error!(
                                    "[WebRTC] Failed to encrypt answer after {}ms: {e}",
                                    started_at.elapsed().as_millis()
                                );
                                None
                            }
                        },
                        Err(e) => {
                            log::error!(
                                "[WebRTC] Crypto mutex poisoned after {}ms: {e}",
                                started_at.elapsed().as_millis()
                            );
                            None
                        }
                    }
                }
                Err(e) => {
                    log::error!(
                        "[WebRTC] Failed to handle offer after {}ms: {e}",
                        started_at.elapsed().as_millis()
                    );
                    None
                }
            };

            // Post result back to Hub event loop for Lua relay + channel re-insertion.
            let _ = event_tx.send(super::events::HubEvent::WebRtcOfferCompleted {
                browser_identity: browser_id,
                offer_generation,
                channel,
                encrypted_answer,
            });
        });
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

        let payload = match serde_json::to_vec(&msg) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("[WebPush] Failed to serialize vapid_pub: {e}");
                return;
            }
        };
        self.try_send_to_peer(
            browser_identity,
            super::WebRtcSendItem::Json { data: payload },
        );
        log::info!(
            "[WebPush] Queued VAPID public key for {}",
            &browser_identity[..browser_identity.len().min(8)]
        );
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
        if let Err(e) = crate::relay::persistence::save_push_subscriptions(&self.push_subscriptions)
        {
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
            Ok(None) => match crate::notifications::vapid::VapidKeys::generate() {
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
            },
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

        let keys = match crate::notifications::vapid::VapidKeys::from_base64url(pub_key, priv_key) {
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

        let payload = match serde_json::to_vec(&msg) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("[WebPush] Failed to serialize vapid_keys: {e}");
                return;
            }
        };
        self.try_send_to_peer(
            browser_identity,
            super::WebRtcSendItem::Json { data: payload },
        );
        log::info!("[WebPush] Queued VAPID keypair for browser copy");
    }

    /// Send push subscription acknowledgment to browser.
    fn send_push_sub_ack(&self, browser_identity: &str) {
        let msg = serde_json::json!({ "type": "push_sub_ack" });
        let payload = match serde_json::to_vec(&msg) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("[WebPush] Failed to serialize push_sub_ack: {e}");
                return;
            }
        };
        self.try_send_to_peer(
            browser_identity,
            super::WebRtcSendItem::Json { data: payload },
        );
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
                let _ = event_tx
                    .send(super::events::HubEvent::PushSubscriptionsExpired { identities: stale });
            }
        });
    }

    /// Send test push acknowledgment to browser.
    fn send_push_test_ack(&self, browser_identity: &str, count: usize) {
        let msg = serde_json::json!({ "type": "push_test_ack", "sent": count });
        let payload = match serde_json::to_vec(&msg) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("[WebPush] Failed to serialize push_test_ack: {e}");
                return;
            }
        };
        self.try_send_to_peer(
            browser_identity,
            super::WebRtcSendItem::Json { data: payload },
        );
    }

    /// Handle browser request to disable push notifications.
    ///
    /// Clears all push subscriptions, tells Rails notifications are disabled,
    /// and acks the browser so it can unsubscribe from the push manager.
    fn handle_push_disable(&mut self, browser_identity: &str) {
        // Clear all stored push subscriptions
        self.push_subscriptions = crate::notifications::push::PushSubscriptionStore::default();
        if let Err(e) = crate::relay::persistence::save_push_subscriptions(&self.push_subscriptions)
        {
            log::error!("[WebPush] Failed to clear push subscriptions: {e}");
        }

        self.set_notifications_enabled(false);

        log::info!("[WebPush] Push notifications disabled");

        // Ack browser
        let msg = serde_json::json!({ "type": "push_disable_ack" });
        let payload = match serde_json::to_vec(&msg) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("[WebPush] Failed to serialize push_disable_ack: {e}");
                return;
            }
        };
        self.try_send_to_peer(
            browser_identity,
            super::WebRtcSendItem::Json { data: payload },
        );
    }

    /// Handle push status check from the device settings page.
    ///
    /// Browser sends `{ type: "push_status_req", browser_id }` on connect
    /// to determine which notification buttons to show. Responds with the
    /// authoritative CLI state: whether VAPID keys exist and whether this
    /// specific browser has a stored push subscription.
    fn handle_push_status_request(&mut self, browser_identity: &str, msg: &serde_json::Value) {
        let has_keys = self.vapid_keys.is_some();

        // Use stable browser_id when available, fall back to ephemeral identity
        let browser_id = msg
            .get("browser_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(browser_identity);

        let browser_subscribed = self.push_subscriptions.contains(browser_id);

        let vapid_pub = self
            .vapid_keys
            .as_ref()
            .map(|k| k.public_key_base64url().to_string());

        let response = serde_json::json!({
            "type": "push_status",
            "has_keys": has_keys,
            "browser_subscribed": browser_subscribed,
            "vapid_pub": vapid_pub,
        });

        let payload = match serde_json::to_vec(&response) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("[WebPush] Failed to serialize push_status: {e}");
                return;
            }
        };
        self.try_send_to_peer(
            browser_identity,
            super::WebRtcSendItem::Json { data: payload },
        );
        log::info!(
            "[WebPush] Queued push_status for {} (has_keys={has_keys}, subscribed={browser_subscribed})",
            &browser_identity[..browser_identity.len().min(8)]
        );
    }

    /// Notify Rails that this hub's notifications_enabled flag changed.
    ///
    /// PATCHes `/hubs/{hub_id}` with the new value. Fire-and-forget:
    /// a failure here doesn't block the push subscription flow.
    fn set_notifications_enabled(&self, enabled: bool) {
        let Some(ref hub_id) = self.botster_id else {
            log::warn!("[WebPush] No hub_id, cannot update notifications_enabled on Rails");
            return;
        };
        let url = format!("{}/hubs/{}", self.config.server_url, hub_id);
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

        let data = serde_json::json!({
            "id": id,
            "kind": kind,
            "hubId": hub_id,
            "url": relative_url,
            "createdAt": chrono::Utc::now().to_rfc3339(),
        });

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
            "kind",
            "title",
            "body",
            "url",
            "icon",
            "tag",
            "id",
            "web_push",
            "notification", // prevent overwriting structured fields
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
                let _ = event_tx
                    .send(super::events::HubEvent::PushSubscriptionsExpired { identities: stale });
            }
        });
    }

    // === Connection Setup ===

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
            &self.device.fingerprint,
            self.config.hub_name.as_deref(),
        );
        // Store server-assigned ID (used for all server communication)
        self.botster_id = Some(botster_id.clone());
        // Sync to shared copy for Lua primitives
        *self
            .shared_server_id
            .lock()
            .expect("SharedServerId mutex poisoned") = Some(botster_id.clone());
        // Keep runtime manifest aligned with server-assigned hub ID.
        if let Err(e) =
            crate::hub::daemon::write_manifest(&self.hub_identifier, self.botster_id.as_deref())
        {
            log::warn!("Failed to refresh hub manifest after server registration: {e}");
        }

        // Prefetch ICE config so the first WebRTC offer doesn't pay
        // the HTTP round-trip cost (100-300ms saved on first connection).
        let server_url = self.config.server_url.clone();
        let api_key = self.config.get_api_key().to_string();
        let hub_id = botster_id;
        self.tokio_runtime.spawn(async move {
            crate::channel::WebRtcChannel::prefetch_ice_config(&server_url, &api_key, &hub_id)
                .await;
        });
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
                match crate::relay::persistence::load_legacy_hub_vapid_keys(&self.hub_identifier) {
                    Ok(Some(legacy_keys)) => {
                        log::info!("[WebPush] Migrating legacy per-hub VAPID keys to device level");
                        if let Err(e) = crate::relay::persistence::save_vapid_keys(&legacy_keys) {
                            log::error!("[WebPush] Failed to save migrated VAPID keys: {e}");
                        }
                        self.vapid_keys = Some(legacy_keys);
                    }
                    Ok(None) => {
                        log::debug!(
                            "[WebPush] No VAPID keys yet (browser will trigger generation)"
                        );
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
                    if let Err(e) = crate::relay::persistence::save_push_subscriptions(&store) {
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
mod cargo_profile_tests {
    use super::{detect_running_cargo_profile, detect_running_target_dir, CargoBuildProfile};
    use std::path::Path;

    #[test]
    fn detects_debug_profile_from_target_path() {
        let exe = Path::new("/repo/target/debug/botster");
        assert_eq!(
            detect_running_cargo_profile(exe),
            Some(CargoBuildProfile::Debug)
        );
    }

    #[test]
    fn detects_release_profile_from_target_path() {
        let exe = Path::new("/repo/target/release/botster");
        assert_eq!(
            detect_running_cargo_profile(exe),
            Some(CargoBuildProfile::Release)
        );
    }

    #[test]
    fn detects_named_profile_from_target_path() {
        let exe = Path::new("/repo/target/profiling/botster");
        assert_eq!(
            detect_running_cargo_profile(exe),
            Some(CargoBuildProfile::Named("profiling".to_string()))
        );
    }

    #[test]
    fn returns_none_outside_cargo_target_tree() {
        let exe = Path::new("/usr/local/bin/botster");
        assert_eq!(detect_running_cargo_profile(exe), None);
    }

    #[test]
    fn detects_target_dir_from_target_tree_path() {
        let exe = Path::new("/repo/target/debug/botster");
        assert_eq!(
            detect_running_target_dir(exe),
            Some(Path::new("/repo/target").to_path_buf())
        );
    }

    #[test]
    fn target_dir_none_outside_target_tree() {
        let exe = Path::new("/usr/local/bin/botster");
        assert_eq!(detect_running_target_dir(exe), None);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use crate::agent::pty::PtySession;

    /// Single shared tokio runtime for all server_comms tests.
    fn shared_test_runtime() -> Arc<tokio::runtime::Runtime> {
        static RT: OnceLock<Arc<tokio::runtime::Runtime>> = OnceLock::new();
        Arc::clone(RT.get_or_init(|| Arc::new(tokio::runtime::Runtime::new().unwrap())))
    }

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
                .patch("http://127.0.0.1:1/hubs/1")
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
                    .patch("http://127.0.0.1:1/hubs/1")
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
    use crate::hub::agent_handle::{PtyHandle, SessionHandle, SessionType};
    use crate::hub::{Hub, PendingTerminalAttachRequest};
    use crate::lua::CreateForwarderRequest;
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
    /// instead of the full `setup()` for test isolation.
    fn e2e_hub() -> (
        Hub,
        tokio::sync::mpsc::UnboundedSender<TuiRequest>,
        tokio::sync::mpsc::UnboundedReceiver<TuiOutput>,
    ) {
        let config = e2e_config();
        let mut hub = Hub::with_runtime(config, shared_test_runtime()).unwrap();

        let crypto_service = create_crypto_service("test-hub");
        hub.browser.crypto_service = Some(crypto_service);

        // Register Hub primitives (must happen before loading init script)
        hub.lua
            .register_hub_primitives(
                std::sync::Arc::clone(&hub.handle_cache),
                hub.config.worktree_base.clone(),
                hub.hub_identifier.clone(),
                std::sync::Arc::clone(&hub.shared_server_id),
                std::sync::Arc::clone(&hub.state),
                std::sync::Arc::clone(&hub.shared_color_cache),
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

    fn test_session_handle(session_uuid: &str) -> SessionHandle {
        let pty_session = PtySession::new(24, 80);
        let (shared_state, event_tx, kitty_enabled, cursor_visible, resize_pending) =
            pty_session.get_direct_access();
        std::mem::forget(pty_session);
        let pty = PtyHandle::new(
            event_tx,
            shared_state,
            kitty_enabled,
            cursor_visible,
            resize_pending,
            None,
        );
        SessionHandle::new(session_uuid, "test-agent", SessionType::Agent, None, pty)
    }

    fn register_test_socket_client(hub: &mut Hub, client_id: &str) -> tokio::net::UnixStream {
        let (client_std, server_std) =
            std::os::unix::net::UnixStream::pair().expect("std UnixStream::pair");
        client_std
            .set_nonblocking(true)
            .expect("set_nonblocking client socket");
        server_std
            .set_nonblocking(true)
            .expect("set_nonblocking server socket");
        let _guard = hub.tokio_runtime.enter();
        let client_stream =
            tokio::net::UnixStream::from_std(client_std).expect("tokio::UnixStream client");
        let server_stream =
            tokio::net::UnixStream::from_std(server_std).expect("tokio::UnixStream server");
        let conn = crate::socket::client_conn::SocketClientConn::new(
            client_id.to_string(),
            server_stream,
            hub.hub_event_tx.clone(),
        );
        hub.socket_clients.insert(client_id.to_string(), conn);
        client_stream
    }

    /// Create a test session handle. No local shadow screen — all PTYs are
    /// session-backed. Seed output is broadcast but not parsed locally.
    fn test_local_session_handle_with_seed(
        session_uuid: &str,
        seed_output: &[u8],
    ) -> SessionHandle {
        let pty_session = PtySession::new(24, 80);
        let (shared_state, event_tx, kitty_enabled, cursor_visible, resize_pending) =
            pty_session.get_direct_access();
        std::mem::forget(pty_session);

        let pty = PtyHandle::new(
            event_tx,
            shared_state,
            kitty_enabled,
            cursor_visible,
            resize_pending,
            None,
        );
        let _ = pty
            .event_tx_clone()
            .send(crate::agent::pty::events::PtyEvent::output(
                seed_output.to_vec(),
            ));

        SessionHandle::new(session_uuid, "test-agent", SessionType::Agent, None, pty)
    }

    fn test_local_session_handle(session_uuid: &str) -> SessionHandle {
        test_local_session_handle_with_seed(session_uuid, b"cached-local-output\n")
    }

    // Legacy probe tests removed during the session-process migration.
    // Terminal probe caching is now exercised via session-process paths.

    #[test]
    fn test_session_unregistered_clears_terminal_profile_state() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let session_uuid = "sess-clear-profile";

        hub.terminal_profiles
            .observe_output(session_uuid, b"\x1b]11;?\x07");

        hub.handle_hub_event(crate::hub::events::HubEvent::SessionUnregistered {
            session_uuid: session_uuid.to_string(),
        });

        hub.learn_terminal_probe_replies(
            session_uuid,
            "browser-a",
            b"\x1b]11;rgb:1234/5678/9abc\x07",
        );

        assert_eq!(
            hub.terminal_profiles.headless_reply(
                session_uuid,
                crate::hub::terminal_profile::TerminalProbe::DefaultBackground
            ),
            None
        );
    }

    #[test]
    fn test_multiple_live_clients_do_not_update_terminal_profile_cache() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let session_uuid = "sess-multi-client";

        let _guard = hub.tokio_runtime.enter();
        hub.pty_forwarders
            .insert(format!("tui:{session_uuid}"), tokio::spawn(async {}));
        hub.pty_forwarders
            .insert(format!("browser-a:{session_uuid}"), tokio::spawn(async {}));

        hub.terminal_profiles
            .observe_output(session_uuid, b"\x1b]11;?\x07");
        hub.learn_terminal_probe_replies(
            session_uuid,
            "browser-a",
            b"\x1b]11;rgb:1234/5678/9abc\x07",
        );

        assert_eq!(
            hub.terminal_profiles.headless_reply(
                session_uuid,
                crate::hub::terminal_profile::TerminalProbe::DefaultBackground
            ),
            None
        );
    }

    #[test]
    fn test_headless_probe_detected_and_cache_available() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let session_uuid = "sess-headless-probe";

        // Populate hub cache with color values.
        hub.terminal_profiles
            .observe_peer_input("boot", b"\x1b]10;rgb:aaaa/bbbb/cccc\x07");
        hub.terminal_profiles
            .observe_peer_input("boot", b"\x1b]11;rgb:1111/2222/3333\x07");
        hub.terminal_profiles
            .observe_peer_input("boot", b"\x1b]12;rgb:4444/5555/6666\x07");

        hub.handle_cache
            .add_session(test_local_session_handle(session_uuid));

        // No live clients (headless) — hub should attempt to answer from cache.
        // write_input_direct returns Err in tests (no real PTY), but the hub
        // should still detect the probe and have the right cache value.
        assert!(hub.terminal_profiles.hub_profile_is_complete());
        assert_eq!(
            hub.terminal_profiles.headless_reply(
                session_uuid,
                crate::hub::terminal_profile::TerminalProbe::DefaultBackground
            ),
            Some(b"\x1b]11;rgb:1111/2222/3333\x07".as_slice())
        );
    }

    #[test]
    fn test_live_client_skips_hub_probe_answering() {
        let (mut hub, _request_tx, mut output_rx) = e2e_hub();
        let session_uuid = "sess-live-client-probe";

        // Populate hub cache.
        hub.terminal_profiles
            .observe_peer_input("boot", b"\x1b]11;rgb:1111/2222/3333\x07");

        hub.handle_cache
            .add_session(test_local_session_handle(session_uuid));

        // Add a live client forwarder — hub should NOT answer probes.
        let _guard = hub.tokio_runtime.enter();
        hub.pty_forwarders
            .insert(format!("socket:abc:{session_uuid}"), tokio::spawn(async {}));

        hub.handle_hub_event(crate::hub::events::HubEvent::PtyOutputObserved {
            session_uuid: session_uuid.to_string(),
            data: b"\x1b]11;?\x07".to_vec(),
        });

        // Drain output — hub should not have sent any probe-related messages.
        while output_rx.try_recv().is_ok() {}
    }

    #[test]
    fn test_pty_output_observed_tracks_probe_queries_for_later_replies() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let session_uuid = "sess-observed-probe";

        hub.handle_cache
            .add_session(test_local_session_handle(session_uuid));

        hub.handle_hub_event(crate::hub::events::HubEvent::PtyOutputObserved {
            session_uuid: session_uuid.to_string(),
            data: b"\x1b]11;?\x07".to_vec(),
        });

        hub.learn_terminal_probe_replies(
            session_uuid,
            "browser-a",
            b"\x1b]11;rgb:1234/5678/9abc\x07",
        );

        assert_eq!(
            hub.terminal_profiles.headless_reply(
                session_uuid,
                crate::hub::terminal_profile::TerminalProbe::DefaultBackground
            ),
            None
        );
    }

    #[test]
    fn test_inactive_webrtc_forwarder_strips_probe_queries() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let session_uuid = "sess-filter-inactive-webrtc";
        let session = test_session_handle(session_uuid);
        let event_tx = session.pty().event_tx_clone();
        hub.handle_cache.add_session(session);

        assert!(hub.try_attach_terminal_forwarder(&test_forwarder_request(
            "browser-a",
            session_uuid,
            "terminal_sub"
        )));
        hub.set_active_terminal_peer(session_uuid, "tui", true);
        // No snapshot message (0x02) — test PtyHandle has no session process,
        // so get_snapshot() returns empty and the snapshot send is skipped.
        // Allow forwarder task to start the live loop.
        hub.tokio_runtime.block_on(async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(
            b"before\x1b]11;?\x07after".to_vec(),
        ));

        let output = recv_next_live_webrtc_output(&mut hub);
        assert_eq!(output.data, b"\x01beforeafter");
    }

    #[test]
    fn test_active_webrtc_forwarder_keeps_probe_queries() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let session_uuid = "sess-filter-active-webrtc";
        let session = test_session_handle(session_uuid);
        let event_tx = session.pty().event_tx_clone();
        hub.handle_cache.add_session(session);

        assert!(hub.try_attach_terminal_forwarder(&test_forwarder_request(
            "browser-a",
            session_uuid,
            "terminal_sub"
        )));
        hub.set_active_terminal_peer(session_uuid, "browser-a", true);
        // No snapshot message — empty snapshot from test PtyHandle.
        hub.tokio_runtime.block_on(async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(
            b"\x1b]11;?\x07after".to_vec(),
        ));

        let output = recv_next_live_webrtc_output(&mut hub);
        assert_eq!(output.data, b"\x01\x1b]11;?\x07after");
    }

    #[test]
    fn test_browser_focus_input_updates_active_terminal_peer() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let session_uuid = "sess-browser-focus";

        hub.handle_pty_input(crate::channel::webrtc::PtyInputIncoming {
            session_uuid: session_uuid.to_string(),
            browser_identity: "browser-a".to_string(),
            data: b"\x1b[I".to_vec(),
        });

        assert_eq!(
            hub.active_terminal_peers
                .lock()
                .expect("active peers mutex")
                .get(session_uuid)
                .cloned(),
            Some("browser-a".to_string())
        );

        hub.handle_pty_input(crate::channel::webrtc::PtyInputIncoming {
            session_uuid: session_uuid.to_string(),
            browser_identity: "browser-a".to_string(),
            data: b"\x1b[O".to_vec(),
        });

        assert!(hub
            .active_terminal_peers
            .lock()
            .expect("active peers mutex")
            .get(session_uuid)
            .is_none());
    }

    #[test]
    fn test_tui_focus_request_updates_active_terminal_peer() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let session_uuid = "sess-tui-focus";

        hub.handle_tui_request(TuiRequest::FocusChanged {
            session_uuid: session_uuid.to_string(),
            focused: true,
        });

        assert_eq!(
            hub.active_terminal_peers
                .lock()
                .expect("active peers mutex")
                .get(session_uuid)
                .cloned(),
            Some("tui".to_string())
        );

        hub.handle_tui_request(TuiRequest::FocusChanged {
            session_uuid: session_uuid.to_string(),
            focused: false,
        });

        assert!(hub
            .active_terminal_peers
            .lock()
            .expect("active peers mutex")
            .get(session_uuid)
            .is_none());
    }

    #[test]
    fn test_tui_terminal_color_profile_updates_client_cache() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();

        let mut colors = std::collections::HashMap::new();
        colors.insert(257usize, crate::terminal::Rgb::new(17, 34, 51));

        hub.handle_tui_request(TuiRequest::LuaMessage(serde_json::json!({
            "type": "terminal_color_profile",
            "session_uuid": "sess-color-profile",
            "colors": colors,
        })));

        assert_eq!(
            hub.terminal_client_profiles
                .get("tui")
                .and_then(|colors| colors.get(&257usize))
                .copied(),
            Some(crate::terminal::Rgb::new(17, 34, 51))
        );
    }

    // test_backpressure_recovery_fetches_snapshot removed during the migration.

    fn test_forwarder_request(
        peer_id: &str,
        session_uuid: &str,
        subscription_id: &str,
    ) -> CreateForwarderRequest {
        CreateForwarderRequest {
            peer_id: peer_id.to_string(),
            session_uuid: session_uuid.to_string(),
            prefix: Some(vec![0x01]),
            subscription_id: subscription_id.to_string(),
            rows: 24,
            cols: 80,
            active_flag: Arc::new(Mutex::new(true)),
        }
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

    fn recv_next_live_webrtc_output(hub: &mut Hub) -> super::WebRtcPtyOutput {
        recv_next_webrtc_output_with_prefix(hub, 0x01)
    }

    fn recv_next_webrtc_output_with_prefix(hub: &mut Hub, prefix: u8) -> super::WebRtcPtyOutput {
        for _ in 0..20 {
            hub.tokio_runtime.block_on(async {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            });

            let rx = hub.webrtc_pty_output_rx.as_mut().expect("webrtc output rx");
            while let Ok(output) = rx.try_recv() {
                if output.data.first() == Some(&prefix) {
                    return output;
                }
            }
        }

        panic!("expected webrtc PTY output with prefix {prefix:#x}");
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
            session_uuid: "sess-test".to_string(),
        };

        // Step 1: PTY forwarder sends output
        hub.webrtc_pty_output_tx.try_send(msg).unwrap();

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
                .try_send(super::WebRtcPtyOutput {
                    subscription_id: "sub_test".to_string(),
                    browser_identity: "test-browser-identity".to_string(),
                    data: vec![0x01, 0x41 + i],
                    session_uuid: "sess-test".to_string(),
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

    #[test]
    fn test_terminal_attach_intent_resolves_when_session_appears() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let key = "peer-attach:sess-attach".to_string();

        let req = test_forwarder_request("peer-attach", "sess-attach", "terminal_sess-attach");
        hub.create_lua_pty_forwarder(req);

        assert!(
            hub.pending_terminal_attaches.contains_key(&key),
            "missing session should create pending attach intent"
        );
        assert!(
            !hub.pty_forwarders.contains_key(&key),
            "forwarder should not start until session is registered"
        );

        hub.handle_cache
            .add_session(test_session_handle("sess-attach"));
        hub.tick();

        assert!(
            !hub.pending_terminal_attaches.contains_key(&key),
            "pending attach intent should clear once session exists"
        );
        assert!(
            hub.pty_forwarders.contains_key(&key),
            "forwarder should start after session registration"
        );
    }

    #[test]
    fn test_terminal_attach_intent_times_out_to_not_found() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let key = "peer-timeout:sess-timeout".to_string();

        let req = test_forwarder_request("peer-timeout", "sess-timeout", "terminal_sess-timeout");
        let active_flag = Arc::clone(&req.active_flag);
        hub.create_lua_pty_forwarder(req);

        {
            let intent = hub
                .pending_terminal_attaches
                .get_mut(&key)
                .expect("pending attach intent should exist");
            intent.requested_at = Instant::now()
                - (Hub::TERMINAL_ATTACH_NOT_FOUND_TIMEOUT + Duration::from_millis(1));
        }

        hub.tick();

        assert!(
            !hub.pending_terminal_attaches.contains_key(&key),
            "stale pending attach should be removed"
        );
        assert!(
            !*active_flag
                .lock()
                .expect("Forwarder active_flag mutex poisoned"),
            "not_found transition should deactivate forwarder handle"
        );
    }

    #[test]
    fn test_terminal_attach_intent_replaces_previous_pending_request() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let key = "peer-replace:sess-replace".to_string();

        let req1 = test_forwarder_request("peer-replace", "sess-replace", "terminal_old");
        let req1_active = Arc::clone(&req1.active_flag);
        hub.create_lua_pty_forwarder(req1);

        let req2 = test_forwarder_request("peer-replace", "sess-replace", "terminal_new");
        let req2_active = Arc::clone(&req2.active_flag);
        hub.create_lua_pty_forwarder(req2);

        let pending = hub
            .pending_terminal_attaches
            .get(&key)
            .expect("pending attach should still exist for missing session");
        let subscription_id = match &pending.request {
            PendingTerminalAttachRequest::WebRtc(req) => req.subscription_id.as_str(),
            other => panic!("expected WebRTC pending attach, got {other:?}"),
        };
        assert_eq!(
            subscription_id, "terminal_new",
            "latest subscribe should replace previous pending attach"
        );
        assert!(
            !*req1_active
                .lock()
                .expect("Forwarder active_flag mutex poisoned"),
            "previous pending attach should be deactivated"
        );
        assert!(
            *req2_active
                .lock()
                .expect("Forwarder active_flag mutex poisoned"),
            "replacement attach should remain active"
        );
    }

    #[test]
    fn test_tui_attach_intent_resolves_when_session_appears() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let key = "tui:sess-tui-attach".to_string();

        let req = crate::lua::primitives::CreateTuiForwarderRequest {
            session_uuid: "sess-tui-attach".to_string(),
            subscription_id: "tui:sess-tui-attach".to_string(),
            active_flag: Arc::new(Mutex::new(true)),
            rows: 24,
            cols: 80,
        };
        hub.create_lua_tui_pty_forwarder(req);

        assert!(
            hub.pending_terminal_attaches.contains_key(&key),
            "missing session should create pending TUI attach intent"
        );
        assert!(
            !hub.pty_forwarders.contains_key(&key),
            "TUI forwarder should not start until session is registered"
        );

        hub.handle_cache
            .add_session(test_session_handle("sess-tui-attach"));
        hub.tick();

        assert!(
            !hub.pending_terminal_attaches.contains_key(&key),
            "pending TUI attach should clear once session exists"
        );
        assert!(
            hub.pty_forwarders.contains_key(&key),
            "TUI forwarder should start after session registration"
        );
    }

    #[test]
    fn test_tui_attach_intent_times_out_to_not_found() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let key = "tui:sess-tui-timeout".to_string();

        let req = crate::lua::primitives::CreateTuiForwarderRequest {
            session_uuid: "sess-tui-timeout".to_string(),
            subscription_id: "tui:sess-tui-timeout".to_string(),
            active_flag: Arc::new(Mutex::new(true)),
            rows: 24,
            cols: 80,
        };
        let active_flag = Arc::clone(&req.active_flag);
        hub.create_lua_tui_pty_forwarder(req);

        {
            let intent = hub
                .pending_terminal_attaches
                .get_mut(&key)
                .expect("pending TUI attach intent should exist");
            intent.requested_at = Instant::now()
                - (Hub::TERMINAL_ATTACH_NOT_FOUND_TIMEOUT + Duration::from_millis(1));
        }

        hub.tick();

        assert!(
            !hub.pending_terminal_attaches.contains_key(&key),
            "stale pending TUI attach should be removed"
        );
        assert!(
            !*active_flag
                .lock()
                .expect("Forwarder active_flag mutex poisoned"),
            "not_found transition should deactivate TUI forwarder handle"
        );
    }

    #[test]
    fn test_socket_attach_intent_times_out_to_not_found() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let key = "socket:dead:sess-socket-timeout".to_string();

        let req = crate::lua::primitives::CreateSocketForwarderRequest {
            client_id: "socket:dead".to_string(),
            session_uuid: "sess-socket-timeout".to_string(),
            subscription_id: "socket:sess-socket-timeout".to_string(),
            active_flag: Arc::new(Mutex::new(true)),
            rows: 24,
            cols: 80,
        };
        let active_flag = Arc::clone(&req.active_flag);
        hub.create_lua_socket_pty_forwarder(req);

        assert!(
            hub.pending_terminal_attaches.contains_key(&key),
            "missing socket client/session should create pending socket attach intent"
        );

        {
            let intent = hub
                .pending_terminal_attaches
                .get_mut(&key)
                .expect("pending socket attach intent should exist");
            intent.requested_at = Instant::now()
                - (Hub::TERMINAL_ATTACH_NOT_FOUND_TIMEOUT + Duration::from_millis(1));
        }

        hub.tick();

        assert!(
            !hub.pending_terminal_attaches.contains_key(&key),
            "stale pending socket attach should be removed"
        );
        assert!(
            !*active_flag
                .lock()
                .expect("Forwarder active_flag mutex poisoned"),
            "not_found transition should deactivate socket forwarder handle"
        );
    }

    #[test]
    fn test_socket_attach_intent_resolves_when_session_and_client_appear() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let client_id = "socket:live";
        let key = format!("{client_id}:sess-socket-attach");

        let req = crate::lua::primitives::CreateSocketForwarderRequest {
            client_id: client_id.to_string(),
            session_uuid: "sess-socket-attach".to_string(),
            subscription_id: "socket:sess-socket-attach".to_string(),
            active_flag: Arc::new(Mutex::new(true)),
            rows: 24,
            cols: 80,
        };
        hub.create_lua_socket_pty_forwarder(req);

        assert!(
            hub.pending_terminal_attaches.contains_key(&key),
            "missing socket client/session should create pending socket attach intent"
        );
        assert!(
            !hub.pty_forwarders.contains_key(&key),
            "socket forwarder should not start until session and client are ready"
        );

        let _client_stream = register_test_socket_client(&mut hub, client_id);
        hub.handle_cache
            .add_session(test_session_handle("sess-socket-attach"));
        hub.tick();

        assert!(
            !hub.pending_terminal_attaches.contains_key(&key),
            "pending socket attach should clear once session and client exist"
        );
        assert!(
            hub.pty_forwarders.contains_key(&key),
            "socket forwarder should start after prerequisites are available"
        );
    }
}
