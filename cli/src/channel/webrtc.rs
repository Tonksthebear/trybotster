//! WebRTC DataChannel implementation.
//!
//! This module provides `WebRtcChannel`, an implementation of the `Channel`
//! trait that communicates via WebRTC DataChannel with Signal Protocol encryption.
//!
//! # Architecture
//!
//! ```text
//! WebRtcChannel
//!     |-- RTCPeerConnection (webrtc-rs)
//!     |-- RTCDataChannel (SCTP - reliable ordered)
//!     |-- Signal encryption (via CryptoServiceHandle)
//!     |-- Gzip compression (via compression module)
//!     `-- Signaling via HTTP to Rails
//! ```
//!
//! # Key Differences from ActionCable
//!
//! - No custom reliable delivery needed (SCTP provides it natively)
//! - Peer-to-peer when possible, TURN relay as fallback
//! - Signaling happens via HTTP, not WebSocket
//!
//! Rust guideline compliant 2025-01

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;

use crate::relay::crypto_service::CryptoServiceHandle;
use crate::relay::signal::SignalEnvelope;

use super::compression::{maybe_compress, maybe_decompress};
use super::{
    Channel, ChannelConfig, ChannelError, ConnectionState, IncomingMessage, PeerId,
    SharedConnectionState,
};

/// Internal message for the receive queue.
#[derive(Debug)]
struct RawIncoming {
    payload: Vec<u8>,
    sender: PeerId,
}

/// Configuration for WebRTC signaling.
#[derive(Clone, Debug)]
pub struct WebRtcConfig {
    /// Base URL for signaling (e.g., "https://trybotster.com").
    pub server_url: String,
    /// API key for authentication.
    pub api_key: String,
    /// Hub ID for routing.
    pub hub_id: String,
    /// Browser identity we're connecting to.
    pub browser_identity: String,
    /// Agent index (for terminal/preview channels).
    pub agent_index: Option<usize>,
    /// PTY index (0=CLI, 1=Server).
    pub pty_index: Option<usize>,
}

/// Builder for `WebRtcChannel`.
#[derive(Debug, Default)]
pub struct WebRtcChannelBuilder {
    server_url: Option<String>,
    api_key: Option<String>,
    crypto_service: Option<CryptoServiceHandle>,
}

impl WebRtcChannelBuilder {
    /// Create a new builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the server URL for signaling.
    #[must_use]
    pub fn server_url(mut self, url: impl Into<String>) -> Self {
        self.server_url = Some(url.into());
        self
    }

    /// Set the API key for authentication.
    #[must_use]
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Set the crypto service for E2E encryption.
    #[must_use]
    pub fn crypto_service(mut self, cs: CryptoServiceHandle) -> Self {
        self.crypto_service = Some(cs);
        self
    }

    /// Build the channel.
    ///
    /// # Panics
    ///
    /// Panics if required fields are not set.
    #[must_use]
    pub fn build(self) -> WebRtcChannel {
        WebRtcChannel {
            server_url: self.server_url.expect("server_url required"),
            api_key: self.api_key.expect("api_key required"),
            crypto_service: self.crypto_service,
            peer_connection: Arc::new(Mutex::new(None)),
            data_channel: Arc::new(Mutex::new(None)),
            state: SharedConnectionState::new(),
            peers: Arc::new(RwLock::new(HashSet::new())),
            config: Arc::new(Mutex::new(None)),
            recv_rx: Arc::new(Mutex::new(None)),
            recv_tx: Arc::new(Mutex::new(None)),
            decrypt_failures: Arc::new(AtomicU32::new(0)),
        }
    }
}

/// WebRTC DataChannel-based channel implementation.
///
/// Provides E2E encrypted communication via WebRTC with SCTP reliable delivery.
pub struct WebRtcChannel {
    /// Server URL for signaling.
    server_url: String,
    /// API key for auth.
    api_key: String,
    /// Optional crypto service for E2E encryption.
    crypto_service: Option<CryptoServiceHandle>,
    /// WebRTC peer connection.
    peer_connection: Arc<Mutex<Option<Arc<RTCPeerConnection>>>>,
    /// WebRTC data channel.
    data_channel: Arc<Mutex<Option<Arc<RTCDataChannel>>>>,
    /// Shared connection state.
    state: Arc<SharedConnectionState>,
    /// Connected peers (browser identities with active sessions).
    peers: Arc<RwLock<HashSet<PeerId>>>,
    /// Channel configuration.
    config: Arc<Mutex<Option<ChannelConfig>>>,
    /// Receive queue.
    recv_rx: Arc<Mutex<Option<mpsc::Receiver<RawIncoming>>>>,
    /// Send side of receive queue.
    recv_tx: Arc<Mutex<Option<mpsc::Sender<RawIncoming>>>>,
    /// Consecutive decryption failure count for session health monitoring.
    decrypt_failures: Arc<AtomicU32>,
}

impl std::fmt::Debug for WebRtcChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebRtcChannel")
            .field("server_url", &self.server_url)
            .field("crypto_service", &self.crypto_service.is_some())
            .finish()
    }
}

impl WebRtcChannel {
    /// Create a new builder.
    #[must_use]
    pub fn builder() -> WebRtcChannelBuilder {
        WebRtcChannelBuilder::new()
    }

    /// Fetch ICE server configuration from Rails.
    async fn fetch_ice_config(&self, hub_id: &str) -> Result<Vec<RTCIceServer>, ChannelError> {
        let url = format!("{}/hubs/{}/webrtc", self.server_url, hub_id);

        let client = reqwest::Client::new();
        let response = client
            .get(&url)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(|e| ChannelError::ConnectionFailed(format!("Failed to fetch ICE config: {e}")))?;

        if !response.status().is_success() {
            return Err(ChannelError::ConnectionFailed(format!(
                "ICE config request failed: {}",
                response.status()
            )));
        }

        #[derive(serde::Deserialize)]
        struct IceConfig {
            ice_servers: Vec<IceServer>,
        }

        #[derive(serde::Deserialize)]
        struct IceServer {
            urls: String,
            username: Option<String>,
            credential: Option<String>,
        }

        let config: IceConfig = response
            .json()
            .await
            .map_err(|e| ChannelError::ConnectionFailed(format!("Failed to parse ICE config: {e}")))?;

        Ok(config
            .ice_servers
            .into_iter()
            .map(|s| RTCIceServer {
                urls: vec![s.urls],
                username: s.username.unwrap_or_default(),
                credential: s.credential.unwrap_or_default(),
                ..Default::default()
            })
            .collect())
    }

    /// Create the WebRTC peer connection.
    async fn create_peer_connection(
        &self,
        ice_servers: Vec<RTCIceServer>,
    ) -> Result<Arc<RTCPeerConnection>, ChannelError> {
        // Create media engine (required even for data-only)
        let mut media_engine = MediaEngine::default();
        media_engine
            .register_default_codecs()
            .map_err(|e| ChannelError::ConnectionFailed(format!("Failed to register codecs: {e}")))?;

        // Create interceptor registry
        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut media_engine)
            .map_err(|e| ChannelError::ConnectionFailed(format!("Failed to register interceptors: {e}")))?;

        // Create API
        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .build();

        // Create peer connection config
        let config = RTCConfiguration {
            ice_servers,
            ..Default::default()
        };

        // Create peer connection
        let pc = api
            .new_peer_connection(config)
            .await
            .map_err(|e| ChannelError::ConnectionFailed(format!("Failed to create peer connection: {e}")))?;

        Ok(Arc::new(pc))
    }

    /// Set up event handlers for the peer connection.
    fn setup_peer_connection_handlers(&self, pc: &Arc<RTCPeerConnection>) {
        let state = Arc::clone(&self.state);
        let data_channel = Arc::clone(&self.data_channel);
        let peer_connection = Arc::clone(&self.peer_connection);

        // Connection state change handler
        pc.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
            let state = Arc::clone(&state);
            let data_channel = Arc::clone(&data_channel);
            let peer_connection = Arc::clone(&peer_connection);
            Box::pin(async move {
                log::info!("[WebRTC] Connection state changed: {s}");
                match s {
                    RTCPeerConnectionState::Connected => {
                        state.set(ConnectionState::Connected).await;
                    }
                    RTCPeerConnectionState::Disconnected | RTCPeerConnectionState::Failed => {
                        state.set(ConnectionState::Disconnected).await;
                        // Clear data channel and peer connection so new offers can be accepted
                        *data_channel.lock().await = None;
                        *peer_connection.lock().await = None;
                    }
                    RTCPeerConnectionState::Closed => {
                        state.set(ConnectionState::Disconnected).await;
                        *data_channel.lock().await = None;
                        *peer_connection.lock().await = None;
                    }
                    _ => {}
                }
            })
        }));
    }

    /// Handle incoming SDP offer from browser and create answer.
    ///
    /// Called when CLI receives a `webrtc_offer` via `HubCommandChannel`.
    pub async fn handle_sdp_offer(&self, sdp: &str, browser_identity: &str) -> Result<String, ChannelError> {
        // Get or create peer connection
        let mut pc_guard = self.peer_connection.lock().await;

        // If we already have a peer connection, we've already processed an offer.
        // Don't process another one - it would create new ICE credentials that mismatch.
        // Wait for the connection to either succeed or fail before accepting new offers.
        if pc_guard.is_some() {
            log::info!("[WebRTC] Ignoring duplicate offer (peer connection already exists)");
            return Err(ChannelError::ConnectionFailed("Connection in progress".to_string()));
        }

        // Fetch ICE config and create peer connection
        let config_guard = self.config.lock().await;
        let hub_id = config_guard
            .as_ref()
            .map(|c| c.hub_id.clone())
            .unwrap_or_default();
        drop(config_guard);

        let ice_servers = self.fetch_ice_config(&hub_id).await?;
        let pc = self.create_peer_connection(ice_servers).await?;
        self.setup_peer_connection_handlers(&pc);
        *pc_guard = Some(pc.clone());

        // Set remote description (offer)
        let offer = RTCSessionDescription::offer(sdp.to_string())
            .map_err(|e| ChannelError::ConnectionFailed(format!("Invalid SDP offer: {e}")))?;

        pc.set_remote_description(offer)
            .await
            .map_err(|e| ChannelError::ConnectionFailed(format!("Failed to set remote description: {e}")))?;

        // Create answer
        let answer = pc
            .create_answer(None)
            .await
            .map_err(|e| ChannelError::ConnectionFailed(format!("Failed to create answer: {e}")))?;

        // Set local description
        pc.set_local_description(answer.clone())
            .await
            .map_err(|e| ChannelError::ConnectionFailed(format!("Failed to set local description: {e}")))?;

        // Set up data channel handler (browser creates the channel, we receive it)
        let recv_tx = Arc::clone(&self.recv_tx);
        let peers = Arc::clone(&self.peers);
        let crypto_service = self.crypto_service.clone();
        let config = Arc::clone(&self.config);
        let browser_id = browser_identity.to_string();
        let data_channel = Arc::clone(&self.data_channel);
        let decrypt_failures = Arc::clone(&self.decrypt_failures);

        pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
            let recv_tx = Arc::clone(&recv_tx);
            let peers = Arc::clone(&peers);
            let crypto_service = crypto_service.clone();
            let config = Arc::clone(&config);
            let browser_id = browser_id.clone();
            let data_channel = Arc::clone(&data_channel);
            let decrypt_failures = Arc::clone(&decrypt_failures);

            Box::pin(async move {
                log::info!("[WebRTC] Data channel opened: {}", dc.label());

                // Store data channel
                *data_channel.lock().await = Some(Arc::clone(&dc));

                // Set up message handler
                let recv_tx_inner = Arc::clone(&recv_tx);
                let peers_inner = Arc::clone(&peers);
                let crypto_inner = crypto_service.clone();
                let config_inner = Arc::clone(&config);
                let browser_inner = browser_id.clone();
                let decrypt_failures_inner = Arc::clone(&decrypt_failures);

                dc.on_message(Box::new(move |msg: DataChannelMessage| {
                    let recv_tx = Arc::clone(&recv_tx_inner);
                    let peers = Arc::clone(&peers_inner);
                    let crypto_service = crypto_inner.clone();
                    let config = Arc::clone(&config_inner);
                    let browser_identity = browser_inner.clone();
                    let decrypt_failures = Arc::clone(&decrypt_failures_inner);

                    Box::pin(async move {
                        let data = msg.data.to_vec();

                        // Try to decrypt if we have a crypto service and it looks like an envelope
                        // Control messages (subscribe/unsubscribe) may be plaintext
                        let decrypted = if let Some(ref cs) = crypto_service {
                            match serde_json::from_slice::<SignalEnvelope>(&data) {
                                Ok(envelope) => match cs.decrypt(&envelope).await {
                                    Ok(plaintext) => {
                                        decrypt_failures.store(0, Ordering::Relaxed);
                                        plaintext
                                    }
                                    Err(e) => {
                                        decrypt_failures.fetch_add(1, Ordering::Relaxed);
                                        log::error!("[WebRTC-DC] Decryption FAILED: {e}");
                                        return;
                                    }
                                },
                                Err(_) => {
                                    // Not a Signal envelope - treat as plaintext control message
                                    data
                                }
                            }
                        } else {
                            data
                        };

                        // Decompress
                        let config_guard = config.lock().await;
                        let decompressed = if config_guard
                            .as_ref()
                            .is_some_and(|c| c.compression_threshold.is_some())
                        {
                            match maybe_decompress(&decrypted) {
                                Ok(d) => d,
                                Err(e) => {
                                    log::error!("[WebRTC] Decompression failed: {e}");
                                    return;
                                }
                            }
                        } else {
                            decrypted
                        };
                        drop(config_guard);

                        // Add peer
                        {
                            let mut peers = peers.write().await;
                            peers.insert(PeerId(browser_identity.clone()));
                        }

                        // Send to receive queue
                        if let Some(tx) = recv_tx.lock().await.as_ref() {
                            let _ = tx
                                .send(RawIncoming {
                                    payload: decompressed,
                                    sender: PeerId(browser_identity.clone()),
                                })
                                .await;
                        } else {
                            log::error!("[WebRTC-DC] recv_tx is None! Cannot queue message");
                        }
                    })
                }));
            })
        }));

        // Set up ICE candidate handler to trickle candidates
        let server_url = self.server_url.clone();
        let api_key = self.api_key.clone();
        let config_guard = self.config.lock().await;
        let hub_id = config_guard
            .as_ref()
            .map(|c| c.hub_id.clone())
            .unwrap_or_default();
        drop(config_guard);
        let browser_id = browser_identity.to_string();

        pc.on_ice_candidate(Box::new(move |candidate| {
            let server_url = server_url.clone();
            let api_key = api_key.clone();
            let hub_id = hub_id.clone();
            let browser_id = browser_id.clone();

            Box::pin(async move {
                if let Some(c) = candidate {
                    let candidate_json = match c.to_json() {
                        Ok(j) => j,
                        Err(e) => {
                            log::error!("[WebRTC] Failed to serialize ICE candidate: {e}");
                            return;
                        }
                    };

                    // Send ICE candidate to browser
                    let url = format!("{}/hubs/{}/webrtc_signals", server_url, hub_id);
                    let client = reqwest::Client::new();
                    let _ = client
                        .post(&url)
                        .bearer_auth(&api_key)
                        .json(&serde_json::json!({
                            "signal_type": "ice",
                            "browser_identity": browser_id,
                            "candidate": candidate_json,
                        }))
                        .send()
                        .await;
                }
            })
        }));

        // Add peer
        {
            let mut peers = self.peers.write().await;
            peers.insert(PeerId(browser_identity.to_string()));
        }

        Ok(answer.sdp)
    }

    /// Handle incoming ICE candidate from browser.
    pub async fn handle_ice_candidate(
        &self,
        candidate: &str,
        sdp_mid: Option<&str>,
        sdp_mline_index: Option<u16>,
    ) -> Result<(), ChannelError> {
        let pc_guard = self.peer_connection.lock().await;
        let pc = pc_guard
            .as_ref()
            .ok_or_else(|| ChannelError::ConnectionFailed("No peer connection".to_string()))?;

        let candidate_init = RTCIceCandidateInit {
            candidate: candidate.to_string(),
            sdp_mid: sdp_mid.map(String::from),
            sdp_mline_index,
            ..Default::default()
        };

        pc.add_ice_candidate(candidate_init)
            .await
            .map_err(|e| ChannelError::ConnectionFailed(format!("Failed to add ICE candidate: {e}")))?;

        Ok(())
    }
}

#[async_trait]
impl Channel for WebRtcChannel {
    async fn connect(&mut self, config: ChannelConfig) -> Result<(), ChannelError> {
        self.state.set(ConnectionState::Connecting).await;

        // Store config
        *self.config.lock().await = Some(config.clone());

        // Create receive channel
        let (recv_tx, recv_rx) = mpsc::channel::<RawIncoming>(256);
        *self.recv_tx.lock().await = Some(recv_tx);
        *self.recv_rx.lock().await = Some(recv_rx);

        // For CLI, we wait for the browser to initiate the offer
        // The connection is established when handle_sdp_offer is called
        log::info!("[WebRTC] Channel configured, waiting for browser offer");

        Ok(())
    }

    async fn disconnect(&mut self) {
        // Close data channel
        if let Some(dc) = self.data_channel.lock().await.take() {
            let _ = dc.close().await;
        }

        // Close peer connection
        if let Some(pc) = self.peer_connection.lock().await.take() {
            let _ = pc.close().await;
        }

        self.state.set(ConnectionState::Disconnected).await;
        self.peers.write().await.clear();
    }

    fn state(&self) -> ConnectionState {
        // Use blocking read for sync interface
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async { self.state.get().await })
        })
    }

    async fn send(&self, msg: &[u8]) -> Result<(), ChannelError> {
        let peers: Vec<PeerId> = self.peers.read().await.iter().cloned().collect();

        for peer in peers {
            self.send_to(msg, &peer).await?;
        }

        Ok(())
    }

    async fn send_to(&self, msg: &[u8], peer: &PeerId) -> Result<(), ChannelError> {
        let dc_guard = self.data_channel.lock().await;
        let dc = dc_guard
            .as_ref()
            .ok_or_else(|| ChannelError::SendFailed("No data channel".to_string()))?;

        // Compress if configured
        let config_guard = self.config.lock().await;
        let compressed =
            if let Some(threshold) = config_guard.as_ref().and_then(|c| c.compression_threshold) {
                maybe_compress(msg, Some(threshold))?
            } else {
                msg.to_vec()
            };
        drop(config_guard);

        // Encrypt if we have a crypto service.
        // Browser identity format is "identityKey:tabId" — extract identity key
        // for Signal encryption (sessions are keyed by identity key only).
        //
        // Base64 encode compressed bytes before encryption because the Signal WASM
        // library expects UTF-8 strings. Gzip-compressed data contains invalid UTF-8.
        let to_send = if let Some(ref cs) = self.crypto_service {
            let identity_key = peer.as_ref().split(':').next().unwrap_or(peer.as_ref());
            let b64_payload = BASE64.encode(&compressed);
            let envelope = cs
                .encrypt(b64_payload.as_bytes(), identity_key)
                .await
                .map_err(|e| ChannelError::EncryptionError(e.to_string()))?;

            serde_json::to_vec(&envelope)
                .map_err(|e| ChannelError::EncryptionError(e.to_string()))?
        } else {
            compressed
        };

        // Send via data channel (SCTP handles reliability)
        dc.send(&bytes::Bytes::from(to_send))
            .await
            .map_err(|e| ChannelError::SendFailed(e.to_string()))?;

        Ok(())
    }

    async fn recv(&mut self) -> Result<IncomingMessage, ChannelError> {
        let mut recv_guard = self.recv_rx.lock().await;
        let recv_rx = recv_guard.as_mut().ok_or(ChannelError::Closed)?;

        match recv_rx.recv().await {
            Some(raw) => Ok(IncomingMessage {
                payload: raw.payload,
                sender: raw.sender,
            }),
            None => Err(ChannelError::Closed),
        }
    }

    fn peers(&self) -> Vec<PeerId> {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { self.peers.read().await.iter().cloned().collect() })
        })
    }

    fn has_peer(&self, peer: &PeerId) -> bool {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { self.peers.read().await.contains(peer) })
        })
    }
}

impl WebRtcChannel {
    /// Try to receive a message without blocking.
    ///
    /// Returns `Some(message)` if a message is available, `None` otherwise.
    /// Must be called from within a tokio runtime context (use `runtime.enter()`).
    pub fn try_recv(&self, runtime: &tokio::runtime::Runtime) -> Option<IncomingMessage> {
        runtime.block_on(async {
            let mut recv_guard = self.recv_rx.lock().await;
            let recv_rx = recv_guard.as_mut()?;

            match recv_rx.try_recv() {
                Ok(raw) => Some(IncomingMessage {
                    payload: raw.payload,
                    sender: raw.sender,
                }),
                Err(_) => None,
            }
        })
    }

    /// Get consecutive decryption failure count.
    pub fn decrypt_failure_count(&self) -> u32 {
        self.decrypt_failures.load(Ordering::Relaxed)
    }

    /// Reset decryption failure counter.
    pub fn reset_decrypt_failures(&self) {
        self.decrypt_failures.store(0, Ordering::Relaxed);
    }

    /// Send a plaintext message through the DataChannel, bypassing encryption.
    ///
    /// Used for session recovery: when encryption is broken, we need to notify
    /// the browser without going through the (broken) Signal session.
    pub async fn send_plaintext(&self, msg: &[u8]) -> Result<(), ChannelError> {
        let dc_guard = self.data_channel.lock().await;
        let dc = dc_guard
            .as_ref()
            .ok_or_else(|| ChannelError::SendFailed("No data channel".to_string()))?;

        dc.send(&bytes::Bytes::from(msg.to_vec()))
            .await
            .map_err(|e| ChannelError::SendFailed(e.to_string()))?;

        Ok(())
    }
}
