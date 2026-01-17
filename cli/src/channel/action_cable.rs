//! ActionCable channel implementation.
//!
//! This module provides `ActionCableChannel`, an implementation of the `Channel`
//! trait that communicates via Rails ActionCable WebSocket with optional Signal
//! Protocol encryption.
//!
//! # Architecture
//!
//! ```text
//! ActionCableChannel
//!     ├── WebSocket connection (tokio-tungstenite)
//!     ├── Signal encryption (optional, via SignalProtocolManager)
//!     ├── Gzip compression (optional, via compression module)
//!     └── Reconnection (exponential backoff)
//! ```
//!
//! Rust guideline compliant 2025-01

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::client::IntoClientRequest, tungstenite::Message};

use crate::relay::crypto_service::CryptoServiceHandle;
use crate::relay::signal::SignalEnvelope;

use super::compression::{maybe_compress, maybe_decompress};
use super::{Channel, ChannelConfig, ChannelError, ConnectionState, IncomingMessage, PeerId, SharedConnectionState};

/// Reconnection backoff configuration.
const INITIAL_BACKOFF_SECS: u64 = 1;
const MAX_BACKOFF_SECS: u64 = 30;

/// Connection health check configuration.
const CONNECTION_STALE_TIMEOUT_SECS: u64 = 15;
const HEALTH_CHECK_INTERVAL_SECS: u64 = 5;

/// Action Cable message format.
#[derive(Debug, Serialize, Deserialize)]
struct CableMessage {
    command: String,
    identifier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<String>,
}

/// Action Cable subscription identifier.
#[derive(Debug, Serialize, Deserialize)]
struct ChannelIdentifier {
    channel: String,
    hub_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_index: Option<usize>,
}

/// Incoming Action Cable message.
#[derive(Debug, Deserialize)]
struct IncomingCableMessage {
    #[serde(rename = "type")]
    msg_type: Option<String>,
    message: Option<serde_json::Value>,
}

/// Internal message type for the send queue.
#[derive(Debug)]
enum OutgoingMessage {
    /// Broadcast to all peers.
    Broadcast(Vec<u8>),
    /// Send to specific peer.
    Targeted { peer: PeerId, data: Vec<u8> },
}

/// Cloneable handle for sending messages through a channel.
///
/// This allows sending messages without holding a reference to the full channel,
/// which is useful for spawning async send tasks.
#[derive(Clone, Debug)]
pub struct ChannelSenderHandle {
    send_tx: mpsc::Sender<OutgoingMessage>,
    peers: Arc<RwLock<HashSet<PeerId>>>,
}

impl ChannelSenderHandle {
    /// Send a message to a specific peer.
    ///
    /// # Errors
    ///
    /// Returns an error if the peer is not connected or if sending fails.
    pub async fn send_to(&self, msg: &[u8], peer: &PeerId) -> Result<(), ChannelError> {
        // Check if peer is connected
        {
            let peers = self.peers.read().await;
            if !peers.contains(peer) {
                return Err(ChannelError::NoSession(peer.clone()));
            }
        }

        self.send_tx
            .send(OutgoingMessage::Targeted {
                peer: peer.clone(),
                data: msg.to_vec(),
            })
            .await
            .map_err(|_| ChannelError::SendFailed("Send channel closed".to_string()))
    }
}

/// Internal message received from WebSocket.
#[derive(Debug)]
struct RawIncoming {
    payload: Vec<u8>,
    sender: PeerId,
}

/// ActionCable channel with optional Signal Protocol encryption.
pub struct ActionCableChannel {
    /// Channel configuration (set on connect).
    config: Option<ChannelConfig>,

    /// Shared connection state.
    state: Arc<SharedConnectionState>,

    /// Crypto service handle for encryption (None = unencrypted).
    /// Uses CryptoServiceHandle which is Send + Clone for thread-safe access.
    crypto_service: Option<CryptoServiceHandle>,

    /// Server URL (without /cable suffix).
    server_url: String,

    /// API key for authentication.
    api_key: String,

    /// Send queue for outgoing messages.
    send_tx: Option<mpsc::Sender<OutgoingMessage>>,

    /// Receive queue for incoming messages.
    recv_rx: Option<mpsc::Receiver<RawIncoming>>,

    /// Connected peer identities.
    peers: Arc<RwLock<HashSet<PeerId>>>,

    /// Shutdown signal sender.
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl std::fmt::Debug for ActionCableChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionCableChannel")
            .field("config", &self.config)
            .field("server_url", &self.server_url)
            .field("encrypted", &self.crypto_service.is_some())
            .finish_non_exhaustive()
    }
}

impl ActionCableChannel {
    /// Create an encrypted channel using the provided crypto service handle.
    ///
    /// The crypto service is shared, enabling session reuse across channels.
    /// CryptoServiceHandle is Send + Clone, so this channel can run on any thread.
    #[must_use]
    pub fn encrypted(
        crypto_service: CryptoServiceHandle,
        server_url: String,
        api_key: String,
    ) -> Self {
        Self {
            config: None,
            state: SharedConnectionState::new(),
            crypto_service: Some(crypto_service),
            server_url,
            api_key,
            send_tx: None,
            recv_rx: None,
            peers: Arc::new(RwLock::new(HashSet::new())),
            shutdown_tx: None,
        }
    }

    /// Create an unencrypted channel.
    #[must_use]
    pub fn unencrypted(server_url: String, api_key: String) -> Self {
        Self {
            config: None,
            state: SharedConnectionState::new(),
            crypto_service: None,
            server_url,
            api_key,
            send_tx: None,
            recv_rx: None,
            peers: Arc::new(RwLock::new(HashSet::new())),
            shutdown_tx: None,
        }
    }

    /// Get the shared connection state for external observation.
    #[must_use]
    pub fn shared_state(&self) -> Arc<SharedConnectionState> {
        Arc::clone(&self.state)
    }

    /// Get a cloneable sender handle for this channel.
    ///
    /// The sender handle can be used to send messages without holding
    /// a reference to the full channel. This is useful for spawning
    /// async send tasks.
    ///
    /// Returns `None` if the channel is not connected.
    #[must_use]
    pub fn get_sender_handle(&self) -> Option<ChannelSenderHandle> {
        self.send_tx.as_ref().map(|tx| ChannelSenderHandle {
            send_tx: tx.clone(),
            peers: Arc::clone(&self.peers),
        })
    }

    /// Non-blocking drain of all available incoming messages.
    ///
    /// Returns a vector of all messages currently in the receive queue.
    /// Does not block if no messages are available.
    ///
    /// This is used by the event loop to poll agent channels for input
    /// without blocking the main loop.
    pub fn drain_incoming(&mut self) -> Vec<IncomingMessage> {
        let Some(ref mut rx) = self.recv_rx else {
            return Vec::new();
        };

        let mut messages = Vec::new();
        while let Ok(raw) = rx.try_recv() {
            messages.push(IncomingMessage {
                payload: raw.payload,
                sender: raw.sender,
            });
        }
        messages
    }

    /// Run the connection loop with automatic reconnection.
    #[allow(clippy::too_many_arguments)]
    async fn run_connection_loop(
        crypto_service: Option<CryptoServiceHandle>,
        config: ChannelConfig,
        server_url: String,
        api_key: String,
        state: Arc<SharedConnectionState>,
        peers: Arc<RwLock<HashSet<PeerId>>>,
        mut send_rx: mpsc::Receiver<OutgoingMessage>,
        recv_tx: mpsc::Sender<RawIncoming>,
        mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    ) {
        let mut backoff_secs = INITIAL_BACKOFF_SECS;

        loop {
            // Check for shutdown
            if shutdown_rx.try_recv().is_ok() {
                log::info!("Channel shutdown requested");
                break;
            }

            state.set(ConnectionState::Connecting).await;

            match Self::connect_websocket(&server_url, &api_key, &config).await {
                Ok((mut write, mut read, identifier_json)) => {
                    log::info!(
                        "Connected to {} for hub {} (agent: {:?})",
                        config.channel_name,
                        config.hub_id,
                        config.agent_index
                    );

                    state.set(ConnectionState::Connected).await;
                    backoff_secs = INITIAL_BACKOFF_SECS;

                    // Run message loop - returns true if shutdown was requested
                    let shutdown_requested = Self::run_message_loop(
                        &crypto_service,
                        &config,
                        &identifier_json,
                        &mut write,
                        &mut read,
                        &mut send_rx,
                        &recv_tx,
                        &peers,
                        &mut shutdown_rx,
                    )
                    .await;

                    if shutdown_requested {
                        log::info!("Shutdown requested, exiting reconnection loop");
                        break;
                    }

                    log::warn!("Disconnected from {}", config.channel_name);
                }
                Err(e) => {
                    log::warn!("Failed to connect to {}: {}", config.channel_name, e);
                }
            }

            // Exponential backoff with jitter
            let jitter_ms = rand::random::<u64>() % 1000;
            let wait_ms = backoff_secs * 1000 + jitter_ms;
            state
                .set(ConnectionState::Reconnecting {
                    attempt: (backoff_secs / INITIAL_BACKOFF_SECS) as u32,
                    next_retry_ms: wait_ms,
                })
                .await;

            log::info!(
                "Reconnecting to {} in {:.1}s...",
                config.channel_name,
                wait_ms as f32 / 1000.0
            );

            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(wait_ms)) => {}
                _ = &mut shutdown_rx => {
                    log::info!("Channel shutdown during reconnect backoff");
                    break;
                }
            }

            backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
        }

        state.set(ConnectionState::Disconnected).await;
    }

    /// Connect to WebSocket and subscribe to channel.
    async fn connect_websocket(
        server_url: &str,
        api_key: &str,
        config: &ChannelConfig,
    ) -> Result<
        (
            futures_util::stream::SplitSink<
                tokio_tungstenite::WebSocketStream<
                    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
                >,
                Message,
            >,
            futures_util::stream::SplitStream<
                tokio_tungstenite::WebSocketStream<
                    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
                >,
            >,
            String,
        ),
        ChannelError,
    > {
        let ws_url = format!(
            "{}/cable",
            server_url
                .replace("https://", "wss://")
                .replace("http://", "ws://")
        );

        log::debug!("Connecting to ActionCable: {}", ws_url);

        let mut request = ws_url
            .into_client_request()
            .map_err(|e| ChannelError::ConnectionFailed(format!("invalid URL: {e}")))?;

        request.headers_mut().insert(
            "Origin",
            server_url
                .parse()
                .unwrap_or_else(|_| "http://localhost".parse().expect("valid")),
        );
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {}", api_key).parse().expect("valid header"),
        );

        let (ws_stream, _) = connect_async(request)
            .await
            .map_err(|e| ChannelError::ConnectionFailed(format!("WebSocket connect failed: {e}")))?;

        let (mut write, mut read) = ws_stream.split();

        // Wait for welcome
        let welcome_timeout = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(msg) = read.next().await {
                if let Ok(Message::Text(text)) = msg {
                    if let Ok(cable_msg) = serde_json::from_str::<IncomingCableMessage>(&text) {
                        if cable_msg.msg_type.as_deref() == Some("welcome") {
                            return Ok(());
                        }
                    }
                }
            }
            Err(ChannelError::ConnectionFailed(
                "WebSocket closed before welcome".into(),
            ))
        })
        .await;

        match welcome_timeout {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(ChannelError::ConnectionFailed(
                    "Timeout waiting for welcome".into(),
                ))
            }
        }

        // Subscribe to channel
        let identifier = ChannelIdentifier {
            channel: config.channel_name.clone(),
            hub_id: config.hub_id.clone(),
            agent_index: config.agent_index,
        };
        let identifier_json =
            serde_json::to_string(&identifier).expect("identifier serializable");

        let subscribe = CableMessage {
            command: "subscribe".to_string(),
            identifier: identifier_json.clone(),
            data: None,
        };

        write
            .send(Message::Text(
                serde_json::to_string(&subscribe).expect("serializable"),
            ))
            .await
            .map_err(|e| ChannelError::ConnectionFailed(format!("subscribe failed: {e}")))?;

        log::info!(
            "Subscribed to {} for hub {}",
            config.channel_name,
            config.hub_id
        );

        Ok((write, read, identifier_json))
    }

    /// Run the message loop until disconnect.
    ///
    /// Returns `true` if exit was due to shutdown signal, `false` otherwise
    /// (WebSocket close, error, health timeout). Caller should break out of
    /// reconnection loop if shutdown was requested.
    #[allow(clippy::too_many_arguments)]
    async fn run_message_loop(
        crypto_service: &Option<CryptoServiceHandle>,
        config: &ChannelConfig,
        identifier_json: &str,
        write: &mut futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
        read: &mut futures_util::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
        send_rx: &mut mpsc::Receiver<OutgoingMessage>,
        recv_tx: &mpsc::Sender<RawIncoming>,
        peers: &Arc<RwLock<HashSet<PeerId>>>,
        shutdown_rx: &mut tokio::sync::oneshot::Receiver<()>,
    ) -> bool {
        let mut last_activity = Instant::now();
        let mut health_interval =
            tokio::time::interval(Duration::from_secs(HEALTH_CHECK_INTERVAL_SECS));

        loop {
            tokio::select! {
                // Outgoing messages
                Some(msg) = send_rx.recv() => {
                    Self::handle_outgoing(
                        crypto_service,
                        config,
                        identifier_json,
                        write,
                        msg,
                        peers,
                    ).await;
                }

                // Incoming messages
                Some(msg) = read.next() => {
                    last_activity = Instant::now();

                    match msg {
                        Ok(Message::Text(text)) => {
                            if let Some(incoming) = Self::handle_incoming(
                                crypto_service,
                                config,
                                &text,
                                peers,
                            ).await {
                                if recv_tx.send(incoming).await.is_err() {
                                    log::warn!("Receive channel closed");
                                    return false;
                                }
                            }
                        }
                        Ok(Message::Ping(data)) => {
                            if write.send(Message::Pong(data)).await.is_err() {
                                log::warn!("Failed to send pong");
                                return false;
                            }
                        }
                        Ok(Message::Close(_)) => {
                            log::info!("WebSocket closed by server");
                            return false;
                        }
                        Err(e) => {
                            log::error!("WebSocket error: {}", e);
                            return false;
                        }
                        _ => {}
                    }
                }

                // Health check
                _ = health_interval.tick() => {
                    if last_activity.elapsed() > Duration::from_secs(CONNECTION_STALE_TIMEOUT_SECS) {
                        log::warn!("Connection stale ({}s), reconnecting", last_activity.elapsed().as_secs());
                        return false;
                    }
                }

                // Shutdown - exit permanently, don't attempt reconnection
                _ = &mut *shutdown_rx => {
                    log::info!("Shutdown signal received");
                    return true;
                }
            }
        }
    }

    /// Handle outgoing message.
    async fn handle_outgoing(
        crypto_service: &Option<CryptoServiceHandle>,
        config: &ChannelConfig,
        identifier_json: &str,
        write: &mut futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
        msg: OutgoingMessage,
        peers: &Arc<RwLock<HashSet<PeerId>>>,
    ) {
        let (data, targets): (Vec<u8>, Vec<PeerId>) = match msg {
            OutgoingMessage::Broadcast(d) => {
                let peer_list: Vec<PeerId> = peers.read().await.iter().cloned().collect();
                (d, peer_list)
            }
            OutgoingMessage::Targeted { peer, data } => (data, vec![peer]),
        };

        if targets.is_empty() {
            return;
        }

        // Compress
        let compressed = match maybe_compress(&data, config.compression_threshold) {
            Ok(c) => c,
            Err(e) => {
                log::error!("Compression failed: {}", e);
                return;
            }
        };

        // Send to each target (encrypted if enabled)
        for target in targets {
            let envelope_data = if let Some(ref cs) = crypto_service {
                // Encrypt via CryptoServiceHandle (message passing, no lock needed)
                match cs.encrypt(&compressed, target.as_ref()).await {
                    Ok(envelope) => {
                        serde_json::json!({
                            "action": "relay",
                            "recipient_identity": target.as_ref(),
                            "envelope": {
                                "version": envelope.version,
                                "message_type": envelope.message_type,
                                "ciphertext": envelope.ciphertext,
                                "sender_identity": envelope.sender_identity,
                                "registration_id": envelope.registration_id,
                                "device_id": envelope.device_id,
                            }
                        })
                    }
                    Err(e) => {
                        log::error!("Encryption failed for {}: {}", target, e);
                        continue;
                    }
                }
            } else {
                // Unencrypted
                serde_json::json!({
                    "action": "relay",
                    "data": data_encoding::BASE64.encode(&compressed),
                })
            };

            let cable_msg = CableMessage {
                command: "message".to_string(),
                identifier: identifier_json.to_string(),
                data: Some(serde_json::to_string(&envelope_data).expect("serializable")),
            };

            if let Err(e) = write
                .send(Message::Text(
                    serde_json::to_string(&cable_msg).expect("serializable"),
                ))
                .await
            {
                log::error!("Failed to send to {}: {}", target, e);
            }
        }
    }

    /// Handle incoming message, returns parsed message if valid.
    async fn handle_incoming(
        crypto_service: &Option<CryptoServiceHandle>,
        config: &ChannelConfig,
        text: &str,
        peers: &Arc<RwLock<HashSet<PeerId>>>,
    ) -> Option<RawIncoming> {
        let cable_msg: IncomingCableMessage = serde_json::from_str(text).ok()?;

        // Handle system messages
        if let Some(ref msg_type) = cable_msg.msg_type {
            match msg_type.as_str() {
                "welcome" => log::debug!("Welcome received"),
                "confirm_subscription" => log::info!("{} subscription confirmed", config.channel_name),
                "ping" => {}
                _ => {}
            }
        }

        // Handle data messages
        let message = cable_msg.message?;

        if let Some(ref cs) = crypto_service {
            // Encrypted - parse envelope
            let envelope_json = message.get("envelope")?;
            let envelope: SignalEnvelope = match envelope_json {
                serde_json::Value::String(s) => serde_json::from_str(s).ok()?,
                _ => serde_json::from_value(envelope_json.clone()).ok()?,
            };

            let sender = PeerId(envelope.sender_identity.clone());

            // Track peer
            {
                let mut peer_set = peers.write().await;
                if peer_set.insert(sender.clone()) {
                    log::info!("New peer connected: {}", sender);
                }
            }

            // Decrypt via CryptoServiceHandle (message passing, no lock needed)
            let plaintext = match cs.decrypt(&envelope).await {
                Ok(p) => p,
                Err(e) => {
                    log::warn!("Decryption failed: {}", e);
                    return None;
                }
            };

            // Persist session state
            if let Err(e) = cs.persist().await {
                log::warn!("Failed to persist session: {}", e);
            }

            // Decompress
            let decompressed = match maybe_decompress(&plaintext) {
                Ok(d) => d,
                Err(e) => {
                    log::warn!("Decompression failed: {}", e);
                    return None;
                }
            };

            Some(RawIncoming {
                payload: decompressed,
                sender,
            })
        } else {
            // Unencrypted - parse raw data
            let data_b64 = message.get("data")?.as_str()?;
            let compressed = data_encoding::BASE64.decode(data_b64.as_bytes()).ok()?;
            let decompressed = maybe_decompress(&compressed).ok()?;

            // Unencrypted channels don't have peer identity
            Some(RawIncoming {
                payload: decompressed,
                sender: PeerId("anonymous".to_string()),
            })
        }
    }
}

#[async_trait]
impl Channel for ActionCableChannel {
    async fn connect(&mut self, config: ChannelConfig) -> Result<(), ChannelError> {
        if self.config.is_some() {
            return Err(ChannelError::ConnectionFailed(
                "Already connected".to_string(),
            ));
        }

        // Create channels
        let (send_tx, send_rx) = mpsc::channel::<OutgoingMessage>(100);
        let (recv_tx, recv_rx) = mpsc::channel::<RawIncoming>(100);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        self.send_tx = Some(send_tx);
        self.recv_rx = Some(recv_rx);
        self.shutdown_tx = Some(shutdown_tx);
        self.config = Some(config.clone());

        // Spawn connection task - using tokio::spawn since CryptoServiceHandle is Send
        let crypto_service = self.crypto_service.clone();
        let server_url = self.server_url.clone();
        let api_key = self.api_key.clone();
        let state = Arc::clone(&self.state);
        let peers = Arc::clone(&self.peers);

        tokio::spawn(async move {
            Self::run_connection_loop(
                crypto_service,
                config,
                server_url,
                api_key,
                state,
                peers,
                send_rx,
                recv_tx,
                shutdown_rx,
            )
            .await;
        });

        Ok(())
    }

    async fn disconnect(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        self.config = None;
        self.send_tx = None;
        self.recv_rx = None;
        self.state.set(ConnectionState::Disconnected).await;
    }

    fn state(&self) -> ConnectionState {
        // Blocking read for sync interface
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.state.get())
        })
    }

    async fn send(&self, msg: &[u8]) -> Result<(), ChannelError> {
        let tx = self
            .send_tx
            .as_ref()
            .ok_or(ChannelError::SendFailed("Not connected".to_string()))?;

        tx.send(OutgoingMessage::Broadcast(msg.to_vec()))
            .await
            .map_err(|_| ChannelError::SendFailed("Send channel closed".to_string()))
    }

    async fn send_to(&self, msg: &[u8], peer: &PeerId) -> Result<(), ChannelError> {
        if !self.has_peer(peer) {
            return Err(ChannelError::NoSession(peer.clone()));
        }

        let tx = self
            .send_tx
            .as_ref()
            .ok_or(ChannelError::SendFailed("Not connected".to_string()))?;

        tx.send(OutgoingMessage::Targeted {
            peer: peer.clone(),
            data: msg.to_vec(),
        })
        .await
        .map_err(|_| ChannelError::SendFailed("Send channel closed".to_string()))
    }

    async fn recv(&mut self) -> Result<IncomingMessage, ChannelError> {
        let rx = self
            .recv_rx
            .as_mut()
            .ok_or(ChannelError::Closed)?;

        let raw = rx.recv().await.ok_or(ChannelError::Closed)?;

        Ok(IncomingMessage {
            payload: raw.payload,
            sender: raw.sender,
        })
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

impl Drop for ActionCableChannel {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}
