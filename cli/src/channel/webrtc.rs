//! WebRTC DataChannel implementation.
//!
//! This module provides `WebRtcChannel`, an implementation of the `Channel`
//! trait that communicates via WebRTC DataChannel with E2E encryption.
//!
//! # Architecture
//!
//! ```text
//! WebRtcChannel
//!     |-- RTCPeerConnection (webrtc-rs)
//!     |-- RTCDataChannel (SCTP - reliable ordered)
//!     |-- E2E encryption (via CryptoService = Arc<Mutex<VodozemacCrypto>>)
//!     |-- Gzip compression (via compression module)
//!     `-- Signaling via ActionCable (encrypted envelopes)
//! ```
//!
//! # Key Differences from ActionCable
//!
//! - No custom reliable delivery needed (SCTP provides it natively)
//! - Peer-to-peer when possible, TURN relay as fallback
//! - Signaling (offer/answer/ICE) via ActionCable, E2E encrypted

use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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

use crate::relay::crypto_service::CryptoService;
use crate::relay::olm_crypto::{CONTENT_FILE, CONTENT_MSG, CONTENT_PTY, CONTENT_STREAM};

/// Incoming PTY input from browser via binary DataChannel frame.
///
/// Parsed from `CONTENT_PTY` with input flag set (flags & 0x02).
/// Bypasses JSON/Lua for zero-overhead keystroke delivery.
#[derive(Debug)]
pub struct PtyInputIncoming {
    /// Agent index parsed from subscription ID.
    pub agent_index: usize,
    /// PTY index parsed from subscription ID.
    pub pty_index: usize,
    /// Raw input bytes from browser.
    pub data: Vec<u8>,
    /// Browser identity key (for per-client focus tracking).
    pub browser_identity: String,
}

/// Incoming file from browser via binary DataChannel frame.
///
/// Parsed from `CONTENT_FILE`. The browser sends image/file data
/// which the CLI writes to a temp file and injects the path into the PTY.
#[derive(Debug)]
pub struct FileInputIncoming {
    /// Agent index parsed from subscription ID.
    pub agent_index: usize,
    /// PTY index parsed from subscription ID.
    pub pty_index: usize,
    /// Original filename from the browser (e.g., "screenshot.png").
    pub filename: String,
    /// Raw file bytes.
    pub data: Vec<u8>,
}

/// Incoming stream frame from browser via DataChannel.
#[derive(Debug)]
pub struct StreamIncoming {
    /// Browser identity that sent this frame.
    pub browser_identity: String,
    /// Stream frame type (OPEN, DATA, CLOSE).
    pub frame_type: u8,
    /// Stream identifier.
    pub stream_id: u16,
    /// Frame payload.
    pub payload: Vec<u8>,
}

use super::compression::maybe_compress;
use super::{
    Channel, ChannelConfig, ChannelError, ConnectionState, IncomingMessage, PeerId,
    SharedConnectionState,
};

/// Internal message for the receive queue.
#[derive(Debug)]
pub(crate) struct RawIncoming {
    pub(crate) payload: Vec<u8>,
    pub(crate) sender: PeerId,
}

/// Outgoing signal destined for a browser, sent via ActionCable relay.
///
/// Produced by async WebRTC callbacks (e.g., `on_ice_candidate`), drained
/// by `server_comms` tick loop, and forwarded through `CommandChannelHandle::perform`.
#[derive(Debug)]
pub enum OutgoingSignal {
    /// Encrypted ICE candidate for a specific browser.
    Ice {
        /// Target browser identity (`identityKey:tabId`).
        browser_identity: String,
        /// E2E encrypted envelope (opaque to Rails).
        envelope: serde_json::Value,
    },
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
pub struct WebRtcChannelBuilder {
    server_url: Option<String>,
    api_key: Option<String>,
    crypto_service: Option<CryptoService>,
    signal_tx: Option<mpsc::UnboundedSender<OutgoingSignal>>,
    stream_frame_tx: Option<mpsc::UnboundedSender<StreamIncoming>>,
    pty_input_tx: Option<mpsc::UnboundedSender<PtyInputIncoming>>,
    file_input_tx: Option<mpsc::UnboundedSender<FileInputIncoming>>,
    hub_event_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::hub::events::HubEvent>>,
}

impl std::fmt::Debug for WebRtcChannelBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebRtcChannelBuilder")
            .field("server_url", &self.server_url)
            .field("crypto_service", &self.crypto_service.is_some())
            .field("signal_tx", &self.signal_tx.is_some())
            .field("stream_frame_tx", &self.stream_frame_tx.is_some())
            .field("pty_input_tx", &self.pty_input_tx.is_some())
            .field("file_input_tx", &self.file_input_tx.is_some())
            .field("hub_event_tx", &self.hub_event_tx.is_some())
            .finish()
    }
}

impl Default for WebRtcChannelBuilder {
    fn default() -> Self {
        Self {
            server_url: None,
            api_key: None,
            crypto_service: None,
            signal_tx: None,
            stream_frame_tx: None,
            pty_input_tx: None,
            file_input_tx: None,
            hub_event_tx: None,
        }
    }
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
    pub fn crypto_service(mut self, cs: CryptoService) -> Self {
        self.crypto_service = Some(cs);
        self
    }

    /// Set the outgoing signal sender for ICE candidate relay via ActionCable.
    #[must_use]
    pub fn signal_tx(mut self, tx: mpsc::UnboundedSender<OutgoingSignal>) -> Self {
        self.signal_tx = Some(tx);
        self
    }

    /// Set the stream frame sender for TCP stream multiplexer frames.
    #[must_use]
    pub fn stream_frame_tx(mut self, tx: mpsc::UnboundedSender<StreamIncoming>) -> Self {
        self.stream_frame_tx = Some(tx);
        self
    }

    /// Set the PTY input sender for binary PTY input from browser.
    #[must_use]
    pub fn pty_input_tx(mut self, tx: mpsc::UnboundedSender<PtyInputIncoming>) -> Self {
        self.pty_input_tx = Some(tx);
        self
    }

    /// Set the file input sender for file transfers from browser.
    #[must_use]
    pub fn file_input_tx(mut self, tx: mpsc::UnboundedSender<FileInputIncoming>) -> Self {
        self.file_input_tx = Some(tx);
        self
    }

    /// Set the Hub event channel sender for DC opened notifications.
    #[must_use]
    pub(crate) fn hub_event_tx(
        mut self,
        tx: tokio::sync::mpsc::UnboundedSender<crate::hub::events::HubEvent>,
    ) -> Self {
        self.hub_event_tx = Some(tx);
        self
    }

    /// Build the channel.
    ///
    /// # Panics
    ///
    /// Panics if required fields are not set.
    #[must_use]
    pub fn build(self) -> WebRtcChannel {
        let (close_tx, close_rx) = tokio::sync::watch::channel(false);
        WebRtcChannel {
            server_url: self.server_url.expect("server_url required"),
            api_key: self.api_key.expect("api_key required"),
            crypto_service: self.crypto_service,
            signal_tx: self.signal_tx,
            stream_frame_tx: self.stream_frame_tx,
            pty_input_tx: self.pty_input_tx,
            file_input_tx: self.file_input_tx,
            peer_connection: Arc::new(Mutex::new(None)),
            data_channel: Arc::new(Mutex::new(None)),
            state: SharedConnectionState::new(),
            peers: Arc::new(RwLock::new(HashSet::new())),
            config: Arc::new(Mutex::new(None)),
            recv_rx: Arc::new(Mutex::new(None)),
            recv_tx: Arc::new(Mutex::new(None)),
            peer_olm_key: Arc::new(Mutex::new(None)),
            decrypt_failures: Arc::new(AtomicU32::new(0)),
            dc_opened: Arc::new(AtomicBool::new(false)),
            hub_event_tx: self.hub_event_tx,
            close_complete_tx: close_tx,
            close_complete_rx: close_rx,
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
    crypto_service: Option<CryptoService>,
    /// Sender for outgoing signals (ICE candidates) to relay via ActionCable.
    signal_tx: Option<mpsc::UnboundedSender<OutgoingSignal>>,
    /// Sender for incoming stream multiplexer frames.
    stream_frame_tx: Option<mpsc::UnboundedSender<StreamIncoming>>,
    /// Sender for incoming PTY input from browser (binary, bypasses JSON/Lua).
    pty_input_tx: Option<mpsc::UnboundedSender<PtyInputIncoming>>,
    /// Sender for incoming file transfers from browser.
    file_input_tx: Option<mpsc::UnboundedSender<FileInputIncoming>>,
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
    /// Peer's Olm identity key (set when SDP offer is handled).
    peer_olm_key: Arc<Mutex<Option<String>>>,
    /// Consecutive decryption failure count for session health monitoring.
    decrypt_failures: Arc<AtomicU32>,
    /// Set to `true` when the DataChannel opens; consumed by `take_dc_opened()`.
    /// Kept as test-only fallback when `hub_event_tx` is None.
    dc_opened: Arc<AtomicBool>,
    /// Event channel sender for DC opened notifications.
    /// When set, `on_data_channel` sends `HubEvent::DcOpened` instead of
    /// setting the atomic bool.
    hub_event_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::hub::events::HubEvent>>,
    /// Set to `true` when the state handler's close task completes (pc/dc sockets released).
    /// Uses `watch` so late subscribers see the value even if the close already happened
    /// (unlike `Notify`, which is fire-and-forget).
    close_complete_tx: tokio::sync::watch::Sender<bool>,
    close_complete_rx: tokio::sync::watch::Receiver<bool>,
}

impl std::fmt::Debug for WebRtcChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebRtcChannel")
            .field("server_url", &self.server_url)
            .field("crypto_service", &self.crypto_service.is_some())
            .field("signal_tx", &self.signal_tx.is_some())
            .field("stream_frame_tx", &self.stream_frame_tx.is_some())
            .field("pty_input_tx", &self.pty_input_tx.is_some())
            .finish()
    }
}

impl WebRtcChannel {
    /// Create a new builder.
    #[must_use]
    pub fn builder() -> WebRtcChannelBuilder {
        WebRtcChannelBuilder::new()
    }

    /// Timeout for the ICE config HTTP request. Keeps the tick loop responsive
    /// even when the endpoint is slow or the runtime is under load from
    /// concurrent WebRTC teardown tasks.
    const ICE_CONFIG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

    /// Fetch ICE server configuration from Rails.
    async fn fetch_ice_config(&self, hub_id: &str) -> Result<Vec<RTCIceServer>, ChannelError> {
        let url = format!("{}/hubs/{}/webrtc", self.server_url, hub_id);

        let client = reqwest::Client::builder()
            .timeout(Self::ICE_CONFIG_TIMEOUT)
            .build()
            .map_err(|e| ChannelError::ConnectionFailed(format!("Failed to build HTTP client: {e:#}")))?;

        let response = client
            .get(&url)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(|e| ChannelError::ConnectionFailed(format!("Failed to fetch ICE config: {e:#}")))?;

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
            .map_err(|e| ChannelError::ConnectionFailed(format!("Failed to parse ICE config: {e:#}")))?;

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
        let close_complete = self.close_complete_tx.clone();

        // Connection state change handler
        pc.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
            let state = Arc::clone(&state);
            let data_channel = Arc::clone(&data_channel);
            let peer_connection = Arc::clone(&peer_connection);
            let close_complete = close_complete.clone();
            Box::pin(async move {
                log::info!("[WebRTC] Connection state changed: {s}");
                match s {
                    RTCPeerConnectionState::Connected => {
                        state.set(ConnectionState::Connected).await;
                    }
                    RTCPeerConnectionState::Disconnected | RTCPeerConnectionState::Failed => {
                        state.set(ConnectionState::Disconnected).await;
                        // Take resources out (so new offers work immediately) and
                        // spawn close task. Can't close inline — pc.close() triggers
                        // a Closed state callback which would deadlock on the Mutex.
                        // webrtc-rs Drop does NOT shut down SCTP; only close() does.
                        let dc = data_channel.lock().await.take();
                        let pc = peer_connection.lock().await.take();
                        if dc.is_some() || pc.is_some() {
                            let close_complete = close_complete.clone();
                            tokio::spawn(async move {
                                // close() may take up to ~60s if SCTP retransmits
                                // exhaust their limit, but it WILL complete. No timeout —
                                // dropping without close() leaks sockets (the original bug).
                                if let Some(dc) = dc {
                                    let _ = dc.close().await;
                                }
                                if let Some(pc) = pc {
                                    let _ = pc.close().await;
                                }
                                log::debug!("[WebRTC] Closed stale peer connection resources");
                                let _ = close_complete.send(true);
                            });
                        } else {
                            // No resources to close — signal immediately
                            let _ = close_complete.send(true);
                        }
                    }
                    RTCPeerConnectionState::Closed => {
                        state.set(ConnectionState::Disconnected).await;
                        // Resources may already be cleaned up by Disconnected/Failed handler
                        let dc = data_channel.lock().await.take();
                        let pc = peer_connection.lock().await.take();
                        if dc.is_some() || pc.is_some() {
                            let close_complete = close_complete.clone();
                            tokio::spawn(async move {
                                if let Some(dc) = dc {
                                    let _ = dc.close().await;
                                }
                                if let Some(pc) = pc {
                                    let _ = pc.close().await;
                                }
                                let _ = close_complete.send(true);
                            });
                        }
                    }
                    _ => {}
                }
            })
        }));
    }

    /// Handle incoming SDP offer from browser and create answer.
    ///
    /// Called when CLI receives an encrypted offer via ActionCable signal channel.
    pub async fn handle_sdp_offer(&self, sdp: &str, browser_identity: &str) -> Result<String, ChannelError> {
        // Get or create peer connection
        let mut pc_guard = self.peer_connection.lock().await;

        if pc_guard.is_some() {
            log::info!("[WebRTC] Ignoring duplicate offer (peer connection already exists)");
            return Err(ChannelError::ConnectionFailed("Connection in progress".to_string()));
        }

        // Reset DC-opened flag so a stale signal from the previous PC doesn't
        // cause a premature peer_connected on the new connection.
        self.dc_opened.store(false, Ordering::Relaxed);

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

        // Store the peer's Olm key for encrypt routing.
        {
            let mut olm_key = self.peer_olm_key.lock().await;
            *olm_key = Some(crate::relay::extract_olm_key(browser_identity).to_string());
        }

        // Set up data channel handler (browser creates the channel, we receive it)
        let recv_tx = Arc::clone(&self.recv_tx);
        let peers = Arc::clone(&self.peers);
        let crypto_service = self.crypto_service.clone();
        let browser_id = browser_identity.to_string();
        let data_channel = Arc::clone(&self.data_channel);
        let decrypt_failures = Arc::clone(&self.decrypt_failures);
        let stream_frame_tx = self.stream_frame_tx.clone();
        let pty_input_tx = self.pty_input_tx.clone();
        let file_input_tx = self.file_input_tx.clone();
        let dc_opened = Arc::clone(&self.dc_opened);
        let hub_event_tx = self.hub_event_tx.clone();

        pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
            let recv_tx = Arc::clone(&recv_tx);
            let peers = Arc::clone(&peers);
            let crypto_service = crypto_service.clone();
            let browser_id = browser_id.clone();
            let data_channel = Arc::clone(&data_channel);
            let decrypt_failures = Arc::clone(&decrypt_failures);
            let stream_frame_tx = stream_frame_tx.clone();
            let pty_input_tx = pty_input_tx.clone();
            let file_input_tx = file_input_tx.clone();
            let dc_opened = Arc::clone(&dc_opened);
            let hub_event_tx = hub_event_tx.clone();

            Box::pin(async move {
                log::info!("[WebRTC] Data channel opened: {}", dc.label());

                // Store data channel and signal readiness
                *data_channel.lock().await = Some(Arc::clone(&dc));

                // Notify the Hub event loop that the DC is open.
                // Event channel path (production): instant delivery.
                // Atomic bool path (tests): polled by tick().
                if let Some(ref tx) = hub_event_tx {
                    let _ = tx.send(crate::hub::events::HubEvent::DcOpened {
                        browser_identity: browser_id.clone(),
                    });
                } else {
                    dc_opened.store(true, Ordering::Relaxed);
                }

                // Set up message handler — every byte is Olm-encrypted
                let recv_tx_inner = Arc::clone(&recv_tx);
                let peers_inner = Arc::clone(&peers);
                let crypto_inner = crypto_service.clone();
                let browser_inner = browser_id.clone();
                let decrypt_failures_inner = Arc::clone(&decrypt_failures);
                let stream_frame_tx_inner = stream_frame_tx.clone();
                let pty_input_tx_inner = pty_input_tx.clone();
                let file_input_tx_inner = file_input_tx.clone();

                dc.on_message(Box::new(move |msg: DataChannelMessage| {
                    let recv_tx = Arc::clone(&recv_tx_inner);
                    let peers = Arc::clone(&peers_inner);
                    let crypto_service = crypto_inner.clone();
                    let browser_identity = browser_inner.clone();
                    let decrypt_failures = Arc::clone(&decrypt_failures_inner);
                    let stream_frame_tx = stream_frame_tx_inner.clone();
                    let pty_input_tx = pty_input_tx_inner.clone();
                    let file_input_tx = file_input_tx_inner.clone();

                    Box::pin(async move {
                        let data = msg.data.to_vec();

                        // Every DataChannel message is a binary Olm frame:
                        // [msg_type:1][ciphertext] or [msg_type:1][key:32][ciphertext]
                        let Some(ref cs) = crypto_service else {
                            log::error!("[WebRTC-DC] No crypto service -- cannot decrypt");
                            return;
                        };

                        // Decrypt binary frame via vodozemac
                        let peer_olm_key = crate::relay::extract_olm_key(&browser_identity);
                        let plaintext = match cs.lock() {
                            Ok(mut guard) => match guard.decrypt_binary(&data, Some(peer_olm_key)) {
                                Ok(pt) => {
                                    decrypt_failures.store(0, Ordering::Relaxed);
                                    pt
                                }
                                Err(e) => {
                                    decrypt_failures.fetch_add(1, Ordering::Relaxed);
                                    log::error!("[WebRTC-DC] Olm decryption FAILED: {e}");
                                    return;
                                }
                            },
                            Err(e) => {
                                log::error!("[WebRTC-DC] Crypto mutex poisoned: {e}");
                                return;
                            }
                        };

                        // Parse binary inner content: first byte = content type
                        if plaintext.is_empty() {
                            log::warn!("[WebRTC-DC] Empty decrypted content");
                            return;
                        }

                        let body_bytes = match plaintext[0] {
                            CONTENT_MSG => {
                                // Control message: [CONTENT_MSG][JSON bytes]
                                plaintext[1..].to_vec()
                            }
                            CONTENT_PTY => {
                                // PTY: [CONTENT_PTY][flags][sub_id_len][sub_id][payload]
                                if plaintext.len() < 4 {
                                    log::warn!("[WebRTC-DC] CONTENT_PTY frame too short");
                                    return;
                                }
                                let flags = plaintext[1];
                                let is_input = flags & 0x02 != 0;

                                if !is_input {
                                    log::warn!("[WebRTC-DC] Unexpected PTY output from browser");
                                    return;
                                }

                                let sub_id_len = plaintext[2] as usize;
                                if plaintext.len() < 3 + sub_id_len {
                                    log::warn!("[WebRTC-DC] CONTENT_PTY sub_id truncated");
                                    return;
                                }
                                let sub_id = std::str::from_utf8(&plaintext[3..3 + sub_id_len])
                                    .unwrap_or("");
                                let payload = plaintext[3 + sub_id_len..].to_vec();

                                // Parse "terminal_{agent}_{pty}" and send directly to PTY
                                if let Some(incoming) = parse_pty_input_sub_id(sub_id, payload, browser_identity.clone()) {
                                    if let Some(ref tx) = pty_input_tx {
                                        let _ = tx.send(incoming);
                                    }
                                } else {
                                    log::warn!(
                                        "[WebRTC-DC] Failed to parse PTY input sub_id: {sub_id}"
                                    );
                                }
                                return;
                            }
                            CONTENT_STREAM => {
                                // Stream mux: [CONTENT_STREAM][frame_type][stream_id_hi][stream_id_lo][payload]
                                if plaintext.len() < 4 {
                                    log::warn!("[WebRTC-DC] CONTENT_STREAM frame too short");
                                    return;
                                }
                                let frame_type = plaintext[1];
                                let stream_id = u16::from_be_bytes([plaintext[2], plaintext[3]]);
                                let payload = plaintext[4..].to_vec();

                                if let Some(ref tx) = stream_frame_tx {
                                    let _ = tx.send(StreamIncoming {
                                        browser_identity: browser_identity.clone(),
                                        frame_type,
                                        stream_id,
                                        payload,
                                    });
                                }
                                return;
                            }
                            CONTENT_FILE => {
                                // File transfer: [CONTENT_FILE][sub_id_len][sub_id][filename_len_lo][filename_len_hi][filename][data]
                                if plaintext.len() < 4 {
                                    log::warn!("[WebRTC-DC] CONTENT_FILE frame too short");
                                    return;
                                }
                                let sub_id_len = plaintext[1] as usize;
                                if plaintext.len() < 2 + sub_id_len + 2 {
                                    log::warn!("[WebRTC-DC] CONTENT_FILE sub_id/filename truncated");
                                    return;
                                }
                                let sub_id = std::str::from_utf8(&plaintext[2..2 + sub_id_len])
                                    .unwrap_or("");
                                let fname_offset = 2 + sub_id_len;
                                let filename_len = u16::from_le_bytes([
                                    plaintext[fname_offset],
                                    plaintext[fname_offset + 1],
                                ]) as usize;
                                let fname_start = fname_offset + 2;
                                if plaintext.len() < fname_start + filename_len {
                                    log::warn!("[WebRTC-DC] CONTENT_FILE filename truncated");
                                    return;
                                }
                                let filename = std::str::from_utf8(
                                    &plaintext[fname_start..fname_start + filename_len],
                                )
                                .unwrap_or("paste.png")
                                .to_string();
                                let data = plaintext[fname_start + filename_len..].to_vec();

                                // Parse sub_id for agent/pty routing
                                if let Some(pty_info) = parse_pty_input_sub_id(sub_id, Vec::new(), browser_identity.clone()) {
                                    if let Some(ref tx) = file_input_tx {
                                        let _ = tx.send(FileInputIncoming {
                                            agent_index: pty_info.agent_index,
                                            pty_index: pty_info.pty_index,
                                            filename,
                                            data,
                                        });
                                    }
                                } else {
                                    log::warn!(
                                        "[WebRTC-DC] Failed to parse file input sub_id: {sub_id}"
                                    );
                                }
                                return;
                            }
                            other => {
                                log::warn!("[WebRTC-DC] Unknown content type: 0x{other:02x}");
                                return;
                            }
                        };

                        // Add peer
                        {
                            let mut peers = peers.write().await;
                            peers.insert(PeerId(browser_identity.clone()));
                        }

                        // Send to receive queue
                        if let Some(tx) = recv_tx.lock().await.as_ref() {
                            let _ = tx
                                .send(RawIncoming {
                                    payload: body_bytes,
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

        // Set up ICE candidate handler -- encrypt and send via mpsc for ActionCable relay.
        let ice_crypto = self.crypto_service.clone();
        let ice_signal_tx = self.signal_tx.clone();
        let browser_id = browser_identity.to_string();

        pc.on_ice_candidate(Box::new(move |candidate| {
            let crypto = ice_crypto.clone();
            let signal_tx = ice_signal_tx.clone();
            let browser_id = browser_id.clone();

            Box::pin(async move {
                let Some(c) = candidate else { return };

                let candidate_json = match c.to_json() {
                    Ok(j) => j,
                    Err(e) => {
                        log::error!("[WebRTC] Failed to serialize ICE candidate: {e}");
                        return;
                    }
                };

                // Build the plaintext payload
                let payload = serde_json::json!({
                    "type": "ice",
                    "candidate": candidate_json,
                });

                // Encrypt with E2E encryption if crypto service available
                let envelope = if let Some(ref cs) = crypto {
                    let plaintext = serde_json::to_vec(&payload).unwrap_or_default();
                    match cs.lock() {
                        Ok(mut guard) => match guard.encrypt(&plaintext, crate::relay::extract_olm_key(&browser_id)) {
                            Ok(env) => match serde_json::to_value(&env) {
                                Ok(v) => v,
                                Err(e) => {
                                    log::error!("[WebRTC] Failed to serialize ICE envelope: {e}");
                                    return;
                                }
                            },
                            Err(e) => {
                                log::error!("[WebRTC] Failed to encrypt ICE candidate: {e}");
                                return;
                            }
                        },
                        Err(e) => {
                            log::error!("[WebRTC] Crypto mutex poisoned: {e}");
                            return;
                        }
                    }
                } else {
                    payload
                };

                // Send via mpsc for ActionCable relay
                if let Some(ref tx) = signal_tx {
                    let _ = tx.send(OutgoingSignal::Ice {
                        browser_identity: browser_id.clone(),
                        envelope,
                    });
                } else {
                    log::warn!("[WebRTC] No signal_tx -- cannot relay ICE candidate");
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

    async fn send_to(&self, msg: &[u8], _peer: &PeerId) -> Result<(), ChannelError> {
        let dc_guard = self.data_channel.lock().await;
        let dc = dc_guard
            .as_ref()
            .ok_or_else(|| ChannelError::SendFailed("No data channel".to_string()))?;

        let cs = self
            .crypto_service
            .as_ref()
            .ok_or_else(|| ChannelError::EncryptionError("No crypto service".into()))?;

        let peer_key = self.get_peer_olm_key().await?;

        // Binary inner: [0x00][JSON bytes] (control message)
        let mut plaintext = Vec::with_capacity(1 + msg.len());
        plaintext.push(CONTENT_MSG);
        plaintext.extend_from_slice(msg);

        // Encrypt → binary frame (no base64, no JSON)
        let encrypted = cs
            .lock()
            .map_err(|e| ChannelError::EncryptionError(format!("Crypto mutex poisoned: {e}")))?
            .encrypt_binary(&plaintext, &peer_key)
            .map_err(|e| ChannelError::EncryptionError(e.to_string()))?;

        dc.send(&bytes::Bytes::from(encrypted))
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

    /// Get a clone of the `recv_rx` Arc for spawning a forwarding task.
    ///
    /// The forwarding task takes the `Option<Receiver>` from inside the Arc
    /// and reads from it, sending each message as a `HubEvent::WebRtcMessage`.
    /// After the receiver is taken, [`try_recv`] will return `None`.
    pub(crate) fn recv_rx_arc(
        &self,
    ) -> Arc<tokio::sync::Mutex<Option<tokio::sync::mpsc::Receiver<RawIncoming>>>> {
        Arc::clone(&self.recv_rx)
    }

    /// Get consecutive decryption failure count.
    pub fn decrypt_failure_count(&self) -> u32 {
        self.decrypt_failures.load(Ordering::Relaxed)
    }

    /// Reset decryption failure counter.
    pub fn reset_decrypt_failures(&self) {
        self.decrypt_failures.store(0, Ordering::Relaxed);
    }

    /// Returns `true` exactly once after the DataChannel opens.
    ///
    /// Polled by the tick loop to fire `on_peer_connected` at the right time
    /// (when the DC is actually usable, not just when ICE connects).
    pub fn take_dc_opened(&self) -> bool {
        self.dc_opened.swap(false, Ordering::Relaxed)
    }

    /// Returns a watch receiver for close-complete signaling.
    ///
    /// The value transitions to `true` when the state handler's close task
    /// finishes releasing pc/dc sockets. Callers can `wait_for(|v| *v)`
    /// (with a timeout) before creating a replacement connection to prevent
    /// fd exhaustion. Unlike `Notify`, `watch` retains the last value so
    /// late subscribers see it immediately.
    pub fn close_receiver(&self) -> tokio::sync::watch::Receiver<bool> {
        self.close_complete_rx.clone()
    }

    /// Get the peer's Olm identity key for encrypting messages.
    async fn get_peer_olm_key(&self) -> Result<String, ChannelError> {
        self.peer_olm_key
            .lock()
            .await
            .clone()
            .ok_or_else(|| ChannelError::EncryptionError("No peer Olm key (SDP offer not yet handled)".into()))
    }

    /// Check if the channel is ready for application messages.
    ///
    /// With vodozemac, the session is established on first PreKey decrypt --
    /// no separate handshake needed.
    pub fn is_ready(&self) -> bool {
        self.crypto_service
            .as_ref()
            .and_then(|cs| cs.lock().ok())
            .is_some_and(|guard| guard.has_session())
    }

    /// Send PTY output via the hot path: compress → binary frame → Olm → wire.
    ///
    /// Zero base64, zero JSON. Binary inner format:
    /// `[0x01][flags:1][sub_id_len:1][sub_id][raw payload]`
    pub async fn send_pty_raw(
        &self,
        subscription_id: &str,
        data: &[u8],
        _peer: &PeerId,
    ) -> Result<(), ChannelError> {
        let dc_guard = self.data_channel.lock().await;
        let dc = dc_guard
            .as_ref()
            .ok_or_else(|| ChannelError::SendFailed("No data channel".to_string()))?;

        let cs = self
            .crypto_service
            .as_ref()
            .ok_or_else(|| ChannelError::EncryptionError("No crypto service".into()))?;

        // Compress raw bytes (gzip is very effective on terminal output)
        let config_guard = self.config.lock().await;
        let threshold = config_guard
            .as_ref()
            .and_then(|c| c.compression_threshold);
        drop(config_guard);

        // Compress if above threshold. Cow avoids cloning the common uncompressed path.
        let (payload, was_compressed): (std::borrow::Cow<'_, [u8]>, bool) =
            if let Some(threshold) = threshold {
                let compressed = maybe_compress(data, Some(threshold))
                    .map_err(|e| ChannelError::CompressionError(e.to_string()))?;
                if compressed[0] == 0x1f {
                    (std::borrow::Cow::Owned(compressed[1..].to_vec()), true)
                } else {
                    (std::borrow::Cow::Borrowed(data), false)
                }
            } else {
                (std::borrow::Cow::Borrowed(data), false)
            };

        let peer_key = self.get_peer_olm_key().await?;

        // Build binary inner content: [CONTENT_PTY][flags][sub_id_len][sub_id][payload]
        let sub_bytes = subscription_id.as_bytes();
        let flags: u8 = if was_compressed { 0x01 } else { 0x00 };
        let mut plaintext = Vec::with_capacity(3 + sub_bytes.len() + payload.len());
        plaintext.push(CONTENT_PTY);
        plaintext.push(flags);
        plaintext.push(sub_bytes.len() as u8);
        plaintext.extend_from_slice(sub_bytes);
        plaintext.extend_from_slice(&payload);

        // Encrypt → binary frame (no base64, no JSON)
        let encrypted = cs
            .lock()
            .map_err(|e| ChannelError::EncryptionError(format!("Crypto mutex poisoned: {e}")))?
            .encrypt_binary(&plaintext, &peer_key)
            .map_err(|e| ChannelError::EncryptionError(e.to_string()))?;

        dc.send(&bytes::Bytes::from(encrypted))
            .await
            .map_err(|e| ChannelError::SendFailed(e.to_string()))?;

        Ok(())
    }

    /// Send a stream multiplexer frame via encrypted DataChannel.
    ///
    /// Binary format: `[CONTENT_STREAM][frame_type][stream_id_hi][stream_id_lo][payload]`
    pub async fn send_stream_raw(
        &self,
        frame_type: u8,
        stream_id: u16,
        payload: &[u8],
        _peer: &PeerId,
    ) -> Result<(), ChannelError> {
        let dc_guard = self.data_channel.lock().await;
        let dc = dc_guard
            .as_ref()
            .ok_or_else(|| ChannelError::SendFailed("No data channel".to_string()))?;

        let cs = self
            .crypto_service
            .as_ref()
            .ok_or_else(|| ChannelError::EncryptionError("No crypto service".into()))?;

        let peer_key = self.get_peer_olm_key().await?;

        let stream_id_bytes = stream_id.to_be_bytes();
        let mut plaintext = Vec::with_capacity(4 + payload.len());
        plaintext.push(CONTENT_STREAM);
        plaintext.push(frame_type);
        plaintext.extend_from_slice(&stream_id_bytes);
        plaintext.extend_from_slice(payload);

        let encrypted = cs
            .lock()
            .map_err(|e| ChannelError::EncryptionError(format!("Crypto mutex poisoned: {e}")))?
            .encrypt_binary(&plaintext, &peer_key)
            .map_err(|e| ChannelError::EncryptionError(e.to_string()))?;

        dc.send(&bytes::Bytes::from(encrypted))
            .await
            .map_err(|e| ChannelError::SendFailed(e.to_string()))?;

        Ok(())
    }

    /// Send a bundle refresh (type 2) via DataChannel.
    ///
    /// Wire format: `[0x02][161-byte DeviceKeyBundle]` (unencrypted).
    /// The browser verifies the identity key matches the original QR trust anchor,
    /// then creates a new outbound Olm session from the fresh one-time key.
    pub async fn send_bundle_refresh(&self, bundle_bytes: &[u8]) -> Result<(), ChannelError> {
        let dc_guard = self.data_channel.lock().await;
        let dc = dc_guard
            .as_ref()
            .ok_or_else(|| ChannelError::SendFailed("No data channel".to_string()))?;

        let mut frame = Vec::with_capacity(1 + bundle_bytes.len());
        frame.push(crate::relay::MSG_TYPE_BUNDLE_REFRESH);
        frame.extend_from_slice(bundle_bytes);

        dc.send(&bytes::Bytes::from(frame))
            .await
            .map_err(|e| ChannelError::SendFailed(e.to_string()))?;

        Ok(())
    }
}

/// Parse a terminal subscription ID ("terminal_{agent}_{pty}") into a [`PtyInputIncoming`].
fn parse_pty_input_sub_id(sub_id: &str, data: Vec<u8>, browser_identity: String) -> Option<PtyInputIncoming> {
    let parts: Vec<&str> = sub_id.split('_').collect();
    if parts.len() == 3 && parts[0] == "terminal" {
        let agent_index = parts[1].parse().ok()?;
        let pty_index = parts[2].parse().ok()?;
        Some(PtyInputIncoming {
            agent_index,
            pty_index,
            data,
            browser_identity,
        })
    } else {
        None
    }
}
