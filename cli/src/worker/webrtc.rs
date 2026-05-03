//! WebRTC transport adapter ownership.
//!
//! This module owns browser WebRTC peer state for the client-worker boundary.
//! The hub keeps orchestration policy, but per-peer channels, send queues,
//! reconnect generation, pending ICE, liveness tasks, and cleanup bookkeeping
//! live behind this registry.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::channel::Channel;
use crate::client::ClientId;

use super::client::{ClientControlFrame, ClientWorkerMessage};
use super::transport::{TransportAdapter, TransportEgress, TransportIngress};

/// Capacity for PTY output messages queued from forwarder tasks.
pub(crate) const WEBRTC_PTY_OUTPUT_QUEUE_CAPACITY: usize = 2048;
/// Capacity for outgoing ActionCable signaling envelopes from WebRTC tasks.
pub(crate) const WEBRTC_OUTGOING_SIGNAL_QUEUE_CAPACITY: usize = 512;
/// Capacity for incoming stream multiplexer frames.
pub(crate) const WEBRTC_STREAM_FRAME_QUEUE_CAPACITY: usize = 1024;
/// Capacity for binary PTY input frames from browsers.
pub(crate) const WEBRTC_PTY_INPUT_QUEUE_CAPACITY: usize = 2048;
/// Capacity for browser file-transfer frames.
pub(crate) const WEBRTC_FILE_INPUT_QUEUE_CAPACITY: usize = 128;
/// Capacity of the per-peer DataChannel send queue.
pub(crate) const PEER_SEND_CHANNEL_CAPACITY: usize = 256;
/// Timeout for individual DataChannel sends in per-peer tasks.
pub(crate) const PEER_SEND_TIMEOUT: Duration = Duration::from_secs(2);
/// Cooldown before sending a backpressure-recovery snapshot.
pub(crate) const BACKPRESSURE_SNAPSHOT_COOLDOWN: Duration = Duration::from_millis(500);

/// Item queued for a per-peer async WebRTC send task.
#[derive(Debug)]
pub(crate) enum WebRtcAdapterCommand {
    /// PTY output (hot path): subscription_id + raw data.
    Pty {
        /// Subscription ID for browser-side routing.
        subscription_id: String,
        /// Raw PTY data.
        data: Vec<u8>,
    },
    /// JSON control message.
    Json {
        /// Serialized JSON bytes.
        data: Vec<u8>,
    },
    /// Binary message.
    Binary {
        /// Raw binary data.
        data: Vec<u8>,
    },
    /// Stream multiplexer frame.
    Stream {
        /// Frame type byte.
        frame_type: u8,
        /// Stream identifier.
        stream_id: u16,
        /// Frame payload.
        payload: Vec<u8>,
    },
    /// Bundle refresh (ratchet restart, unencrypted).
    BundleRefresh {
        /// DeviceKeyBundle bytes.
        bundle_bytes: Vec<u8>,
    },
}

/// Per-peer send task state owned by the WebRTC registry.
pub(crate) struct PeerSendState {
    /// Bounded channel sender for queuing send items.
    pub tx: tokio::sync::mpsc::Sender<WebRtcAdapterCommand>,
    /// Set when the send task detects a dead peer.
    pub dead: Arc<AtomicBool>,
    /// Handle for the spawned send task.
    pub task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for PeerSendState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerSendState")
            .field(
                "dead",
                &self.dead.load(std::sync::atomic::Ordering::Relaxed),
            )
            .finish_non_exhaustive()
    }
}

/// Bounded unknown-peer send burst guardrail state.
#[derive(Debug, Default)]
pub(crate) struct PeerBurstState {
    entries: VecDeque<(String, Instant)>,
    warned: HashSet<String>,
}

impl PeerBurstState {
    pub(crate) const WINDOW: Duration = Duration::from_secs(30);
    pub(crate) const THRESHOLD: usize = 10;
    pub(crate) const PEER_CAP: usize = 16;

    pub(crate) fn record(&mut self, peer_id: &str, now: Instant) -> Option<(String, usize)> {
        let prefix = peer_id.chars().take(24).collect::<String>();
        while self
            .entries
            .front()
            .is_some_and(|(_, at)| now.duration_since(*at) > Self::WINDOW)
        {
            if let Some((old_prefix, _)) = self.entries.pop_front() {
                if !self.entries.iter().any(|(p, _)| p == &old_prefix) {
                    self.warned.remove(&old_prefix);
                }
            }
        }

        let distinct = self.entries.iter().map(|(p, _)| p).collect::<HashSet<_>>();
        if distinct.len() >= Self::PEER_CAP && !distinct.contains(&prefix) {
            return None;
        }

        self.entries.push_back((prefix.clone(), now));
        let count = self.entries.iter().filter(|(p, _)| p == &prefix).count();
        if count >= Self::THRESHOLD && self.warned.insert(prefix.clone()) {
            Some((prefix, count))
        } else {
            None
        }
    }

    #[cfg(test)]
    pub(crate) fn distinct_peer_count(&self) -> usize {
        self.entries
            .iter()
            .map(|(peer, _)| peer)
            .collect::<HashSet<_>>()
            .len()
    }
}

/// Result of attempting to queue a PTY frame for WebRTC delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebRtcSendOutcome {
    /// Frame queued successfully.
    Sent,
    /// Per-peer channel full; frame was dropped.
    Backpressure,
    /// Peer is dead or disconnected.
    Dead,
}

/// Entry tracking a peer+session that needs a recovery snapshot.
#[derive(Debug, Clone)]
pub(crate) struct BackpressureRecoveryEntry {
    pub(crate) browser_identity: String,
    pub(crate) session_uuid: String,
    pub(crate) subscription_id: String,
    pub(crate) last_drop: Instant,
}

/// Adapter-owned per-peer WebRTC registry.
#[derive(Debug)]
pub(crate) struct WebRtcPeerRegistry {
    channels: HashMap<String, crate::channel::WebRtcChannel>,
    connection_started: HashMap<String, Instant>,
    connected_at: HashMap<String, Instant>,
    send_tasks: HashMap<String, PeerSendState>,
    unknown_peer_bursts: std::sync::Mutex<PeerBurstState>,
    ping_tasks: HashMap<String, tokio::task::JoinHandle<()>>,
    pending_closes: HashMap<String, tokio::sync::watch::Receiver<bool>>,
    offer_generation: HashMap<String, u64>,
    pending_ice_candidates: HashMap<String, Vec<(u64, serde_json::Value)>>,
    pty_output_tx: tokio::sync::mpsc::Sender<crate::hub::WebRtcPtyOutput>,
    pty_output_rx: Option<tokio::sync::mpsc::Receiver<crate::hub::WebRtcPtyOutput>>,
    backpressure_recovery: HashMap<String, BackpressureRecoveryEntry>,
    outgoing_signal_tx: tokio::sync::mpsc::Sender<crate::channel::webrtc::OutgoingSignal>,
    outgoing_signal_rx: Option<tokio::sync::mpsc::Receiver<crate::channel::webrtc::OutgoingSignal>>,
    stream_frame_rx: Option<tokio::sync::mpsc::Receiver<crate::channel::webrtc::StreamIncoming>>,
    stream_frame_tx: tokio::sync::mpsc::Sender<crate::channel::webrtc::StreamIncoming>,
    pty_input_rx: Option<tokio::sync::mpsc::Receiver<crate::channel::webrtc::PtyInputIncoming>>,
    pty_input_tx: tokio::sync::mpsc::Sender<crate::channel::webrtc::PtyInputIncoming>,
    file_input_rx: Option<tokio::sync::mpsc::Receiver<crate::channel::webrtc::FileInputIncoming>>,
    file_input_tx: tokio::sync::mpsc::Sender<crate::channel::webrtc::FileInputIncoming>,
    runners: HashMap<String, WebRtcTransportRunner>,
}

/// Async peer runner ownership marker.
///
/// The production hub creates and drives runners through `WebRtcPeerRegistry`;
/// this struct keeps runner-scoped dependencies explicit as the registry
/// absorbs the remaining peer lifecycle methods.
#[derive(Debug, Clone)]
pub(crate) struct WebRtcTransportRunner {
    pub(crate) client_id: ClientId,
    pub(crate) browser_identity: String,
    pub(crate) generation: u64,
    pub(crate) command_tx: tokio::sync::mpsc::Sender<WebRtcAdapterCommand>,
    adapter: WebRtcTransportAdapter,
}

impl WebRtcTransportRunner {
    #[must_use]
    pub(crate) fn new(
        client_id: ClientId,
        browser_identity: String,
        generation: u64,
        command_tx: tokio::sync::mpsc::Sender<WebRtcAdapterCommand>,
    ) -> Self {
        let adapter = WebRtcTransportAdapter::new(client_id.clone());
        Self {
            client_id,
            browser_identity,
            generation,
            command_tx,
            adapter,
        }
    }

    pub(crate) fn ingress_to_client(&self, ingress: TransportIngress) -> ClientWorkerMessage {
        let _runner_scope = (
            &self.client_id,
            &self.browser_identity,
            self.generation,
            self.command_tx.capacity(),
        );
        self.adapter.ingress_to_client(ingress)
    }
}

impl WebRtcPeerRegistry {
    /// Build a registry with all hot-path queues bounded.
    #[must_use]
    pub(crate) fn new() -> Self {
        let (pty_output_tx, pty_output_rx) =
            tokio::sync::mpsc::channel(WEBRTC_PTY_OUTPUT_QUEUE_CAPACITY);
        let (outgoing_signal_tx, outgoing_signal_rx) =
            tokio::sync::mpsc::channel(WEBRTC_OUTGOING_SIGNAL_QUEUE_CAPACITY);
        let (stream_frame_tx, stream_frame_rx) =
            tokio::sync::mpsc::channel(WEBRTC_STREAM_FRAME_QUEUE_CAPACITY);
        let (pty_input_tx, pty_input_rx) =
            tokio::sync::mpsc::channel(WEBRTC_PTY_INPUT_QUEUE_CAPACITY);
        let (file_input_tx, file_input_rx) =
            tokio::sync::mpsc::channel(WEBRTC_FILE_INPUT_QUEUE_CAPACITY);

        Self {
            channels: HashMap::new(),
            connection_started: HashMap::new(),
            connected_at: HashMap::new(),
            send_tasks: HashMap::new(),
            unknown_peer_bursts: std::sync::Mutex::new(PeerBurstState::default()),
            ping_tasks: HashMap::new(),
            pending_closes: HashMap::new(),
            offer_generation: HashMap::new(),
            pending_ice_candidates: HashMap::new(),
            pty_output_tx,
            pty_output_rx: Some(pty_output_rx),
            backpressure_recovery: HashMap::new(),
            outgoing_signal_tx,
            outgoing_signal_rx: Some(outgoing_signal_rx),
            stream_frame_rx: Some(stream_frame_rx),
            stream_frame_tx,
            pty_input_rx: Some(pty_input_rx),
            pty_input_tx,
            file_input_rx: Some(file_input_rx),
            file_input_tx,
            runners: HashMap::new(),
        }
    }

    pub(crate) fn pty_output_tx(&self) -> tokio::sync::mpsc::Sender<crate::hub::WebRtcPtyOutput> {
        self.pty_output_tx.clone()
    }

    pub(crate) fn outgoing_signal_tx(
        &self,
    ) -> tokio::sync::mpsc::Sender<crate::channel::webrtc::OutgoingSignal> {
        self.outgoing_signal_tx.clone()
    }

    pub(crate) fn stream_frame_tx(
        &self,
    ) -> tokio::sync::mpsc::Sender<crate::channel::webrtc::StreamIncoming> {
        self.stream_frame_tx.clone()
    }

    pub(crate) fn pty_input_tx(
        &self,
    ) -> tokio::sync::mpsc::Sender<crate::channel::webrtc::PtyInputIncoming> {
        self.pty_input_tx.clone()
    }

    pub(crate) fn file_input_tx(
        &self,
    ) -> tokio::sync::mpsc::Sender<crate::channel::webrtc::FileInputIncoming> {
        self.file_input_tx.clone()
    }

    pub(crate) fn take_pty_input_rx(
        &mut self,
    ) -> Option<tokio::sync::mpsc::Receiver<crate::channel::webrtc::PtyInputIncoming>> {
        self.pty_input_rx.take()
    }

    pub(crate) fn restore_pty_input_rx(
        &mut self,
        rx: Option<tokio::sync::mpsc::Receiver<crate::channel::webrtc::PtyInputIncoming>>,
    ) {
        self.pty_input_rx = rx;
    }

    pub(crate) fn take_file_input_rx(
        &mut self,
    ) -> Option<tokio::sync::mpsc::Receiver<crate::channel::webrtc::FileInputIncoming>> {
        self.file_input_rx.take()
    }

    pub(crate) fn restore_file_input_rx(
        &mut self,
        rx: Option<tokio::sync::mpsc::Receiver<crate::channel::webrtc::FileInputIncoming>>,
    ) {
        self.file_input_rx = rx;
    }

    pub(crate) fn take_outgoing_signal_rx(
        &mut self,
    ) -> Option<tokio::sync::mpsc::Receiver<crate::channel::webrtc::OutgoingSignal>> {
        self.outgoing_signal_rx.take()
    }

    pub(crate) fn restore_outgoing_signal_rx(
        &mut self,
        rx: Option<tokio::sync::mpsc::Receiver<crate::channel::webrtc::OutgoingSignal>>,
    ) {
        self.outgoing_signal_rx = rx;
    }

    pub(crate) fn take_pty_output_rx(
        &mut self,
    ) -> Option<tokio::sync::mpsc::Receiver<crate::hub::WebRtcPtyOutput>> {
        self.pty_output_rx.take()
    }

    pub(crate) fn restore_pty_output_rx(
        &mut self,
        rx: Option<tokio::sync::mpsc::Receiver<crate::hub::WebRtcPtyOutput>>,
    ) {
        self.pty_output_rx = rx;
    }

    pub(crate) fn take_stream_frame_rx(
        &mut self,
    ) -> Option<tokio::sync::mpsc::Receiver<crate::channel::webrtc::StreamIncoming>> {
        self.stream_frame_rx.take()
    }

    pub(crate) fn restore_stream_frame_rx(
        &mut self,
        rx: Option<tokio::sync::mpsc::Receiver<crate::channel::webrtc::StreamIncoming>>,
    ) {
        self.stream_frame_rx = rx;
    }

    pub(crate) fn send_item_len(item: &WebRtcAdapterCommand) -> usize {
        match item {
            WebRtcAdapterCommand::Pty { data, .. }
            | WebRtcAdapterCommand::Json { data }
            | WebRtcAdapterCommand::Binary { data }
            | WebRtcAdapterCommand::Stream { payload: data, .. } => data.len(),
            WebRtcAdapterCommand::BundleRefresh { bundle_bytes } => bundle_bytes.len(),
        }
    }

    pub(crate) fn channel_count(&self) -> usize {
        self.channels.len()
    }

    #[cfg(test)]
    pub(crate) fn unknown_peer_distinct_count(&self) -> usize {
        self.unknown_peer_bursts
            .lock()
            .expect("unknown peer burst mutex poisoned")
            .distinct_peer_count()
    }

    pub(crate) fn has_channel(&self, browser_identity: &str) -> bool {
        self.channels.contains_key(browser_identity)
    }

    pub(crate) fn remove_channel(
        &mut self,
        browser_identity: &str,
    ) -> Option<crate::channel::WebRtcChannel> {
        self.channels.remove(browser_identity)
    }

    pub(crate) fn insert_channel(
        &mut self,
        browser_identity: String,
        channel: crate::channel::WebRtcChannel,
    ) -> Option<crate::channel::WebRtcChannel> {
        self.channels.insert(browser_identity, channel)
    }

    pub(crate) fn mark_connecting(&mut self, browser_identity: String) {
        self.connection_started
            .insert(browser_identity, Instant::now());
    }

    pub(crate) fn mark_connected(&mut self, browser_identity: String) {
        self.connected_at.insert(browser_identity, Instant::now());
    }

    pub(crate) fn same_device_channels(&self, browser_identity: &str) -> Vec<String> {
        let olm_key = crate::relay::extract_olm_key(browser_identity);
        self.channels
            .keys()
            .filter(|id| {
                id.as_str() != browser_identity && crate::relay::extract_olm_key(id) == olm_key
            })
            .cloned()
            .collect()
    }

    pub(crate) fn take_pending_close_for_olm(
        &mut self,
        olm_key: &str,
    ) -> Option<tokio::sync::watch::Receiver<bool>> {
        self.pending_closes.remove(olm_key)
    }

    pub(crate) fn next_offer_generation(&mut self, browser_identity: &str) -> u64 {
        let entry = self
            .offer_generation
            .entry(browser_identity.to_string())
            .or_insert(0);
        *entry += 1;
        *entry
    }

    pub(crate) fn current_offer_generation(&self, browser_identity: &str) -> u64 {
        self.offer_generation
            .get(browser_identity)
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn clear_offer_state(&mut self, browser_identity: &str) {
        self.connection_started.remove(browser_identity);
        self.offer_generation.remove(browser_identity);
        self.pending_ice_candidates.remove(browser_identity);
    }

    pub(crate) fn queue_ice_for_current_generation(
        &mut self,
        browser_identity: &str,
        candidate: serde_json::Value,
        max_queued: usize,
    ) -> Option<usize> {
        let current_generation = self.current_offer_generation(browser_identity);
        if current_generation == 0 {
            return None;
        }
        let queue = self
            .pending_ice_candidates
            .entry(browser_identity.to_string())
            .or_default();
        queue.push((current_generation, candidate));
        if queue.len() > max_queued {
            let dropped = queue.len() - max_queued;
            queue.drain(..dropped);
        }
        Some(queue.len())
    }

    pub(crate) fn drain_pending_ice(
        &mut self,
        browser_identity: &str,
    ) -> Option<Vec<(u64, serde_json::Value)>> {
        self.pending_ice_candidates.remove(browser_identity)
    }

    pub(crate) fn start_recv_forwarder(
        &self,
        browser_identity: &str,
        runtime: &tokio::runtime::Runtime,
        hub_event_tx: crate::hub::events::HubEventTx,
    ) -> bool {
        let Some(channel) = self.channels.get(browser_identity) else {
            return false;
        };

        let recv_rx_arc = channel.recv_rx_arc();
        let bi = browser_identity.to_string();
        let handle = runtime.handle().clone();
        handle.spawn(async move {
            let mut rx = {
                let mut guard = recv_rx_arc.lock().await;
                match guard.take() {
                    Some(rx) => rx,
                    None => return,
                }
            };
            while let Some(raw) = rx.recv().await {
                if hub_event_tx
                    .send(crate::hub::events::HubEvent::WebRtcMessage {
                        browser_identity: bi.clone(),
                        payload: raw.payload,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        true
    }

    pub(crate) fn apply_browser_ice_candidate(
        &self,
        browser_identity: &str,
        candidate_str: &str,
        sdp_mid: Option<&str>,
        sdp_mline_index: Option<u16>,
        runtime: &tokio::runtime::Runtime,
    ) -> Option<Result<(), crate::channel::ChannelError>> {
        let channel = self.channels.get(browser_identity)?;
        Some(tokio::task::block_in_place(|| {
            runtime.block_on(channel.handle_ice_candidate(candidate_str, sdp_mid, sdp_mline_index))
        }))
    }

    pub(crate) fn apply_pending_ice_candidates<F>(
        &self,
        browser_identity: &str,
        offer_generation: u64,
        candidates: Vec<(u64, serde_json::Value)>,
        runtime: &tokio::runtime::Runtime,
        mut on_error: F,
    ) where
        F: FnMut(u64, &str, Option<&str>, Option<u16>, crate::channel::ChannelError),
    {
        let Some(channel) = self.channels.get(browser_identity) else {
            return;
        };

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
                Some((
                    candidate_generation,
                    candidate_str,
                    sdp_mid,
                    sdp_mline_index,
                ))
            })
            .collect();

        if valid.is_empty() {
            return;
        }

        tokio::task::block_in_place(|| {
            runtime.block_on(async {
                for (generation, candidate_str, sdp_mid, sdp_mline_index) in &valid {
                    if let Err(error) = channel
                        .handle_ice_candidate(candidate_str, sdp_mid.as_deref(), *sdp_mline_index)
                        .await
                    {
                        on_error(
                            *generation,
                            candidate_str,
                            sdp_mid.as_deref(),
                            *sdp_mline_index,
                            error,
                        );
                    }
                }
            });
        });
    }

    pub(crate) fn poll_received_messages(
        &self,
        runtime: &tokio::runtime::Runtime,
    ) -> Vec<(String, Vec<u8>)> {
        let mut messages = Vec::new();
        for (browser_identity, channel) in &self.channels {
            while let Some(message) = channel.try_recv(runtime) {
                messages.push((browser_identity.clone(), message.payload));
            }
        }
        messages
    }

    pub(crate) fn drain_decrypt_failure_triggers(&self) -> Vec<String> {
        self.channels
            .iter()
            .filter_map(|(browser_identity, channel)| {
                let failures = channel.decrypt_failure_count();
                if failures >= 3 {
                    channel.reset_decrypt_failures();
                    Some(browser_identity.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    pub(crate) fn take_opened_peers(&self) -> Vec<String> {
        self.channels
            .iter()
            .filter_map(|(browser_identity, channel)| {
                channel.take_dc_opened().then(|| browser_identity.clone())
            })
            .collect()
    }

    pub(crate) fn ingress_to_client(
        &self,
        browser_identity: &str,
        ingress: TransportIngress,
    ) -> ClientWorkerMessage {
        self.runners.get(browser_identity).map_or_else(
            || {
                WebRtcTransportAdapter::new(ClientId::browser(browser_identity.to_string()))
                    .ingress_to_client(ingress.clone())
            },
            |runner| runner.ingress_to_client(ingress.clone()),
        )
    }

    pub(crate) fn queue_pty_frame(
        &self,
        subscription_id: &str,
        browser_identity: &str,
        data: Vec<u8>,
        metrics: &crate::hub::events::HubEventMetrics,
    ) -> WebRtcSendOutcome {
        let Some(state) = self.send_tasks.get(browser_identity) else {
            metrics.record_counter("webrtc_send.dead_peer", 1);
            return WebRtcSendOutcome::Dead;
        };

        if state.dead.load(std::sync::atomic::Ordering::Relaxed) {
            metrics.record_counter("webrtc_send.dead_peer", 1);
            return WebRtcSendOutcome::Dead;
        }

        match state.tx.try_send(WebRtcAdapterCommand::Pty {
            subscription_id: subscription_id.to_string(),
            data,
        }) {
            Ok(()) => {
                metrics.record_counter("webrtc_send.queued", 1);
                WebRtcSendOutcome::Sent
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                metrics.record_counter("webrtc_send.full", 1);
                log::warn!(
                    "[WebRTC] Backpressure: send channel full for peer {}, dropping PTY frame for subscription {}",
                    &browser_identity[..browser_identity.len().min(8)],
                    &subscription_id[..subscription_id.len().min(20)]
                );
                WebRtcSendOutcome::Backpressure
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                metrics.record_counter("webrtc_send.closed", 1);
                state.dead.store(true, std::sync::atomic::Ordering::Relaxed);
                WebRtcSendOutcome::Dead
            }
        }
    }

    pub(crate) fn queue_command(
        &self,
        peer_id: &str,
        item: WebRtcAdapterCommand,
        metrics: &crate::hub::events::HubEventMetrics,
    ) {
        let bytes = Self::send_item_len(&item);
        let Some(state) = self.send_tasks.get(peer_id) else {
            metrics.record_counter("webrtc_send.unknown_peer", 1);
            let burst = self
                .unknown_peer_bursts
                .lock()
                .ok()
                .and_then(|mut guard| guard.record(peer_id, Instant::now()));
            if let Some((prefix, count)) = burst {
                metrics.record_counter("webrtc_send.unknown_peer_burst", 1);
                log::warn!(
                    "[WebRTC-Guardrail] event=unknown_peer_burst peer={} count={} window_ms=30000",
                    prefix,
                    count
                );
            } else {
                log::debug!(
                    "[WebRTC] Send to unknown/disconnected peer: {}",
                    &peer_id[..peer_id.len().min(8)]
                );
            }
            return;
        };

        if state.dead.load(std::sync::atomic::Ordering::Relaxed) {
            metrics.record_counter("webrtc_send.dead_peer", 1);
            log::debug!(
                "[WebRTC] Send to dead peer: {}",
                &peer_id[..peer_id.len().min(8)]
            );
            return;
        }

        match state.tx.try_send(item) {
            Ok(()) => {
                metrics.record_counter("webrtc_send.queued", 1);
                metrics.record_span("webrtc_send.queue", Duration::ZERO, bytes);
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                metrics.record_counter("webrtc_send.full", 1);
                log::debug!(
                    "[WebRTC] Send channel full for {}, dropping frame",
                    &peer_id[..peer_id.len().min(8)]
                );
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                metrics.record_counter("webrtc_send.closed", 1);
                state.dead.store(true, std::sync::atomic::Ordering::Relaxed);
                log::debug!(
                    "[WebRTC] Send channel closed for {}, marking peer dead",
                    &peer_id[..peer_id.len().min(8)]
                );
            }
        }
    }

    pub(crate) fn record_backpressure_recovery(
        &mut self,
        key: String,
        entry: BackpressureRecoveryEntry,
    ) {
        self.backpressure_recovery.insert(key, entry);
    }

    pub(crate) fn cooled_backpressure_recoveries(
        &self,
        now: Instant,
    ) -> Vec<(String, BackpressureRecoveryEntry)> {
        self.backpressure_recovery
            .iter()
            .filter(|(_, entry)| {
                now.duration_since(entry.last_drop) >= BACKPRESSURE_SNAPSHOT_COOLDOWN
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub(crate) fn take_recovery_sender(
        &mut self,
        key: &str,
        browser_identity: &str,
    ) -> Option<tokio::sync::mpsc::Sender<WebRtcAdapterCommand>> {
        let peer_state = self.send_tasks.get(browser_identity)?;
        if peer_state.dead.load(std::sync::atomic::Ordering::Relaxed)
            || peer_state.tx.capacity() == 0
        {
            return None;
        }
        let tx = peer_state.tx.clone();
        self.backpressure_recovery.remove(key);
        Some(tx)
    }

    pub(crate) fn spawn_peer_sender(
        &mut self,
        browser_identity: &str,
        runtime: &tokio::runtime::Runtime,
        metrics: Arc<crate::hub::events::HubEventMetrics>,
    ) {
        if let Some(old) = self.send_tasks.remove(browser_identity) {
            old.task.abort();
        }

        let Some(channel) = self.channels.get(browser_identity) else {
            return;
        };

        let sender = channel.sender();
        let (tx, mut rx) =
            tokio::sync::mpsc::channel::<WebRtcAdapterCommand>(PEER_SEND_CHANNEL_CAPACITY);
        let dead = Arc::new(AtomicBool::new(false));
        let dead_clone = Arc::clone(&dead);
        let bi = browser_identity.to_string();

        let task = runtime.spawn(async move {
            while let Some(item) = rx.recv().await {
                let result =
                    tokio::time::timeout(PEER_SEND_TIMEOUT, execute_adapter_command(&sender, item))
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
                            metrics.record_counter("webrtc_send.dead_peer", 1);
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
                        metrics.record_counter("webrtc_send.dead_peer", 1);
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

        self.send_tasks.insert(
            browser_identity.to_string(),
            PeerSendState { tx, dead, task },
        );
        let generation = self.current_offer_generation(browser_identity);
        let runner_tx = self
            .send_tasks
            .get(browser_identity)
            .map(|state| state.tx.clone())
            .expect("send task inserted");
        self.runners.insert(
            browser_identity.to_string(),
            WebRtcTransportRunner::new(
                ClientId::browser(browser_identity.to_string()),
                browser_identity.to_string(),
                generation,
                runner_tx,
            ),
        );
    }

    pub(crate) fn spawn_liveness_probe(
        &mut self,
        browser_identity: &str,
        runtime: &tokio::runtime::Runtime,
    ) {
        const DC_PING_INTERVAL: Duration = Duration::from_secs(10);

        if let Some(old) = self.ping_tasks.remove(browser_identity) {
            old.abort();
        }

        let Some(state) = self.send_tasks.get(browser_identity) else {
            return;
        };
        let tx = state.tx.clone();
        let bi = browser_identity.to_string();
        let ping_payload = serde_json::to_vec(&serde_json::json!({ "type": "dc_ping" }))
            .expect("static JSON serialization cannot fail");

        let task = runtime.spawn(async move {
            let mut interval = tokio::time::interval(DC_PING_INTERVAL);
            interval.tick().await;

            loop {
                interval.tick().await;
                let item = WebRtcAdapterCommand::Json {
                    data: ping_payload.clone(),
                };
                if tx.send(item).await.is_err() {
                    log::debug!(
                        "[WebRTC] DC ping task exiting for {} (channel closed)",
                        &bi[..bi.len().min(8)]
                    );
                    break;
                }
            }
        });

        self.ping_tasks.insert(browser_identity.to_string(), task);
    }

    pub(crate) fn cleanup_scan(&mut self, timeout: Duration) -> Vec<(String, &'static str)> {
        use crate::channel::ConnectionState;

        let now = Instant::now();
        let to_cleanup: Vec<(String, &'static str)> = self
            .channels
            .iter()
            .filter_map(|(id, ch)| {
                let state = ch.state();
                if state == ConnectionState::Disconnected {
                    Some((id.clone(), "disconnected"))
                } else if state == ConnectionState::Connecting {
                    if let Some(started) = self.connection_started.get(id) {
                        if now.duration_since(*started) > timeout {
                            return Some((id.clone(), "timeout"));
                        }
                    }
                    None
                } else {
                    None
                }
            })
            .collect();

        let connected: Vec<String> = self
            .channels
            .iter()
            .filter(|(_, ch)| ch.state() == ConnectionState::Connected)
            .map(|(id, _)| id.clone())
            .collect();
        for id in connected {
            self.connection_started.remove(&id);
            self.connected_at.entry(id).or_insert_with(Instant::now);
        }

        self.pending_closes
            .retain(|_, close_rx| !*close_rx.borrow());
        to_cleanup
    }

    pub(crate) fn cleanup_peer_transport(
        &mut self,
        browser_identity: &str,
        runtime: &tokio::runtime::Runtime,
    ) -> Option<TransportCleanup> {
        let mut channel = self.channels.remove(browser_identity)?;
        let connected_age = self
            .connected_at
            .remove(browser_identity)
            .map(|connected_at| connected_at.elapsed());

        let close_rx = channel.close_receiver();
        let olm_key = crate::relay::extract_olm_key(browser_identity).to_string();
        self.pending_closes.insert(olm_key, close_rx);

        runtime.spawn(async move {
            channel.disconnect().await;
            log::debug!("[WebRTC] Channel disconnect completed");
        });

        self.connection_started.remove(browser_identity);
        self.offer_generation.remove(browser_identity);
        self.pending_ice_candidates.remove(browser_identity);

        if let Some(state) = self.send_tasks.remove(browser_identity) {
            drop(state.tx);
            state.task.abort();
        }

        self.backpressure_recovery
            .retain(|_, entry| entry.browser_identity != browser_identity);

        if let Some(task) = self.ping_tasks.remove(browser_identity) {
            task.abort();
        }

        Some(TransportCleanup { connected_age })
    }

    pub(crate) fn shutdown(&mut self, runtime: &tokio::runtime::Runtime) {
        for (_id, state) in self.send_tasks.drain() {
            drop(state.tx);
            state.task.abort();
        }

        for (_id, mut channel) in self.channels.drain() {
            runtime.spawn(async move {
                channel.disconnect().await;
            });
        }

        for (_id, task) in self.ping_tasks.drain() {
            task.abort();
        }

        self.connection_started.clear();
        self.connected_at.clear();
        self.pending_ice_candidates.clear();
        self.pending_closes.clear();
        self.offer_generation.clear();
        self.backpressure_recovery.clear();
        self.runners.clear();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TransportCleanup {
    pub(crate) connected_age: Option<Duration>,
}

/// Execute a single queued WebRTC send command.
pub(crate) async fn execute_adapter_command(
    sender: &crate::channel::webrtc::WebRtcSender,
    command: WebRtcAdapterCommand,
) -> Result<(), crate::channel::ChannelError> {
    match command {
        WebRtcAdapterCommand::Pty {
            subscription_id,
            data,
        } => sender.send_pty_raw(&subscription_id, &data).await,
        WebRtcAdapterCommand::Json { data } => sender.send_json(&data).await,
        WebRtcAdapterCommand::Binary { data } => sender.send_message_raw(&data).await,
        WebRtcAdapterCommand::Stream {
            frame_type,
            stream_id,
            payload,
        } => {
            sender
                .send_stream_raw(frame_type, stream_id, &payload)
                .await
        }
        WebRtcAdapterCommand::BundleRefresh { bundle_bytes } => {
            sender.send_bundle_refresh(&bundle_bytes).await
        }
    }
}

/// Synchronous WebRTC adapter conversion boundary.
#[derive(Debug, Clone)]
pub(crate) struct WebRtcTransportAdapter {
    client_id: ClientId,
}

impl WebRtcTransportAdapter {
    #[must_use]
    pub(crate) fn new(client_id: ClientId) -> Self {
        Self { client_id }
    }
}

impl TransportAdapter for WebRtcTransportAdapter {
    fn client_id(&self) -> &ClientId {
        &self.client_id
    }

    fn ingress_to_client(&self, ingress: TransportIngress) -> ClientWorkerMessage {
        match ingress {
            TransportIngress::Subscribe {
                session_uuid,
                subscription_id,
            } => ClientWorkerMessage::SubscribeSession {
                session_uuid,
                subscription_id,
            },
            TransportIngress::Unsubscribe {
                session_uuid,
                subscription_id,
            } => ClientWorkerMessage::UnsubscribeSession {
                session_uuid,
                subscription_id,
            },
            TransportIngress::TerminalInput { session_uuid, data } => {
                ClientWorkerMessage::SessionInput { session_uuid, data }
            }
            TransportIngress::Json(value) => {
                ClientWorkerMessage::ControlFrame(ClientControlFrame::Json(value))
            }
            TransportIngress::Binary(data) => {
                ClientWorkerMessage::ControlFrame(ClientControlFrame::Binary(data))
            }
            TransportIngress::Health(health) => ClientWorkerMessage::Health(health),
        }
    }

    fn client_to_egress(&self, message: ClientWorkerMessage) -> Option<TransportEgress> {
        match message {
            ClientWorkerMessage::TerminalBytes { session_uuid, data } => {
                Some(TransportEgress::TerminalBytes {
                    subscription_id: self.client_id.to_string(),
                    session_uuid,
                    data,
                })
            }
            ClientWorkerMessage::ControlFrame(ClientControlFrame::Json(value)) => {
                Some(TransportEgress::Control(value))
            }
            ClientWorkerMessage::Shutdown { reason } => Some(TransportEgress::Close { reason }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_uses_bounded_queues() {
        let registry = WebRtcPeerRegistry::new();

        assert_eq!(
            registry.pty_output_tx.max_capacity(),
            WEBRTC_PTY_OUTPUT_QUEUE_CAPACITY
        );
        assert_eq!(
            registry.outgoing_signal_tx.max_capacity(),
            WEBRTC_OUTGOING_SIGNAL_QUEUE_CAPACITY
        );
        assert_eq!(
            registry.stream_frame_tx.max_capacity(),
            WEBRTC_STREAM_FRAME_QUEUE_CAPACITY
        );
        assert_eq!(
            registry.pty_input_tx.max_capacity(),
            WEBRTC_PTY_INPUT_QUEUE_CAPACITY
        );
        assert_eq!(
            registry.file_input_tx.max_capacity(),
            WEBRTC_FILE_INPUT_QUEUE_CAPACITY
        );
    }

    #[test]
    fn adapter_converts_webrtc_ingress_without_async_trait_changes() {
        let adapter = WebRtcTransportAdapter::new(ClientId::Browser("browser-1".to_string()));

        let message = adapter.ingress_to_client(TransportIngress::TerminalInput {
            session_uuid: "sess-1".to_string(),
            data: b"abc".to_vec(),
        });

        assert!(matches!(
            message,
            ClientWorkerMessage::SessionInput { ref session_uuid, ref data }
                if session_uuid == "sess-1" && data == b"abc"
        ));
    }

    #[test]
    fn unknown_peer_burst_is_bounded_and_coalesced() {
        let mut burst = PeerBurstState::default();
        let now = Instant::now();
        let mut emitted = None;

        for _ in 0..PeerBurstState::THRESHOLD {
            emitted = burst.record("peer:abc", now);
        }

        assert_eq!(
            emitted,
            Some(("peer:abc".to_string(), PeerBurstState::THRESHOLD))
        );
        assert_eq!(burst.record("peer:abc", now), None);
    }

    #[test]
    fn registry_unknown_peer_burst_is_coalesced_on_send_path() {
        let registry = WebRtcPeerRegistry::new();
        let metrics = crate::hub::events::HubEventMetrics::default();

        for _ in 0..PeerBurstState::THRESHOLD {
            registry.queue_command(
                "peer:abc",
                WebRtcAdapterCommand::Json { data: vec![b'{'] },
                &metrics,
            );
        }

        assert_eq!(registry.unknown_peer_distinct_count(), 1);
    }

    #[test]
    fn registry_tags_pending_ice_by_current_offer_generation() {
        let mut registry = WebRtcPeerRegistry::new();
        let browser_identity = "olm-key:tab-1";
        let first = registry.next_offer_generation(browser_identity);
        assert_eq!(first, 1);
        let first_candidate = serde_json::json!({
            "candidate": "candidate:first",
            "sdpMid": "0",
            "sdpMLineIndex": 0,
        });
        assert_eq!(
            registry.queue_ice_for_current_generation(browser_identity, first_candidate, 128),
            Some(1)
        );

        let second = registry.next_offer_generation(browser_identity);
        let second_candidate = serde_json::json!({
            "candidate": "candidate:second",
            "sdpMid": "0",
            "sdpMLineIndex": 0,
        });
        assert_eq!(
            registry.queue_ice_for_current_generation(browser_identity, second_candidate, 128),
            Some(2)
        );

        let queued = registry
            .drain_pending_ice(browser_identity)
            .expect("queued candidates");
        assert_eq!(queued[0].0, first);
        assert_eq!(queued[1].0, second);
        assert_eq!(registry.current_offer_generation(browser_identity), second);
    }

    #[test]
    fn registry_recovery_and_generation_state_stay_bounded_under_reconnect_churn() {
        let mut registry = WebRtcPeerRegistry::new();
        let browser_identity = "olm-key:tab-1";
        let first = registry.next_offer_generation(browser_identity);
        assert_eq!(first, 1);
        assert_eq!(
            registry.queue_ice_for_current_generation(
                browser_identity,
                serde_json::json!({ "candidate": "candidate:first" }),
                1,
            ),
            Some(1)
        );
        let second = registry.next_offer_generation(browser_identity);
        assert_eq!(
            registry.queue_ice_for_current_generation(
                browser_identity,
                serde_json::json!({ "candidate": "candidate:second" }),
                1,
            ),
            Some(1)
        );

        let queued = registry
            .drain_pending_ice(browser_identity)
            .expect("queued candidates");
        assert_eq!(
            queued,
            vec![(
                second,
                serde_json::json!({ "candidate": "candidate:second" })
            )]
        );

        let key = format!("{browser_identity}:sess-1");
        registry.record_backpressure_recovery(
            key.clone(),
            BackpressureRecoveryEntry {
                browser_identity: browser_identity.to_string(),
                session_uuid: "sess-1".to_string(),
                subscription_id: "sub-1".to_string(),
                last_drop: Instant::now() - BACKPRESSURE_SNAPSHOT_COOLDOWN,
            },
        );

        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let task = runtime.spawn(async {});
        registry.send_tasks.insert(
            browser_identity.to_string(),
            PeerSendState {
                tx,
                dead: Arc::new(AtomicBool::new(false)),
                task,
            },
        );

        assert_eq!(
            registry
                .cooled_backpressure_recoveries(Instant::now())
                .len(),
            1
        );
        assert!(registry
            .take_recovery_sender(&key, browser_identity)
            .is_some());
        assert!(registry
            .cooled_backpressure_recoveries(Instant::now())
            .is_empty());

        registry.clear_offer_state(browser_identity);
        assert_eq!(registry.current_offer_generation(browser_identity), 0);
        assert!(registry.drain_pending_ice(browser_identity).is_none());
    }

    #[test]
    fn runner_keeps_peer_generation_and_bounded_command_queue() {
        let (command_tx, _rx) = tokio::sync::mpsc::channel(PEER_SEND_CHANNEL_CAPACITY);
        let runner = WebRtcTransportRunner::new(
            ClientId::browser("browser-1"),
            "browser-1".to_string(),
            7,
            command_tx,
        );

        assert_eq!(runner.client_id, ClientId::browser("browser-1"));
        assert_eq!(runner.browser_identity, "browser-1");
        assert_eq!(runner.generation, 7);
        assert_eq!(runner.command_tx.max_capacity(), PEER_SEND_CHANNEL_CAPACITY);
    }
}
