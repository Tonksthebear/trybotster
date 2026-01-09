//! Terminal relay for E2E encrypted communication with browser via Action Cable.
//!
//! This module handles:
//! - WebSocket connection to Rails Action Cable (TerminalChannel)
//! - E2E encryption using vodozemac Olm (Matrix's audited crypto library)
//! - Relaying encrypted terminal output to browser
//! - Receiving encrypted terminal input from browser
//!
//! # Protocol
//!
//! 1. CLI connects to Action Cable WebSocket
//! 2. CLI subscribes to TerminalChannel with hub_identifier
//! 3. CLI displays QR code with (ed25519, curve25519, one_time_key)
//! 4. Browser scans QR, creates outbound Olm session
//! 5. Browser sends PreKey message in presence "join"
//! 6. CLI creates inbound Olm session from PreKey message
//! 7. Both sides have Olm session - server only sees encrypted blobs
//!
//! Rust guideline compliant 2025-01

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::client::IntoClientRequest, tungstenite::Message};

use super::olm::{OlmAccount, OlmEnvelope, OlmSession};
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
/// - `account` holds the Olm account with identity keys and one-time keys
/// - `session` is created when browser sends PreKey message
/// - All session keys are securely managed by vodozemac
struct RelayState {
    account: OlmAccount,
    session: Option<OlmSession>,
    browser_connected: bool,
    /// Hub identifier for session persistence.
    hub_identifier: String,
}

impl RelayState {
    /// Create state without session (for tests only).
    #[cfg(test)]
    fn new(account: OlmAccount) -> Self {
        Self {
            account,
            session: None,
            browser_connected: false,
            hub_identifier: "test-hub".to_string(),
        }
    }

    fn new_with_session(
        account: OlmAccount,
        session: Option<OlmSession>,
        hub_identifier: String,
    ) -> Self {
        let browser_connected = session.is_some();
        Self {
            account,
            session,
            browser_connected,
            hub_identifier,
        }
    }

    /// Get the account's Curve25519 identity key (for envelope construction)
    fn our_curve25519(&self) -> String {
        self.account.curve25519_key()
    }

    /// Persist the current session to disk for surviving restarts.
    fn persist_session(&self) {
        if let Some(ref session) = self.session {
            if let Err(e) = super::persistence::save_session(&self.hub_identifier, session) {
                log::warn!("Failed to persist Olm session: {e}");
            }
        }
    }

    /// Create inbound Olm session from browser's PreKey message.
    ///
    /// The browser sends a PreKey message that includes:
    /// - Their Curve25519 identity key
    /// - The initial encrypted message
    fn create_session_from_prekey(
        &mut self,
        sender_curve25519: &str,
        prekey_message: &OlmEnvelope,
    ) -> Result<Vec<u8>> {
        let (session, plaintext) = self.account
            .create_inbound_session(sender_curve25519, prekey_message)
            .context("Failed to create inbound Olm session")?;

        self.session = Some(session);
        self.browser_connected = true;

        // Persist session for surviving CLI restarts
        self.persist_session();

        log::info!("Olm session established for E2E encryption with browser");
        Ok(plaintext)
    }

    /// Encrypt a message for the browser using Olm
    fn encrypt(&mut self, message: &TerminalMessage) -> Result<OlmEnvelope> {
        // Get our key before borrowing session mutably
        let our_key = self.our_curve25519();

        let session = self.session.as_mut()
            .context("No Olm session - browser not connected")?;

        let plaintext = serde_json::to_vec(message)
            .context("Failed to serialize message")?;

        let envelope = session.encrypt(&plaintext, &our_key);

        // Persist session after encrypt (ratchet advances)
        self.persist_session();

        Ok(envelope)
    }

    /// Decrypt a command from the browser using Olm
    fn decrypt_command(&mut self, envelope: &OlmEnvelope) -> Result<BrowserCommand> {
        let session = self.session.as_mut()
            .context("No Olm session - browser not connected")?;

        let plaintext = session.decrypt(envelope)?;

        // Persist session after decrypt (ratchet advances)
        // (mutable borrow of session ends here naturally)
        self.persist_session();

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
    fn decrypt(&mut self, envelope: &OlmEnvelope) -> Result<TerminalMessage> {
        let session = self.session.as_mut()
            .context("No Olm session - browser not connected")?;

        let plaintext = session.decrypt(envelope)?;

        let message: TerminalMessage = serde_json::from_slice(&plaintext)
            .context("Failed to parse decrypted message")?;

        Ok(message)
    }

    fn is_ready(&self) -> bool {
        self.browser_connected && self.session.is_some()
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
    account: OlmAccount,
    existing_session: Option<OlmSession>,
    hub_identifier: String,
    server_url: String,
    api_key: String,
}

impl std::fmt::Debug for TerminalRelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalRelay")
            .field("hub_identifier", &self.hub_identifier)
            .field("server_url", &self.server_url)
            .field("has_existing_session", &self.existing_session.is_some())
            .finish_non_exhaustive()
    }
}

impl TerminalRelay {
    /// Create a new terminal relay
    pub fn new(
        account: OlmAccount,
        hub_identifier: String,
        server_url: String,
        api_key: String,
    ) -> Self {
        Self {
            account,
            existing_session: None,
            hub_identifier,
            server_url,
            api_key,
        }
    }

    /// Create a new terminal relay with an existing session (for reconnection).
    pub fn new_with_session(
        account: OlmAccount,
        existing_session: Option<OlmSession>,
        hub_identifier: String,
        server_url: String,
        api_key: String,
    ) -> Self {
        Self {
            account,
            existing_session,
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

        // Create shared state (consumes account, may include existing session)
        let state = Arc::new(RwLock::new(RelayState::new_with_session(
            self.account,
            self.existing_session,
            hub_identifier.clone(),
        )));

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

        // Set Authorization header with bearer token (Fizzy pattern)
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {}", self.api_key)
                .parse()
                .expect("Bearer token is valid header value"),
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

                        // Serialize OlmEnvelope for transmission
                        let data = serde_json::json!({
                            "action": "relay",
                            "version": envelope.version,
                            "message_type": envelope.message_type,
                            "ciphertext": envelope.ciphertext,
                            "sender_key": envelope.sender_key,
                        });
                        let cable_msg = CableMessage {
                            command: "message".to_string(),
                            identifier: identifier_out.clone(),
                            data: Some(serde_json::to_string(&data).expect("OlmEnvelope is serializable")),
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
                                                // Parse OlmEnvelope (v3 format)
                                                if let Ok(envelope) = serde_json::from_value::<OlmEnvelope>(message.clone()) {
                                                    // Need write lock for decryption (session mutates state)
                                                    let mut state = state_in.write().await;
                                                    match state.decrypt_command(&envelope) {
                                                        Ok(cmd) => {
                                                            drop(state);
                                                            // Convert BrowserCommand to BrowserEvent
                                                            let event = match cmd {
                                                                BrowserCommand::Handshake { .. } => {
                                                                    // Handshake is handled in presence join, skip here
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
                                                            // Browser joined - handle session establishment or reconnection
                                                            let prekey_message = message.get("prekey_message");

                                                            let device_name = message.get("device_name")
                                                                .and_then(|v| v.as_str())
                                                                .unwrap_or("Browser")
                                                                .to_string();

                                                            if let Some(prekey) = prekey_message {
                                                                // Parse the envelope
                                                                match serde_json::from_value::<OlmEnvelope>(prekey.clone()) {
                                                                    Ok(envelope) => {
                                                                        let sender_key = &envelope.sender_key;
                                                                        let mut state = state_in.write().await;

                                                                        // Check if we already have a session (browser reconnecting)
                                                                        if state.session.is_some() {
                                                                            // Session exists - browser is reconnecting
                                                                            log::info!(
                                                                                "Browser reconnected: {} - message_type={} (0=PreKey, 1=Normal)",
                                                                                device_name,
                                                                                envelope.message_type
                                                                            );

                                                                            // Try to decrypt with existing session
                                                                            match state.decrypt_command(&envelope) {
                                                                                Ok(cmd) => {
                                                                                    // Verify it's a handshake command
                                                                                    match cmd {
                                                                                        BrowserCommand::Handshake { device_name: dn, .. } => {
                                                                                            log::info!("Reconnection successful for {}", dn);
                                                                                            state.browser_connected = true;
                                                                                            drop(state);
                                                                                            if let Err(e) = event_tx.send(BrowserEvent::Connected {
                                                                                                public_key: sender_key.to_string(),
                                                                                                device_name: dn,
                                                                                            }).await {
                                                                                                log::error!("Failed to send connected event: {}", e);
                                                                                            }
                                                                                        }
                                                                                        _ => {
                                                                                            log::warn!("Expected handshake on reconnect, got {:?}", cmd);
                                                                                            state.browser_connected = true;
                                                                                            drop(state);
                                                                                            if let Err(e) = event_tx.send(BrowserEvent::Connected {
                                                                                                public_key: sender_key.to_string(),
                                                                                                device_name,
                                                                                            }).await {
                                                                                                log::error!("Failed to send connected event: {}", e);
                                                                                            }
                                                                                        }
                                                                                    }
                                                                                }
                                                                                Err(e) => {
                                                                                    // Session mismatch - browser has stale session
                                                                                    log::warn!(
                                                                                        "Reconnection decrypt failed: {} - message_type was {}",
                                                                                        e,
                                                                                        envelope.message_type
                                                                                    );
                                                                                    log::info!("Clearing CLI session - browser should rescan QR");
                                                                                    state.session = None;
                                                                                    state.browser_connected = false;
                                                                                    drop(state);
                                                                                    // Don't send Connected - browser will see decryption failures
                                                                                }
                                                                            }
                                                                        } else {
                                                                            // No session - this should be a PreKey message
                                                                            log::info!("Browser connected: {} - establishing Olm session", device_name);

                                                                            match state.create_session_from_prekey(sender_key, &envelope) {
                                                                                Ok(_plaintext) => {
                                                                                    drop(state);
                                                                                    if let Err(e) = event_tx.send(BrowserEvent::Connected {
                                                                                        public_key: sender_key.to_string(),
                                                                                        device_name,
                                                                                    }).await {
                                                                                        log::error!("Failed to send connected event: {}", e);
                                                                                    }
                                                                                }
                                                                                Err(e) => {
                                                                                    log::error!("Failed to create Olm session: {}", e);
                                                                                }
                                                                            }
                                                                        }
                                                                    }
                                                                    Err(e) => {
                                                                        log::error!("Invalid message format: {}", e);
                                                                    }
                                                                }
                                                            } else {
                                                                log::warn!("Browser joined without PreKey message - cannot establish E2E encryption");
                                                            }
                                                        }
                                                        "leave" => {
                                                            log::info!("Browser disconnected");
                                                            let mut state = state_in.write().await;
                                                            state.browser_connected = false;
                                                            // Keep session alive for reconnection!
                                                            // Browser will reconnect with its saved session.
                                                            // state.session = None;  // DON'T clear session
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

        format!("{}/cable", base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

    #[test]
    fn test_relay_state_is_ready() {
        let account = OlmAccount::new();
        let state = RelayState::new(account);
        assert!(!state.is_ready());
    }

    #[test]
    fn test_relay_state_becomes_ready_after_prekey() {
        // CLI creates account and generates one-time key
        let mut cli_account = OlmAccount::new();
        cli_account.generate_one_time_keys(1);
        let cli_identity = cli_account.curve25519_key();
        let cli_otk = cli_account.get_one_time_key().unwrap();

        // Browser creates account and outbound session
        let browser_account = OlmAccount::new();
        let browser_identity = browser_account.curve25519_key();

        // Browser creates outbound session to CLI
        let mut browser_session = browser_account
            .create_outbound_session(&cli_identity, &cli_otk)
            .unwrap();

        // Browser sends PreKey message
        let plaintext = b"Hello CLI!";
        let message = browser_session.encrypt(plaintext);
        let (message_type, ciphertext) = match message {
            vodozemac::olm::OlmMessage::PreKey(m) => (0u8, BASE64.encode(m.to_bytes())),
            vodozemac::olm::OlmMessage::Normal(m) => (1u8, BASE64.encode(m.to_bytes())),
        };
        assert_eq!(message_type, 0); // First message should be PreKey

        let envelope = OlmEnvelope {
            version: OlmEnvelope::VERSION,
            message_type,
            ciphertext,
            sender_key: browser_identity.clone(),
        };

        // CLI receives and creates inbound session
        let mut state = RelayState::new(cli_account);
        assert!(!state.is_ready());

        let decrypted = state.create_session_from_prekey(&browser_identity, &envelope).unwrap();
        assert_eq!(decrypted, plaintext);
        assert!(state.is_ready());
    }

    #[test]
    fn test_encrypt_produces_olm_envelope() {
        // Set up a session first
        let mut cli_account = OlmAccount::new();
        cli_account.generate_one_time_keys(1);
        let cli_identity = cli_account.curve25519_key();
        let cli_otk = cli_account.get_one_time_key().unwrap();

        let browser_account = OlmAccount::new();
        let browser_identity = browser_account.curve25519_key();

        let mut browser_session = browser_account
            .create_outbound_session(&cli_identity, &cli_otk)
            .unwrap();

        let message = browser_session.encrypt(b"handshake");
        let (message_type, ciphertext) = match message {
            vodozemac::olm::OlmMessage::PreKey(m) => (0u8, BASE64.encode(m.to_bytes())),
            vodozemac::olm::OlmMessage::Normal(m) => (1u8, BASE64.encode(m.to_bytes())),
        };
        let envelope = OlmEnvelope {
            version: OlmEnvelope::VERSION,
            message_type,
            ciphertext,
            sender_key: browser_identity.clone(),
        };

        let mut state = RelayState::new(cli_account);
        state.create_session_from_prekey(&browser_identity, &envelope).unwrap();

        // Now test encryption
        let message = TerminalMessage::Output {
            data: "Hello, browser!".to_string(),
        };
        let encrypted = state.encrypt(&message).unwrap();

        // Verify envelope structure
        assert_eq!(encrypted.version, 3);
        assert!(!encrypted.ciphertext.is_empty());
        assert!(!encrypted.sender_key.is_empty());
        // After session established, messages should be Normal (type 1)
        assert_eq!(encrypted.message_type, 1);
    }

    #[test]
    fn test_full_roundtrip() {
        // Set up CLI side
        let mut cli_account = OlmAccount::new();
        cli_account.generate_one_time_keys(1);
        let cli_identity = cli_account.curve25519_key();
        let cli_otk = cli_account.get_one_time_key().unwrap();

        // Set up browser side
        let browser_account = OlmAccount::new();
        let browser_identity = browser_account.curve25519_key();

        let mut browser_session = browser_account
            .create_outbound_session(&cli_identity, &cli_otk)
            .unwrap();

        // Browser sends PreKey
        let handshake = browser_session.encrypt(b"handshake");
        let (message_type, ciphertext) = match handshake {
            vodozemac::olm::OlmMessage::PreKey(m) => (0u8, BASE64.encode(m.to_bytes())),
            vodozemac::olm::OlmMessage::Normal(m) => (1u8, BASE64.encode(m.to_bytes())),
        };
        let prekey_envelope = OlmEnvelope {
            version: OlmEnvelope::VERSION,
            message_type,
            ciphertext,
            sender_key: browser_identity.clone(),
        };

        // CLI establishes session
        let mut cli_state = RelayState::new(cli_account);
        cli_state.create_session_from_prekey(&browser_identity, &prekey_envelope).unwrap();

        // CLI encrypts a message
        let message = TerminalMessage::Output {
            data: "Hello from CLI!".to_string(),
        };
        let encrypted = cli_state.encrypt(&message).unwrap();

        // Browser decrypts the message
        let ciphertext_bytes = BASE64.decode(&encrypted.ciphertext).unwrap();
        let olm_msg = vodozemac::olm::Message::try_from(ciphertext_bytes.as_slice()).unwrap();
        let decrypted = browser_session.decrypt(&vodozemac::olm::OlmMessage::Normal(olm_msg)).unwrap();
        let decrypted_message: TerminalMessage = serde_json::from_slice(&decrypted).unwrap();

        match decrypted_message {
            TerminalMessage::Output { data } => {
                assert_eq!(data, "Hello from CLI!");
            }
            _ => panic!("Expected Output message"),
        }
    }
}
