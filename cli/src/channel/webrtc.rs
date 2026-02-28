//! WebRTC DataChannel implementation.
//!
//! This module provides `WebRtcChannel`, an implementation of the `Channel`
//! trait that communicates via WebRTC DataChannel with E2E encryption.
//!
//! # Architecture
//!
//! ```text
//! WebRtcChannel
//!     |-- PeerConnection (rustrtc)
//!     |-- DataChannel (SCTP - reliable ordered, no message size limit)
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

use rustrtc::transports::ice::IceCandidate;
use rustrtc::{
    IceServer, PeerConnection, PeerConnectionEvent, RtcConfiguration, SdpType,
    SessionDescription,
};
use rustrtc::transports::sctp::DataChannel;
use rustrtc::DataChannelEvent;

use crate::relay::crypto_service::CryptoService;
use crate::relay::olm_crypto::{CONTENT_FILE, CONTENT_FILE_CHUNK, CONTENT_MSG, CONTENT_PTY, CONTENT_STREAM};

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

/// In-progress chunked file transfer reassembly state.
struct FileChunkAssembly {
    /// Metadata from the first chunk (sub_id, filename parsing).
    agent_index: usize,
    pty_index: usize,
    filename: String,
    /// Accumulated file data across chunks.
    data: Vec<u8>,
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
/// Produced by the ICE candidate forwarder task, drained
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
            data_channel_id: Arc::new(Mutex::new(None)),
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
            event_loop_handle: Arc::new(Mutex::new(None)),
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
    /// WebRTC peer connection (rustrtc — Clone wraps Arc internally).
    peer_connection: Arc<Mutex<Option<PeerConnection>>>,
    /// WebRTC data channel (set by event loop when browser creates it).
    data_channel: Arc<Mutex<Option<Arc<DataChannel>>>>,
    /// DataChannel SCTP stream ID (for `pc.send_data(channel_id, ...)`).
    data_channel_id: Arc<Mutex<Option<u16>>>,
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
    /// When set, the event loop sends `HubEvent::DcOpened` instead of
    /// setting the atomic bool.
    hub_event_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::hub::events::HubEvent>>,
    /// Set to `true` when the connection closes (pc/dc sockets released).
    /// Uses `watch` so late subscribers see the value even if the close already happened.
    close_complete_tx: tokio::sync::watch::Sender<bool>,
    close_complete_rx: tokio::sync::watch::Receiver<bool>,
    /// Handle for the spawned event loop task (for cleanup on disconnect).
    event_loop_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
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
    async fn fetch_ice_config(&self, hub_id: &str) -> Result<Vec<IceServer>, ChannelError> {
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
            ice_servers: Vec<IceServerJson>,
        }

        #[derive(serde::Deserialize)]
        struct IceServerJson {
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
            .map(|s| IceServer {
                urls: vec![s.urls],
                username: s.username,
                credential: s.credential,
                credential_type: rustrtc::IceCredentialType::Password,
            })
            .collect())
    }

    /// Create the WebRTC peer connection.
    fn create_peer_connection(
        &self,
        ice_servers: Vec<IceServer>,
    ) -> Result<PeerConnection, ChannelError> {
        let config = RtcConfiguration {
            ice_servers,
            ..Default::default()
        };

        Ok(PeerConnection::new(config))
    }

    /// Handle incoming SDP offer from browser and create answer.
    ///
    /// Called when CLI receives an encrypted offer via ActionCable signal channel.
    pub async fn handle_sdp_offer(&self, sdp: &str, browser_identity: &str) -> Result<String, ChannelError> {
        // Check for existing connection
        let mut pc_guard = self.peer_connection.lock().await;

        if pc_guard.is_some() {
            // ICE restart: apply new offer on existing PC for renegotiation.
            // The browser sends a new offer with fresh ICE credentials when the
            // network path changes (wifi→cellular, NAT rebinding, etc.). We apply
            // it to the existing PC so ICE can gather new candidates without
            // tearing down the DataChannel or SCTP association.
            let pc = pc_guard.as_ref().unwrap().clone();
            drop(pc_guard);

            log::info!("[WebRTC] Applying ICE restart offer on existing connection");

            let offer = SessionDescription::parse(SdpType::Offer, sdp)
                .map_err(|e| ChannelError::ConnectionFailed(format!("Invalid SDP offer: {e}")))?;

            pc.set_remote_description(offer)
                .await
                .map_err(|e| ChannelError::ConnectionFailed(format!("Failed to set remote description: {e}")))?;

            let answer = pc
                .create_answer()
                .await
                .map_err(|e| ChannelError::ConnectionFailed(format!("Failed to create answer: {e}")))?;

            pc.set_local_description(answer.clone())
                .map_err(|e| ChannelError::ConnectionFailed(format!("Failed to set local description: {e}")))?;

            let mut sdp = answer.to_sdp_string();
            if !sdp.contains("max-message-size") {
                sdp = inject_max_message_size(&sdp, 16 * 1024 * 1024);
            }

            return Ok(sdp);
        }

        // Reset DC-opened flag so a stale signal from the previous PC doesn't
        // cause a premature peer_connected on the new connection.
        self.dc_opened.store(false, Ordering::Relaxed);

        // Fetch ICE config
        let config_guard = self.config.lock().await;
        let hub_id = config_guard
            .as_ref()
            .map(|c| c.hub_id.clone())
            .unwrap_or_default();
        drop(config_guard);

        let ice_servers = self.fetch_ice_config(&hub_id).await?;

        // Create peer connection (sync — no MediaEngine/Registry/APIBuilder boilerplate)
        let pc = self.create_peer_connection(ice_servers)?;

        // Parse and set remote description (offer from browser)
        let offer = SessionDescription::parse(SdpType::Offer, sdp)
            .map_err(|e| ChannelError::ConnectionFailed(format!("Invalid SDP offer: {e}")))?;

        pc.set_remote_description(offer)
            .await
            .map_err(|e| ChannelError::ConnectionFailed(format!("Failed to set remote description: {e}")))?;

        // Create and set local description (answer)
        let answer = pc
            .create_answer()
            .await
            .map_err(|e| ChannelError::ConnectionFailed(format!("Failed to create answer: {e}")))?;

        pc.set_local_description(answer.clone())
            .map_err(|e| ChannelError::ConnectionFailed(format!("Failed to set local description: {e}")))?;

        // Store the peer's Olm key for encrypt routing.
        {
            let mut olm_key = self.peer_olm_key.lock().await;
            *olm_key = Some(crate::relay::extract_olm_key(browser_identity).to_string());
        }

        // Store PC clone in struct (for handle_ice_candidate and send methods)
        *pc_guard = Some(pc.clone());
        drop(pc_guard);

        // Spawn event loop task (replaces all on_* callbacks from webrtc-rs)
        let handle = self.spawn_event_loop(pc, browser_identity);
        *self.event_loop_handle.lock().await = Some(handle);

        // Add peer
        {
            let mut peers = self.peers.write().await;
            peers.insert(PeerId(browser_identity.to_string()));
        }

        // rustrtc omits a=max-message-size from SDP. Per RFC 8841, browsers
        // default to 65536 when absent, causing RTCErrorEvent on large sends
        // (e.g. encrypted screenshots). Chrome caps at 256KB when value is 0,
        // so use an explicit large value (16MB) instead.
        let mut sdp = answer.to_sdp_string();
        if !sdp.contains("max-message-size") {
            sdp = inject_max_message_size(&sdp, 16 * 1024 * 1024);
        }

        // Log the application section of SDP answer for debugging
        if let Some(app_idx) = sdp.find("m=application") {
            let section: String = sdp[app_idx..].lines().take(5).collect::<Vec<_>>().join(" | ");
            log::debug!("[WebRTC] SDP answer application section: {section}");
        } else {
            log::warn!("[WebRTC] SDP answer has no m=application section!");
        }

        Ok(sdp)
    }

    /// Spawn the event loop task that replaces webrtc-rs callbacks.
    ///
    /// This single task handles:
    /// - ICE candidate forwarding (via sub-task)
    /// - Peer connection state changes
    /// - DataChannel open/message/close events
    fn spawn_event_loop(
        &self,
        pc: PeerConnection,
        browser_identity: &str,
    ) -> tokio::task::JoinHandle<()> {
        // Clone all Arc/mpsc handles needed (same set as the old callbacks)
        let state = Arc::clone(&self.state);
        let data_channel = Arc::clone(&self.data_channel);
        let data_channel_id = Arc::clone(&self.data_channel_id);
        let peer_connection = Arc::clone(&self.peer_connection);
        let close_complete = self.close_complete_tx.clone();
        let recv_tx = Arc::clone(&self.recv_tx);
        let peers = Arc::clone(&self.peers);
        let crypto_service = self.crypto_service.clone();
        let browser_id = browser_identity.to_string();
        let decrypt_failures = Arc::clone(&self.decrypt_failures);
        let stream_frame_tx = self.stream_frame_tx.clone();
        let pty_input_tx = self.pty_input_tx.clone();
        let file_input_tx = self.file_input_tx.clone();
        let dc_opened = Arc::clone(&self.dc_opened);
        let hub_event_tx = self.hub_event_tx.clone();

        // Subscribe to ICE candidates for forwarding
        let mut ice_rx = pc.subscribe_ice_candidates();
        let ice_crypto = self.crypto_service.clone();
        let ice_signal_tx = self.signal_tx.clone();
        let ice_browser_id = browser_id.clone();

        // Subscribe to peer connection state changes
        let mut peer_state_rx = pc.subscribe_peer_state();

        tokio::spawn(async move {
            // Sub-task: forward local ICE candidates to browser via ActionCable
            let ice_task = tokio::spawn(async move {
                loop {
                    match ice_rx.recv().await {
                        Ok(candidate) => {
                            // Build JSON matching browser's RTCIceCandidateInit format
                            let candidate_json = serde_json::json!({
                                "candidate": format!("candidate:{}", candidate.to_sdp()),
                                "sdpMid": "0",
                                "sdpMLineIndex": 0,
                            });

                            let payload = serde_json::json!({
                                "type": "ice",
                                "candidate": candidate_json,
                            });

                            // Encrypt with E2E encryption
                            let envelope = if let Some(ref cs) = ice_crypto {
                                let plaintext = serde_json::to_vec(&payload).unwrap_or_default();
                                match cs.lock() {
                                    Ok(mut guard) => match guard.encrypt(&plaintext, crate::relay::extract_olm_key(&ice_browser_id)) {
                                        Ok(env) => match serde_json::to_value(&env) {
                                            Ok(v) => v,
                                            Err(e) => {
                                                log::error!("[WebRTC] Failed to serialize ICE envelope: {e}");
                                                continue;
                                            }
                                        },
                                        Err(e) => {
                                            log::error!("[WebRTC] Failed to encrypt ICE candidate: {e}");
                                            continue;
                                        }
                                    },
                                    Err(e) => {
                                        log::error!("[WebRTC] Crypto mutex poisoned: {e}");
                                        continue;
                                    }
                                }
                            } else {
                                payload
                            };

                            if let Some(ref tx) = ice_signal_tx {
                                let _ = tx.send(OutgoingSignal::Ice {
                                    browser_identity: ice_browser_id.clone(),
                                    envelope,
                                });
                            } else {
                                log::warn!("[WebRTC] No signal_tx -- cannot relay ICE candidate");
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            log::warn!("[WebRTC] ICE candidate subscription lagged by {n}");
                        }
                    }
                }
            });

            // Track the DC reader so we can abort it on exit
            let mut dc_reader_handle: Option<tokio::task::JoinHandle<()>> = None;

            // Notify from DC reader → event loop when DataChannel closes.
            // Without this, the event loop stays alive after DC close (peer
            // state may remain Connected), blocking new offers with
            // "Connection in progress".
            let dc_closed = Arc::new(tokio::sync::Notify::new());

            // Main event loop: select between PeerConnection events, state changes,
            // and DC close notifications
            loop {
                tokio::select! {
                    event = pc.recv() => {
                        match event {
                            Some(PeerConnectionEvent::DataChannel(dc)) => {
                                log::info!("[WebRTC] Data channel opened: {}", dc.label);

                                let channel_id = dc.id;

                                // Store data channel and ID for send methods
                                // dc is already Arc<DataChannel> from PeerConnectionEvent
                                *data_channel.lock().await = Some(Arc::clone(&dc));
                                *data_channel_id.lock().await = Some(channel_id);

                                // Notify DC opened
                                if let Some(ref tx) = hub_event_tx {
                                    let _ = tx.send(crate::hub::events::HubEvent::DcOpened {
                                        browser_identity: browser_id.clone(),
                                    });
                                } else {
                                    dc_opened.store(true, Ordering::Relaxed);
                                }

                                // Abort previous DC reader if replacing (defensive)
                                if let Some(h) = dc_reader_handle.take() {
                                    h.abort();
                                }

                                // Spawn DataChannel message reader
                                let recv_tx = Arc::clone(&recv_tx);
                                let peers = Arc::clone(&peers);
                                let crypto = crypto_service.clone();
                                let browser = browser_id.clone();
                                let failures = Arc::clone(&decrypt_failures);
                                let stream_tx = stream_frame_tx.clone();
                                let pty_tx = pty_input_tx.clone();
                                let file_tx = file_input_tx.clone();
                                let dc_reader = Arc::clone(&dc);
                                let dc_closed_signal = Arc::clone(&dc_closed);

                                dc_reader_handle = Some(tokio::spawn(async move {
                                    let mut chunk_assemblies: std::collections::HashMap<u8, FileChunkAssembly> = std::collections::HashMap::new();
                                    loop {
                                        match dc_reader.recv().await {
                                            Some(DataChannelEvent::Message(data)) => {
                                                handle_dc_message(
                                                    &data,
                                                    &browser,
                                                    &crypto,
                                                    &failures,
                                                    &recv_tx,
                                                    &peers,
                                                    &stream_tx,
                                                    &pty_tx,
                                                    &file_tx,
                                                    &mut chunk_assemblies,
                                                )
                                                .await;
                                            }
                                            Some(DataChannelEvent::Open) => {
                                                log::debug!("[WebRTC-DC] DataChannel Open event");
                                            }
                                            Some(DataChannelEvent::Close) | None => {
                                                log::info!("[WebRTC-DC] DataChannel closed");
                                                dc_closed_signal.notify_one();
                                                break;
                                            }
                                        }
                                    }
                                }));
                            }
                            Some(PeerConnectionEvent::Track(_)) => {
                                // We don't use media tracks, ignore
                            }
                            None => {
                                // PeerConnection event channel closed
                                log::info!("[WebRTC] PeerConnection event loop ended");
                                state.set(ConnectionState::Disconnected).await;
                                data_channel.lock().await.take();
                                data_channel_id.lock().await.take();
                                peer_connection.lock().await.take();
                                let _ = close_complete.send(true);
                                break;
                            }
                        }
                    }
                    _ = peer_state_rx.changed() => {
                        let s = *peer_state_rx.borrow();
                        log::info!("[WebRTC] Connection state changed: {s:?}");
                        match s {
                            rustrtc::PeerConnectionState::Connected => {
                                state.set(ConnectionState::Connected).await;
                            }
                            rustrtc::PeerConnectionState::Disconnected
                            | rustrtc::PeerConnectionState::Failed => {
                                state.set(ConnectionState::Disconnected).await;
                                data_channel.lock().await.take();
                                data_channel_id.lock().await.take();
                                // Close is sync — no 60-second wait
                                if let Some(pc) = peer_connection.lock().await.take() {
                                    pc.close();
                                }
                                let _ = close_complete.send(true);
                                break;
                            }
                            rustrtc::PeerConnectionState::Closed => {
                                state.set(ConnectionState::Disconnected).await;
                                data_channel.lock().await.take();
                                data_channel_id.lock().await.take();
                                peer_connection.lock().await.take();
                                let _ = close_complete.send(true);
                                break;
                            }
                            _ => {}
                        }
                    }
                    _ = dc_closed.notified() => {
                        // DataChannel closed but peer state didn't transition —
                        // tear down so new offers aren't blocked.
                        log::info!("[WebRTC] DataChannel closed, tearing down connection");
                        state.set(ConnectionState::Disconnected).await;
                        data_channel.lock().await.take();
                        data_channel_id.lock().await.take();
                        if let Some(pc) = peer_connection.lock().await.take() {
                            pc.close();
                        }
                        let _ = close_complete.send(true);
                        break;
                    }
                }
            }

            // Cleanup: abort sub-tasks so nothing leaks
            ice_task.abort();
            if let Some(h) = dc_reader_handle {
                h.abort();
            }
        })
    }

    /// Handle incoming ICE candidate from browser.
    pub async fn handle_ice_candidate(
        &self,
        candidate: &str,
        _sdp_mid: Option<&str>,
        _sdp_mline_index: Option<u16>,
    ) -> Result<(), ChannelError> {
        let pc_guard = self.peer_connection.lock().await;
        let pc = pc_guard
            .as_ref()
            .ok_or_else(|| ChannelError::ConnectionFailed("No peer connection".to_string()))?;

        // Parse the candidate SDP string (browser sends "candidate:..." format)
        let sdp_str = candidate.trim_start_matches("candidate:");
        let ice_candidate = IceCandidate::from_sdp(sdp_str)
            .map_err(|e| ChannelError::ConnectionFailed(format!("Failed to parse ICE candidate: {e}")))?;

        pc.add_ice_candidate(ice_candidate)
            .map_err(|e| ChannelError::ConnectionFailed(format!("Failed to add ICE candidate: {e}")))?;

        Ok(())
    }

    /// Get the peer's Olm identity key for encrypting messages.
    async fn get_peer_olm_key(&self) -> Result<String, ChannelError> {
        self.peer_olm_key
            .lock()
            .await
            .clone()
            .ok_or_else(|| ChannelError::EncryptionError("No peer Olm key (SDP offer not yet handled)".into()))
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
        // Abort event loop task (stops recv() loop)
        if let Some(handle) = self.event_loop_handle.lock().await.take() {
            handle.abort();
        }

        // Clear data channel
        self.data_channel.lock().await.take();
        self.data_channel_id.lock().await.take();

        // Close and drop peer connection (sync, immediate — no 60-second wait)
        if let Some(pc) = self.peer_connection.lock().await.take() {
            pc.close();
        }

        self.state.set(ConnectionState::Disconnected).await;
        self.peers.write().await.clear();
        let _ = self.close_complete_tx.send(true);
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
        let pc_guard = self.peer_connection.lock().await;
        let pc = pc_guard
            .as_ref()
            .ok_or_else(|| ChannelError::SendFailed("No peer connection".to_string()))?;

        let dc_id = self.data_channel_id.lock().await
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

        pc.send_data(dc_id, &encrypted)
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
    /// The value transitions to `true` when the connection closes and
    /// sockets are released. Callers can `wait_for(|v| *v)`
    /// (with a timeout) before creating a replacement connection to prevent
    /// fd exhaustion. Unlike `Notify`, `watch` retains the last value so
    /// late subscribers see it immediately.
    pub fn close_receiver(&self) -> tokio::sync::watch::Receiver<bool> {
        self.close_complete_rx.clone()
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
        let pc_guard = self.peer_connection.lock().await;
        let pc = pc_guard
            .as_ref()
            .ok_or_else(|| ChannelError::SendFailed("No peer connection".to_string()))?;

        let dc_id = self.data_channel_id.lock().await
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

        pc.send_data(dc_id, &encrypted)
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
        let pc_guard = self.peer_connection.lock().await;
        let pc = pc_guard
            .as_ref()
            .ok_or_else(|| ChannelError::SendFailed("No peer connection".to_string()))?;

        let dc_id = self.data_channel_id.lock().await
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

        pc.send_data(dc_id, &encrypted)
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
        let pc_guard = self.peer_connection.lock().await;
        let pc = pc_guard
            .as_ref()
            .ok_or_else(|| ChannelError::SendFailed("No peer connection".to_string()))?;

        let dc_id = self.data_channel_id.lock().await
            .ok_or_else(|| ChannelError::SendFailed("No data channel".to_string()))?;

        let mut frame = Vec::with_capacity(1 + bundle_bytes.len());
        frame.push(crate::relay::MSG_TYPE_BUNDLE_REFRESH);
        frame.extend_from_slice(bundle_bytes);

        pc.send_data(dc_id, &frame)
            .await
            .map_err(|e| ChannelError::SendFailed(e.to_string()))?;

        Ok(())
    }
}

/// Lightweight, cloneable send handle for a [`WebRtcChannel`].
///
/// Holds only the `Arc`-wrapped fields needed by the async send methods
/// (`send_pty_raw`, `send_to`, `send_stream_raw`, `send_bundle_refresh`).
/// Designed to be moved into a per-peer `tokio::spawn` task so that
/// DataChannel sends run off the Hub event loop.
#[derive(Clone)]
pub struct WebRtcSender {
    /// Peer connection (owns the SCTP transport).
    peer_connection: Arc<Mutex<Option<PeerConnection>>>,
    /// SCTP stream ID for `pc.send_data(channel_id, ...)`.
    data_channel_id: Arc<Mutex<Option<u16>>>,
    /// Optional Olm crypto for E2E encryption.
    crypto_service: Option<CryptoService>,
    /// Channel configuration (compression threshold, etc.).
    config: Arc<Mutex<Option<ChannelConfig>>>,
    /// Peer's Olm identity key for encryption.
    peer_olm_key: Arc<Mutex<Option<String>>>,
}

impl std::fmt::Debug for WebRtcSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebRtcSender")
            .field("crypto_service", &self.crypto_service.is_some())
            .finish_non_exhaustive()
    }
}

impl WebRtcSender {
    /// Send PTY output: compress, encrypt, send via DataChannel.
    ///
    /// Same logic as [`WebRtcChannel::send_pty_raw`] but operates on the
    /// extracted Arc fields so it can run in a spawned task.
    pub async fn send_pty_raw(
        &self,
        subscription_id: &str,
        data: &[u8],
    ) -> Result<(), ChannelError> {
        let pc_guard = self.peer_connection.lock().await;
        let pc = pc_guard
            .as_ref()
            .ok_or_else(|| ChannelError::SendFailed("No peer connection".to_string()))?;

        let dc_id = self.data_channel_id.lock().await
            .ok_or_else(|| ChannelError::SendFailed("No data channel".to_string()))?;

        let cs = self
            .crypto_service
            .as_ref()
            .ok_or_else(|| ChannelError::EncryptionError("No crypto service".into()))?;

        let config_guard = self.config.lock().await;
        let threshold = config_guard
            .as_ref()
            .and_then(|c| c.compression_threshold);
        drop(config_guard);

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

        let sub_bytes = subscription_id.as_bytes();
        let flags: u8 = if was_compressed { 0x01 } else { 0x00 };
        let mut plaintext = Vec::with_capacity(3 + sub_bytes.len() + payload.len());
        plaintext.push(CONTENT_PTY);
        plaintext.push(flags);
        plaintext.push(sub_bytes.len() as u8);
        plaintext.extend_from_slice(sub_bytes);
        plaintext.extend_from_slice(&payload);

        let encrypted = cs
            .lock()
            .map_err(|e| ChannelError::EncryptionError(format!("Crypto mutex poisoned: {e}")))?
            .encrypt_binary(&plaintext, &peer_key)
            .map_err(|e| ChannelError::EncryptionError(e.to_string()))?;

        pc.send_data(dc_id, &encrypted)
            .await
            .map_err(|e| ChannelError::SendFailed(e.to_string()))?;

        Ok(())
    }

    /// Send a JSON message: serialize, encrypt, send via DataChannel.
    ///
    /// Same logic as [`WebRtcChannel::send_to`] but for the extracted handle.
    pub async fn send_json(
        &self,
        payload: &[u8],
    ) -> Result<(), ChannelError> {
        let pc_guard = self.peer_connection.lock().await;
        let pc = pc_guard
            .as_ref()
            .ok_or_else(|| ChannelError::SendFailed("No peer connection".to_string()))?;

        let dc_id = self.data_channel_id.lock().await
            .ok_or_else(|| ChannelError::SendFailed("No data channel".to_string()))?;

        let cs = self
            .crypto_service
            .as_ref()
            .ok_or_else(|| ChannelError::EncryptionError("No crypto service".into()))?;

        let peer_key = self.get_peer_olm_key().await?;

        // Wrap in CONTENT_MSG frame: [0x00][json bytes]
        let mut plaintext = Vec::with_capacity(1 + payload.len());
        plaintext.push(CONTENT_MSG);
        plaintext.extend_from_slice(payload);

        let encrypted = cs
            .lock()
            .map_err(|e| ChannelError::EncryptionError(format!("Crypto mutex poisoned: {e}")))?
            .encrypt_binary(&plaintext, &peer_key)
            .map_err(|e| ChannelError::EncryptionError(e.to_string()))?;

        pc.send_data(dc_id, &encrypted)
            .await
            .map_err(|e| ChannelError::SendFailed(e.to_string()))?;

        Ok(())
    }

    /// Send a stream multiplexer frame via encrypted DataChannel.
    pub async fn send_stream_raw(
        &self,
        frame_type: u8,
        stream_id: u16,
        payload: &[u8],
    ) -> Result<(), ChannelError> {
        let pc_guard = self.peer_connection.lock().await;
        let pc = pc_guard
            .as_ref()
            .ok_or_else(|| ChannelError::SendFailed("No peer connection".to_string()))?;

        let dc_id = self.data_channel_id.lock().await
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

        pc.send_data(dc_id, &encrypted)
            .await
            .map_err(|e| ChannelError::SendFailed(e.to_string()))?;

        Ok(())
    }

    /// Send a bundle refresh (type 2) via DataChannel (unencrypted).
    pub async fn send_bundle_refresh(&self, bundle_bytes: &[u8]) -> Result<(), ChannelError> {
        let pc_guard = self.peer_connection.lock().await;
        let pc = pc_guard
            .as_ref()
            .ok_or_else(|| ChannelError::SendFailed("No peer connection".to_string()))?;

        let dc_id = self.data_channel_id.lock().await
            .ok_or_else(|| ChannelError::SendFailed("No data channel".to_string()))?;

        let mut frame = Vec::with_capacity(1 + bundle_bytes.len());
        frame.push(crate::relay::MSG_TYPE_BUNDLE_REFRESH);
        frame.extend_from_slice(bundle_bytes);

        pc.send_data(dc_id, &frame)
            .await
            .map_err(|e| ChannelError::SendFailed(e.to_string()))?;

        Ok(())
    }

    /// Get the peer's Olm identity key.
    async fn get_peer_olm_key(&self) -> Result<String, ChannelError> {
        self.peer_olm_key
            .lock()
            .await
            .clone()
            .ok_or_else(|| ChannelError::EncryptionError("No peer Olm key set".into()))
    }
}

impl WebRtcChannel {
    /// Create a [`WebRtcSender`] handle for off-event-loop async sends.
    ///
    /// The sender holds `Arc` clones of the fields needed for encryption
    /// and DataChannel transmission. Safe to move into a `tokio::spawn` task.
    pub fn sender(&self) -> WebRtcSender {
        WebRtcSender {
            peer_connection: Arc::clone(&self.peer_connection),
            data_channel_id: Arc::clone(&self.data_channel_id),
            crypto_service: self.crypto_service.clone(),
            config: Arc::clone(&self.config),
            peer_olm_key: Arc::clone(&self.peer_olm_key),
        }
    }
}

/// Handle an incoming DataChannel message: decrypt and route by content type.
///
/// Extracted as a standalone async fn to avoid deep nesting in the event loop.
#[allow(clippy::too_many_arguments)]
async fn handle_dc_message(
    data: &[u8],
    browser_identity: &str,
    crypto_service: &Option<CryptoService>,
    decrypt_failures: &Arc<AtomicU32>,
    recv_tx: &Arc<Mutex<Option<mpsc::Sender<RawIncoming>>>>,
    peers: &Arc<RwLock<HashSet<PeerId>>>,
    stream_frame_tx: &Option<mpsc::UnboundedSender<StreamIncoming>>,
    pty_input_tx: &Option<mpsc::UnboundedSender<PtyInputIncoming>>,
    file_input_tx: &Option<mpsc::UnboundedSender<FileInputIncoming>>,
    chunk_assemblies: &mut std::collections::HashMap<u8, FileChunkAssembly>,
) {
    // Every DataChannel message is a binary Olm frame:
    // [msg_type:1][ciphertext] or [msg_type:1][key:32][ciphertext]
    let Some(ref cs) = crypto_service else {
        log::error!("[WebRTC-DC] No crypto service -- cannot decrypt");
        return;
    };

    // Decrypt binary frame via vodozemac
    let peer_olm_key = crate::relay::extract_olm_key(browser_identity);
    let plaintext = match cs.lock() {
        Ok(mut guard) => match guard.decrypt_binary(data, Some(peer_olm_key)) {
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
            if let Some(incoming) = parse_pty_input_sub_id(sub_id, payload, browser_identity.to_string()) {
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
                    browser_identity: browser_identity.to_string(),
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
            if let Some(pty_info) = parse_pty_input_sub_id(sub_id, Vec::new(), browser_identity.to_string()) {
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
        CONTENT_FILE_CHUNK => {
            // Chunked file transfer: [0x04][transfer_id][flags][payload...]
            // flags: bit 0 = START (first chunk), bit 1 = END (last chunk)
            if plaintext.len() < 4 {
                log::warn!("[WebRTC-DC] CONTENT_FILE_CHUNK frame too short");
                return;
            }
            let transfer_id = plaintext[1];
            let flags = plaintext[2];
            let is_start = (flags & 0x01) != 0;
            let is_end = (flags & 0x02) != 0;
            let payload = &plaintext[3..];

            if is_start {
                // First chunk: payload = [sub_id_len][sub_id][fname_len:2LE][fname][data...]
                // Same layout as CONTENT_FILE minus the 0x03 byte
                if payload.len() < 3 {
                    log::warn!("[WebRTC-DC] CONTENT_FILE_CHUNK START too short");
                    return;
                }
                let sub_id_len = payload[0] as usize;
                if payload.len() < 1 + sub_id_len + 2 {
                    log::warn!("[WebRTC-DC] CONTENT_FILE_CHUNK START sub_id truncated");
                    return;
                }
                let sub_id = std::str::from_utf8(&payload[1..1 + sub_id_len]).unwrap_or("");
                let fname_offset = 1 + sub_id_len;
                let filename_len = u16::from_le_bytes([
                    payload[fname_offset],
                    payload[fname_offset + 1],
                ]) as usize;
                let fname_start = fname_offset + 2;
                if payload.len() < fname_start + filename_len {
                    log::warn!("[WebRTC-DC] CONTENT_FILE_CHUNK START filename truncated");
                    return;
                }
                let filename = std::str::from_utf8(&payload[fname_start..fname_start + filename_len])
                    .unwrap_or("paste.png")
                    .to_string();
                let file_data = &payload[fname_start + filename_len..];

                // Parse routing info
                let (agent_index, pty_index) = if let Some(pty_info) =
                    parse_pty_input_sub_id(sub_id, Vec::new(), browser_identity.to_string())
                {
                    (pty_info.agent_index, pty_info.pty_index)
                } else {
                    log::warn!("[WebRTC-DC] CONTENT_FILE_CHUNK: bad sub_id: {sub_id}");
                    return;
                };

                log::debug!(
                    "[WebRTC-DC] File chunk START: transfer_id={transfer_id}, filename={filename}, first_data={}",
                    file_data.len()
                );

                let mut assembly = FileChunkAssembly {
                    agent_index,
                    pty_index,
                    filename,
                    data: Vec::with_capacity(512 * 1024), // pre-allocate for typical file
                };
                assembly.data.extend_from_slice(file_data);

                if is_end {
                    // Single chunk with both START and END (small file sent as chunk)
                    if let Some(ref tx) = file_input_tx {
                        let _ = tx.send(FileInputIncoming {
                            agent_index: assembly.agent_index,
                            pty_index: assembly.pty_index,
                            filename: assembly.filename,
                            data: assembly.data,
                        });
                    }
                } else {
                    chunk_assemblies.insert(transfer_id, assembly);
                }
            } else {
                // Middle or last chunk: payload = raw file data
                let assembly = match chunk_assemblies.get_mut(&transfer_id) {
                    Some(a) => a,
                    None => {
                        log::warn!(
                            "[WebRTC-DC] CONTENT_FILE_CHUNK: no assembly for transfer_id={transfer_id}"
                        );
                        return;
                    }
                };
                assembly.data.extend_from_slice(payload);

                if is_end {
                    let assembly = chunk_assemblies.remove(&transfer_id).unwrap();
                    log::debug!(
                        "[WebRTC-DC] File chunk END: transfer_id={transfer_id}, total={}",
                        assembly.data.len()
                    );
                    if let Some(ref tx) = file_input_tx {
                        let _ = tx.send(FileInputIncoming {
                            agent_index: assembly.agent_index,
                            pty_index: assembly.pty_index,
                            filename: assembly.filename,
                            data: assembly.data,
                        });
                    }
                }
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
        peers.insert(PeerId(browser_identity.to_string()));
    }

    // Send to receive queue
    if let Some(tx) = recv_tx.lock().await.as_ref() {
        let _ = tx
            .send(RawIncoming {
                payload: body_bytes,
                sender: PeerId(browser_identity.to_string()),
            })
            .await;
    } else {
        log::error!("[WebRTC-DC] recv_tx is None! Cannot queue message");
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

/// Inject `a=max-message-size:{value}` into the application media section of an SDP string.
///
/// Browsers default to 65536 when this attribute is absent (RFC 8841 §6.1),
/// which causes `RTCErrorEvent` when sending encrypted payloads larger than 64 KB
/// (e.g. screenshot file transfers). A value of 0 means no limit.
fn inject_max_message_size(sdp: &str, value: u64) -> String {
    // Insert after the m=application line (before the next m= or at end)
    let mut result = String::with_capacity(sdp.len() + 30);
    let mut injected = false;

    for line in sdp.lines() {
        result.push_str(line);
        result.push_str("\r\n");

        if !injected && line.starts_with("m=application") {
            result.push_str(&format!("a=max-message-size:{value}\r\n"));
            injected = true;
        }
    }

    result
}
