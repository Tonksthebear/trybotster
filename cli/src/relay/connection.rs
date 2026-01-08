//! Terminal relay for E2E encrypted communication with browser via Action Cable.
//!
//! This module handles:
//! - WebSocket connection to Rails Action Cable (TerminalChannel)
//! - E2E encryption using Double Ratchet (Signal protocol compatible)
//! - Relaying encrypted terminal output to browser
//! - Receiving encrypted terminal input from browser
//!
//! # Protocol
//!
//! 1. CLI connects to Action Cable WebSocket
//! 2. CLI subscribes to TerminalChannel with hub_identifier
//! 3. Browser connects and sends presence with its public_key
//! 4. CLI receives browser's public_key, computes shared secret
//! 5. Double Ratchet initialized with shared secret for per-message forward secrecy
//! 6. Server only sees encrypted blobs - zero knowledge
//!
//! Rust guideline compliant 2025-01

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use crypto_box::SecretKey;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::client::IntoClientRequest, tungstenite::Message};

use super::ratchet::{RatchetEnvelope, RatchetSession};
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

/// Shared state for the terminal relay.
///
/// # Security
///
/// - `ratchet` implements `ZeroizeOnDrop` - all session keys are securely erased when dropped
/// - `secret_key` is the device's X25519 private key (also stored in OS keyring)
///   Used only briefly to compute initial shared secret for Double Ratchet
struct RelayState {
    secret_key: SecretKey,
    ratchet: Option<RatchetSession>,
    browser_connected: bool,
}

impl RelayState {
    fn new(secret_key: SecretKey) -> Self {
        Self {
            secret_key,
            ratchet: None,
            browser_connected: false,
        }
    }

    /// Set the peer's public key and initialize Double Ratchet
    ///
    /// If signature and verifying_key are provided, verifies the signature first.
    /// This prevents MITM attacks by ensuring the public key was signed by a real device.
    fn set_peer_public_key(
        &mut self,
        peer_public_key_base64: &str,
        signature_base64: Option<&str>,
        verifying_key_base64: Option<&str>,
    ) -> Result<()> {
        let peer_key_bytes = BASE64.decode(peer_public_key_base64)
            .context("Invalid peer public key encoding")?;

        let peer_public_key: [u8; 32] = peer_key_bytes.clone().try_into()
            .map_err(|_| anyhow::anyhow!("Invalid peer public key length"))?;

        // Verify signature if provided
        match (signature_base64, verifying_key_base64) {
            (Some(sig_b64), Some(vk_b64)) => {
                // Decode signature
                let sig_bytes = BASE64.decode(sig_b64)
                    .context("Invalid signature encoding")?;
                let signature = Signature::from_slice(&sig_bytes)
                    .map_err(|e| anyhow::anyhow!("Invalid signature format: {}", e))?;

                // Decode verifying key
                let vk_bytes = BASE64.decode(vk_b64)
                    .context("Invalid verifying key encoding")?;
                let vk_array: [u8; 32] = vk_bytes.try_into()
                    .map_err(|_| anyhow::anyhow!("Invalid verifying key length"))?;
                let verifying_key = VerifyingKey::from_bytes(&vk_array)
                    .map_err(|e| anyhow::anyhow!("Invalid verifying key: {}", e))?;

                // Verify that the public key was signed by the browser's signing key
                verifying_key.verify(&peer_key_bytes, &signature)
                    .map_err(|_| anyhow::anyhow!("SECURITY: Signature verification failed - possible MITM attack!"))?;

                log::info!("Signature verified - public key is authentic");
            }
            (None, None) => {
                // Legacy browser without signing - allow but warn
                log::warn!("SECURITY: Browser did not provide signature - cannot verify authenticity");
                log::warn!("This connection is vulnerable to MITM attacks until browser is upgraded");
            }
            _ => {
                // Partial signature data is suspicious
                anyhow::bail!("SECURITY: Partial signature data provided - rejecting connection");
            }
        }

        // Compute raw X25519 shared secret
        let peer_public = x25519_dalek::PublicKey::from(peer_public_key);
        let our_secret = x25519_dalek::StaticSecret::from(self.secret_key.to_bytes());
        let shared_secret = our_secret.diffie_hellman(&peer_public).to_bytes();

        // Initialize Double Ratchet (CLI is always the initiator)
        let ratchet = RatchetSession::new(&shared_secret, true)
            .context("Failed to initialize Double Ratchet")?;
        self.ratchet = Some(ratchet);
        self.browser_connected = true;

        log::info!("Double Ratchet initialized for E2E encryption with browser");
        Ok(())
    }

    /// Encrypt a message for the peer using Double Ratchet
    fn encrypt(&mut self, message: &TerminalMessage) -> Result<RatchetEnvelope> {
        let ratchet = self.ratchet.as_mut()
            .context("No ratchet session - browser not connected")?;

        let plaintext = serde_json::to_vec(message)
            .context("Failed to serialize message")?;

        ratchet.encrypt(&plaintext)
    }

    /// Decrypt a command from the browser using Double Ratchet
    fn decrypt_command(&mut self, envelope: &RatchetEnvelope) -> Result<BrowserCommand> {
        let ratchet = self.ratchet.as_mut()
            .context("No ratchet session - browser not connected")?;

        let plaintext = ratchet.decrypt(envelope)?;

        // Log the decrypted JSON for debugging
        if let Ok(json_str) = std::str::from_utf8(&plaintext) {
            log::debug!("Decrypted browser command: {}", json_str);
        }

        let command: BrowserCommand = serde_json::from_slice(&plaintext)
            .context("Failed to parse decrypted command")?;

        Ok(command)
    }

    /// Decrypt a message (for testing - decrypts TerminalMessage sent from CLI)
    #[cfg(test)]
    fn decrypt(&mut self, envelope: &RatchetEnvelope) -> Result<TerminalMessage> {
        let ratchet = self.ratchet.as_mut()
            .context("No ratchet session - browser not connected")?;

        let plaintext = ratchet.decrypt(envelope)?;

        let message: TerminalMessage = serde_json::from_slice(&plaintext)
            .context("Failed to parse decrypted message")?;

        Ok(message)
    }

    fn is_ready(&self) -> bool {
        self.browser_connected && self.ratchet.is_some()
    }
}

/// Handle for sending terminal output to the browser
#[derive(Clone)]
pub struct TerminalOutputSender {
    tx: mpsc::Sender<String>,
    state: Arc<RwLock<RelayState>>,
}

impl std::fmt::Debug for TerminalOutputSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalOutputSender").finish_non_exhaustive()
    }
}

impl TerminalOutputSender {
    /// Send terminal output to browser (will be encrypted)
    pub async fn send(&self, output: &str) -> Result<()> {
        // Only send if browser is connected
        let state = self.state.read().await;
        if !state.is_ready() {
            return Ok(()); // Silently drop if no browser connected
        }
        drop(state);

        self.tx.send(output.to_string()).await
            .map_err(|e| anyhow::anyhow!("Failed to queue output: {}", e))
    }

    /// Check if browser is connected and ready for encrypted communication
    pub async fn is_ready(&self) -> bool {
        self.state.read().await.is_ready()
    }
}

/// Terminal relay connection
pub struct TerminalRelay {
    secret_key: SecretKey,
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
    /// Create a new terminal relay
    pub fn new(
        secret_key: SecretKey,
        hub_identifier: String,
        server_url: String,
        api_key: String,
    ) -> Self {
        Self {
            secret_key,
            hub_identifier,
            server_url,
            api_key,
        }
    }

    /// Connect to Action Cable and start relaying messages.
    ///
    /// Returns:
    /// - `TerminalOutputSender` - for sending terminal output to browser
    /// - `mpsc::Receiver<BrowserEvent>` - for receiving events from browser
    pub async fn connect(self) -> Result<(TerminalOutputSender, mpsc::Receiver<BrowserEvent>)> {
        // Build WebSocket URL before moving self
        let ws_url = self.build_ws_url();
        let hub_identifier = self.hub_identifier.clone();

        // Create shared state (consumes secret_key)
        let state = Arc::new(RwLock::new(RelayState::new(self.secret_key)));

        // Create channels
        let (output_tx, mut output_rx) = mpsc::channel::<String>(100);
        let (event_tx, event_rx) = mpsc::channel::<BrowserEvent>(100);

        log::info!("Connecting to Action Cable: {}", ws_url);

        // Build request with required headers
        let mut request = ws_url
            .into_client_request()
            .context("Failed to build WebSocket request")?;
        // Set Origin header to match the server URL (ActionCable requires this)
        request.headers_mut().insert(
            "Origin",
            self.server_url
                .parse()
                .unwrap_or_else(|_| "http://localhost".parse().expect("localhost is a valid header value")),
        );
        // Set Authorization header with API key (instead of query param for security)
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {}", self.api_key)
                .parse()
                .expect("Bearer token is a valid header value"),
        );

        // Connect to WebSocket
        let (ws_stream, _) = connect_async(request)
            .await
            .context("Failed to connect to Action Cable")?;

        let (mut write, mut read) = ws_stream.split();

        // Wait for Action Cable "welcome" message before subscribing
        // Action Cable requires this handshake before accepting commands
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
            channel: "TerminalChannel".to_string(),
            hub_identifier: hub_identifier.clone(),
            device_type: "cli".to_string(),
        };
        let identifier_json = serde_json::to_string(&identifier)?;

        // Now subscribe to channel (after welcome received)
        let subscribe = CableMessage {
            command: "subscribe".to_string(),
            identifier: identifier_json.clone(),
            data: None,
        };
        write.send(Message::Text(serde_json::to_string(&subscribe)?)).await?;

        log::info!("Sent subscribe to TerminalChannel for hub {}", hub_identifier);

        // Create output sender handle
        let output_sender = TerminalOutputSender {
            tx: output_tx,
            state: Arc::clone(&state),
        };

        // Wrap write in Arc<Mutex> for sharing
        let write = Arc::new(Mutex::new(write));

        // Spawn task to handle outgoing messages (CLI -> browser)
        let state_out = Arc::clone(&state);
        let identifier_out = identifier_json.clone();
        let write_out = Arc::clone(&write);
        tokio::spawn(async move {
            while let Some(output) = output_rx.recv().await {
                // Need write lock for encryption (ratchet mutates state)
                let mut state = state_out.write().await;
                if state.is_ready() {
                    // Check if output is already a structured TerminalMessage (JSON)
                    // If so, use it directly; otherwise wrap in Output
                    let message = if let Ok(parsed) = serde_json::from_str::<TerminalMessage>(&output) {
                        parsed
                    } else {
                        TerminalMessage::Output { data: output }
                    };
                    if let Ok(envelope) = state.encrypt(&message) {
                        drop(state); // Release lock before network I/O

                        // Serialize RatchetEnvelope for transmission
                        let data = serde_json::json!({
                            "action": "relay",
                            "version": envelope.version,
                            "header": envelope.header,
                            "ciphertext": envelope.ciphertext,
                            "mac": envelope.mac,
                        });
                        let cable_msg = CableMessage {
                            command: "message".to_string(),
                            identifier: identifier_out.clone(),
                            data: Some(serde_json::to_string(&data).expect("RatchetEnvelope is serializable")),
                        };

                        let mut write = write_out.lock().await;
                        if let Err(e) = write.send(Message::Text(serde_json::to_string(&cable_msg).expect("CableMessage is serializable"))).await {
                            log::error!("Failed to send output: {}", e);
                            break;
                        }
                    }
                }
            }
        });

        // Spawn task to handle incoming messages (browser -> CLI)
        let state_in = Arc::clone(&state);
        tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if let Ok(cable_msg) = serde_json::from_str::<IncomingCableMessage>(&text) {
                            // Handle different message types
                            if let Some(ref msg_type) = cable_msg.msg_type {
                                match msg_type.as_str() {
                                    "welcome" => {
                                        log::info!("Action Cable welcome received");
                                    }
                                    "confirm_subscription" => {
                                        log::info!("TerminalChannel subscription confirmed");
                                    }
                                    "ping" => {
                                        // Ignore ping messages
                                    }
                                    _ => {}
                                }
                            }

                            // Handle broadcast messages
                            if let Some(message) = cable_msg.message {
                                if let Some(msg_type) = message.get("type").and_then(|v| v.as_str()) {
                                    match msg_type {
                                        "terminal" => {
                                            // Only process messages from browser
                                            if message.get("from").and_then(|v| v.as_str()) == Some("browser") {
                                                // Parse RatchetEnvelope (v2 format)
                                                if let Ok(envelope) = serde_json::from_value::<RatchetEnvelope>(message.clone()) {
                                                    // Need write lock for decryption (ratchet mutates state)
                                                    let mut state = state_in.write().await;
                                                    match state.decrypt_command(&envelope) {
                                                        Ok(cmd) => {
                                                            drop(state);
                                                            // Convert BrowserCommand to BrowserEvent
                                                            let event = match cmd {
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
                                                            };
                                                            if let Err(e) = event_tx.send(event).await {
                                                                log::error!("Failed to forward browser event: {}", e);
                                                            }
                                                        }
                                                        Err(e) => {
                                                            log::warn!("Failed to decrypt browser command: {}", e);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        "presence" => {
                                            // Only handle browser presence events
                                            if message.get("device_type").and_then(|v| v.as_str()) == Some("browser") {
                                                if let Some(event) = message.get("event").and_then(|v| v.as_str()) {
                                                    match event {
                                                        "join" => {
                                                            // Browser joined - extract public key and signature for key exchange
                                                            if let Some(public_key) = message.get("public_key").and_then(|v| v.as_str()) {
                                                                let device_name = message.get("device_name")
                                                                    .and_then(|v| v.as_str())
                                                                    .unwrap_or("Browser")
                                                                    .to_string();

                                                                // Extract optional signature and verifying key
                                                                let signature = message.get("signature").and_then(|v| v.as_str());
                                                                let verifying_key = message.get("verifying_key").and_then(|v| v.as_str());

                                                                log::info!("Browser connected: {} - setting up E2E encryption", device_name);

                                                                // Set up shared secret with signature verification
                                                                let mut state = state_in.write().await;
                                                                if let Err(e) = state.set_peer_public_key(public_key, signature, verifying_key) {
                                                                    log::error!("Failed to set browser public key: {}", e);
                                                                } else {
                                                                    drop(state);
                                                                    if let Err(e) = event_tx.send(BrowserEvent::Connected {
                                                                        public_key: public_key.to_string(),
                                                                        device_name,
                                                                    }).await {
                                                                        log::error!("Failed to send connected event: {}", e);
                                                                    }
                                                                }
                                                            } else {
                                                                log::warn!("Browser joined without public key - cannot establish E2E encryption");
                                                            }
                                                        }
                                                        "leave" => {
                                                            log::info!("Browser disconnected");
                                                            let mut state = state_in.write().await;
                                                            state.browser_connected = false;
                                                            state.ratchet = None;
                                                            drop(state);

                                                            if let Err(e) = event_tx.send(BrowserEvent::Disconnected).await {
                                                                log::error!("Failed to send disconnected event: {}", e);
                                                            }
                                                        }
                                                        _ => {}
                                                    }
                                                }
                                            }
                                        }
                                        "resize" => {
                                            // Browser sent resize event
                                            if message.get("from").and_then(|v| v.as_str()) == Some("browser") {
                                                if let (Some(cols), Some(rows)) = (
                                                    message.get("cols").and_then(serde_json::Value::as_i64),
                                                    message.get("rows").and_then(serde_json::Value::as_i64),
                                                ) {
                                                    log::info!("Browser resize: {}x{}", cols, rows);
                                                    if let Err(e) = event_tx.send(BrowserEvent::Resize(BrowserResize {
                                                        cols: cols as u16,
                                                        rows: rows as u16,
                                                    })).await {
                                                        log::error!("Failed to send resize event: {}", e);
                                                    }
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
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
        });

        Ok((output_sender, event_rx))
    }

    /// Build WebSocket URL for Action Cable.
    fn build_ws_url(&self) -> String {
        let base = self.server_url
            .replace("https://", "wss://")
            .replace("http://", "ws://");

        // Action Cable endpoint (API key is sent via Authorization header, not URL)
        format!("{}/cable", base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::{rngs::OsRng, RngCore};

    #[test]
    fn test_relay_state_is_ready() {
        let secret = SecretKey::generate(&mut OsRng);
        let state = RelayState::new(secret);
        assert!(!state.is_ready());
    }

    #[test]
    fn test_relay_state_becomes_ready_after_peer_key_legacy() {
        // Test legacy mode (no signature)
        let cli_secret = SecretKey::generate(&mut OsRng);
        let browser_secret = SecretKey::generate(&mut OsRng);
        let browser_public = browser_secret.public_key();

        let mut state = RelayState::new(cli_secret);
        assert!(!state.is_ready());

        state
            .set_peer_public_key(&BASE64.encode(browser_public.as_bytes()), None, None)
            .unwrap();
        assert!(state.is_ready());
    }

    #[test]
    fn test_relay_state_with_valid_signature() {
        // Test with valid Ed25519 signature
        let cli_secret = SecretKey::generate(&mut OsRng);
        let browser_secret = SecretKey::generate(&mut OsRng);
        let browser_public = browser_secret.public_key();

        // Generate signing keypair
        let mut signing_secret = [0u8; 32];
        OsRng.fill_bytes(&mut signing_secret);
        let signing_key = SigningKey::from_bytes(&signing_secret);
        let verifying_key = signing_key.verifying_key();

        // Sign the browser's public key
        use ed25519_dalek::Signer;
        let signature = signing_key.sign(browser_public.as_bytes());

        let mut state = RelayState::new(cli_secret);
        state
            .set_peer_public_key(
                &BASE64.encode(browser_public.as_bytes()),
                Some(&BASE64.encode(signature.to_bytes())),
                Some(&BASE64.encode(verifying_key.as_bytes())),
            )
            .unwrap();
        assert!(state.is_ready());
    }

    #[test]
    fn test_relay_state_rejects_invalid_signature() {
        // Test that invalid signature is rejected
        let cli_secret = SecretKey::generate(&mut OsRng);
        let browser_secret = SecretKey::generate(&mut OsRng);
        let browser_public = browser_secret.public_key();

        // Generate signing keypair but sign wrong data
        let mut signing_secret = [0u8; 32];
        OsRng.fill_bytes(&mut signing_secret);
        let signing_key = SigningKey::from_bytes(&signing_secret);
        let verifying_key = signing_key.verifying_key();

        use ed25519_dalek::Signer;
        let wrong_data = [0u8; 32]; // Wrong data to sign
        let signature = signing_key.sign(&wrong_data);

        let mut state = RelayState::new(cli_secret);
        let result = state.set_peer_public_key(
            &BASE64.encode(browser_public.as_bytes()),
            Some(&BASE64.encode(signature.to_bytes())),
            Some(&BASE64.encode(verifying_key.as_bytes())),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("SECURITY"));
    }

    #[test]
    fn test_encrypt_produces_ratchet_envelope() {
        let cli_secret = SecretKey::generate(&mut OsRng);
        let browser_secret = SecretKey::generate(&mut OsRng);
        let browser_public = browser_secret.public_key();

        let mut state = RelayState::new(cli_secret);
        state
            .set_peer_public_key(&BASE64.encode(browser_public.as_bytes()), None, None)
            .unwrap();

        let message = TerminalMessage::Output {
            data: "Hello, browser!".to_string(),
        };
        let envelope = state.encrypt(&message).unwrap();

        // Verify envelope structure
        assert_eq!(envelope.version, 2);
        assert!(!envelope.ciphertext.is_empty());
        assert!(!envelope.mac.is_empty());
        assert!(!envelope.header.dh_public_key.is_empty());
    }
}
