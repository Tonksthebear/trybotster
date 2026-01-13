//! Terminal relay for E2E encrypted communication with browser via Action Cable.
//!
//! This module handles:
//! - WebSocket connection to Rails Action Cable (TerminalChannel)
//! - E2E encryption using Signal Protocol (X3DH + Double Ratchet)
//! - Relaying encrypted terminal output to browser
//! - Receiving encrypted terminal input from browser
//!
//! # Protocol
//!
//! 1. CLI connects to Action Cable WebSocket
//! 2. CLI subscribes to TerminalChannel with hub_identifier
//! 3. CLI displays QR code with PreKeyBundle
//! 4. Browser scans QR, processes PreKeyBundle, creates session
//! 5. Browser sends PreKeySignalMessage in presence "join"
//! 6. CLI decrypts PreKeySignalMessage, creating Double Ratchet session
//! 7. Both sides have session - server only sees encrypted blobs
//!
//! # Architecture Note
//!
//! Signal Protocol uses non-Send futures (async_trait(?Send)), so all
//! encryption/decryption must run on the same task. We use tokio::select!
//! to multiplex WebSocket I/O with output processing on a single task.
//!
//! Rust guideline compliant 2025-01

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::client::IntoClientRequest, tungstenite::Message};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

use super::signal::{SignalEnvelope, SignalProtocolManager};
use super::state::IdentifiedBrowserEvent;
use super::types::{BrowserCommand, BrowserEvent, BrowserResize, TerminalMessage};

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
    hub_identifier: String,
    device_type: String,
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
}

/// Handle for sending terminal output to the browser.
///
/// This is a simple channel sender that queues output for the relay task.
#[derive(Clone)]
pub struct TerminalOutputSender {
    tx: mpsc::Sender<OutputMessage>,
    connected: Arc<RwLock<bool>>,
}

impl std::fmt::Debug for TerminalOutputSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalOutputSender").finish_non_exhaustive()
    }
}

impl TerminalOutputSender {
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
}

/// Terminal relay connection manager.
pub struct TerminalRelay {
    signal_manager: SignalProtocolManager,
    hub_identifier: String,
    server_url: String,
    api_key: String,
}

impl std::fmt::Debug for TerminalRelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalRelay")
            .field("hub_identifier", &self.hub_identifier)
            .field("server_url", &self.server_url)
            .finish_non_exhaustive()
    }
}

impl TerminalRelay {
    /// Create a new terminal relay with a Signal Protocol manager.
    pub fn new(
        signal_manager: SignalProtocolManager,
        hub_identifier: String,
        server_url: String,
        api_key: String,
    ) -> Self {
        Self {
            signal_manager,
            hub_identifier,
            server_url,
            api_key,
        }
    }

    /// Connect to Action Cable and start relaying messages.
    ///
    /// Returns:
    /// - `TerminalOutputSender` - for sending terminal output to browser
    /// - `mpsc::Receiver<IdentifiedBrowserEvent>` - for receiving events from browser with identity
    ///
    /// The relay runs on the current task using select! to multiplex I/O.
    /// This is required because Signal Protocol futures are not Send.
    pub async fn connect(self) -> Result<(TerminalOutputSender, mpsc::Receiver<IdentifiedBrowserEvent>)> {
        let (event_tx, event_rx) = mpsc::channel::<IdentifiedBrowserEvent>(100);
        let sender = self.connect_with_event_channel(event_tx).await?;
        Ok((sender, event_rx))
    }

    /// Connect to Action Cable with an external event channel.
    ///
    /// This variant is used when the caller needs to manage the event channel
    /// separately (e.g., for cross-thread communication).
    ///
    /// Returns `TerminalOutputSender` for sending terminal output to browser.
    /// Events are sent to the provided `event_tx` channel with browser identity attached.
    pub async fn connect_with_event_channel(
        mut self,
        event_tx: mpsc::Sender<IdentifiedBrowserEvent>,
    ) -> Result<TerminalOutputSender> {
        let ws_url = self.build_ws_url();
        let hub_identifier = self.hub_identifier.clone();

        log::info!("Connecting to Action Cable: {}", ws_url);

        // Build request with required headers
        let mut request = ws_url
            .into_client_request()
            .context("Failed to build WebSocket request")?;
        request.headers_mut().insert(
            "Origin",
            self.server_url
                .parse()
                .unwrap_or_else(|_| "http://localhost".parse().expect("localhost is valid")),
        );
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {}", self.api_key)
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
            std::time::Duration::from_secs(10),
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

        // Build channel identifier
        let identifier = ChannelIdentifier {
            channel: "TerminalRelayChannel".to_string(),
            hub_identifier: hub_identifier.clone(),
            device_type: "cli".to_string(),
        };
        let identifier_json = serde_json::to_string(&identifier)?;

        // Subscribe to channel
        let subscribe = CableMessage {
            command: "subscribe".to_string(),
            identifier: identifier_json.clone(),
            data: None,
        };
        write.send(Message::Text(serde_json::to_string(&subscribe)?)).await?;

        log::info!("Sent subscribe to TerminalRelayChannel for hub {}", hub_identifier);

        // Create channel for terminal output
        let (output_tx, mut output_rx) = mpsc::channel::<OutputMessage>(100);

        // Shared connection state
        let connected = Arc::new(RwLock::new(false));

        // Create output sender handle
        let output_sender = TerminalOutputSender {
            tx: output_tx,
            connected: Arc::clone(&connected),
        };

        // Track browser identities for multi-session encryption
        // Each connected browser has its own Signal session
        let mut browser_identities: HashSet<String> = HashSet::new();

        // Spawn single task that handles all I/O using select!
        // This avoids Send requirements since everything runs on one task
        let connected_clone = Arc::clone(&connected);
        tokio::task::spawn_local(async move {
            loop {
                tokio::select! {
                    // Handle outgoing messages (CLI -> browser)
                    Some(output_msg) = output_rx.recv() => {
                        if *connected_clone.read().await && !browser_identities.is_empty() {
                            // Determine targets and output based on message type
                            let (output, targets): (String, Vec<&String>) = match &output_msg {
                                OutputMessage::Broadcast(data) => {
                                    // Broadcast to all connected browsers
                                    (data.clone(), browser_identities.iter().collect())
                                }
                                OutputMessage::Targeted { identity, data } => {
                                    // Send to specific browser only (if connected)
                                    if browser_identities.contains(identity) {
                                        (data.clone(), vec![identity])
                                    } else {
                                        log::warn!("Targeted send to unknown identity: {}", identity);
                                        continue;
                                    }
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
                                    continue;
                                }
                            };

                            // Encrypt and send to target browser(s)
                            for identity in targets {
                                match self.signal_manager.encrypt(&plaintext, identity).await {
                                    Ok(envelope) => {
                                        // recipient_identity at top level for server-side routing
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
                                            identifier: identifier_json.clone(),
                                            data: Some(serde_json::to_string(&data).expect("serializable")),
                                        };

                                        if let Err(e) = write.send(Message::Text(
                                            serde_json::to_string(&cable_msg).expect("serializable")
                                        )).await {
                                            log::error!("Failed to send output to {}: {}", identity, e);
                                            // Don't break - continue sending to other browsers
                                        }
                                    }
                                    Err(e) => {
                                        log::error!("Encryption failed for {}: {}", identity, e);
                                    }
                                }
                            }
                        }
                    }

                    // Handle incoming messages (browser -> CLI)
                    Some(msg) = read.next() => {
                        match msg {
                            Ok(Message::Text(text)) => {
                                if let Ok(cable_msg) = serde_json::from_str::<IncomingCableMessage>(&text) {
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
                                    if let Some(message) = cable_msg.message {
                                        // Browser sends { envelope: SignalEnvelope } via relay action
                                        if let Some(envelope_json) = message.get("envelope") {
                                            // Parse the envelope string (it's JSON-encoded)
                                            let envelope: SignalEnvelope = match envelope_json {
                                                serde_json::Value::String(s) => {
                                                    match serde_json::from_str(s) {
                                                        Ok(e) => e,
                                                        Err(e) => {
                                                            log::warn!("Failed to parse envelope string: {}", e);
                                                            continue;
                                                        }
                                                    }
                                                }
                                                _ => {
                                                    match serde_json::from_value(envelope_json.clone()) {
                                                        Ok(e) => e,
                                                        Err(e) => {
                                                            log::warn!("Failed to parse envelope: {}", e);
                                                            continue;
                                                        }
                                                    }
                                                }
                                            };

                                            // Decrypt the envelope
                                            match self.signal_manager.decrypt(&envelope).await {
                                                Ok(plaintext) => {
                                                    if let Err(e) = self.signal_manager.persist().await {
                                                        log::warn!("Failed to persist: {}", e);
                                                    }

                                                    // Track browser identity for multi-session
                                                    // Add each new browser to the set
                                                    let is_new = browser_identities.insert(envelope.sender_identity.clone());
                                                    if is_new {
                                                        log::info!("Browser connected: {} (total: {})", envelope.sender_identity, browser_identities.len());
                                                        *connected_clone.write().await = true;
                                                    }

                                                    // Parse decrypted message
                                                    match serde_json::from_slice::<BrowserCommand>(&plaintext) {
                                                        Ok(cmd) => {
                                                            let event = match cmd {
                                                                BrowserCommand::Handshake { device_name, .. } => {
                                                                    log::info!("Browser handshake from: {}", device_name);

                                                                    // Send handshake_ack back to browser
                                                                    let ack = serde_json::json!({
                                                                        "type": "handshake_ack",
                                                                        "cli_version": env!("CARGO_PKG_VERSION"),
                                                                        "hub_id": hub_identifier,
                                                                    });
                                                                    let ack_bytes = serde_json::to_vec(&ack).expect("serializable");

                                                                    match self.signal_manager.encrypt(&ack_bytes, &envelope.sender_identity).await {
                                                                        Ok(ack_envelope) => {
                                                                            let ack_data = serde_json::json!({
                                                                                "action": "relay",
                                                                                "recipient_identity": &envelope.sender_identity,
                                                                                "envelope": {
                                                                                    "version": ack_envelope.version,
                                                                                    "message_type": ack_envelope.message_type,
                                                                                    "ciphertext": ack_envelope.ciphertext,
                                                                                    "sender_identity": ack_envelope.sender_identity,
                                                                                    "registration_id": ack_envelope.registration_id,
                                                                                    "device_id": ack_envelope.device_id,
                                                                                }
                                                                            });
                                                                            let ack_cable = CableMessage {
                                                                                command: "message".to_string(),
                                                                                identifier: identifier_json.clone(),
                                                                                data: Some(serde_json::to_string(&ack_data).expect("serializable")),
                                                                            };

                                                                            if let Err(e) = write.send(Message::Text(
                                                                                serde_json::to_string(&ack_cable).expect("serializable")
                                                                            )).await {
                                                                                log::error!("Failed to send handshake_ack: {}", e);
                                                                            } else {
                                                                                log::info!("Sent handshake_ack to browser");
                                                                            }
                                                                        }
                                                                        Err(e) => {
                                                                            log::error!("Failed to encrypt handshake_ack: {}", e);
                                                                        }
                                                                    }

                                                                    let browser_identity = envelope.sender_identity.clone();
                                                                    if let Err(e) = event_tx.send((
                                                                        BrowserEvent::Connected {
                                                                            public_key: browser_identity.clone(),
                                                                            device_name,
                                                                        },
                                                                        browser_identity,
                                                                    )).await {
                                                                        log::error!("Failed to send event: {}", e);
                                                                    }
                                                                    continue;
                                                                }
                                                                BrowserCommand::GenerateInvite => {
                                                                    log::info!("Browser requested invite bundle");

                                                                    // Generate fresh PreKeyBundle for sharing
                                                                    // Use a higher prekey ID than the initial QR (which uses 1)
                                                                    match self.signal_manager.build_prekey_bundle_data(2).await {
                                                                        Ok(bundle) => {
                                                                            // Encode bundle for URL fragment
                                                                            let bundle_json = serde_json::to_string(&bundle)
                                                                                .expect("PreKeyBundle serializable");
                                                                            let bundle_encoded = URL_SAFE_NO_PAD.encode(bundle_json.as_bytes());

                                                                            // Build shareable URL (fragment never sent to server)
                                                                            let invite_url = format!(
                                                                                "{}/hubs/{}#bundle={}",
                                                                                self.server_url, hub_identifier, bundle_encoded
                                                                            );

                                                                            // Send invite_bundle response
                                                                            let response = TerminalMessage::InviteBundle {
                                                                                bundle: bundle_encoded,
                                                                                url: invite_url,
                                                                            };
                                                                            let response_bytes = serde_json::to_vec(&response)
                                                                                .expect("InviteBundle serializable");

                                                                            match self.signal_manager.encrypt(&response_bytes, &envelope.sender_identity).await {
                                                                                Ok(invite_envelope) => {
                                                                                    let invite_data = serde_json::json!({
                                                                                        "action": "relay",
                                                                                        "recipient_identity": &envelope.sender_identity,
                                                                                        "envelope": {
                                                                                            "version": invite_envelope.version,
                                                                                            "message_type": invite_envelope.message_type,
                                                                                            "ciphertext": invite_envelope.ciphertext,
                                                                                            "sender_identity": invite_envelope.sender_identity,
                                                                                            "registration_id": invite_envelope.registration_id,
                                                                                            "device_id": invite_envelope.device_id,
                                                                                        }
                                                                                    });
                                                                                    let invite_cable = CableMessage {
                                                                                        command: "message".to_string(),
                                                                                        identifier: identifier_json.clone(),
                                                                                        data: Some(serde_json::to_string(&invite_data).expect("serializable")),
                                                                                    };

                                                                                    if let Err(e) = write.send(Message::Text(
                                                                                        serde_json::to_string(&invite_cable).expect("serializable")
                                                                                    )).await {
                                                                                        log::error!("Failed to send invite_bundle: {}", e);
                                                                                    } else {
                                                                                        log::info!("Sent invite_bundle to browser");
                                                                                    }
                                                                                }
                                                                                Err(e) => {
                                                                                    log::error!("Failed to encrypt invite_bundle: {}", e);
                                                                                }
                                                                            }
                                                                        }
                                                                        Err(e) => {
                                                                            log::error!("Failed to generate invite bundle: {}", e);
                                                                            // Send error response to browser
                                                                            let error_msg = TerminalMessage::Error {
                                                                                message: format!("Failed to generate invite: {}", e),
                                                                            };
                                                                            let error_bytes = serde_json::to_vec(&error_msg)
                                                                                .expect("Error serializable");

                                                                            if let Ok(error_envelope) = self.signal_manager.encrypt(&error_bytes, &envelope.sender_identity).await {
                                                                                let error_data = serde_json::json!({
                                                                                    "action": "relay",
                                                                                    "recipient_identity": &envelope.sender_identity,
                                                                                    "envelope": {
                                                                                        "version": error_envelope.version,
                                                                                        "message_type": error_envelope.message_type,
                                                                                        "ciphertext": error_envelope.ciphertext,
                                                                                        "sender_identity": error_envelope.sender_identity,
                                                                                        "registration_id": error_envelope.registration_id,
                                                                                        "device_id": error_envelope.device_id,
                                                                                    }
                                                                                });
                                                                                let error_cable = CableMessage {
                                                                                    command: "message".to_string(),
                                                                                    identifier: identifier_json.clone(),
                                                                                    data: Some(serde_json::to_string(&error_data).expect("serializable")),
                                                                                };
                                                                                let _ = write.send(Message::Text(
                                                                                    serde_json::to_string(&error_cable).expect("serializable")
                                                                                )).await;
                                                                            }
                                                                        }
                                                                    }
                                                                    continue;
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
                                                            // Send event with browser identity for client-scoped routing
                                                            if let Err(e) = event_tx.send((event, envelope.sender_identity.clone())).await {
                                                                log::error!("Failed to forward event: {}", e);
                                                            }
                                                        }
                                                        Err(e) => {
                                                            // Try parsing as raw string (legacy)
                                                            if let Ok(text) = String::from_utf8(plaintext.clone()) {
                                                                log::debug!("Received text message: {}", text);
                                                            } else {
                                                                log::warn!("Failed to parse command: {}", e);
                                                            }
                                                        }
                                                    }
                                                }
                                                Err(e) => log::warn!("Decryption failed: {}", e),
                                            }
                                        }
                                    }
                                }
                            }
                            Ok(Message::Ping(data)) => {
                                // Respond to WebSocket ping with pong
                                if let Err(e) = write.send(Message::Pong(data)).await {
                                    log::warn!("Failed to send pong: {}", e);
                                }
                            }
                            Ok(Message::Close(_)) => {
                                log::info!("Action Cable WebSocket closed");
                                break;
                            }
                            Err(e) => {
                                log::error!("WebSocket error: {}", e);
                                break;
                            }
                            _ => {}
                        }
                    }

                    else => break,
                }
            }
        });

        Ok(output_sender)
    }

    /// Build WebSocket URL for Action Cable.
    fn build_ws_url(&self) -> String {
        let base = self.server_url
            .replace("https://", "wss://")
            .replace("http://", "ws://");
        format!("{}/cable", base)
    }
}
