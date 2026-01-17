//! Hub relay for E2E encrypted hub-level communication with browsers.
//!
//! This module handles hub-level commands and responses between CLI and browsers:
//! - Agent list updates (broadcast to all browsers)
//! - Agent creation progress (broadcast)
//! - Browser commands (create agent, select agent, etc.)
//! - Browser handshake and connection management
//!
//! Terminal I/O (PTY output/input) is handled by agent-owned channels, not this relay.
//! See `cli/src/channel/action_cable.rs` for per-agent terminal channels.
//!
//! # Protocol
//!
//! 1. CLI connects to Action Cable WebSocket
//! 2. CLI subscribes to TerminalRelayChannel with hub_id
//! 3. CLI displays QR code with PreKeyBundle
//! 4. Browser scans QR, processes PreKeyBundle, creates session
//! 5. Browser sends encrypted handshake
//! 6. CLI decrypts, creating Double Ratchet session
//! 7. Hub-level messages flow encrypted through this relay
//!
//! # Architecture
//!
//! ```text
//! HubRelay (this module)           Agent Channels
//! - Browser handshake              - Per-agent terminal I/O
//! - Agent list broadcasts          - PTY output → browser
//! - Hub commands from browser      - Keyboard input → PTY
//! ```
//!
//! Rust guideline compliant 2025-01

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::client::IntoClientRequest, tungstenite::Message};

use data_encoding::BASE32_NOPAD;

use super::crypto_service::CryptoServiceHandle;
use super::signal::SignalEnvelope;
use super::state::IdentifiedBrowserEvent;
use super::types::{BrowserCommand, BrowserEvent, BrowserResize, TerminalMessage};

/// Reconnection backoff configuration.
const INITIAL_BACKOFF_SECS: u64 = 1;
const MAX_BACKOFF_SECS: u64 = 30;

/// Connection health check configuration.
///
/// ActionCable sends pings every 3 seconds. If we don't receive any server
/// activity (pings, messages, or subscription confirmations) for this duration,
/// we assume the connection is dead and trigger a reconnect.
///
/// This catches silent connection failures (half-open TCP, NAT timeout, load
/// balancer disconnect) that would otherwise leave the CLI thinking it's
/// connected while the WebSocket is actually dead.
const CONNECTION_STALE_TIMEOUT_SECS: u64 = 15;

/// How often to check connection health.
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
///
/// CLI subscribes without browser_identity → gets CLI stream.
/// Rails uses hub_id for routing.
#[derive(Debug, Serialize, Deserialize)]
struct ChannelIdentifier {
    channel: String,
    hub_id: String,
}

/// Incoming Action Cable message.
#[derive(Debug, Deserialize)]
struct IncomingCableMessage {
    #[serde(rename = "type")]
    msg_type: Option<String>,
    message: Option<serde_json::Value>,
}

/// Output message for relay task.
#[derive(Debug)]
enum OutputMessage {
    /// Broadcast to all connected browsers.
    Broadcast(String),
    /// Send to a specific browser by identity.
    Targeted {
        identity: String,
        data: String,
    },
    /// Request to regenerate the PreKeyBundle with a fresh PreKey.
    /// Response will be sent via the event channel as BrowserEvent::BundleRegenerated.
    RegenerateBundle,
}

/// Handle for sending terminal output to the browser.
///
/// This is a simple channel sender that queues output for the relay task.
#[derive(Clone)]
pub struct HubSender {
    tx: mpsc::Sender<OutputMessage>,
    connected: Arc<RwLock<bool>>,
}

impl std::fmt::Debug for HubSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubSender").finish_non_exhaustive()
    }
}

impl HubSender {
    /// Send terminal output to all browsers (will be encrypted by relay task).
    pub async fn send(&self, output: &str) -> Result<()> {
        // Only send if browser is connected
        if !*self.connected.read().await {
            return Ok(()); // Silently drop if no browser connected
        }

        self.tx.send(OutputMessage::Broadcast(output.to_string())).await
            .map_err(|e| anyhow::anyhow!("Failed to queue output: {}", e))
    }

    /// Send terminal output to a specific browser by identity.
    ///
    /// This enables per-client output routing: each browser only receives
    /// output from agents it's viewing.
    pub async fn send_to(&self, identity: &str, output: &str) -> Result<()> {
        // Only send if browser is connected
        if !*self.connected.read().await {
            return Ok(()); // Silently drop if no browser connected
        }

        self.tx.send(OutputMessage::Targeted {
            identity: identity.to_string(),
            data: output.to_string(),
        }).await
            .map_err(|e| anyhow::anyhow!("Failed to queue targeted output: {}", e))
    }

    /// Check if browser is connected and ready for encrypted communication.
    pub async fn is_ready(&self) -> bool {
        *self.connected.read().await
    }

    /// Request regeneration of the PreKeyBundle with a fresh PreKey.
    /// The new bundle will be sent back via the event channel as BrowserEvent::BundleRegenerated.
    pub async fn request_bundle_regeneration(&self) -> Result<()> {
        self.tx.send(OutputMessage::RegenerateBundle).await
            .map_err(|e| anyhow::anyhow!("Failed to request bundle regeneration: {}", e))
    }
}

/// Terminal relay connection manager.
///
/// Uses a `CryptoServiceHandle` for all encryption/decryption operations,
/// allowing the relay to run in any thread (not restricted to a LocalSet).
pub struct HubRelay {
    crypto_service: CryptoServiceHandle,
    hub_identifier: String,
    server_url: String,
    api_key: String,
}

impl std::fmt::Debug for HubRelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubRelay")
            .field("hub_identifier", &self.hub_identifier)
            .field("server_url", &self.server_url)
            .finish_non_exhaustive()
    }
}

impl HubRelay {
    /// Create a new terminal relay with a crypto service handle.
    ///
    /// The `crypto_service` handle is Send + Clone, so this relay can run
    /// in any thread, not just a LocalSet.
    pub fn new(
        crypto_service: CryptoServiceHandle,
        hub_identifier: String,
        server_url: String,
        api_key: String,
    ) -> Self {
        Self {
            crypto_service,
            hub_identifier,
            server_url,
            api_key,
        }
    }

    /// Connect to Action Cable and start relaying messages.
    ///
    /// Returns:
    /// - `HubSender` - for sending terminal output to browser
    /// - `mpsc::Receiver<IdentifiedBrowserEvent>` - for receiving events from browser with identity
    /// - `oneshot::Receiver<()>` - signals when the relay task exits permanently
    ///
    /// The relay automatically reconnects on WebSocket disconnection with exponential backoff.
    /// The shutdown receiver only fires when the entire relay task exits (rare).
    pub async fn connect(self) -> Result<(HubSender, mpsc::Receiver<IdentifiedBrowserEvent>, oneshot::Receiver<()>)> {
        let (event_tx, event_rx) = mpsc::channel::<IdentifiedBrowserEvent>(100);
        let (sender, shutdown_rx) = self.connect_with_event_channel(event_tx).await?;
        Ok((sender, event_rx, shutdown_rx))
    }

    /// Connect to Action Cable with an external event channel.
    ///
    /// This variant is used when the caller needs to manage the event channel
    /// separately (e.g., for cross-thread communication).
    ///
    /// Returns:
    /// - `HubSender` for sending terminal output to browser
    /// - `oneshot::Receiver<()>` that fires when the relay task exits permanently
    ///
    /// Events are sent to the provided `event_tx` channel with browser identity attached.
    /// The relay automatically reconnects on WebSocket disconnection with exponential backoff.
    ///
    /// Note: Since we now use `CryptoServiceHandle` instead of owning `SignalProtocolManager`,
    /// this task can run in any thread (no LocalSet required).
    pub async fn connect_with_event_channel(
        self,
        event_tx: mpsc::Sender<IdentifiedBrowserEvent>,
    ) -> Result<(HubSender, oneshot::Receiver<()>)> {
        // Create channel for terminal output (lives across reconnections)
        let (output_tx, output_rx) = mpsc::channel::<OutputMessage>(100);

        // Shared connection state
        let connected = Arc::new(RwLock::new(false));

        // Create output sender handle
        let output_sender = HubSender {
            tx: output_tx,
            connected: Arc::clone(&connected),
        };

        // Shutdown signal - fires when relay task exits permanently
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        // Spawn reconnection task - no LocalSet required since crypto ops go through CryptoService
        let hub_identifier = self.hub_identifier.clone();
        let server_url = self.server_url.clone();
        let api_key = self.api_key.clone();
        let crypto_service = self.crypto_service;

        tokio::spawn(async move {
            // Ensure shutdown signal is sent when task exits
            let _shutdown_guard = scopeguard::guard(shutdown_tx, |tx| {
                let _ = tx.send(());
            });

            Self::run_with_reconnection(
                crypto_service,
                hub_identifier,
                server_url,
                api_key,
                output_rx,
                connected,
                event_tx,
            ).await;
        });

        Ok((output_sender, shutdown_rx))
    }

    /// Run the relay with automatic reconnection on WebSocket failure.
    async fn run_with_reconnection(
        crypto_service: CryptoServiceHandle,
        hub_identifier: String,
        server_url: String,
        api_key: String,
        mut output_rx: mpsc::Receiver<OutputMessage>,
        connected: Arc<RwLock<bool>>,
        event_tx: mpsc::Sender<IdentifiedBrowserEvent>,
    ) {
        let mut backoff_secs = INITIAL_BACKOFF_SECS;
        let mut browser_identities: HashSet<String> = HashSet::new();

        loop {
            // Attempt to connect
            match Self::connect_websocket(&server_url, &api_key, &hub_identifier).await {
                Ok((mut write, mut read, identifier_json)) => {
                    log::info!("WebSocket connected to terminal relay");
                    backoff_secs = INITIAL_BACKOFF_SECS; // Reset backoff on success

                    // Run message loop until WebSocket dies
                    Self::run_message_loop(
                        &crypto_service,
                        &hub_identifier,
                        &server_url,
                        &mut write,
                        &mut read,
                        &identifier_json,
                        &mut output_rx,
                        &connected,
                        &event_tx,
                        &mut browser_identities,
                    ).await;

                    // Mark disconnected
                    *connected.write().await = false;
                    log::warn!("WebSocket disconnected from terminal relay");
                }
                Err(e) => {
                    log::warn!("Failed to connect to terminal relay: {e}");
                }
            }

            // Exponential backoff with jitter before reconnecting
            let jitter_ms = rand::random::<u64>() % 1000;
            let wait = Duration::from_secs(backoff_secs) + Duration::from_millis(jitter_ms);
            log::info!("Reconnecting to terminal relay in {:.1}s...", wait.as_secs_f32());
            tokio::time::sleep(wait).await;

            backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
        }
    }

    /// Connect to WebSocket and subscribe to channel.
    async fn connect_websocket(
        server_url: &str,
        api_key: &str,
        hub_identifier: &str,
    ) -> Result<(
        futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, Message>,
        futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>,
        String,
    )> {
        let ws_url = format!(
            "{}/cable",
            server_url.replace("https://", "wss://").replace("http://", "ws://")
        );

        log::debug!("Connecting to Action Cable: {}", ws_url);

        // Build request with required headers
        let mut request = ws_url
            .into_client_request()
            .context("Failed to build WebSocket request")?;
        request.headers_mut().insert(
            "Origin",
            server_url
                .parse()
                .unwrap_or_else(|_| "http://localhost".parse().expect("localhost is valid")),
        );
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {}", api_key)
                .parse()
                .expect("Bearer token is valid"),
        );

        // Connect to WebSocket
        let (ws_stream, _) = connect_async(request)
            .await
            .context("Failed to connect to Action Cable")?;

        let (mut write, mut read) = ws_stream.split();

        // Wait for Action Cable "welcome" message
        log::debug!("Waiting for Action Cable welcome message...");
        let welcome_timeout = tokio::time::timeout(
            Duration::from_secs(10),
            async {
                while let Some(msg) = read.next().await {
                    if let Ok(Message::Text(text)) = msg {
                        if let Ok(cable_msg) = serde_json::from_str::<IncomingCableMessage>(&text) {
                            if cable_msg.msg_type.as_deref() == Some("welcome") {
                                log::info!("Action Cable welcome received");
                                return Ok(());
                            }
                        }
                    }
                }
                anyhow::bail!("WebSocket closed before welcome")
            }
        ).await;

        match welcome_timeout {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => anyhow::bail!("Timeout waiting for Action Cable welcome"),
        }

        // Build channel identifier for hub-level commands
        let identifier = ChannelIdentifier {
            channel: "HubChannel".to_string(),
            hub_id: hub_identifier.to_string(),
        };
        let identifier_json = serde_json::to_string(&identifier)?;

        // Subscribe to channel
        let subscribe = CableMessage {
            command: "subscribe".to_string(),
            identifier: identifier_json.clone(),
            data: None,
        };
        write.send(Message::Text(serde_json::to_string(&subscribe)?)).await?;

        log::info!("Subscribed to HubChannel for hub {}", hub_identifier);

        Ok((write, read, identifier_json))
    }

    /// Run the message loop until WebSocket disconnects.
    ///
    /// Includes connection health monitoring: if no server activity (pings,
    /// messages) is received for `CONNECTION_STALE_TIMEOUT_SECS`, assumes the
    /// connection is dead and breaks to trigger reconnection.
    #[allow(clippy::too_many_arguments)]
    async fn run_message_loop(
        crypto_service: &CryptoServiceHandle,
        hub_identifier: &str,
        server_url: &str,
        write: &mut futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, Message>,
        read: &mut futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>,
        identifier_json: &str,
        output_rx: &mut mpsc::Receiver<OutputMessage>,
        connected: &Arc<RwLock<bool>>,
        event_tx: &mpsc::Sender<IdentifiedBrowserEvent>,
        browser_identities: &mut HashSet<String>,
    ) {
        // Track last server activity for connection health monitoring.
        // ActionCable sends pings every 3 seconds, so if we don't see any
        // activity for CONNECTION_STALE_TIMEOUT_SECS, the connection is likely dead.
        let mut last_server_activity = Instant::now();
        let mut health_check_interval = tokio::time::interval(
            Duration::from_secs(HEALTH_CHECK_INTERVAL_SECS)
        );

        loop {
            tokio::select! {
                // Handle outgoing messages (CLI -> browser) and commands
                Some(output_msg) = output_rx.recv() => {
                    match output_msg {
                        OutputMessage::RegenerateBundle => {
                            // Generate a new PreKeyBundle with a fresh PreKey
                            log::info!("Regenerating PreKeyBundle on request");
                            match crypto_service.get_prekey_bundle(
                                crypto_service.next_prekey_id().await.unwrap_or(1)
                            ).await {
                                Ok(bundle) => {
                                    log::info!("New PreKeyBundle generated with PreKey {}",
                                        bundle.prekey_id.unwrap_or(0));
                                    // Send the new bundle back to the hub via event channel
                                    let _ = event_tx.send((
                                        BrowserEvent::BundleRegenerated { bundle },
                                        String::new(), // No browser identity for this event
                                    )).await;
                                }
                                Err(e) => {
                                    log::error!("Failed to regenerate PreKeyBundle: {}", e);
                                }
                            }
                        }
                        _ => {
                            // Normal output handling - requires connected browsers
                            if *connected.read().await && !browser_identities.is_empty() {
                                Self::handle_output(
                                    crypto_service,
                                    write,
                                    identifier_json,
                                    output_msg,
                                    browser_identities,
                                ).await;
                            }
                        }
                    }
                }

                // Handle incoming messages (browser -> CLI)
                Some(msg) = read.next() => {
                    // Any message from server means connection is alive
                    last_server_activity = Instant::now();

                    match msg {
                        Ok(Message::Text(text)) => {
                            Self::handle_incoming_text(
                                crypto_service,
                                hub_identifier,
                                server_url,
                                write,
                                identifier_json,
                                &text,
                                connected,
                                event_tx,
                                browser_identities,
                            ).await;
                        }
                        Ok(Message::Ping(data)) => {
                            if let Err(e) = write.send(Message::Pong(data)).await {
                                log::warn!("Failed to send pong: {}", e);
                            }
                        }
                        Ok(Message::Close(_)) => {
                            log::info!("Action Cable WebSocket closed by server");
                            break;
                        }
                        Err(e) => {
                            log::error!("WebSocket error: {}", e);
                            break;
                        }
                        _ => {}
                    }
                }

                // Periodic health check for stale connections
                _ = health_check_interval.tick() => {
                    let elapsed = last_server_activity.elapsed();
                    if elapsed > Duration::from_secs(CONNECTION_STALE_TIMEOUT_SECS) {
                        log::warn!(
                            "No server activity for {}s (timeout: {}s), connection likely dead - reconnecting",
                            elapsed.as_secs(),
                            CONNECTION_STALE_TIMEOUT_SECS
                        );
                        break;
                    }
                }

                else => break,
            }
        }
    }

    /// Handle outgoing message to browser(s).
    async fn handle_output(
        crypto_service: &CryptoServiceHandle,
        write: &mut futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, Message>,
        identifier_json: &str,
        output_msg: OutputMessage,
        browser_identities: &HashSet<String>,
    ) {
        let (output, targets): (String, Vec<&String>) = match &output_msg {
            OutputMessage::Broadcast(data) => {
                (data.clone(), browser_identities.iter().collect())
            }
            OutputMessage::Targeted { identity, data } => {
                if browser_identities.contains(identity) {
                    (data.clone(), vec![identity])
                } else {
                    log::warn!("Targeted send to unknown identity: {}", identity);
                    return;
                }
            }
            OutputMessage::RegenerateBundle => {
                // This is handled in the main select! loop, not here
                log::warn!("RegenerateBundle reached handle_output - should not happen");
                return;
            }
        };

        let message = if let Ok(parsed) = serde_json::from_str::<TerminalMessage>(&output) {
            parsed
        } else {
            TerminalMessage::Output { data: output }
        };

        let plaintext = match serde_json::to_vec(&message) {
            Ok(p) => p,
            Err(e) => {
                log::error!("Failed to serialize message: {}", e);
                return;
            }
        };

        for identity in targets {
            match crypto_service.encrypt(&plaintext, identity).await {
                Ok(envelope) => {
                    let data = serde_json::json!({
                        "action": "relay",
                        "recipient_identity": identity,
                        "envelope": {
                            "version": envelope.version,
                            "message_type": envelope.message_type,
                            "ciphertext": envelope.ciphertext,
                            "sender_identity": envelope.sender_identity,
                            "registration_id": envelope.registration_id,
                            "device_id": envelope.device_id,
                        }
                    });
                    let cable_msg = CableMessage {
                        command: "message".to_string(),
                        identifier: identifier_json.to_string(),
                        data: Some(serde_json::to_string(&data).expect("serializable")),
                    };

                    if let Err(e) = write.send(Message::Text(
                        serde_json::to_string(&cable_msg).expect("serializable")
                    )).await {
                        log::error!("Failed to send output to {}: {}", identity, e);
                    }
                }
                Err(e) => {
                    log::error!("Encryption failed for {}: {}", identity, e);
                }
            }
        }
    }

    /// Handle incoming text message from Action Cable.
    #[allow(clippy::too_many_arguments)]
    async fn handle_incoming_text(
        crypto_service: &CryptoServiceHandle,
        hub_identifier: &str,
        server_url: &str,
        write: &mut futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, Message>,
        identifier_json: &str,
        text: &str,
        connected: &Arc<RwLock<bool>>,
        event_tx: &mpsc::Sender<IdentifiedBrowserEvent>,
        browser_identities: &mut HashSet<String>,
    ) {
        let Ok(cable_msg) = serde_json::from_str::<IncomingCableMessage>(text) else {
            return;
        };

        // Handle different message types
        if let Some(ref msg_type) = cable_msg.msg_type {
            match msg_type.as_str() {
                "welcome" => log::info!("Action Cable welcome received"),
                "confirm_subscription" => log::info!("TerminalChannel subscription confirmed"),
                "ping" => {} // Ignore
                _ => {}
            }
        }

        // Handle broadcast messages (encrypted envelopes from browser)
        let Some(message) = cable_msg.message else {
            return;
        };

        let Some(envelope_json) = message.get("envelope") else {
            return;
        };

        // Parse the envelope
        let envelope: SignalEnvelope = match envelope_json {
            serde_json::Value::String(s) => {
                match serde_json::from_str(s) {
                    Ok(e) => e,
                    Err(e) => {
                        log::warn!("Failed to parse envelope string: {}", e);
                        return;
                    }
                }
            }
            _ => {
                match serde_json::from_value(envelope_json.clone()) {
                    Ok(e) => e,
                    Err(e) => {
                        log::warn!("Failed to parse envelope: {}", e);
                        return;
                    }
                }
            }
        };

        // Decrypt the envelope
        let plaintext = match crypto_service.decrypt(&envelope).await {
            Ok(p) => p,
            Err(e) => {
                log::warn!("Decryption failed: {}", e);
                return;
            }
        };

        if let Err(e) = crypto_service.persist().await {
            log::warn!("Failed to persist: {}", e);
        }

        // Track browser identity and ensure connected flag is set.
        // IMPORTANT: Always set connected=true when we receive a valid message,
        // not just for new identities. This handles the case where:
        // 1. WebSocket reconnects after silent disconnect
        // 2. browser_identities still has the identity from before
        // 3. But connected was set to false on disconnect
        // Without this, all sends would silently fail.
        let is_new = browser_identities.insert(envelope.sender_identity.clone());
        if is_new {
            log::info!("Browser connected: {} (total: {})", envelope.sender_identity, browser_identities.len());
        }
        // Always ensure connected=true when we have active browsers
        if !browser_identities.is_empty() {
            *connected.write().await = true;
        }

        // Parse and handle command
        let cmd: BrowserCommand = match serde_json::from_slice(&plaintext) {
            Ok(c) => c,
            Err(e) => {
                if let Ok(text) = String::from_utf8(plaintext.clone()) {
                    log::debug!("Received text message: {}", text);
                } else {
                    log::warn!("Failed to parse command: {}", e);
                }
                return;
            }
        };

        Self::handle_browser_command(
            crypto_service,
            hub_identifier,
            server_url,
            write,
            identifier_json,
            cmd,
            &envelope.sender_identity,
            event_tx,
        ).await;
    }

    /// Handle a parsed browser command.
    #[allow(clippy::too_many_arguments)]
    async fn handle_browser_command(
        crypto_service: &CryptoServiceHandle,
        hub_identifier: &str,
        server_url: &str,
        write: &mut futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, Message>,
        identifier_json: &str,
        cmd: BrowserCommand,
        sender_identity: &str,
        event_tx: &mpsc::Sender<IdentifiedBrowserEvent>,
    ) {
        let event = match cmd {
            BrowserCommand::Handshake { device_name, .. } => {
                log::info!("Browser handshake from: {}", device_name);

                // Send handshake_ack back to browser
                let ack = serde_json::json!({
                    "type": "handshake_ack",
                    "cli_version": env!("CARGO_PKG_VERSION"),
                    "hub_id": hub_identifier,
                });
                Self::send_encrypted(crypto_service, write, identifier_json, sender_identity, &ack).await;

                if let Err(e) = event_tx.send((
                    BrowserEvent::Connected {
                        public_key: sender_identity.to_string(),
                        device_name,
                    },
                    sender_identity.to_string(),
                )).await {
                    log::error!("Failed to send event: {}", e);
                }
                return;
            }
            BrowserCommand::GenerateInvite => {
                log::info!("Browser requested invite bundle");

                match crypto_service.get_prekey_bundle(2).await {
                    Ok(bundle) => {
                        let bundle_bytes = bundle.to_binary().expect("PreKeyBundle binary serializable");
                        let bundle_encoded = BASE32_NOPAD.encode(&bundle_bytes);
                        let invite_url = format!("{}/hubs/{}#{}", server_url, hub_identifier, bundle_encoded);

                        let response = TerminalMessage::InviteBundle {
                            bundle: bundle_encoded,
                            url: invite_url,
                        };
                        Self::send_encrypted(crypto_service, write, identifier_json, sender_identity, &response).await;
                    }
                    Err(e) => {
                        log::error!("Failed to generate invite bundle: {}", e);
                        let error_msg = TerminalMessage::Error {
                            message: format!("Failed to generate invite: {}", e),
                        };
                        Self::send_encrypted(crypto_service, write, identifier_json, sender_identity, &error_msg).await;
                    }
                }
                return;
            }
            BrowserCommand::Input { data } => BrowserEvent::Input(data),
            BrowserCommand::SetMode { mode } => BrowserEvent::SetMode { mode },
            BrowserCommand::ListAgents => BrowserEvent::ListAgents,
            BrowserCommand::ListWorktrees => BrowserEvent::ListWorktrees,
            BrowserCommand::SelectAgent { id } => BrowserEvent::SelectAgent { id },
            BrowserCommand::CreateAgent { issue_or_branch, prompt } => {
                BrowserEvent::CreateAgent { issue_or_branch, prompt }
            }
            BrowserCommand::ReopenWorktree { path, branch, prompt } => {
                BrowserEvent::ReopenWorktree { path, branch, prompt }
            }
            BrowserCommand::DeleteAgent { id, delete_worktree } => {
                BrowserEvent::DeleteAgent {
                    id,
                    delete_worktree: delete_worktree.unwrap_or(false),
                }
            }
            BrowserCommand::TogglePtyView => BrowserEvent::TogglePtyView,
            BrowserCommand::Scroll { direction, lines } => {
                BrowserEvent::Scroll { direction, lines: lines.unwrap_or(3) }
            }
            BrowserCommand::ScrollToBottom => BrowserEvent::ScrollToBottom,
            BrowserCommand::ScrollToTop => BrowserEvent::ScrollToTop,
            BrowserCommand::Resize { cols, rows } => {
                BrowserEvent::Resize(BrowserResize { cols, rows })
            }
        };

        if let Err(e) = event_tx.send((event, sender_identity.to_string())).await {
            log::error!("Failed to forward event: {}", e);
        }
    }

    /// Send an encrypted message to a browser.
    async fn send_encrypted<T: Serialize>(
        crypto_service: &CryptoServiceHandle,
        write: &mut futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, Message>,
        identifier_json: &str,
        recipient_identity: &str,
        message: &T,
    ) {
        let bytes = match serde_json::to_vec(message) {
            Ok(b) => b,
            Err(e) => {
                log::error!("Failed to serialize message: {}", e);
                return;
            }
        };

        match crypto_service.encrypt(&bytes, recipient_identity).await {
            Ok(envelope) => {
                let data = serde_json::json!({
                    "action": "relay",
                    "recipient_identity": recipient_identity,
                    "envelope": {
                        "version": envelope.version,
                        "message_type": envelope.message_type,
                        "ciphertext": envelope.ciphertext,
                        "sender_identity": envelope.sender_identity,
                        "registration_id": envelope.registration_id,
                        "device_id": envelope.device_id,
                    }
                });
                let cable_msg = CableMessage {
                    command: "message".to_string(),
                    identifier: identifier_json.to_string(),
                    data: Some(serde_json::to_string(&data).expect("serializable")),
                };

                if let Err(e) = write.send(Message::Text(
                    serde_json::to_string(&cable_msg).expect("serializable")
                )).await {
                    log::error!("Failed to send encrypted message: {}", e);
                }
            }
            Err(e) => {
                log::error!("Encryption failed: {}", e);
            }
        }
    }
}
