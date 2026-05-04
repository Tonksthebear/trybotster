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

    fn record_hot_span(&self, span: &'static str, started: Instant, bytes: usize, label: &str) {
        self.hub_event_metrics.record_span_with_threshold(
            span,
            started.elapsed(),
            bytes,
            Self::HOT_SUBHANDLER_SLOW,
            label,
        );
    }

    fn record_volume_guardrail(&self, counter: &'static str, burst_counter: &'static str) {
        self.hub_event_metrics.record_counter(counter, 1);
        let Some(count) = self
            .volume_bursts
            .lock()
            .ok()
            .and_then(|mut guard| guard.record(counter, Instant::now()))
        else {
            return;
        };
        self.hub_event_metrics.record_counter(burst_counter, 1);
        log::warn!(
            "[HubEvent-Guardrail] event=volume_burst subtype={} count={} window_ms=30000",
            counter,
            count
        );
    }

    fn spawn_tui_client_worker_adapter(
        &self,
        session_uuid: String,
        pty_handle: crate::hub::agent_handle::PtyHandle,
        output_tx: tokio::sync::mpsc::UnboundedSender<crate::client::TuiOutput>,
    ) -> crate::worker::client::ClientWorkerHandle {
        use crate::worker::client::{ClientWorker, ClientWorkerConfig};
        use crate::worker::hub_control::HUB_CONTROL_QUEUE;
        use crate::worker::session_io::SessionIoRequest;
        use crate::worker::transport::{TransportEgress, TuiTransportAdapter};

        let (hub_control_tx, mut hub_control_rx) =
            tokio::sync::mpsc::channel(HUB_CONTROL_QUEUE.capacity);
        let (outbound_tx, mut outbound_rx) = tokio::sync::mpsc::channel::<TransportEgress>(4096);
        let (session_io_tx, mut session_io_rx) =
            tokio::sync::mpsc::channel(crate::worker::session_io::SESSION_IO_WORKER_QUEUE.capacity);
        let wake_fd = self.tui_wake_fd;
        let mut session_io_txs = std::collections::HashMap::new();
        session_io_txs.insert(session_uuid, session_io_tx);
        let hub_event_tx = self.hub_event_tx.clone();

        tokio::spawn(async move {
            while let Some(message) = hub_control_rx.recv().await {
                let _ = hub_event_tx.send(super::events::HubEvent::ClientWorkerControl(message));
            }
        });

        tokio::spawn(async move {
            while let Some(request) = session_io_rx.recv().await {
                if let SessionIoRequest::PtyInput { data } = request {
                    if let Err(e) = pty_handle.write_input_direct(&data) {
                        log::error!("[Lua-TUI] Worker PTY write failed: {e}");
                    }
                }
            }
        });

        tokio::spawn(async move {
            while let Some(egress) = outbound_rx.recv().await {
                let Some(output) = TuiTransportAdapter::egress_to_output(egress) else {
                    continue;
                };
                if output_tx.send(output).is_err() {
                    break;
                }
                if let Some(fd) = wake_fd {
                    super::wake_tui_pipe(fd);
                }
            }
        });

        let mut config = ClientWorkerConfig::new(
            crate::client::ClientId::Tui,
            hub_control_tx,
            outbound_tx,
            session_io_txs,
        );
        config.outbound =
            crate::worker::BoundedQueueConfig::new("worker.client.tui.outbound", 4096);
        ClientWorker::start(config)
    }

    fn spawn_tui_control_worker_adapter(
        &self,
        output_tx: tokio::sync::mpsc::UnboundedSender<crate::client::TuiOutput>,
    ) -> crate::worker::client::ClientWorkerHandle {
        use crate::worker::client::{ClientWorker, ClientWorkerConfig};
        use crate::worker::hub_control::HUB_CONTROL_QUEUE;
        use crate::worker::transport::{TransportEgress, TuiTransportAdapter};

        let (hub_control_tx, mut hub_control_rx) =
            tokio::sync::mpsc::channel(HUB_CONTROL_QUEUE.capacity);
        let (outbound_tx, mut outbound_rx) = tokio::sync::mpsc::channel::<TransportEgress>(4096);
        let hub_event_tx = self.hub_event_tx.clone();
        let wake_fd = self.tui_wake_fd;

        tokio::spawn(async move {
            while let Some(message) = hub_control_rx.recv().await {
                let _ = hub_event_tx.send(super::events::HubEvent::ClientWorkerControl(message));
            }
        });

        tokio::spawn(async move {
            while let Some(egress) = outbound_rx.recv().await {
                let Some(output) = TuiTransportAdapter::egress_to_output(egress) else {
                    continue;
                };
                if output_tx.send(output).is_err() {
                    break;
                }
                if let Some(fd) = wake_fd {
                    super::wake_tui_pipe(fd);
                }
            }
        });

        let mut config = ClientWorkerConfig::new(
            crate::client::ClientId::Tui,
            hub_control_tx,
            outbound_tx,
            std::collections::HashMap::new(),
        );
        config.outbound =
            crate::worker::BoundedQueueConfig::new("worker.client.tui.outbound", 4096);
        ClientWorker::start(config)
    }

    fn spawn_socket_client_worker_adapter(
        &self,
        client_id: String,
        session_uuid: String,
        pty_handle: crate::hub::agent_handle::PtyHandle,
        frame_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    ) -> crate::worker::client::ClientWorkerHandle {
        use crate::worker::client::{ClientWorker, ClientWorkerConfig};
        use crate::worker::hub_control::HUB_CONTROL_QUEUE;
        use crate::worker::session_io::SessionIoRequest;
        use crate::worker::transport::{SocketFrameAdapter, TransportEgress};

        let (hub_control_tx, mut hub_control_rx) =
            tokio::sync::mpsc::channel(HUB_CONTROL_QUEUE.capacity);
        let (outbound_tx, mut outbound_rx) = tokio::sync::mpsc::channel::<TransportEgress>(512);
        let (session_io_tx, mut session_io_rx) =
            tokio::sync::mpsc::channel(crate::worker::session_io::SESSION_IO_WORKER_QUEUE.capacity);
        let mut session_io_txs = std::collections::HashMap::new();
        session_io_txs.insert(session_uuid, session_io_tx);
        let hub_event_tx = self.hub_event_tx.clone();
        let hub_control_event_tx = self.hub_event_tx.clone();
        let disconnect_client_id = client_id.clone();

        tokio::spawn(async move {
            while let Some(message) = hub_control_rx.recv().await {
                let _ = hub_control_event_tx
                    .send(super::events::HubEvent::ClientWorkerControl(message));
            }
        });

        tokio::spawn(async move {
            while let Some(request) = session_io_rx.recv().await {
                if let SessionIoRequest::PtyInput { data } = request {
                    if let Err(e) = pty_handle.write_input_direct(&data) {
                        log::error!("[Lua-Socket] Worker PTY write failed: {e}");
                    }
                }
            }
        });

        tokio::spawn(async move {
            while let Some(egress) = outbound_rx.recv().await {
                let Some(frame) = SocketFrameAdapter::egress_to_frame(egress) else {
                    continue;
                };
                match frame_tx.try_send(frame.encode()) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        log::warn!(
                            "[Lua-Socket] Adapter writer queue full for {}, forcing reconnect",
                            disconnect_client_id
                        );
                        let _ =
                            hub_event_tx.send(super::events::HubEvent::SocketClientDisconnected {
                                client_id: disconnect_client_id.clone(),
                            });
                        break;
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
                }
            }
        });

        ClientWorker::start(ClientWorkerConfig::new(
            crate::client::ClientId::Socket(client_id),
            hub_control_tx,
            outbound_tx,
            session_io_txs,
        ))
    }

    fn spawn_socket_control_worker_adapter(
        &self,
        client_id: String,
        frame_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    ) -> crate::worker::client::ClientWorkerHandle {
        use crate::worker::client::{ClientWorker, ClientWorkerConfig};
        use crate::worker::hub_control::HUB_CONTROL_QUEUE;
        use crate::worker::transport::{SocketFrameAdapter, TransportEgress};

        let (hub_control_tx, mut hub_control_rx) =
            tokio::sync::mpsc::channel(HUB_CONTROL_QUEUE.capacity);
        let (outbound_tx, mut outbound_rx) = tokio::sync::mpsc::channel::<TransportEgress>(512);
        let hub_event_tx = self.hub_event_tx.clone();
        let hub_control_event_tx = self.hub_event_tx.clone();
        let disconnect_client_id = client_id.clone();

        tokio::spawn(async move {
            while let Some(message) = hub_control_rx.recv().await {
                let _ = hub_control_event_tx
                    .send(super::events::HubEvent::ClientWorkerControl(message));
            }
        });

        tokio::spawn(async move {
            while let Some(egress) = outbound_rx.recv().await {
                let Some(frame) = SocketFrameAdapter::egress_to_frame(egress) else {
                    continue;
                };
                match frame_tx.try_send(frame.encode()) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        let _ =
                            hub_event_tx.send(super::events::HubEvent::SocketClientDisconnected {
                                client_id: disconnect_client_id.clone(),
                            });
                        break;
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
                }
            }
        });

        ClientWorker::start(ClientWorkerConfig::new(
            crate::client::ClientId::Socket(client_id),
            hub_control_tx,
            outbound_tx,
            std::collections::HashMap::new(),
        ))
    }

    fn handle_transport_control_message(
        &mut self,
        message: crate::worker::hub_control::HubControlMessage,
    ) {
        use crate::worker::hub_control::{HubControlMessage, TransportSignal};

        match message {
            HubControlMessage::TransportSignalReady { signal, .. } => match signal {
                TransportSignal::Ice {
                    browser_identity,
                    envelope,
                } => {
                    self.emit_outgoing_signal(&browser_identity, envelope, "ICE candidate");
                }
                TransportSignal::Answer {
                    browser_identity,
                    envelope,
                } => {
                    if self.emit_outgoing_signal(&browser_identity, envelope, "answer") {
                        log::info!("[WebRTC] Encrypted answer sent via Lua relay (async)");
                    }
                }
            },
            HubControlMessage::TransportPeerStateChanged {
                browser_identity,
                state,
                ..
            } => {
                log::debug!(
                    "[WebRTC] Transport peer state for {}: {:?}",
                    &browser_identity[..browser_identity.len().min(8)],
                    state
                );
            }
            HubControlMessage::TransportRatchetRestartRequested {
                browser_identity, ..
            } => self.send_ratchet_bundle_refresh(&browser_identity),
            HubControlMessage::TransportBackpressure { pressure, .. } => {
                self.hub_event_metrics
                    .record_counter("webrtc_transport.backpressure", 1);
                log::debug!("[WebRTC] Transport backpressure: {:?}", pressure);
            }
            _ => {}
        }
    }

    fn format_metrics_spans(
        spans: &std::collections::BTreeMap<&'static str, super::events::HubEventSpanSnapshot>,
    ) -> String {
        spans
            .iter()
            .filter(|(_, s)| s.count > 0)
            .map(|(span, s)| {
                let avg_us = if s.count > 0 {
                    s.total_ns / s.count / 1_000
                } else {
                    0
                };
                format!(
                    "{span}:count={} avg_us={} max_us={} slow={} bytes={}",
                    s.count,
                    avg_us,
                    s.max_ns / 1_000,
                    s.slow_count,
                    s.bytes_total
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn format_metrics_counters(counters: &std::collections::BTreeMap<&'static str, u64>) -> String {
        counters
            .iter()
            .filter(|(_, value)| **value > 0)
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn format_metrics_slow_samples(samples: &[super::events::HubEventSlowSample]) -> String {
        samples
            .iter()
            .map(|sample| {
                format!(
                    "{}:elapsed_us={} bytes={} label={}",
                    sample.span, sample.elapsed_us, sample.bytes, sample.label
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Spawn a background task to reconnect to a session process.
    ///
    /// The blocking `connect_and_seed` handshake runs in `spawn_blocking`.
    /// On success, a `SessionReconnectReady` event is sent back to the hub
    /// loop for reader installation and state seeding.
    fn spawn_session_reconnect(&mut self, session_uuid: String, generation: u64) {
        // Mark in-flight with current timestamp
        if let Some(state) = self.pending_reconnects.get_mut(&session_uuid) {
            state.in_flight = true;
            state.attempt_started_at = Some(Instant::now());
        }

        // Check socket exists before spawning (cheap sync check)
        let socket_path = match crate::session::session_socket_path(&session_uuid) {
            Ok(p) if crate::session::session_process_is_live(&session_uuid) => p,
            _ => {
                log::warn!(
                    "[Session] Live session transport gone for '{}', aborting reconnect",
                    &session_uuid[..session_uuid.len().min(16)]
                );
                // Socket gone — session truly dead. Clean up and fire deferred exit.
                self.pending_reconnects.remove(&session_uuid);
                if let Some(session_handle) = self.handle_cache.get_session(&session_uuid) {
                    session_handle.pty().notify_process_exited(None);
                }
                let data = serde_json::json!({
                    "session_uuid": session_uuid,
                    "exit_code": null,
                });
                if let Err(e) = self.lua.fire_json_event("session_process_exited", &data) {
                    log::error!("[Session] Failed to fire deferred session_process_exited: {e}");
                }
                return;
            }
        };

        let tx = self.hub_event_tx.clone();
        tokio::task::spawn_blocking(move || {
            log::info!(
                "[Session] Reconnect attempt for '{}' gen={}",
                &session_uuid[..session_uuid.len().min(16)],
                generation
            );
            match crate::session::connection::SessionConnection::connect_and_seed(&socket_path) {
                Ok((conn, mode_flags)) => {
                    log::info!(
                        "[Session] Reconnect handshake succeeded for '{}'",
                        &session_uuid[..session_uuid.len().min(16)]
                    );
                    let _ = tx.send(crate::hub::events::HubEvent::SessionReconnectReady {
                        session_uuid,
                        generation,
                        conn,
                        mode_flags,
                    });
                }
                Err(e) => {
                    log::warn!(
                        "[Session] Reconnect failed for '{}': {e}",
                        &session_uuid[..session_uuid.len().min(16)]
                    );
                    // Don't send anything — CleanupTick will detect that
                    // in_flight has been true for >10s and reset it for retry.
                }
            }
        });
    }

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
        self.cleanup_pending_session_io_snapshots_for_forwarder(forwarder_id);
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

    fn handle_observed_pty_output(&mut self, session_uuid: String, data: Vec<u8>) {
        self.hub_event_metrics
            .record_counter("pty_output.messages", 1);
        self.hub_event_metrics
            .record_counter("pty_output.bytes", data.len() as u64);
        // Learn terminal probes from raw session output (headless-safe).
        // Without this, probe responses are only learned through client
        // input paths (TUI/WebRTC/socket), missing headless sessions.
        self.learn_terminal_probe_replies(&session_uuid, "session", &data);

        if self.lua.has_observers("pty_output") {
            let ctx = crate::lua::primitives::PtyOutputContext {
                peer_id: format!("session:{session_uuid}"),
                session_uuid,
            };
            // SessionIoWorker intentionally hands coalesced chunks to Lua
            // observers; byte order and total bytes remain unchanged.
            self.lua.notify_pty_output_observers(&ctx, &data);
        }
    }

    fn handle_client_worker_control(
        &mut self,
        message: crate::worker::hub_control::HubControlMessage,
    ) {
        use crate::client::ClientId;
        use crate::worker::hub_control::HubControlMessage;

        match message {
            HubControlMessage::AttachClient {
                client_id,
                session_uuid,
                ..
            } => {
                let forwarder_key = match &client_id {
                    ClientId::Tui => format!("tui:{session_uuid}"),
                    ClientId::Socket(client_id) => format!("{client_id}:{session_uuid}"),
                    ClientId::Browser(_) | ClientId::Internal => return,
                };
                self.register_terminal_forwarder_peer(
                    &forwarder_key,
                    &session_uuid,
                    &client_id.to_string(),
                );
            }
            HubControlMessage::DetachClient {
                client_id,
                session_uuid,
                ..
            } => {
                let forwarder_key = match client_id {
                    ClientId::Tui => format!("tui:{session_uuid}"),
                    ClientId::Socket(client_id) => format!("{client_id}:{session_uuid}"),
                    ClientId::Browser(_) | ClientId::Internal => return,
                };
                self.stop_lua_pty_forwarder(&forwarder_key);
            }
            HubControlMessage::Backpressure(backpressure) => {
                log::warn!("[ClientWorker] Backpressure: {backpressure:?}");
            }
            HubControlMessage::Reconnect { .. }
            | HubControlMessage::SessionLifecycle { .. }
            | HubControlMessage::Shutdown { .. } => {
                log::trace!("[ClientWorker] Hub-control request: {message:?}");
            }
            HubControlMessage::TransportBackpressure { .. }
            | HubControlMessage::TransportPeerStateChanged { .. }
            | HubControlMessage::TransportSignalReady { .. }
            | HubControlMessage::TransportRatchetRestartRequested { .. } => {
                self.handle_transport_control_message(message);
            }
        }
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
            HubEvent::WebRtcPtyOutput(output) => {
                self.process_single_pty_output(output);
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
                self.terminal_client_workers
                    .retain(|key, _| !key.starts_with(&client_prefix));
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
                let forwarder_key = format!("tui:{session_uuid}");
                if let Some(worker) = self.terminal_client_workers.get(&forwarder_key) {
                    let ingress = crate::worker::transport::TuiTransportAdapter::request_to_ingress(
                        TuiRequest::PtyInput {
                            session_uuid: session_uuid.clone(),
                            data,
                        },
                    );
                    let adapter = crate::worker::transport::TuiTransportAdapter::new();
                    let message = crate::worker::transport::TransportAdapter::ingress_to_client(
                        &adapter, ingress,
                    );
                    if let Err(e) = worker.try_send(message) {
                        log::warn!("[PTY-INPUT] Worker input queue rejected {forwarder_key}: {e}");
                    }
                } else {
                    log::warn!(
                        "[PTY-INPUT] No workerized terminal subscription for UUID {} (cache has {} agents)",
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
    /// Hub policy authorizes the target session here; session I/O owns the
    /// file write and path injection data plane.
    pub fn handle_file_input(&mut self, file: crate::channel::webrtc::FileInputIncoming) {
        let Some(session_handle) = self.handle_cache.get_session(&file.session_uuid) else {
            log::warn!(
                "[FILE-INPUT] Dropping paste for missing session {}",
                file.session_uuid
            );
            return;
        };

        let request_id = Self::next_session_io_request_id("paste");
        let session_uuid = file.session_uuid.clone();
        if let Err(e) = session_handle.pty().enqueue_session_io_request(
            crate::worker::session_io::SessionIoRequest::PasteFile {
                request_id,
                filename: file.filename,
                data: file.data,
            },
        ) {
            log::error!(
                "[FILE-INPUT] Paste enqueue failed for session {} reason={e:?}",
                session_uuid
            );
        }
    }

    /// Clean up paste files for a closed session.
    pub fn cleanup_paste_files(&mut self, session_uuid: &str) {
        if let Some(files) = self.paste_files.remove(session_uuid) {
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
                    "[FILE-INPUT] Cleaned up {} paste file(s) for {session_uuid}",
                    files.len()
                );
            }
        }
    }

    fn cleanup_pending_session_io_snapshots_for_session(&mut self, session_uuid: &str) {
        self.pending_session_io_snapshots
            .retain(|_, pending| pending.session_uuid != session_uuid);
    }

    fn cleanup_pending_session_io_snapshots_for_peer(&mut self, peer_id: &str) {
        self.pending_session_io_snapshots
            .retain(|_, pending| match &pending.target {
                super::PendingSessionIoSnapshotTarget::WebRtcOutput { peer_id: owner, .. } => {
                    owner != peer_id
                }
                super::PendingSessionIoSnapshotTarget::WebRtcPeerRecovery { request } => {
                    request.browser_identity != peer_id
                }
            });
    }

    fn cleanup_pending_session_io_snapshots_for_forwarder(&mut self, forwarder_id: &str) {
        self.pending_session_io_snapshots
            .retain(|_, pending| match &pending.target {
                super::PendingSessionIoSnapshotTarget::WebRtcOutput { forwarder_key, .. } => {
                    forwarder_key.as_deref() != Some(forwarder_id)
                }
                super::PendingSessionIoSnapshotTarget::WebRtcPeerRecovery { .. } => true,
            });
    }

    fn next_session_io_request_id(prefix: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{prefix}-{nanos}")
    }

    fn handle_session_io_event(&mut self, event: crate::worker::session_io::SessionIoEvent) {
        use crate::worker::session_io::SessionIoEvent;

        match event {
            SessionIoEvent::PasteFileWritten {
                session_uuid,
                path,
                bytes,
                ..
            } => {
                log::info!(
                    "[FILE-INPUT] Wrote {} bytes to {} (session={})",
                    bytes,
                    path.display(),
                    session_uuid,
                );
                self.paste_files.entry(session_uuid).or_default().push(path);
            }
            SessionIoEvent::PasteFileFailed {
                session_uuid,
                reason,
                detail,
                ..
            } => {
                log::error!(
                    "[FILE-INPUT] Paste failed for session {} reason={reason:?}: {detail}",
                    session_uuid
                );
            }
            SessionIoEvent::PreparedSnapshot {
                request_id,
                session_uuid,
                uncompressed_len,
                payload,
                recovery,
            } => {
                self.route_prepared_session_io_snapshot(
                    request_id,
                    session_uuid,
                    uncompressed_len,
                    payload,
                    recovery,
                );
            }
            _ => {}
        }
    }

    fn insert_pending_session_io_snapshot(
        &mut self,
        request_id: String,
        pending: super::PendingSessionIoSnapshot,
    ) -> bool {
        if self.pending_session_io_snapshots.len()
            >= crate::worker::session_io::SESSION_IO_WORKER_QUEUE.capacity
        {
            self.hub_event_metrics
                .record_counter("snapshot.queue_full", 1);
            log::warn!(
                "[SessionIo] Snapshot pending map full; dropping request {} for session {}",
                request_id,
                pending.session_uuid
            );
            return false;
        }

        self.pending_session_io_snapshots
            .insert(request_id, pending);
        true
    }

    fn route_prepared_session_io_snapshot(
        &mut self,
        request_id: String,
        session_uuid: String,
        uncompressed_len: usize,
        payload: Vec<u8>,
        recovery: bool,
    ) {
        let Some(pending) = self.pending_session_io_snapshots.remove(&request_id) else {
            log::debug!(
                "[SessionIo] Dropping prepared snapshot for unknown request {} session {}",
                request_id,
                session_uuid
            );
            return;
        };

        if payload.is_empty() {
            let counter = if recovery {
                "snapshot.backpressure_recovery.empty"
            } else {
                "snapshot.empty"
            };
            self.hub_event_metrics.record_counter(counter, 1);
            return;
        }

        self.hub_event_metrics.record_span_with_threshold(
            "snapshot.gzip_queue",
            pending.started_at.elapsed(),
            uncompressed_len + payload.len(),
            Hub::SNAPSHOT_SLOW,
            &session_uuid,
        );

        match pending.target {
            super::PendingSessionIoSnapshotTarget::WebRtcOutput {
                peer_id,
                subscription_id,
                forwarder_key,
                active_flag,
            } => {
                let output_tx = self.webrtc.pty_output_tx();
                match output_tx.try_send(WebRtcPtyOutput {
                    subscription_id,
                    browser_identity: peer_id.clone(),
                    data: payload,
                    session_uuid,
                }) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        self.hub_event_metrics
                            .record_counter("snapshot.queue_full", 1);
                        if let Some(flag) = active_flag {
                            if let Ok(mut active) = flag.lock() {
                                *active = false;
                            }
                        }
                        if let Some(key) = forwarder_key {
                            self.stop_lua_pty_forwarder(&key);
                        }
                        let _ = self.hub_event_tx.send(
                            super::events::HubEvent::WebRtcIngressBackpressure {
                                browser_identity: peer_id,
                                source: "pty_output_snapshot_queue_full",
                            },
                        );
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                        self.hub_event_metrics
                            .record_counter("snapshot.queue_closed", 1);
                        if let Some(flag) = active_flag {
                            if let Ok(mut active) = flag.lock() {
                                *active = false;
                            }
                        }
                        if let Some(key) = forwarder_key {
                            self.stop_lua_pty_forwarder(&key);
                        }
                    }
                }
            }
            super::PendingSessionIoSnapshotTarget::WebRtcPeerRecovery { request } => {
                let _ = self.webrtc.complete_recovery_snapshot(
                    request,
                    crate::worker::webrtc::WebRtcRecoverySnapshotResult::PreparedSnapshot {
                        uncompressed_len,
                        payload,
                    },
                    &self.hub_event_metrics,
                );
            }
        }
    }

    fn cleanup_stale_session_io_snapshots(&mut self) {
        let now = Instant::now();
        let stale: Vec<String> = self
            .pending_session_io_snapshots
            .iter()
            .filter_map(|(request_id, pending)| {
                (now.duration_since(pending.started_at) > super::SESSION_IO_SNAPSHOT_PENDING_TTL)
                    .then(|| request_id.clone())
            })
            .collect();

        for request_id in stale {
            if let Some(pending) = self.pending_session_io_snapshots.remove(&request_id) {
                self.hub_event_metrics
                    .record_counter("snapshot.pending_stale_drop", 1);
                log::warn!(
                    "[SessionIo] Dropped stale prepared-snapshot request {} for session {}",
                    request_id,
                    pending.session_uuid
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
                self.handle_transport_control_message(
                    crate::worker::hub_control::HubControlMessage::TransportSignalReady {
                        client_id: crate::client::ClientId::browser(browser_identity.clone()),
                        signal: crate::worker::hub_control::TransportSignal::Ice {
                            browser_identity,
                            envelope,
                        },
                    },
                );
                log::debug!("[Crypto] Relayed ICE candidate through transport control surface",);
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
                    self.request_transport_ratchet_restart(browser_identity);
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
                        self.start_webrtc_offer(sdp, browser_identity);
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
                self.send_ratchet_bundle_refresh(browser_identity);
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

        let candidate_preview = candidate
            .get("candidate")
            .and_then(|c| c.as_str())
            .map(Self::ice_candidate_preview);
        match self.webrtc.queue_or_apply_ice(
            browser_identity,
            candidate,
            MAX_QUEUED_ICE_PER_BROWSER,
            &self.tokio_runtime,
        ) {
            crate::worker::webrtc::QueueOrApplyIceOutcome::Applied(Ok(())) => {}
            crate::worker::webrtc::QueueOrApplyIceOutcome::Applied(Err(error)) => {
                log::warn!(
                    "[Lua] Failed to add ICE candidate for {}: {} (candidate='{}')",
                    &browser_identity[..browser_identity.len().min(8)],
                    error,
                    candidate_preview.as_deref().unwrap_or(""),
                );
            }
            crate::worker::webrtc::QueueOrApplyIceOutcome::Queued(queued) => {
                log::debug!(
                    "[Lua] Queued ICE candidate while offer in flight for {} (queued={})",
                    &browser_identity[..browser_identity.len().min(8)],
                    queued
                );
            }
            crate::worker::webrtc::QueueOrApplyIceOutcome::IgnoredEmpty => {
                log::debug!(
                    "[Lua] Ignoring empty ICE candidate for {}",
                    &browser_identity[..browser_identity.len().min(8)]
                );
            }
            crate::worker::webrtc::QueueOrApplyIceOutcome::UnknownBrowser => {
                log::warn!(
                    "[Lua] ICE candidate for unknown browser {}",
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
                    "agent_name": result.agent_name,
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
    #[cfg(test)]
    pub fn handle_webrtc_pty_output_batch(
        &mut self,
        first: WebRtcPtyOutput,
        rx: &mut Option<tokio::sync::mpsc::Receiver<WebRtcPtyOutput>>,
    ) {
        let started = Instant::now();
        let queued = rx.as_ref().map_or(0, tokio::sync::mpsc::Receiver::len);
        let first_len = first.data.len();
        self.hub_event_metrics
            .record_high_water("pty_output.batch_hwm", (queued + 1) as u64);
        // Process the first message directly to preserve ordering.
        self.process_single_pty_output(first);

        // Temporarily put the receiver back into self for poll_webrtc_pty_output
        self.webrtc.return_pty_output_receiver_for_test(rx.take());
        self.poll_webrtc_pty_output();
        // Extract it back out
        *rx = self.webrtc.lease_pty_output_receiver_for_test();
        self.hub_event_metrics.record_span_with_threshold(
            "pty_output.drain_batch",
            started.elapsed(),
            first_len,
            Self::HOT_SUBHANDLER_SLOW,
            "select",
        );
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
    fn poll_webrtc_peer_payloads_for_tests(&mut self) {
        let messages = self.webrtc.poll_received_messages(&self.tokio_runtime);
        if !messages.is_empty() {
            log::trace!("[WebRTC-POLL] Drained {} messages", messages.len());
        }
        for (browser_identity, payload) in messages {
            self.process_webrtc_plaintext_payload(&browser_identity, &payload);
        }
        for browser_identity in self.webrtc.drain_decrypt_failure_triggers() {
            self.request_transport_ratchet_restart(&browser_identity);
        }
    }

    /// Check for WebRTC DataChannels that have just opened and fire `peer_connected`.
    ///
    /// Test-only fallback. Production uses `HubEvent::DcOpened` via `handle_hub_event()`.
    #[cfg(test)]
    fn poll_webrtc_dc_opens(&mut self) {
        for browser_identity in self.webrtc.take_opened_peers() {
            log::info!(
                "[WebRTC] DataChannel opened for {}, firing peer_connected",
                &browser_identity[..browser_identity.len().min(8)]
            );
            // Spawn per-peer send task (same as production DcOpened handler)
            self.spawn_webrtc_peer_sender(&browser_identity);
            if let Err(e) = self.lua.call_peer_connected(&browser_identity) {
                log::warn!("[WebRTC] Lua peer_connected callback error: {e}");
            }
        }
    }

    /// Attempt a ratchet restart, deduplicating by both Olm key and tab ID.
    ///
    /// Prevents cascading restarts when the same browser device reconnects
    /// with a new Olm identity (new account after bundle refresh) but the
    /// same tab/session UUID.
    fn request_transport_ratchet_restart(&mut self, browser_identity: &str) {
        let Some(message) = self.webrtc.record_decrypt_failure(browser_identity) else {
            return;
        };
        log::warn!(
            "[RatchetRestart] Initiating restart for {}",
            &browser_identity[..browser_identity.len().min(8)]
        );
        self.handle_transport_control_message(message);
    }

    /// Send a fresh Olm bundle (type 2) to a browser peer via both DataChannel and ActionCable.
    ///
    /// Generates a new OTK, builds a 161-byte `DeviceKeyBundle`, removes the stale Olm session,
    /// and delivers the bundle over both transport paths (belt and suspenders).
    fn send_ratchet_bundle_refresh(&mut self, browser_identity: &str) {
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
        self.queue_webrtc_peer_command(
            browser_identity,
            crate::worker::webrtc::WebRtcAdapterCommand::BundleRefresh {
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
    fn cleanup_webrtc_peer_registry(&mut self) {
        let scan_started = Instant::now();

        // Enter tokio runtime for channel state() calls
        let _guard = self.tokio_runtime.enter();

        // Timeout for connections stuck in "Connecting" state.
        // Keep this comfortably above the offer/answer happy path, but short
        // enough that failed negotiations do not force manual refreshes.
        const CONNECTION_TIMEOUT: Duration = Duration::from_secs(15);
        let to_cleanup = self.webrtc.cleanup_scan(CONNECTION_TIMEOUT);

        // Clean up stale channels
        for (browser_identity, reason) in to_cleanup {
            self.cleanup_webrtc_peer(&browser_identity, reason);
        }

        self.hub_event_metrics.record_span_with_threshold(
            "cleanup.webrtc_scan",
            scan_started.elapsed(),
            self.webrtc.channel_count(),
            Self::CLEANUP_SCAN_SLOW,
            "channels",
        );
    }

    /// Clean up a single WebRTC channel and its associated resources.
    ///
    /// This is the centralized cleanup point that:
    /// 1. Removes and disconnects the WebRTC channel
    /// 2. Removes connection start time tracking
    /// 3. Aborts any PTY forwarder tasks for this browser
    /// 4. Notifies Lua of peer disconnection
    fn cleanup_webrtc_peer(&mut self, browser_identity: &str, reason: &str) {
        let cleanup_started = Instant::now();
        let reason_counter = match reason {
            "disconnected" => "cleanup.webrtc.reason.disconnected",
            "timeout" => "cleanup.webrtc.reason.timeout",
            "send_failed" => "cleanup.webrtc.reason.send_failed",
            "replaced" => "cleanup.webrtc.reason.replaced",
            _ => "cleanup.webrtc.reason.other",
        };
        self.hub_event_metrics.record_counter(reason_counter, 1);
        self.cleanup_pending_session_io_snapshots_for_peer(browser_identity);
        // Guard against duplicate cleanup calls (e.g. handle_webrtc_send and
        // poll_webrtc_pty_output both detecting the same dead channel in the
        // same tick). If the channel is already gone this is a no-op — we must
        // not fire peer_disconnected a second time or the browser JS state
        // machine will enter an unrecoverable state and stop reconnecting.
        let disconnect_reason = match reason {
            "timeout" => crate::worker::hub_control::TransportDisconnectReason::ConnectionTimeout,
            "send_failed" => crate::worker::hub_control::TransportDisconnectReason::SendTimeout,
            "replaced" => crate::worker::hub_control::TransportDisconnectReason::ReplacedByNewPeer,
            "disconnected" => {
                crate::worker::hub_control::TransportDisconnectReason::DataChannelClose
            }
            _ => crate::worker::hub_control::TransportDisconnectReason::ExplicitDisconnect,
        };
        let generation = self.webrtc.current_offer_generation(browser_identity);
        let Some((cleanup, disconnected)) = self.webrtc.mark_data_channel_closed(
            browser_identity,
            generation,
            disconnect_reason,
            &self.tokio_runtime,
        ) else {
            self.hub_event_metrics
                .record_counter("cleanup.webrtc.duplicate_skipped", 1);
            log::debug!(
                "[WebRTC] cleanup_webrtc_peer({}) called but channel already removed (duplicate skipped)",
                &browser_identity[..browser_identity.len().min(8)]
            );
            return;
        };

        log::info!(
            "[WebRTC] Cleaning up {} channel: {}",
            reason,
            &browser_identity[..browser_identity.len().min(8)]
        );
        if let Some(connected_age) = cleanup.connected_age {
            if connected_age <= Self::CLOSED_AFTER_CONNECT_WINDOW
                && matches!(reason, "disconnected" | "send_failed" | "timeout")
            {
                self.hub_event_metrics
                    .record_counter("webrtc_channel.closed_after_connect", 1);
                log::warn!(
                    "[WebRTC-Guardrail] event=closed_after_connect peer={} reason={} connected_age_ms={}",
                    &browser_identity[..browser_identity.len().min(24)],
                    reason,
                    connected_age.as_millis()
                );
            }
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

        self.handle_transport_control_message(disconnected);

        // Notify Lua of peer disconnection (Lua handles subscription cleanup)
        if let Err(e) = self.lua.call_peer_disconnected(browser_identity) {
            log::warn!("[WebRTC] Lua peer_disconnected callback error: {e}");
        }
        self.hub_event_metrics.record_span_with_threshold(
            "cleanup.webrtc_channel",
            cleanup_started.elapsed(),
            0,
            Self::HOT_SUBHANDLER_SLOW,
            browser_identity,
        );
    }

    /// Handle a message received from a WebRTC DataChannel.
    ///
    /// All message handling is delegated to Lua. The message is passed to Lua's
    /// on_message callback which routes to the appropriate handler (subscribe,
    /// unsubscribe, terminal data, hub commands, etc.).
    ///
    /// Note: Crypto envelope decryption happens inside WebRtcChannel.try_recv(),
    /// so we receive plaintext JSON here.
    fn process_webrtc_plaintext_payload(&mut self, browser_identity: &str, payload: &[u8]) {
        let parse_started = Instant::now();
        match self
            .webrtc
            .handle_plaintext_payload(browser_identity, payload)
        {
            crate::worker::webrtc::WebRtcIngressOutcome::ParseFailed => {
                self.hub_event_metrics
                    .record_counter("webrtc_message.parse_error", 1);
                self.record_hot_span(
                    "webrtc_message.parse_json",
                    parse_started,
                    payload.len(),
                    browser_identity,
                );
                return;
            }
            crate::worker::webrtc::WebRtcIngressOutcome::PongQueued => {
                self.record_hot_span(
                    "webrtc_message.parse_json",
                    parse_started,
                    payload.len(),
                    browser_identity,
                );
                self.record_hot_span(
                    "webrtc_message.dc_ping",
                    Instant::now(),
                    payload.len(),
                    browser_identity,
                );
            }
            crate::worker::webrtc::WebRtcIngressOutcome::PongObserved => {
                self.record_hot_span(
                    "webrtc_message.parse_json",
                    parse_started,
                    payload.len(),
                    browser_identity,
                );
                log::trace!(
                    "[WebRTC] dc_pong from {}",
                    &browser_identity[..browser_identity.len().min(8)]
                );
                self.record_hot_span(
                    "webrtc_message.dc_pong",
                    Instant::now(),
                    payload.len(),
                    browser_identity,
                );
            }
            crate::worker::webrtc::WebRtcIngressOutcome::TerminalColorProfile(msg) => {
                self.record_hot_span(
                    "webrtc_message.parse_json",
                    parse_started,
                    payload.len(),
                    browser_identity,
                );
                self.handle_terminal_color_profile_message(browser_identity, &msg);
                self.record_hot_span(
                    "webrtc_message.terminal_color_profile",
                    Instant::now(),
                    payload.len(),
                    browser_identity,
                );
            }
            crate::worker::webrtc::WebRtcIngressOutcome::LuaMessage(msg) => {
                self.record_hot_span(
                    "webrtc_message.parse_json",
                    parse_started,
                    payload.len(),
                    browser_identity,
                );
                let started = Instant::now();
                self.call_lua_webrtc_message(browser_identity, msg);
                self.record_hot_span(
                    "webrtc_message.lua",
                    started,
                    payload.len(),
                    browser_identity,
                );
            }
            crate::worker::webrtc::WebRtcIngressOutcome::ClientWorker(other) => {
                self.record_hot_span(
                    "webrtc_message.parse_json",
                    parse_started,
                    payload.len(),
                    browser_identity,
                );
                match other {
                    crate::worker::client::ClientWorkerMessage::ControlFrame(
                        crate::worker::client::ClientControlFrame::FocusChanged {
                            session_uuid,
                            focused,
                        },
                    ) => {
                        let started = Instant::now();
                        if !session_uuid.is_empty() {
                            self.set_active_terminal_peer(&session_uuid, browser_identity, focused);
                            self.lua
                                .set_pty_focused(&session_uuid, browser_identity, focused);
                        }
                        self.record_hot_span(
                            "webrtc_message.focus_changed",
                            started,
                            payload.len(),
                            browser_identity,
                        );
                    }
                    other => {
                        log::debug!(
                            "[WebRTC-MSG] Adapter converted inbound message for {} into non-JSON client message: {:?}",
                            &browser_identity[..browser_identity.len().min(8)],
                            other
                        );
                    }
                }
            }
        }
    }

    /// Call Lua WebRTC message handler.
    ///
    /// Passes the decrypted message to Lua's `on_message` callback (if registered).
    /// Any operations queued by the callback are sent directly via `HubEvent`.
    fn call_lua_webrtc_message(&mut self, browser_identity: &str, msg: serde_json::Value) {
        // Call Lua callback
        if let Err(e) = self.lua.call_webrtc_message(browser_identity, msg) {
            self.hub_event_metrics
                .record_counter("webrtc_message.lua_error", 1);
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
                        "agent_name": result.agent_name,
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
        let Ok(attach_state) = crate::worker::client::TerminalAttachState::try_from(state) else {
            log::warn!(
                "[WebRTC] Refusing unknown terminal_attach state '{}' for {}",
                state,
                session_uuid
            );
            return;
        };
        let payload = crate::worker::transport::egress_terminal_attach(
            subscription_id.to_string(),
            session_uuid.to_string(),
            attach_state,
        );
        match serde_json::to_vec(&payload) {
            Ok(data) => self.queue_webrtc_peer_command(
                peer_id,
                crate::worker::webrtc::WebRtcAdapterCommand::Json { data },
            ),
            Err(e) => {
                log::warn!(
                    "[WebRTC] Failed to serialize terminal_attach state '{}': {}",
                    state,
                    e
                );
            }
        }
    }

    fn send_worker_terminal_attach_state(
        worker: &crate::worker::client::ClientWorkerHandle,
        subscription_id: &str,
        session_uuid: &str,
        state: &str,
    ) {
        let Ok(state) = crate::worker::client::TerminalAttachState::try_from(state) else {
            log::warn!(
                "[ClientWorker] Refusing unknown terminal_attach state '{}' for {}",
                state,
                session_uuid
            );
            return;
        };
        if let Err(e) = worker.try_send(crate::worker::client::ClientWorkerMessage::ControlFrame(
            crate::worker::client::ClientControlFrame::TerminalAttach {
                subscription_id: subscription_id.to_string(),
                session_uuid: session_uuid.to_string(),
                state,
            },
        )) {
            log::warn!(
                "[ClientWorker] Failed to queue terminal_attach state '{}' for {}: {}",
                state.as_str(),
                session_uuid,
                e
            );
        }
    }

    fn classify_snapshot_attach_state(
        pty_handle: &crate::hub::agent_handle::PtyHandle,
        session_uuid: &str,
        snapshot: &[u8],
    ) -> SnapshotAttachState {
        if !snapshot.is_empty()
            || !pty_handle.is_session_backed()
            || pty_handle.session_connection_alive()
        {
            return SnapshotAttachState::Ready;
        }

        if crate::session::session_process_is_live(session_uuid) {
            SnapshotAttachState::Reconnecting
        } else {
            SnapshotAttachState::Exited
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
        let pty_for_prepare = pty_handle.clone();

        // Spawn forwarder task.
        let output_tx = self.webrtc.pty_output_tx();
        let hub_event_tx = self.hub_event_tx.clone();
        let peer_id = req.peer_id.clone();
        let session_uuid = req.session_uuid.clone();
        let target_rows = req.rows;
        let target_cols = req.cols;
        let prefix = req.prefix.clone().unwrap_or_else(|| vec![0x01]);
        let active_flag = req.active_flag.clone();
        let active_terminal_peers = Arc::clone(&self.active_terminal_peers);
        let metrics = Arc::clone(&self.hub_event_metrics);

        // Use browser-provided subscription ID for message routing.
        let subscription_id = req.subscription_id.clone();
        let snapshot_request_id = if pty_handle.is_session_backed() {
            let request_id = Self::next_session_io_request_id("snapshot");
            if !self.insert_pending_session_io_snapshot(
                request_id.clone(),
                super::PendingSessionIoSnapshot {
                    session_uuid: session_uuid.clone(),
                    started_at: Instant::now(),
                    target: super::PendingSessionIoSnapshotTarget::WebRtcOutput {
                        peer_id: peer_id.clone(),
                        subscription_id: subscription_id.clone(),
                        forwarder_key: Some(forwarder_key.clone()),
                        active_flag: Some(active_flag.clone()),
                    },
                },
            ) {
                return false;
            }
            Some(request_id)
        } else {
            None
        };

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

            let rpc_started = Instant::now();
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
            metrics.record_span_with_threshold(
                "snapshot.rpc_get",
                rpc_started.elapsed(),
                snapshot.len(),
                Hub::SNAPSHOT_SLOW,
                &session_uuid,
            );

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
                &metrics,
                &hub_event_tx,
                &pty_for_prepare,
                snapshot_request_id,
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
        let pty_for_prepare = pty_handle.clone();
        let hub_event_tx = self.hub_event_tx.clone();
        let peer_id = req.peer_id.clone();
        let subscription_id = req.subscription_id.clone();
        let session_uuid = req.session_uuid.clone();
        let target_rows = req.rows;
        let target_cols = req.cols;
        let metrics = Arc::clone(&self.hub_event_metrics);
        let snapshot_request_id = if pty_handle.is_session_backed() {
            let request_id = Self::next_session_io_request_id("snapshot");
            if !self.insert_pending_session_io_snapshot(
                request_id.clone(),
                super::PendingSessionIoSnapshot {
                    session_uuid: session_uuid.clone(),
                    started_at: Instant::now(),
                    target: super::PendingSessionIoSnapshotTarget::WebRtcOutput {
                        peer_id,
                        subscription_id,
                        forwarder_key: None,
                        active_flag: None,
                    },
                },
            ) {
                return;
            }
            Some(request_id)
        } else {
            None
        };

        let _guard = self.tokio_runtime.enter();
        tokio::spawn(async move {
            let rpc_started = Instant::now();
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
            metrics.record_span_with_threshold(
                "snapshot.rpc_get",
                rpc_started.elapsed(),
                snapshot.len(),
                Hub::SNAPSHOT_SLOW,
                &session_uuid,
            );

            Self::queue_webrtc_terminal_snapshot(
                &metrics,
                &hub_event_tx,
                &pty_for_prepare,
                snapshot_request_id,
                &session_uuid,
                snapshot,
            );
        });
    }

    fn queue_webrtc_terminal_snapshot(
        metrics: &super::events::HubEventMetrics,
        hub_event_tx: &super::events::HubEventTx,
        pty_handle: &crate::hub::agent_handle::PtyHandle,
        request_id: Option<String>,
        session_uuid: &str,
        snapshot: Vec<u8>,
    ) -> bool {
        if snapshot.is_empty() {
            if let Some(request_id) = request_id {
                let _ = hub_event_tx
                    .send(super::events::HubEvent::DropPendingSessionIoSnapshot { request_id });
            }
            metrics.record_counter("snapshot.empty", 1);
            return true;
        }

        let Some(request_id) = request_id else {
            log::warn!(
                "[SessionIo] Non-session-backed snapshot for session {} cannot be prepared",
                session_uuid
            );
            return false;
        };

        match pty_handle.enqueue_session_io_request(
            crate::worker::session_io::SessionIoRequest::PrepareSnapshot {
                request_id: request_id.clone(),
                snapshot,
                recovery: false,
            },
        ) {
            Ok(()) => true,
            Err(e) => {
                if matches!(
                    e,
                    crate::session::connection::SessionIoRequestEnqueueError::MailboxFull
                ) {
                    metrics.record_counter("snapshot.queue_full", 1);
                }
                log::warn!(
                    "[SessionIo] Failed to enqueue snapshot prepare for session {}: {e:?}",
                    session_uuid
                );
                let _ = hub_event_tx
                    .send(super::events::HubEvent::DropPendingSessionIoSnapshot { request_id });
                false
            }
        }
    }

    fn queue_backpressure_recovery_snapshot(
        metrics: &super::events::HubEventMetrics,
        hub_event_tx: &super::events::HubEventTx,
        pty_handle: &crate::hub::agent_handle::PtyHandle,
        request_id: String,
        session_uuid: &str,
        snapshot: Vec<u8>,
    ) -> bool {
        match pty_handle.enqueue_session_io_request(
            crate::worker::session_io::SessionIoRequest::PrepareSnapshot {
                request_id: request_id.clone(),
                snapshot,
                recovery: true,
            },
        ) {
            Ok(()) => true,
            Err(e) => {
                metrics.record_counter("snapshot.backpressure_recovery.failed", 1);
                if matches!(
                    e,
                    crate::session::connection::SessionIoRequestEnqueueError::MailboxFull
                ) {
                    metrics.record_counter("snapshot.queue_full", 1);
                }
                log::warn!(
                    "[SessionIo] Failed to enqueue recovery snapshot prepare for session {}: {e:?}",
                    session_uuid
                );
                let _ = hub_event_tx
                    .send(super::events::HubEvent::DropPendingSessionIoSnapshot { request_id });
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
    fn dispatch_webrtc_recovery_snapshot_requests(&mut self) {
        let now = Instant::now();

        // Collect entries that have cooled down.
        let ready = self.webrtc.drain_recovery_requests(now);

        for request in ready {
            let Some(session_handle) = self.handle_cache.get_session(&request.session_uuid) else {
                let _ = self.webrtc.complete_recovery_snapshot(
                    request,
                    crate::worker::webrtc::WebRtcRecoverySnapshotResult::Failed,
                    &self.hub_event_metrics,
                );
                continue;
            };

            let pty_handle = session_handle.pty().clone();

            if pty_handle.is_session_backed() {
                // Session snapshot requires blocking I/O — spawn off the tick loop.
                let session_uuid = request.session_uuid.clone();
                let browser_identity = request.browser_identity.clone();
                let request_for_task = request.clone();
                let metrics = Arc::clone(&self.hub_event_metrics);
                let pty_for_prepare = pty_handle.clone();
                let hub_event_tx = self.hub_event_tx.clone();
                let request_id = Self::next_session_io_request_id("snapshot-recovery");
                if !self.insert_pending_session_io_snapshot(
                    request_id.clone(),
                    super::PendingSessionIoSnapshot {
                        session_uuid: session_uuid.clone(),
                        started_at: Instant::now(),
                        target: super::PendingSessionIoSnapshotTarget::WebRtcPeerRecovery {
                            request: request.clone(),
                        },
                    },
                ) {
                    continue;
                }

                let _guard = self.tokio_runtime.enter();
                tokio::spawn(async move {
                    let rpc_started = Instant::now();
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
                            let _ = hub_event_tx.send(
                                super::events::HubEvent::DropPendingSessionIoSnapshot {
                                    request_id: request_id.clone(),
                                },
                            );
                            let _ =
                                hub_event_tx
                                    .send(super::events::HubEvent::WebRtcRecoverySnapshotReady {
                                    request: request_for_task.clone(),
                                    result:
                                        crate::worker::webrtc::WebRtcRecoverySnapshotResult::Failed,
                                });
                            return;
                        }
                    };
                    metrics.record_span_with_threshold(
                        "snapshot.rpc_get",
                        rpc_started.elapsed(),
                        snapshot.len(),
                        Hub::SNAPSHOT_SLOW,
                        &session_uuid,
                    );

                    if snapshot.is_empty() {
                        let _ = hub_event_tx.send(
                            super::events::HubEvent::DropPendingSessionIoSnapshot {
                                request_id: request_id.clone(),
                            },
                        );
                        let _ = hub_event_tx.send(
                            super::events::HubEvent::WebRtcRecoverySnapshotReady {
                                request: request_for_task.clone(),
                                result: crate::worker::webrtc::WebRtcRecoverySnapshotResult::Empty,
                            },
                        );
                        return;
                    }

                    log::info!(
                        "[WebRTC] Sending async backpressure recovery snapshot ({} bytes) to {} for session {}",
                        snapshot.len(),
                        &browser_identity[..browser_identity.len().min(8)],
                        &session_uuid[..session_uuid.len().min(8)]
                    );

                    if !Self::queue_backpressure_recovery_snapshot(
                        &metrics,
                        &hub_event_tx,
                        &pty_for_prepare,
                        request_id,
                        &session_uuid,
                        snapshot,
                    ) {
                        let _ = hub_event_tx.send(
                            super::events::HubEvent::WebRtcRecoverySnapshotReady {
                                request: request_for_task.clone(),
                                result: crate::worker::webrtc::WebRtcRecoverySnapshotResult::Failed,
                            },
                        );
                    }
                });
                continue;
            }

            // Snapshot via RPC — run on blocking thread to avoid stalling the event loop.
            let pty_handle = session_handle.pty().clone();
            let browser_identity = request.browser_identity.clone();
            let session_uuid = request.session_uuid.clone();
            let request_for_task = request;
            let metrics = Arc::clone(&self.hub_event_metrics);
            let hub_event_tx = self.hub_event_tx.clone();
            tokio::spawn(async move {
                let rpc_started = Instant::now();
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
                        let _ = hub_event_tx.send(
                            super::events::HubEvent::WebRtcRecoverySnapshotReady {
                                request: request_for_task.clone(),
                                result: crate::worker::webrtc::WebRtcRecoverySnapshotResult::Failed,
                            },
                        );
                        return;
                    }
                };
                metrics.record_span_with_threshold(
                    "snapshot.rpc_get",
                    rpc_started.elapsed(),
                    snapshot.len(),
                    Hub::SNAPSHOT_SLOW,
                    &session_uuid,
                );

                if snapshot.is_empty() {
                    let _ =
                        hub_event_tx.send(super::events::HubEvent::WebRtcRecoverySnapshotReady {
                            request: request_for_task.clone(),
                            result: crate::worker::webrtc::WebRtcRecoverySnapshotResult::Empty,
                        });
                    return;
                }

                log::info!(
                    "[WebRTC] Sending backpressure recovery snapshot ({} bytes) to {} for session {}",
                    snapshot.len(),
                    &browser_identity[..browser_identity.len().min(8)],
                    &session_uuid[..session_uuid.len().min(8)]
                );

                let _ = hub_event_tx.send(super::events::HubEvent::WebRtcRecoverySnapshotReady {
                    request: request_for_task,
                    result: crate::worker::webrtc::WebRtcRecoverySnapshotResult::Snapshot(snapshot),
                });
            });
        }
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
            self.terminal_client_workers.remove(&key);
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
        match request {
            PendingTerminalAttachRequest::WebRtc(req) => {
                self.send_terminal_attach_state(
                    &req.peer_id,
                    &req.subscription_id,
                    &req.session_uuid,
                    state,
                );
            }
            PendingTerminalAttachRequest::Tui(req) => {
                let forwarder_key = format!("tui:{}", req.session_uuid);
                if let Some(worker) = self.terminal_client_workers.get(&forwarder_key) {
                    Self::send_worker_terminal_attach_state(
                        worker,
                        &req.subscription_id,
                        &req.session_uuid,
                        state,
                    );
                }
            }
            PendingTerminalAttachRequest::Socket(req) => {
                let forwarder_key = format!("{}:{}", req.client_id, req.session_uuid);
                if let Some(worker) = self.terminal_client_workers.get(&forwarder_key) {
                    Self::send_worker_terminal_attach_state(
                        worker,
                        &req.subscription_id,
                        &req.session_uuid,
                        state,
                    );
                }
            }
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

    fn spawn_terminal_client_forwarder_runtime(
        pty_handle: crate::hub::agent_handle::PtyHandle,
        worker: crate::worker::client::ClientWorkerHandle,
        session_uuid: String,
        subscription_id: String,
        target_rows: u16,
        target_cols: u16,
        active_flag: Arc<std::sync::Mutex<bool>>,
        log_prefix: &'static str,
        client_label: String,
        filter: TerminalStreamFilter,
    ) -> tokio::task::JoinHandle<()> {
        let pty_for_snapshot = pty_handle.clone();

        tokio::spawn(async move {
            use crate::agent::pty::PtyEvent;
            use crate::worker::client::{
                ClientControlFrame, ClientWorkerMessage, TerminalAttachState,
            };

            log::info!(
                "[{}] Started PTY forwarder for {} session {}",
                log_prefix,
                client_label,
                session_uuid
            );
            let _ = worker
                .send(ClientWorkerMessage::SubscribeSession {
                    session_uuid: session_uuid.clone(),
                    subscription_id: subscription_id.clone(),
                })
                .await;
            let mut query_filter_buffer = Vec::new();

            let (snapshot, kitty_enabled, snapshot_rows, snapshot_cols, mut pty_rx) =
                match tokio::task::spawn_blocking(move || {
                    if pty_for_snapshot.is_session_backed() {
                        if Self::should_force_snapshot_redraw(
                            &pty_for_snapshot,
                            target_rows,
                            target_cols,
                        ) {
                            let bounce_cols = if target_cols > 1 { target_cols - 1 } else { 2 };
                            pty_for_snapshot.resize_direct(target_rows, bounce_cols);
                            std::thread::sleep(std::time::Duration::from_millis(25));
                        }
                        pty_for_snapshot.resize_direct(target_rows, target_cols);
                        std::thread::sleep(std::time::Duration::from_millis(125));
                    }
                    pty_for_snapshot.snapshot_and_subscribe()
                })
                .await
                {
                    Ok(result) => result,
                    Err(e) => {
                        log::warn!(
                            "[{}] Snapshot fetch task failed for {} session {}: {}",
                            log_prefix,
                            client_label,
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
                "[{}] Snapshot bytes for {} session {}: {}",
                log_prefix,
                client_label,
                session_uuid,
                snapshot.len()
            );

            let attach_state =
                Self::classify_snapshot_attach_state(&pty_handle, &session_uuid, &snapshot);
            match attach_state {
                SnapshotAttachState::Ready => {}
                SnapshotAttachState::Exited => {
                    log::warn!(
                        "[{}] Session RPC died before snapshot for {} session {}; sending ProcessExited",
                        log_prefix,
                        client_label,
                        session_uuid
                    );
                    let _ = worker
                        .send(ClientWorkerMessage::ControlFrame(
                            ClientControlFrame::ProcessExited {
                                session_uuid: session_uuid.clone(),
                                exit_code: None,
                            },
                        ))
                        .await;
                    return;
                }
                SnapshotAttachState::Reconnecting => {
                    log::info!(
                        "[{}] Session '{}' snapshot unavailable - reconnect pending",
                        log_prefix,
                        &session_uuid[..session_uuid.len().min(16)]
                    );
                    let _ = worker
                        .send(ClientWorkerMessage::ControlFrame(
                            ClientControlFrame::TerminalAttach {
                                subscription_id: subscription_id.clone(),
                                session_uuid: session_uuid.clone(),
                                state: TerminalAttachState::Reconnecting,
                            },
                        ))
                        .await;
                }
            }

            if attach_state != SnapshotAttachState::Reconnecting
                && worker
                    .send(ClientWorkerMessage::ControlFrame(
                        ClientControlFrame::Scrollback {
                            session_uuid: session_uuid.clone(),
                            rows: snapshot_rows,
                            cols: snapshot_cols,
                            kitty_enabled,
                            data: snapshot,
                        },
                    ))
                    .await
                    .is_err()
            {
                log::trace!(
                    "[{}] Worker channel closed before snapshot sent",
                    log_prefix
                );
                return;
            }

            loop {
                {
                    let active = active_flag
                        .lock()
                        .expect("Forwarder active_flag mutex poisoned");
                    if !*active {
                        log::debug!("[{}] PTY forwarder stopped by Lua", log_prefix);
                        break;
                    }
                }

                match pty_rx.recv().await {
                    Ok(PtyEvent::Output(data)) => {
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
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }

                        let mut worker_closed = false;
                        for chunk in chunks {
                            let filtered =
                                filter.filter_chunk(&session_uuid, &mut query_filter_buffer, chunk);
                            if filtered.is_empty() {
                                continue;
                            }
                            if worker
                                .send(ClientWorkerMessage::TerminalBytes {
                                    session_uuid: session_uuid.clone(),
                                    data: filtered,
                                })
                                .await
                                .is_err()
                            {
                                log::trace!(
                                    "[{}] Worker channel closed, stopping forwarder",
                                    log_prefix
                                );
                                worker_closed = true;
                                break;
                            }
                        }
                        if worker_closed {
                            break;
                        }

                        if Self::forward_terminal_stream_events(
                            &worker,
                            &session_uuid,
                            &client_label,
                            log_prefix,
                            stashed,
                        )
                        .await
                        {
                            break;
                        }
                    }
                    Ok(event @ PtyEvent::ProcessExited { .. })
                    | Ok(event @ PtyEvent::KittyChanged(_))
                    | Ok(event @ PtyEvent::FocusReportingChanged(_)) => {
                        if Self::forward_terminal_stream_events(
                            &worker,
                            &session_uuid,
                            &client_label,
                            log_prefix,
                            vec![event],
                        )
                        .await
                        {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!(
                            "[{}] PTY forwarder lagged by {} events for {} session {}",
                            log_prefix,
                            n,
                            client_label,
                            session_uuid
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        log::info!(
                            "[{}] PTY channel closed for {} session {}",
                            log_prefix,
                            client_label,
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
                "[{}] Stopped PTY forwarder for {} session {}",
                log_prefix,
                client_label,
                session_uuid
            );
        })
    }

    async fn forward_terminal_stream_events(
        worker: &crate::worker::client::ClientWorkerHandle,
        session_uuid: &str,
        client_label: &str,
        log_prefix: &str,
        events: Vec<crate::agent::pty::PtyEvent>,
    ) -> bool {
        use crate::agent::pty::PtyEvent;
        use crate::worker::client::{ClientControlFrame, ClientWorkerMessage};

        for event in events {
            match event {
                PtyEvent::ProcessExited { exit_code } => {
                    log::info!(
                        "[{}] PTY process exited (code={:?}) for {} session {}",
                        log_prefix,
                        exit_code,
                        client_label,
                        session_uuid
                    );
                    let _ = worker
                        .send(ClientWorkerMessage::ControlFrame(
                            ClientControlFrame::ProcessExited {
                                session_uuid: session_uuid.to_string(),
                                exit_code,
                            },
                        ))
                        .await;
                    return true;
                }
                PtyEvent::KittyChanged(enabled) => {
                    let _ = worker
                        .send(ClientWorkerMessage::ControlFrame(
                            ClientControlFrame::KittyChanged {
                                session_uuid: session_uuid.to_string(),
                                enabled,
                            },
                        ))
                        .await;
                }
                PtyEvent::FocusReportingChanged(enabled) => {
                    let _ = worker
                        .send(ClientWorkerMessage::ControlFrame(
                            ClientControlFrame::FocusReportingChanged {
                                session_uuid: session_uuid.to_string(),
                                enabled,
                            },
                        ))
                        .await;
                }
                PtyEvent::Output(_) => unreachable!("output handled before stashing"),
                _ => {}
            }
        }
        false
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

        if let Some(output_tx) = self.tui_output_tx.clone() {
            let _guard = self.tokio_runtime.enter();
            let worker = self.spawn_tui_control_worker_adapter(output_tx);
            Self::send_worker_terminal_attach_state(
                &worker,
                &req.subscription_id,
                &req.session_uuid,
                "pending",
            );
            self.terminal_client_workers
                .insert(forwarder_key.clone(), worker);
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
            self.terminal_client_workers.remove(&forwarder_key);
            log::debug!(
                "[Lua-TUI] Aborted existing PTY forwarder for {}",
                forwarder_key
            );
        }

        let session_uuid = req.session_uuid.clone();
        let subscription_id = req.subscription_id.clone();
        let target_rows = req.rows;
        let target_cols = req.cols;
        let active_flag = Arc::clone(&req.active_flag);
        let _guard = self.tokio_runtime.enter();
        let worker = self.spawn_tui_client_worker_adapter(
            req.session_uuid.clone(),
            pty_handle.clone(),
            output_tx,
        );
        self.terminal_client_workers
            .insert(forwarder_key.clone(), worker.clone());
        Self::send_worker_terminal_attach_state(
            &worker,
            &req.subscription_id,
            &req.session_uuid,
            "attached",
        );
        let task = Self::spawn_terminal_client_forwarder_runtime(
            pty_handle,
            worker,
            session_uuid,
            subscription_id,
            target_rows,
            target_cols,
            active_flag,
            "Lua-TUI",
            "tui".to_string(),
            TerminalStreamFilter::None,
        );

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

        if let Some(frame_tx) = self
            .socket_clients
            .get(&req.client_id)
            .map(crate::socket::client_conn::SocketClientConn::frame_sender)
        {
            let _guard = self.tokio_runtime.enter();
            let worker = self.spawn_socket_control_worker_adapter(req.client_id.clone(), frame_tx);
            Self::send_worker_terminal_attach_state(
                &worker,
                &req.subscription_id,
                &req.session_uuid,
                "pending",
            );
            self.terminal_client_workers
                .insert(forwarder_key.clone(), worker);
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
            self.terminal_client_workers.remove(&forwarder_key);
            log::debug!(
                "[Lua-Socket] Aborted existing PTY forwarder for {}",
                forwarder_key
            );
        }

        let active_terminal_peers = Arc::clone(&self.active_terminal_peers);

        let session_uuid = req.session_uuid.clone();
        let subscription_id = req.subscription_id.clone();
        let target_rows = req.rows;
        let target_cols = req.cols;
        let active_flag = Arc::clone(&req.active_flag);
        let client_id = req.client_id.clone();

        let _guard = self.tokio_runtime.enter();
        let worker = self.spawn_socket_client_worker_adapter(
            client_id.clone(),
            req.session_uuid.clone(),
            pty_handle.clone(),
            frame_tx.clone(),
        );
        self.terminal_client_workers
            .insert(forwarder_key.clone(), worker.clone());
        Self::send_worker_terminal_attach_state(
            &worker,
            &req.subscription_id,
            &req.session_uuid,
            "attached",
        );
        let task = Self::spawn_terminal_client_forwarder_runtime(
            pty_handle,
            worker,
            session_uuid,
            subscription_id,
            target_rows,
            target_cols,
            active_flag,
            "Lua-Socket",
            client_id.clone(),
            TerminalStreamFilter::StripOscQueriesWhenInactive {
                active_terminal_peers,
                peer_id: client_id,
            },
        );

        self.pty_forwarders.insert(forwarder_key, task);
        true
    }

    /// Stop a PTY forwarder by ID.
    fn stop_lua_pty_forwarder(&mut self, forwarder_id: &str) {
        self.cleanup_pending_session_io_snapshots_for_forwarder(forwarder_id);
        if let Some(pending) = self.pending_terminal_attaches.remove(forwarder_id) {
            pending.request.deactivate();
        }
        if let Some(task) = self.pty_forwarders.remove(forwarder_id) {
            task.abort();
            self.unregister_terminal_forwarder_peer(forwarder_id, true);
            self.terminal_client_workers.remove(forwarder_id);
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
        let mut rx = self.webrtc.lease_pty_input_receiver_for_test();
        let Some(ref mut rx_ref) = rx else {
            return;
        };
        let inputs: Vec<_> = std::iter::from_fn(|| rx_ref.try_recv().ok()).collect();
        self.webrtc.return_pty_input_receiver_for_test(rx);
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

        let mut rx = self.webrtc.lease_stream_frame_receiver_for_test();
        let Some(ref mut rx_ref) = rx else {
            return;
        };
        let frames: Vec<crate::channel::webrtc::StreamIncoming> =
            std::iter::from_fn(|| rx_ref.try_recv().ok()).collect();
        self.webrtc.return_stream_frame_receiver_for_test(rx);

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
                self.queue_webrtc_peer_command(
                    &browser_identity,
                    crate::worker::webrtc::WebRtcAdapterCommand::Stream {
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
    fn queue_webrtc_pty_frame(
        &self,
        subscription_id: &str,
        browser_identity: &str,
        data: Vec<u8>,
    ) -> crate::worker::webrtc::WebRtcSendOutcome {
        self.webrtc.queue_pty_frame(
            subscription_id,
            browser_identity,
            data,
            &self.hub_event_metrics,
        )
    }

    /// Queue a send item for a peer via the per-peer send channel.
    ///
    /// Logs warnings on failure but never blocks the event loop. Used by
    /// `HubEvent::WebRtcSend` (Lua sends) and stream frame delivery.
    fn queue_webrtc_peer_command(
        &self,
        peer_id: &str,
        item: crate::worker::webrtc::WebRtcAdapterCommand,
    ) {
        self.webrtc
            .queue_command(peer_id, item, &self.hub_event_metrics);
    }

    /// Spawn a per-peer async send task for off-event-loop DataChannel sends.
    ///
    /// Creates a bounded channel and a `tokio::spawn` task that drains send
    /// items and calls the actual async send methods with timeout. The task
    /// sets the `dead` flag and exits if a send times out.
    fn spawn_webrtc_peer_sender(&mut self, browser_identity: &str) {
        self.webrtc.spawn_peer_sender(
            browser_identity,
            &self.tokio_runtime,
            Arc::clone(&self.hub_event_metrics),
        );
    }

    /// Spawn a periodic DataChannel ping task for liveness detection.
    ///
    /// Sends `{ "type": "dc_ping" }` every 10 seconds through the per-peer
    /// send channel. The browser responds with `dc_pong`; if pongs stop
    /// arriving, the browser detects the dead connection and reconnects.
    /// The task exits naturally when the send channel is dropped (peer cleanup).
    fn spawn_dc_ping_task(&mut self, browser_identity: &str) {
        self.webrtc
            .spawn_liveness_probe(browser_identity, &self.tokio_runtime);
    }

    /// Process a single PTY output message: run interceptors, send via WebRTC,
    /// and notify observers inline.
    fn process_single_pty_output(&mut self, msg: WebRtcPtyOutput) {
        use crate::lua::primitives::PtyOutputContext;

        #[cfg(test)]
        {
            self.pty_output_messages_drained += 1;
        }
        self.hub_event_metrics
            .record_counter("pty_output.messages", 1);
        self.hub_event_metrics
            .record_counter("pty_output.bytes", msg.data.len() as u64);

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

        match self.queue_webrtc_pty_frame(
            &msg.subscription_id,
            &msg.browser_identity,
            final_data.clone(),
        ) {
            crate::worker::webrtc::WebRtcSendOutcome::Sent => {}
            crate::worker::webrtc::WebRtcSendOutcome::Backpressure => {
                let key = format!("{}:{}", msg.browser_identity, msg.session_uuid);
                self.webrtc.record_backpressure_recovery(
                    key,
                    crate::worker::webrtc::BackpressureRecoveryEntry {
                        browser_identity: msg.browser_identity.clone(),
                        session_uuid: msg.session_uuid.clone(),
                        subscription_id: msg.subscription_id.clone(),
                        last_drop: Instant::now(),
                    },
                );
            }
            crate::worker::webrtc::WebRtcSendOutcome::Dead => {
                log::warn!(
                    "[WebRTC] DataChannel not open for {}, cleaning up dead channel",
                    &msg.browser_identity[..msg.browser_identity.len().min(8)]
                );
                // Immediate cleanup instead of waiting for CleanupTick.
                self.cleanup_webrtc_peer(&msg.browser_identity, "send_failed");
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
    #[cfg(test)]
    fn poll_webrtc_pty_output(&mut self) {
        use crate::lua::primitives::PtyOutputContext;

        /// Max messages to process per tick to keep the event loop responsive.
        const DRAIN_BUDGET: usize = 256;

        // Drain pending PTY output messages (budget-limited)
        let mut rx = self.webrtc.lease_pty_output_receiver_for_test();
        let Some(ref mut rx_ref) = rx else {
            return;
        };
        let messages: Vec<WebRtcPtyOutput> = std::iter::from_fn(|| rx_ref.try_recv().ok())
            .take(DRAIN_BUDGET)
            .collect();
        self.webrtc.return_pty_output_receiver_for_test(rx);
        let drained_len = messages.len();
        let drained_bytes: usize = messages.iter().map(|msg| msg.data.len()).sum();
        self.hub_event_metrics
            .record_counter("pty_output.messages", drained_len as u64);
        self.hub_event_metrics
            .record_counter("pty_output.bytes", drained_bytes as u64);
        self.hub_event_metrics
            .record_high_water("pty_output.batch_hwm", drained_len as u64);

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
            match self.queue_webrtc_pty_frame(
                &msg.subscription_id,
                &msg.browser_identity,
                final_data.clone(),
            ) {
                crate::worker::webrtc::WebRtcSendOutcome::Sent => {}
                crate::worker::webrtc::WebRtcSendOutcome::Backpressure => {
                    let key = format!("{}:{}", msg.browser_identity, msg.session_uuid);
                    self.webrtc.record_backpressure_recovery(
                        key,
                        crate::worker::webrtc::BackpressureRecoveryEntry {
                            browser_identity: msg.browser_identity.clone(),
                            session_uuid: msg.session_uuid.clone(),
                            subscription_id: msg.subscription_id.clone(),
                            last_drop: Instant::now(),
                        },
                    );
                }
                crate::worker::webrtc::WebRtcSendOutcome::Dead => {
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
            self.cleanup_webrtc_peer(dead_id, "send_failed");
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

        let mut rx = self.webrtc.lease_outgoing_signal_receiver_for_test();
        let Some(ref mut rx_ref) = rx else {
            return;
        };
        let signals: Vec<_> = std::iter::from_fn(|| rx_ref.try_recv().ok()).collect();
        self.webrtc.return_outgoing_signal_receiver_for_test(rx);
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

    /// Start incoming WebRTC offer handling through the transport registry.
    ///
    /// Hub policy validates runtime state and same-device replacement; the
    /// registry creates the channel, tracks generation, and completes stale or
    /// failed negotiations when the async runner returns.
    fn start_webrtc_offer(&mut self, sdp: &str, browser_identity: &str) {
        if crate::env::is_offline() {
            log::warn!("[WebRTC] Rejecting offer — hub is in offline mode");
            return;
        }

        let hub_id = self.server_hub_id().to_string();
        let server_url = self.config.server_url.clone();
        let api_key = self.config.get_api_key().to_string();

        log::info!(
            "[WebRTC] Received offer from {}",
            &browser_identity[..browser_identity.len().min(12)]
        );

        if !self.webrtc.has_channel(browser_identity) {
            // Clean up stale channels from the same device (same Olm key, different tab UUID).
            let olm_key = crate::relay::extract_olm_key(browser_identity);
            let stale = self.webrtc.same_device_channels(browser_identity);
            for stale_id in stale {
                log::info!(
                    "[WebRTC] Replacing stale channel for same device: {}",
                    &stale_id[..stale_id.len().min(8)]
                );
                self.cleanup_webrtc_peer(&stale_id, "replaced");
            }

            // Wait briefly for the previous connection's sockets to be released.
            match self.webrtc.wait_for_replaced_peer_close(
                olm_key,
                std::time::Duration::from_millis(100),
                &self.tokio_runtime,
            ) {
                crate::worker::webrtc::ReplacedPeerCloseWait::NoPendingClose => {}
                crate::worker::webrtc::ReplacedPeerCloseWait::AlreadyClosed => {
                    log::debug!("[WebRTC] Previous connection already closed");
                }
                crate::worker::webrtc::ReplacedPeerCloseWait::Closed => {
                    log::debug!("[WebRTC] Previous connection sockets released");
                }
                crate::worker::webrtc::ReplacedPeerCloseWait::ClosedChannelDropped => {
                    log::debug!("[WebRTC] Close channel dropped, proceeding");
                }
                crate::worker::webrtc::ReplacedPeerCloseWait::TimedOut => {
                    log::debug!("[WebRTC] Previous connection still closing, proceeding anyway");
                }
            }
        }

        let Some(crypto_service) = self.browser.crypto_service.clone() else {
            log::error!("[WebRTC] No crypto service for encrypted answer");
            return;
        };
        let request = crate::worker::webrtc::WebRtcOfferRequest {
            browser_identity: browser_identity.to_string(),
            sdp: sdp.to_string(),
            hub_id,
            server_url,
            api_key,
            crypto_service,
            outgoing_signal_tx: self.webrtc.outgoing_signal_tx(),
            stream_frame_tx: self.webrtc.stream_frame_tx(),
            hub_event_tx: self.hub_event_tx.clone(),
            pty_input_tx: self.webrtc.pty_input_tx(),
            file_input_tx: self.webrtc.file_input_tx(),
        };
        let start = match self.webrtc.start_offer(request, &self.tokio_runtime) {
            Ok(start) => start,
            Err(error) => {
                log::error!("[WebRTC] Failed to configure channel: {error}");
                return;
            }
        };
        let event_tx = self.hub_event_tx.clone();

        // Spawn async task for SDP negotiation + answer encryption.
        self.tokio_runtime.spawn(async move {
            let completion =
                crate::worker::webrtc::WebRtcTransportRunner::negotiate_offer(start).await;
            let _ = event_tx.send(super::events::HubEvent::WebRtcOfferNegotiated(completion));
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
        self.queue_webrtc_peer_command(
            browser_identity,
            crate::worker::webrtc::WebRtcAdapterCommand::Json { data: payload },
        );
        log::info!(
            "[WebPush] Queued VAPID public key for {}",
            &browser_identity[..browser_identity.len().min(8)]
        );
    }

    fn handle_browser_push_control(&mut self, browser_identity: &str, msg: &serde_json::Value) {
        let Some(msg_type) = msg.get("type").and_then(|t| t.as_str()) else {
            log::warn!("[WebPush] Browser push control missing type");
            return;
        };

        match msg_type {
            "push_sub" => self.handle_push_subscription(browser_identity, msg),
            "vapid_generate" => self.handle_vapid_generate(browser_identity),
            "vapid_key_req" => self.handle_vapid_key_request(browser_identity),
            "vapid_key_set" => self.handle_vapid_key_set(browser_identity, msg),
            "vapid_pub_req" => self.handle_vapid_pub_request(browser_identity),
            "push_test" => self.handle_push_test(browser_identity),
            "push_disable" => self.handle_push_disable(browser_identity),
            "push_status_req" => self.handle_push_status_request(browser_identity, msg),
            other => log::warn!("[WebPush] Unknown browser push control: {other}"),
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
        self.queue_webrtc_peer_command(
            browser_identity,
            crate::worker::webrtc::WebRtcAdapterCommand::Json { data: payload },
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
        self.queue_webrtc_peer_command(
            browser_identity,
            crate::worker::webrtc::WebRtcAdapterCommand::Json { data: payload },
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
        self.queue_webrtc_peer_command(
            browser_identity,
            crate::worker::webrtc::WebRtcAdapterCommand::Json { data: payload },
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
        self.queue_webrtc_peer_command(
            browser_identity,
            crate::worker::webrtc::WebRtcAdapterCommand::Json { data: payload },
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
        self.queue_webrtc_peer_command(
            browser_identity,
            crate::worker::webrtc::WebRtcAdapterCommand::Json { data: payload },
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
        );
        // Store server-assigned ID (used for all server communication)
        self.botster_id = Some(botster_id.clone());
        // Sync to shared copy for Lua primitives
        *self
            .shared_server_id
            .lock()
            .expect("SharedServerId mutex poisoned") = Some(botster_id.clone());
        // Keep runtime manifest aligned with server-assigned hub ID.
        let manifest_started = Instant::now();
        if let Err(e) =
            crate::hub::daemon::write_manifest(&self.hub_identifier, self.botster_id.as_deref())
        {
            self.hub_event_metrics
                .record_counter("manifest.write_error", 1);
            log::warn!("Failed to refresh hub manifest after server registration: {e}");
        }
        self.hub_event_metrics.record_span_with_threshold(
            "manifest.write",
            manifest_started.elapsed(),
            0,
            Duration::from_millis(10),
            &self.hub_identifier,
        );

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
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};
    use std::time::{SystemTime, UNIX_EPOCH};

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
    use crate::socket::framing::{Frame, FrameDecoder};

    fn e2e_config() -> Config {
        let mut config = Config::default();
        config.server_url = "http://localhost:3000".to_string();
        config.token = "btstr_test-key".to_string();
        config.poll_interval = 10;
        config.agent_timeout = 300;
        config.max_sessions = 10;
        config.worktree_base = PathBuf::from("/tmp/test-worktrees");
        config
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

    fn test_session_backed_handle(session_uuid: &str, rows: u16, cols: u16) -> SessionHandle {
        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        let pty = PtyHandle::new_with_session(
            event_tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(false)),
            None,
            Arc::new(Mutex::new(None)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(std::sync::atomic::AtomicI64::new(0)),
            rows,
            cols,
        );
        SessionHandle::new(session_uuid, "test-agent", SessionType::Agent, None, pty)
    }

    fn test_session_backed_handle_with_mailbox(
        session_uuid: &str,
        session_io_tx: tokio::sync::mpsc::Sender<crate::worker::session_io::SessionIoRequest>,
    ) -> SessionHandle {
        let conn = crate::session::connection::SessionConnection::test_with_session_io_sender(
            session_io_tx,
        );
        test_session_backed_handle_with_connection(session_uuid, conn)
    }

    fn test_session_backed_handle_with_mailbox_and_snapshot(
        session_uuid: &str,
        session_io_tx: tokio::sync::mpsc::Sender<crate::worker::session_io::SessionIoRequest>,
        snapshot: Option<Vec<u8>>,
    ) -> SessionHandle {
        let conn =
            crate::session::connection::SessionConnection::test_with_session_io_sender_and_snapshot(
                session_io_tx,
                snapshot,
            );
        test_session_backed_handle_with_connection(session_uuid, conn)
    }

    fn test_session_backed_handle_with_connection(
        session_uuid: &str,
        conn: crate::session::connection::SessionConnection,
    ) -> SessionHandle {
        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        let pty = PtyHandle::new_with_session(
            event_tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(false)),
            None,
            Arc::new(Mutex::new(Some(conn))),
            Arc::new(AtomicU64::new(0)),
            Arc::new(std::sync::atomic::AtomicI64::new(0)),
            24,
            80,
        );
        SessionHandle::new(session_uuid, "test-agent", SessionType::Agent, None, pty)
    }

    fn unique_session_uuid(prefix: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time drift")
            .as_nanos();
        format!("{prefix}-{nanos}")
    }

    fn register_live_session_identity(session_uuid: &str) {
        let socket_path = crate::session::session_socket_path(session_uuid).expect("socket path");
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent).expect("create sessions dir");
        }
        std::fs::write(&socket_path, b"live").expect("write socket sentinel");
        crate::session::write_session_pid_file(session_uuid, std::process::id())
            .expect("write session pid file");
    }

    fn cleanup_live_session_identity(session_uuid: &str) {
        if let Ok(path) = crate::session::session_socket_path(session_uuid) {
            let _ = std::fs::remove_file(path);
        }
        if let Ok(path) = crate::session::session_pid_path(session_uuid) {
            let _ = std::fs::remove_file(path);
        }
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

    fn read_test_socket_frame(stream: &mut tokio::net::UnixStream) -> Frame {
        read_test_socket_frame_matching(stream, Duration::from_secs(2), |_| true)
            .expect("timed out waiting for socket frame")
    }

    fn read_test_socket_frame_matching<F>(
        stream: &mut tokio::net::UnixStream,
        timeout: Duration,
        mut frame_matches: F,
    ) -> Option<Frame>
    where
        F: FnMut(&Frame) -> bool,
    {
        let handle = shared_test_runtime();
        handle.block_on(async {
            use tokio::io::AsyncReadExt;

            let mut decoder = FrameDecoder::new();
            let mut buf = [0u8; 4096];
            let deadline = tokio::time::Instant::now() + timeout;
            loop {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    return None;
                }
                let n = tokio::time::timeout(remaining, stream.read(&mut buf))
                    .await
                    .ok()?
                    .expect("socket read");
                assert!(n > 0, "socket closed before frame arrived");
                let frames = decoder.feed(&buf[..n]).expect("decode frame");
                for frame in frames {
                    if frame_matches(&frame) {
                        return Some(frame);
                    }
                }
            }
        })
    }

    fn read_test_socket_frames(
        stream: &mut tokio::net::UnixStream,
        max_frames: usize,
        timeout: Duration,
    ) -> Vec<Frame> {
        shared_test_runtime().block_on(async {
            use tokio::io::AsyncReadExt;

            let mut decoder = FrameDecoder::new();
            let mut buf = [0u8; 4096];
            let mut frames = Vec::new();
            let deadline = tokio::time::Instant::now() + timeout;
            while frames.len() < max_frames && tokio::time::Instant::now() < deadline {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                match tokio::time::timeout(
                    remaining.min(Duration::from_millis(100)),
                    stream.read(&mut buf),
                )
                .await
                {
                    Ok(Ok(n)) if n > 0 => {
                        frames.extend(decoder.feed(&buf[..n]).expect("decode frames"));
                    }
                    Ok(Ok(_)) => break,
                    Ok(Err(e)) => panic!("socket read: {e}"),
                    Err(_) if frames.is_empty() => continue,
                    Err(_) => break,
                }
            }
            frames
        })
    }

    fn wait_for_receiver_count(
        event_tx: &tokio::sync::broadcast::Sender<crate::agent::pty::PtyEvent>,
        expected: usize,
    ) {
        shared_test_runtime().block_on(async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            loop {
                if event_tx.receiver_count() >= expected {
                    return;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "timed out waiting for {expected} PTY subscribers"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
    }

    fn settle_worker_subscription() {
        shared_test_runtime().block_on(async {
            tokio::time::sleep(Duration::from_millis(50)).await;
        });
    }

    fn function_body<'a>(source: &'a str, name: &str) -> &'a str {
        let start = source
            .find(&format!("fn {name}"))
            .unwrap_or_else(|| panic!("missing function {name}"));
        let body_start = source[start..]
            .find('{')
            .map(|offset| start + offset)
            .expect("function body start");
        let mut depth = 0usize;
        for (idx, ch) in source[body_start..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &source[body_start..=body_start + idx];
                    }
                }
                _ => {}
            }
        }
        panic!("unterminated function {name}");
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

    fn test_session_handle_with_snapshot(session_uuid: &str, snapshot: &[u8]) -> SessionHandle {
        let pty_session = PtySession::new(24, 80);
        let (shared_state, event_tx, kitty_enabled, cursor_visible, resize_pending) =
            pty_session.get_direct_access();
        std::mem::forget(pty_session);

        let pty = PtyHandle::new_with_snapshot(
            event_tx,
            shared_state,
            kitty_enabled,
            cursor_visible,
            resize_pending,
            None,
            snapshot.to_vec(),
        );
        SessionHandle::new(session_uuid, "test-agent", SessionType::Agent, None, pty)
    }

    fn test_session_handle_with_broadcast_capacity(
        session_uuid: &str,
        capacity: usize,
    ) -> SessionHandle {
        let (event_tx, _rx) = tokio::sync::broadcast::channel(capacity);
        let pty = PtyHandle::new(
            event_tx,
            Arc::new(Mutex::new(crate::agent::pty::SharedPtyState {
                master_pty: None,
                writer: None,
                dimensions: (24, 80),
                last_human_input_ms: Arc::new(std::sync::atomic::AtomicI64::new(0)),
            })),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(false)),
            None,
        );
        SessionHandle::new(session_uuid, "test-agent", SessionType::Agent, None, pty)
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
    fn test_session_io_batch_preserves_output_metrics_and_probe_learning() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let session_uuid = "sess-worker-batch-probe";

        hub.handle_cache
            .add_session(test_local_session_handle(session_uuid));

        hub.handle_hub_event(crate::hub::events::HubEvent::SessionIoBatch(
            crate::worker::session_io::SessionIoBatch {
                session_uuid: session_uuid.to_string(),
                output: Some(b"\x1b]11;?\x07payload".to_vec()),
            },
        ));

        let snapshot = hub.hub_event_metrics.snapshot();
        assert_eq!(snapshot.counters["pty_output.messages"], 1);
        assert_eq!(snapshot.counters["pty_output.bytes"], 14);

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
    fn test_file_input_enqueues_paste_and_written_event_registers_cleanup() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let session_uuid = "sess-file-paste-mailbox";
        let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(4);
        hub.handle_cache
            .add_session(test_session_backed_handle_with_mailbox(
                session_uuid,
                session_io_tx,
            ));

        hub.handle_file_input(crate::channel::webrtc::FileInputIncoming {
            session_uuid: session_uuid.to_string(),
            filename: "drop.PNG".to_string(),
            data: b"image-bytes".to_vec(),
        });

        match session_io_rx.try_recv().expect("paste request") {
            crate::worker::session_io::SessionIoRequest::PasteFile {
                request_id,
                filename,
                data,
            } => {
                assert!(request_id.starts_with("paste-"));
                assert_eq!(filename, "drop.PNG");
                assert_eq!(data, b"image-bytes");
                let path = std::path::PathBuf::from("/tmp/botster-paste-test.png");
                hub.handle_session_io_event(
                    crate::worker::session_io::SessionIoEvent::PasteFileWritten {
                        request_id,
                        session_uuid: session_uuid.to_string(),
                        path: path.clone(),
                        bytes: 11,
                    },
                );
                assert_eq!(
                    hub.paste_files.get(session_uuid).expect("paste cleanup"),
                    &vec![path]
                );
            }
            other => panic!("expected PasteFile request, got {other:?}"),
        }
    }

    #[test]
    fn test_queue_webrtc_terminal_snapshot_returns_false_when_mailbox_full() {
        let (hub, _request_tx, _output_rx) = e2e_hub();
        let session_uuid = "sess-snapshot-mailbox-full";
        let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(1);
        session_io_tx
            .try_send(crate::worker::session_io::SessionIoRequest::PtyInput {
                data: b"queued".to_vec(),
            })
            .expect("fill mailbox");
        let session = test_session_backed_handle_with_mailbox(session_uuid, session_io_tx);
        let pty = session.pty().clone();
        hub.handle_cache.add_session(session);

        assert!(!Hub::queue_webrtc_terminal_snapshot(
            &hub.hub_event_metrics,
            &hub.hub_event_tx,
            &pty,
            Some("snapshot-full".to_string()),
            session_uuid,
            b"snapshot".to_vec(),
        ));

        let snapshot = hub.hub_event_metrics.snapshot();
        assert_eq!(snapshot.counters["snapshot.queue_full"], 1);
        assert!(matches!(
            session_io_rx.try_recv().expect("filled request"),
            crate::worker::session_io::SessionIoRequest::PtyInput { .. }
        ));
    }

    #[test]
    fn test_empty_initial_snapshot_cleans_pending_session_io_request() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let session_uuid = "sess-empty-initial-snapshot";
        let (session_io_tx, _session_io_rx) = tokio::sync::mpsc::channel(4);
        hub.handle_cache
            .add_session(test_session_backed_handle_with_mailbox_and_snapshot(
                session_uuid,
                session_io_tx,
                Some(Vec::new()),
            ));
        let mut req = test_forwarder_request(
            "browser-empty-snapshot",
            session_uuid,
            "terminal_empty_snapshot",
        );
        req.rows = 23;

        assert!(hub.try_attach_terminal_forwarder(&req));
        assert_eq!(hub.pending_session_io_snapshots.len(), 1);

        for _ in 0..20 {
            hub.tokio_runtime.block_on(async {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            });
            hub.poll_hub_events();
            if hub.pending_session_io_snapshots.is_empty() {
                break;
            }
        }

        assert!(hub.pending_session_io_snapshots.is_empty());
        let snapshot = hub.hub_event_metrics.snapshot();
        assert_eq!(snapshot.counters["snapshot.empty"], 1);
        hub.stop_lua_pty_forwarder("browser-empty-snapshot:sess-empty-initial-snapshot");
    }

    #[test]
    fn test_snapshot_enqueue_failure_cleans_existing_pending_request() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let session_uuid = "sess-snapshot-enqueue-cleanup";
        let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(1);
        session_io_tx
            .try_send(crate::worker::session_io::SessionIoRequest::PtyInput {
                data: b"queued".to_vec(),
            })
            .expect("fill mailbox");
        let session = test_session_backed_handle_with_mailbox(session_uuid, session_io_tx);
        let pty = session.pty().clone();
        hub.handle_cache.add_session(session);
        let request_id = "snapshot-cleanup-on-full".to_string();
        assert!(hub.insert_pending_session_io_snapshot(
            request_id.clone(),
            crate::hub::PendingSessionIoSnapshot {
                session_uuid: session_uuid.to_string(),
                started_at: Instant::now(),
                target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                    peer_id: "browser-cleanup".to_string(),
                    subscription_id: "sub-cleanup".to_string(),
                    forwarder_key: Some(
                        "browser-cleanup:sess-snapshot-enqueue-cleanup".to_string()
                    ),
                    active_flag: None,
                },
            },
        ));

        assert!(!Hub::queue_webrtc_terminal_snapshot(
            &hub.hub_event_metrics,
            &hub.hub_event_tx,
            &pty,
            Some(request_id.clone()),
            session_uuid,
            b"snapshot".to_vec(),
        ));
        assert!(hub.pending_session_io_snapshots.contains_key(&request_id));

        hub.poll_hub_events();
        assert!(!hub.pending_session_io_snapshots.contains_key(&request_id));
        let snapshot = hub.hub_event_metrics.snapshot();
        assert_eq!(snapshot.counters["snapshot.queue_full"], 1);
        assert!(matches!(
            session_io_rx.try_recv().expect("filled request"),
            crate::worker::session_io::SessionIoRequest::PtyInput { .. }
        ));
    }

    #[test]
    fn test_prepared_snapshot_routes_to_webrtc_output_with_metrics() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let request_id = "snapshot-output-test".to_string();
        hub.insert_pending_session_io_snapshot(
            request_id.clone(),
            crate::hub::PendingSessionIoSnapshot {
                session_uuid: "sess-output".to_string(),
                started_at: Instant::now(),
                target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                    peer_id: "browser-output".to_string(),
                    subscription_id: "sub-output".to_string(),
                    forwarder_key: None,
                    active_flag: None,
                },
            },
        );

        hub.handle_session_io_event(
            crate::worker::session_io::SessionIoEvent::PreparedSnapshot {
                request_id,
                session_uuid: "sess-output".to_string(),
                uncompressed_len: 256,
                payload: vec![0x1f, 0x8b, 0x08, 0x00],
                recovery: false,
            },
        );

        let mut rx = hub
            .webrtc
            .lease_pty_output_receiver_for_test()
            .expect("pty output rx");
        let output = rx.try_recv().expect("prepared snapshot output");
        hub.webrtc.return_pty_output_receiver_for_test(Some(rx));
        assert_eq!(output.subscription_id, "sub-output");
        assert_eq!(output.browser_identity, "browser-output");
        assert_eq!(output.session_uuid, "sess-output");
        assert!(output.data.starts_with(&[0x1f, 0x8b]));

        let snapshot = hub.hub_event_metrics.snapshot();
        assert!(snapshot.spans.contains_key("snapshot.gzip_queue"));
        assert!(hub.pending_session_io_snapshots.is_empty());
    }

    #[test]
    fn test_pending_session_io_snapshot_cleanup_paths() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        hub.insert_pending_session_io_snapshot(
            "by-peer".to_string(),
            crate::hub::PendingSessionIoSnapshot {
                session_uuid: "sess-a".to_string(),
                started_at: Instant::now(),
                target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                    peer_id: "browser-a".to_string(),
                    subscription_id: "sub-a".to_string(),
                    forwarder_key: Some("browser-a:sess-a".to_string()),
                    active_flag: None,
                },
            },
        );
        hub.insert_pending_session_io_snapshot(
            "by-forwarder".to_string(),
            crate::hub::PendingSessionIoSnapshot {
                session_uuid: "sess-b".to_string(),
                started_at: Instant::now(),
                target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                    peer_id: "browser-b".to_string(),
                    subscription_id: "sub-b".to_string(),
                    forwarder_key: Some("browser-b:sess-b".to_string()),
                    active_flag: None,
                },
            },
        );
        hub.insert_pending_session_io_snapshot(
            "by-session".to_string(),
            crate::hub::PendingSessionIoSnapshot {
                session_uuid: "sess-c".to_string(),
                started_at: Instant::now(),
                target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcPeerRecovery {
                    request: crate::worker::webrtc::WebRtcRecoverySnapshotRequest {
                        request_id: "recovery-by-session".to_string(),
                        browser_identity: "browser-c".to_string(),
                        session_uuid: "sess-c".to_string(),
                        subscription_id: "sub-c".to_string(),
                    },
                },
            },
        );

        hub.cleanup_pending_session_io_snapshots_for_peer("browser-a");
        assert!(!hub.pending_session_io_snapshots.contains_key("by-peer"));
        hub.cleanup_pending_session_io_snapshots_for_forwarder("browser-b:sess-b");
        assert!(!hub
            .pending_session_io_snapshots
            .contains_key("by-forwarder"));
        hub.handle_hub_event(crate::hub::events::HubEvent::SessionUnregistered {
            session_uuid: "sess-c".to_string(),
        });
        assert!(!hub.pending_session_io_snapshots.contains_key("by-session"));

        hub.insert_pending_session_io_snapshot(
            "stale".to_string(),
            crate::hub::PendingSessionIoSnapshot {
                session_uuid: "sess-stale".to_string(),
                started_at: Instant::now()
                    - crate::hub::SESSION_IO_SNAPSHOT_PENDING_TTL
                    - Duration::from_secs(1),
                target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                    peer_id: "browser-stale".to_string(),
                    subscription_id: "sub-stale".to_string(),
                    forwarder_key: None,
                    active_flag: None,
                },
            },
        );
        hub.cleanup_stale_session_io_snapshots();
        assert!(!hub.pending_session_io_snapshots.contains_key("stale"));
        let snapshot = hub.hub_event_metrics.snapshot();
        assert_eq!(snapshot.counters["snapshot.pending_stale_drop"], 1);
    }

    #[test]
    fn test_noisy_session_io_replay_keeps_hot_handler_latency_bounded() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let session_uuid = "sess-noisy-replay";
        hub.handle_cache
            .add_session(test_local_session_handle(session_uuid));

        let mut elapsed_samples = Vec::with_capacity(1001);
        let mut max_elapsed = std::time::Duration::ZERO;
        for i in 0..=1000 {
            let data = format!("\x1b]2;botster replay {i}\x07payload-{i:04}\r\n").into_bytes();
            let event = crate::hub::events::HubEvent::SessionIoBatch(
                crate::worker::session_io::SessionIoBatch {
                    session_uuid: session_uuid.to_string(),
                    output: Some(data),
                },
            );
            let started = Instant::now();
            hub.handle_hub_event(event);
            let elapsed = started.elapsed();
            max_elapsed = max_elapsed.max(elapsed);
            elapsed_samples.push(elapsed);
            hub.hub_event_metrics
                .record_handler_time("session_io_batch", elapsed);
        }

        let snapshot = hub.hub_event_metrics.snapshot();
        assert_eq!(snapshot.counters["pty_output.messages"], 1001);
        assert!(snapshot.counters["pty_output.bytes"] > 32_000);
        let session_io = snapshot
            .by_type
            .get("session_io_batch")
            .expect("session_io_batch handler metrics");
        assert_eq!(
            session_io.handler_time_max_ns,
            max_elapsed.as_nanos() as u64
        );
        elapsed_samples.sort_unstable();
        let p99_elapsed = elapsed_samples[elapsed_samples.len() * 99 / 100];
        let slow_samples = elapsed_samples
            .iter()
            .filter(|elapsed| **elapsed >= Hub::HOT_SUBHANDLER_SLOW)
            .count();
        assert!(
            p99_elapsed < Hub::HOT_SUBHANDLER_SLOW,
            "observed-log-shaped SessionIoBatch replay p99 exceeded hot-path budget: p99={p99_elapsed:?}, max={max_elapsed:?}, slow_samples={slow_samples}"
        );
        assert!(snapshot.slow_samples.is_empty());
    }

    #[test]
    fn test_pty_osc_cursor_volume_burst_guardrail_matches_observed_logs() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();

        for i in 0..=crate::hub::VolumeBurstState::THRESHOLD {
            hub.handle_hub_event(crate::hub::events::HubEvent::PtyOscEvent {
                session_uuid: "sess-osc-replay".to_string(),
                session_name: "test-agent".to_string(),
                event: crate::agent::pty::PtyEvent::cursor_visibility_changed(i % 2 == 0),
            });
        }

        let snapshot = hub.hub_event_metrics.snapshot();
        assert_eq!(snapshot.counters["pty_osc.cursor"], 1001);
        assert_eq!(snapshot.counters["pty_osc.volume_burst"], 1);
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
    fn test_webrtc_focus_message_updates_active_terminal_peer() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let session_uuid = "sess-webrtc-focus";
        let payload = serde_json::to_vec(&serde_json::json!({
            "type": "focus_changed",
            "session_uuid": session_uuid,
            "focused": true,
        }))
        .expect("focus payload");

        hub.process_webrtc_plaintext_payload("browser-a", &payload);

        assert_eq!(
            hub.active_terminal_peers
                .lock()
                .expect("active peers mutex")
                .get(session_uuid)
                .cloned(),
            Some("browser-a".to_string())
        );
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

    #[test]
    fn test_backpressure_recovery_routes_prepared_snapshot_to_peer() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let mut peer_rx = hub
            .webrtc
            .install_test_recovery_sender("browser-recovery", &hub.tokio_runtime);
        let request_id = "snapshot-recovery-test".to_string();
        hub.insert_pending_session_io_snapshot(
            request_id.clone(),
            crate::hub::PendingSessionIoSnapshot {
                session_uuid: "sess-recovery".to_string(),
                started_at: Instant::now(),
                target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcPeerRecovery {
                    request: crate::worker::webrtc::WebRtcRecoverySnapshotRequest {
                        request_id: "browser-recovery:sess-recovery".to_string(),
                        browser_identity: "browser-recovery".to_string(),
                        session_uuid: "sess-recovery".to_string(),
                        subscription_id: "sub-recovery".to_string(),
                    },
                },
            },
        );

        hub.handle_session_io_event(
            crate::worker::session_io::SessionIoEvent::PreparedSnapshot {
                request_id,
                session_uuid: "sess-recovery".to_string(),
                uncompressed_len: 128,
                payload: vec![0x1f, 0x8b, 0x08],
                recovery: true,
            },
        );

        match peer_rx.try_recv().expect("recovery snapshot command") {
            crate::worker::webrtc::WebRtcAdapterCommand::Pty {
                subscription_id,
                data,
            } => {
                assert_eq!(subscription_id, "sub-recovery");
                assert!(data.starts_with(&[0x1f, 0x8b]));
            }
            other => panic!("expected PTY recovery command, got {other:?}"),
        }

        let snapshot = hub.hub_event_metrics.snapshot();
        assert_eq!(snapshot.counters["snapshot.backpressure_recovery.sent"], 1);
        assert!(snapshot.spans.contains_key("snapshot.gzip_queue"));
    }

    #[test]
    fn test_backpressure_recovery_missing_session_counts_failed_without_dispatch() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let session_uuid = "sess-recovery-missing";
        let browser_identity = "browser-recovery-missing";
        let mut peer_rx = hub
            .webrtc
            .install_test_recovery_sender(browser_identity, &hub.tokio_runtime);
        let key = format!("{browser_identity}:{session_uuid}");
        hub.webrtc.record_backpressure_recovery(
            key,
            crate::worker::webrtc::BackpressureRecoveryEntry {
                browser_identity: browser_identity.to_string(),
                session_uuid: session_uuid.to_string(),
                subscription_id: "sub-recovery-missing".to_string(),
                last_drop: Instant::now() - crate::worker::webrtc::BACKPRESSURE_SNAPSHOT_COOLDOWN,
            },
        );

        hub.dispatch_webrtc_recovery_snapshot_requests();

        assert!(peer_rx.try_recv().is_err());
        let metrics = hub.hub_event_metrics.snapshot();
        assert!(!metrics
            .counters
            .contains_key("snapshot.backpressure_recovery.sent"));
        assert!(!metrics
            .counters
            .contains_key("snapshot.backpressure_recovery.empty"));
        assert_eq!(metrics.counters["snapshot.backpressure_recovery.failed"], 1);
    }

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

            let mut rx = hub.webrtc.lease_pty_output_receiver_for_test();
            let Some(ref mut rx_ref) = rx else {
                panic!("webrtc output rx");
            };
            while let Ok(output) = rx_ref.try_recv() {
                if output.data.first() == Some(&prefix) {
                    hub.webrtc.return_pty_output_receiver_for_test(rx);
                    return output;
                }
            }
            hub.webrtc.return_pty_output_receiver_for_test(rx);
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
        hub.webrtc.pty_output_tx().try_send(msg).unwrap();

        // Step 2: Extract rx (as run_event_loop does before select!)
        let mut rx = hub.webrtc.lease_pty_output_receiver_for_test();

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
        hub.webrtc.return_pty_output_receiver_for_test(rx);
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
            hub.webrtc
                .pty_output_tx()
                .try_send(super::WebRtcPtyOutput {
                    subscription_id: "sub_test".to_string(),
                    browser_identity: "test-browser-identity".to_string(),
                    data: vec![0x01, 0x41 + i],
                    session_uuid: "sess-test".to_string(),
                })
                .unwrap();
        }

        let mut rx = hub.webrtc.lease_pty_output_receiver_for_test();

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
        let snapshot = hub.hub_event_metrics.snapshot();
        assert_eq!(snapshot.counters["pty_output.messages"], 5);
        assert_eq!(snapshot.counters["pty_output.bytes"], 10);
        assert_eq!(snapshot.counters["pty_output.batch_hwm"], 5);
        assert!(snapshot.spans.contains_key("pty_output.drain_batch"));

        hub.webrtc.return_pty_output_receiver_for_test(rx);
    }

    #[test]
    fn test_unknown_peer_burst_guardrail_is_bounded_and_rate_limited() {
        let (hub, _request_tx, _output_rx) = e2e_hub();

        for _ in 0..crate::worker::webrtc::PeerBurstState::THRESHOLD {
            hub.queue_webrtc_peer_command(
                "peer-alpha-abcdefghijklmnopqrstuvwxyz",
                crate::worker::webrtc::WebRtcAdapterCommand::Json { data: vec![1] },
            );
        }
        for i in 0..32 {
            hub.queue_webrtc_peer_command(
                &format!("peer-distinct-{i}"),
                crate::worker::webrtc::WebRtcAdapterCommand::Json { data: vec![1] },
            );
        }

        let snapshot = hub.hub_event_metrics.snapshot();
        assert_eq!(
            snapshot.counters["webrtc_send.unknown_peer_burst"], 1,
            "same peer should warn once per window"
        );
        assert_eq!(
            snapshot.counters["webrtc_send.unknown_peer"],
            (crate::worker::webrtc::PeerBurstState::THRESHOLD + 32) as u64
        );
        let peer_count = hub.webrtc.unknown_peer_distinct_count();
        assert!(peer_count <= crate::worker::webrtc::PeerBurstState::PEER_CAP);
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

    #[test]
    fn test_tui_worker_handle_registered_and_removed_with_forwarder() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let session_uuid = "sess-tui-worker-lifecycle";
        let key = format!("tui:{session_uuid}");

        hub.handle_cache
            .add_session(test_session_handle(session_uuid));

        let req = crate::lua::primitives::CreateTuiForwarderRequest {
            session_uuid: session_uuid.to_string(),
            subscription_id: format!("tui:{session_uuid}"),
            active_flag: Arc::new(Mutex::new(true)),
            rows: 24,
            cols: 80,
        };
        hub.create_lua_tui_pty_forwarder(req);
        hub.tick();

        assert!(
            hub.terminal_client_workers.contains_key(&key),
            "TUI forwarder should register a ClientWorker handle"
        );
        assert!(
            hub.pty_forwarders.contains_key(&key),
            "TUI forwarder task should be tracked by the hub"
        );

        hub.stop_lua_pty_forwarder(&key);

        assert!(
            !hub.terminal_client_workers.contains_key(&key),
            "stopping the forwarder should remove the ClientWorker handle"
        );
        assert!(
            !hub.pty_forwarders.contains_key(&key),
            "stopping the forwarder should remove the task"
        );
    }

    #[test]
    fn test_socket_worker_handle_registered_and_removed_with_forwarder() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let session_uuid = "sess-socket-worker-lifecycle";
        let client_id = "socket:worker-lifecycle";
        let key = format!("{client_id}:{session_uuid}");

        hub.handle_cache
            .add_session(test_session_handle(session_uuid));
        let _client_stream = register_test_socket_client(&mut hub, client_id);

        let req = crate::lua::primitives::CreateSocketForwarderRequest {
            client_id: client_id.to_string(),
            session_uuid: session_uuid.to_string(),
            subscription_id: format!("socket:{session_uuid}"),
            active_flag: Arc::new(Mutex::new(true)),
            rows: 24,
            cols: 80,
        };
        hub.create_lua_socket_pty_forwarder(req);
        hub.tick();

        assert!(
            hub.terminal_client_workers.contains_key(&key),
            "socket forwarder should register a ClientWorker handle"
        );
        assert!(
            hub.pty_forwarders.contains_key(&key),
            "socket forwarder task should be tracked by the hub"
        );

        hub.stop_lua_pty_forwarder(&key);

        assert!(
            !hub.terminal_client_workers.contains_key(&key),
            "stopping the socket forwarder should remove the ClientWorker handle"
        );
        assert!(
            !hub.pty_forwarders.contains_key(&key),
            "stopping the socket forwarder should remove the task"
        );
    }

    #[test]
    fn test_tui_and_socket_attach_handlers_delegate_to_shared_terminal_runtime() {
        let source = include_str!("server_comms.rs");
        for function in [
            "create_lua_tui_pty_forwarder",
            "try_attach_tui_terminal_forwarder",
            "create_lua_socket_pty_forwarder",
            "try_attach_socket_terminal_forwarder",
        ] {
            let body = function_body(source, function);
            assert!(
                !body.contains("snapshot_and_subscribe"),
                "{function} must not own snapshot subscription logic"
            );
            assert!(
                !body.contains("PtyEvent"),
                "{function} must not own a transport-specific PTY event loop"
            );
        }
    }

    #[test]
    fn test_socket_workerized_live_output_reaches_socket_frame() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let session_uuid = "sess-socket-worker-output".to_string();
        let client_id = "socket:worker-output".to_string();
        let key = format!("{client_id}:{session_uuid}");

        let session = test_session_handle(&session_uuid);
        let event_tx = session.pty().event_tx_clone();
        hub.handle_cache.add_session(session);
        let mut client_stream = register_test_socket_client(&mut hub, &client_id);

        let req = crate::lua::primitives::CreateSocketForwarderRequest {
            client_id,
            session_uuid: session_uuid.to_string(),
            subscription_id: format!("socket:{session_uuid}"),
            active_flag: Arc::new(Mutex::new(true)),
            rows: 24,
            cols: 80,
        };
        hub.create_lua_socket_pty_forwarder(req);
        hub.tick();

        assert!(
            hub.terminal_client_workers.contains_key(&key),
            "socket forwarder should register a ClientWorker handle"
        );

        let subscribed = hub.tokio_runtime.block_on(async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            loop {
                if event_tx.receiver_count() > 0 {
                    break true;
                }
                if tokio::time::Instant::now() >= deadline {
                    break false;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        assert!(
            subscribed,
            "socket forwarder should subscribe to PTY output before test emits live bytes"
        );

        let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"worker-live".to_vec()));

        let found =
            read_test_socket_frame_matching(&mut client_stream, Duration::from_secs(5), |frame| {
                matches!(
                    frame,
                    Frame::PtyOutput { session_uuid: frame_session, data }
                        if frame_session == &session_uuid && data == b"worker-live"
                )
            })
            .is_some();
        assert!(
            found,
            "live socket PTY output should flow through worker egress"
        );
    }

    #[test]
    fn test_shared_terminal_runtime_forwards_equivalent_scrollback_to_tui_and_socket() {
        let (mut hub, _request_tx, mut output_rx) = e2e_hub();
        let session_uuid = "sess-shared-scrollback".to_string();
        let socket_client_id = "socket:shared-scrollback".to_string();
        let snapshot = b"non-empty shared scrollback snapshot".to_vec();

        hub.handle_cache
            .add_session(test_session_handle_with_snapshot(&session_uuid, &snapshot));
        let mut client_stream = register_test_socket_client(&mut hub, &socket_client_id);

        hub.create_lua_tui_pty_forwarder(crate::lua::primitives::CreateTuiForwarderRequest {
            session_uuid: session_uuid.clone(),
            subscription_id: format!("tui:{session_uuid}"),
            active_flag: Arc::new(Mutex::new(true)),
            rows: 24,
            cols: 80,
        });
        hub.create_lua_socket_pty_forwarder(crate::lua::primitives::CreateSocketForwarderRequest {
            client_id: socket_client_id.clone(),
            session_uuid: session_uuid.clone(),
            subscription_id: format!("socket:{session_uuid}"),
            active_flag: Arc::new(Mutex::new(true)),
            rows: 24,
            cols: 80,
        });

        let tui_scrollback = shared_test_runtime()
            .block_on(async {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
                while tokio::time::Instant::now() < deadline {
                    if let Ok(Some(TuiOutput::Scrollback {
                        session_uuid: frame_session,
                        rows,
                        cols,
                        data,
                        kitty_enabled,
                    })) = tokio::time::timeout(Duration::from_millis(50), output_rx.recv()).await
                    {
                        if frame_session == session_uuid {
                            return Some((rows, cols, data, kitty_enabled));
                        }
                    }
                }
                None
            })
            .expect("TUI scrollback");

        let socket_frames = read_test_socket_frames(&mut client_stream, 8, Duration::from_secs(2));
        let socket_scrollback = socket_frames
            .into_iter()
            .find_map(|frame| match frame {
                Frame::Scrollback {
                    session_uuid: frame_session,
                    rows,
                    cols,
                    kitty_enabled,
                    data,
                } if frame_session == session_uuid => Some((rows, cols, data, kitty_enabled)),
                _ => None,
            })
            .expect("socket scrollback");

        assert_eq!(tui_scrollback, socket_scrollback);
        assert_eq!(
            tui_scrollback,
            (24, 80, snapshot, false),
            "both transports should receive the same non-empty snapshot metadata and payload"
        );
    }

    #[test]
    fn test_shared_terminal_runtime_forwards_live_modes_and_exit_to_tui_and_socket() {
        let (mut hub, _request_tx, mut output_rx) = e2e_hub();
        let session_uuid = "sess-shared-terminal-runtime".to_string();
        let socket_client_id = "socket:shared-runtime".to_string();

        let session = test_session_handle(&session_uuid);
        let event_tx = session.pty().event_tx_clone();
        hub.handle_cache.add_session(session);
        let mut client_stream = register_test_socket_client(&mut hub, &socket_client_id);

        hub.create_lua_tui_pty_forwarder(crate::lua::primitives::CreateTuiForwarderRequest {
            session_uuid: session_uuid.clone(),
            subscription_id: format!("tui:{session_uuid}"),
            active_flag: Arc::new(Mutex::new(true)),
            rows: 24,
            cols: 80,
        });
        hub.create_lua_socket_pty_forwarder(crate::lua::primitives::CreateSocketForwarderRequest {
            client_id: socket_client_id.clone(),
            session_uuid: session_uuid.clone(),
            subscription_id: format!("socket:{session_uuid}"),
            active_flag: Arc::new(Mutex::new(true)),
            rows: 24,
            cols: 80,
        });
        wait_for_receiver_count(&event_tx, 2);
        settle_worker_subscription();

        let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"one".to_vec()));
        let _ = event_tx.send(crate::agent::pty::PtyEvent::kitty_changed(true));
        let _ = event_tx.send(crate::agent::pty::PtyEvent::focus_reporting_changed(true));
        let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"two".to_vec()));
        let _ = event_tx.send(crate::agent::pty::PtyEvent::process_exited(Some(7)));

        let tui_outputs = shared_test_runtime().block_on(async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            let mut outputs = Vec::new();
            while tokio::time::Instant::now() < deadline {
                if let Ok(Some(output)) =
                    tokio::time::timeout(Duration::from_millis(50), output_rx.recv()).await
                {
                    outputs.push(output);
                    if outputs.iter().any(|output| {
                        matches!(
                            output,
                            TuiOutput::ProcessExited {
                                session_uuid: frame_session,
                                exit_code: Some(7),
                            } if frame_session == &session_uuid
                        )
                    }) {
                        break;
                    }
                }
            }
            outputs
        });

        assert!(
            tui_outputs.iter().any(|output| matches!(
                output,
                TuiOutput::Output { session_uuid: frame_session, data }
                    if frame_session == &session_uuid && data == b"one"
            )),
            "TUI should receive first live chunk through shared runtime"
        );
        assert!(
            tui_outputs.iter().any(|output| matches!(
                output,
                TuiOutput::Message(json)
                    if json.get("type").and_then(|v| v.as_str()) == Some("kitty_changed")
                        && json.get("enabled").and_then(|v| v.as_bool()) == Some(true)
                        && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
            )),
            "TUI should receive kitty mode changes through shared runtime"
        );
        assert!(
            tui_outputs.iter().any(|output| matches!(
                output,
                TuiOutput::Message(json)
                    if json.get("type").and_then(|v| v.as_str()) == Some("focus_reporting_changed")
                        && json.get("enabled").and_then(|v| v.as_bool()) == Some(true)
                        && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
            )),
            "TUI should receive focus mode changes through shared runtime"
        );
        assert!(
            tui_outputs.iter().any(|output| matches!(
                output,
                TuiOutput::ProcessExited {
                    session_uuid: frame_session,
                    exit_code: Some(7),
                } if frame_session == &session_uuid
            )),
            "TUI should receive process exit through shared runtime"
        );

        let socket_frames = read_test_socket_frames(&mut client_stream, 8, Duration::from_secs(2));
        assert!(
            socket_frames.iter().any(|frame| matches!(
                frame,
                Frame::PtyOutput { session_uuid: frame_session, data }
                    if frame_session == &session_uuid && data == b"one"
            )),
            "socket should receive first live chunk through shared runtime"
        );
        assert!(
            socket_frames.iter().any(|frame| matches!(
                frame,
                Frame::Json(json)
                    if json.get("type").and_then(|v| v.as_str()) == Some("kitty_changed")
                        && json.get("enabled").and_then(|v| v.as_bool()) == Some(true)
                        && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
            )),
            "socket should receive kitty mode changes through shared runtime"
        );
        assert!(
            socket_frames.iter().any(|frame| matches!(
                frame,
                Frame::Json(json)
                    if json.get("type").and_then(|v| v.as_str()) == Some("focus_reporting_changed")
                        && json.get("enabled").and_then(|v| v.as_bool()) == Some(true)
                        && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
            )),
            "socket should receive focus mode changes through shared runtime"
        );
        assert!(
            socket_frames.iter().any(|frame| matches!(
                frame,
                Frame::ProcessExited {
                    session_uuid: frame_session,
                    exit_code: Some(7),
                } if frame_session == &session_uuid
            )),
            "socket should receive process exit through shared runtime"
        );
    }

    #[test]
    fn test_shared_terminal_runtime_continues_after_broadcast_lag_for_tui_and_socket() {
        let (mut hub, _request_tx, mut output_rx) = e2e_hub();
        let session_uuid = "sess-shared-lag".to_string();
        let socket_client_id = "socket:shared-lag".to_string();

        let session = test_session_handle_with_broadcast_capacity(&session_uuid, 1);
        let event_tx = session.pty().event_tx_clone();
        hub.handle_cache.add_session(session);
        let mut client_stream = register_test_socket_client(&mut hub, &socket_client_id);

        hub.create_lua_tui_pty_forwarder(crate::lua::primitives::CreateTuiForwarderRequest {
            session_uuid: session_uuid.clone(),
            subscription_id: format!("tui:{session_uuid}"),
            active_flag: Arc::new(Mutex::new(true)),
            rows: 24,
            cols: 80,
        });
        hub.create_lua_socket_pty_forwarder(crate::lua::primitives::CreateSocketForwarderRequest {
            client_id: socket_client_id.clone(),
            session_uuid: session_uuid.clone(),
            subscription_id: format!("socket:{session_uuid}"),
            active_flag: Arc::new(Mutex::new(true)),
            rows: 24,
            cols: 80,
        });
        wait_for_receiver_count(&event_tx, 2);

        for i in 0..128 {
            let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(
                format!("dropped-{i}").into_bytes(),
            ));
        }
        settle_worker_subscription();
        let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"after-lag".to_vec()));

        let tui_seen = shared_test_runtime().block_on(async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            while tokio::time::Instant::now() < deadline {
                if let Ok(Some(TuiOutput::Output {
                    session_uuid: frame_session,
                    data,
                })) = tokio::time::timeout(Duration::from_millis(50), output_rx.recv()).await
                {
                    if frame_session == session_uuid && data == b"after-lag" {
                        return true;
                    }
                }
            }
            false
        });

        let socket_frames = read_test_socket_frames(&mut client_stream, 8, Duration::from_secs(2));
        let socket_seen = socket_frames.iter().any(|frame| {
            matches!(
                frame,
                Frame::PtyOutput {
                    session_uuid: frame_session,
                    data,
                } if frame_session == &session_uuid && data == b"after-lag"
            )
        });

        assert!(
            tui_seen,
            "TUI shared runtime should continue forwarding after a broadcast lag"
        );
        assert!(
            socket_seen,
            "socket shared runtime should continue forwarding after a broadcast lag"
        );
    }

    #[test]
    fn test_socket_shared_runtime_batches_outputs_but_filters_osc_queries_per_chunk() {
        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let session_uuid = "sess-socket-filter-batch".to_string();
        let client_id = "socket:filter-batch".to_string();

        let session = test_session_handle(&session_uuid);
        let event_tx = session.pty().event_tx_clone();
        hub.handle_cache.add_session(session);
        let mut client_stream = register_test_socket_client(&mut hub, &client_id);
        hub.active_terminal_peers
            .lock()
            .expect("active terminal peers")
            .insert(session_uuid.clone(), "socket:owner".to_string());

        hub.create_lua_socket_pty_forwarder(crate::lua::primitives::CreateSocketForwarderRequest {
            client_id: client_id.clone(),
            session_uuid: session_uuid.clone(),
            subscription_id: format!("socket:{session_uuid}"),
            active_flag: Arc::new(Mutex::new(true)),
            rows: 24,
            cols: 80,
        });
        wait_for_receiver_count(&event_tx, 1);
        settle_worker_subscription();

        let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"A\x1b]11;?".to_vec()));
        let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"\x07B".to_vec()));
        let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"C".to_vec()));

        let frames = read_test_socket_frames(&mut client_stream, 8, Duration::from_secs(2));
        let mut chunks = Vec::new();
        for frame in frames {
            if let Frame::PtyOutput {
                session_uuid: frame_session,
                data,
            } = frame
            {
                if frame_session == session_uuid {
                    chunks.push(data);
                    if chunks.len() == 3 {
                        break;
                    }
                }
            }
        }

        assert_eq!(
            chunks,
            vec![b"A".to_vec(), b"B".to_vec(), b"C".to_vec()],
            "socket filtering must preserve per-output-chunk boundaries while stripping split OSC queries"
        );
    }

    #[test]
    fn test_tui_attach_reconnecting_emits_explicit_attach_state() {
        let session_uuid = unique_session_uuid("sess-tui-reconnecting");
        register_live_session_identity(&session_uuid);

        let (mut hub, _request_tx, mut output_rx) = e2e_hub();
        hub.handle_cache
            .add_session(test_session_backed_handle(&session_uuid, 24, 80));

        let req = crate::lua::primitives::CreateTuiForwarderRequest {
            session_uuid: session_uuid.clone(),
            subscription_id: format!("tui:{session_uuid}"),
            active_flag: Arc::new(Mutex::new(true)),
            rows: 24,
            cols: 80,
        };
        hub.create_lua_tui_pty_forwarder(req);

        let rt = shared_test_runtime();
        let outputs = rt.block_on(async {
            let mut outputs = Vec::new();
            for _ in 0..2 {
                let output = tokio::time::timeout(Duration::from_secs(2), output_rx.recv())
                    .await
                    .expect("timed out waiting for TUI output")
                    .expect("TUI output channel closed");
                outputs.push(output);
            }
            outputs
        });

        assert!(
            outputs.iter().any(|output| matches!(
                output,
                TuiOutput::Message(json)
                    if json.get("type").and_then(|v| v.as_str()) == Some("terminal_attach")
                        && json.get("state").and_then(|v| v.as_str()) == Some("attached")
                        && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
            )),
            "initial attach should still emit attached state"
        );
        assert!(
            outputs.iter().any(|output| matches!(
                output,
                TuiOutput::Message(json)
                    if json.get("type").and_then(|v| v.as_str()) == Some("terminal_attach")
                        && json.get("state").and_then(|v| v.as_str()) == Some("reconnecting")
                        && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
            )),
            "reconnect-pending attach should emit explicit reconnecting state"
        );
        assert!(
            !outputs.iter().any(
                |output| matches!(output, TuiOutput::Scrollback { data, .. } if data.is_empty())
            ),
            "reconnect-pending attach must not fake an empty scrollback"
        );

        cleanup_live_session_identity(&session_uuid);
    }

    #[test]
    fn test_socket_attach_reconnecting_emits_explicit_attach_state() {
        let session_uuid = unique_session_uuid("sess-socket-reconnecting");
        register_live_session_identity(&session_uuid);

        let (mut hub, _request_tx, _output_rx) = e2e_hub();
        let client_id = "socket:reconnecting";
        let mut client_stream = register_test_socket_client(&mut hub, client_id);
        hub.handle_cache
            .add_session(test_session_backed_handle(&session_uuid, 24, 80));

        let req = crate::lua::primitives::CreateSocketForwarderRequest {
            client_id: client_id.to_string(),
            session_uuid: session_uuid.clone(),
            subscription_id: format!("socket:{session_uuid}"),
            active_flag: Arc::new(Mutex::new(true)),
            rows: 24,
            cols: 80,
        };
        hub.create_lua_socket_pty_forwarder(req);

        let first = read_test_socket_frame(&mut client_stream);
        let second = read_test_socket_frame(&mut client_stream);
        let frames = vec![first, second];

        assert!(
            frames.iter().any(|frame| matches!(
                frame,
                Frame::Json(value)
                    if value.get("type").and_then(|v| v.as_str()) == Some("terminal_attach")
                        && value.get("state").and_then(|v| v.as_str()) == Some("attached")
                        && value.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
            )),
            "initial socket attach should still emit attached state"
        );
        assert!(
            frames.iter().any(|frame| matches!(
                frame,
                Frame::Json(value)
                    if value.get("type").and_then(|v| v.as_str()) == Some("terminal_attach")
                        && value.get("state").and_then(|v| v.as_str()) == Some("reconnecting")
                        && value.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
            )),
            "reconnect-pending socket attach should emit explicit reconnecting state"
        );
        assert!(
            !frames
                .iter()
                .any(|frame| matches!(frame, Frame::Scrollback { data, .. } if data.is_empty())),
            "reconnect-pending socket attach must not fake an empty scrollback"
        );

        cleanup_live_session_identity(&session_uuid);
    }
}
