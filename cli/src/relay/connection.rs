//! Terminal relay for E2E encrypted communication with browser via Action Cable.
//!
//! This module handles:
//! - WebSocket connection to Rails Action Cable (TerminalChannel)
//! - E2E encryption using crypto_box (compatible with TweetNaCl)
//! - Relaying encrypted terminal output to browser
//! - Receiving encrypted terminal input from browser
//!
//! # Protocol
//!
//! 1. CLI connects to Action Cable WebSocket
//! 2. CLI subscribes to TerminalChannel with hub_identifier
//! 3. Browser connects and sends presence with its public_key
//! 4. CLI receives browser's public_key, computes shared secret
//! 5. All terminal data is encrypted with the shared secret
//! 6. Server only sees encrypted blobs - zero knowledge
//!
//! Rust guideline compliant 2025-01-05

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use crypto_box::{aead::Aead, PublicKey, SalsaBox, SecretKey};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::Message, tungstenite::client::IntoClientRequest};

/// Message types for terminal relay (CLI -> browser)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TerminalMessage {
    /// Terminal output from CLI to browser
    #[serde(rename = "output")]
    Output { data: String },
    /// Agent list response
    #[serde(rename = "agents")]
    Agents { agents: Vec<AgentInfo> },
    /// Worktree list response
    #[serde(rename = "worktrees")]
    Worktrees { worktrees: Vec<WorktreeInfo>, repo: Option<String> },
    /// Agent selected confirmation
    #[serde(rename = "agent_selected")]
    AgentSelected { id: String },
    /// Agent created confirmation
    #[serde(rename = "agent_created")]
    AgentCreated { id: String },
    /// Agent deleted confirmation
    #[serde(rename = "agent_deleted")]
    AgentDeleted { id: String },
    /// Error message
    #[serde(rename = "error")]
    Error { message: String },
}

/// Agent info for list response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub id: String,
    pub repo: Option<String>,
    pub issue_number: Option<u64>,
    pub branch_name: Option<String>,
    pub name: Option<String>,
    pub status: Option<String>,
    pub tunnel_port: Option<u16>,
    pub server_running: Option<bool>,
    pub has_server_pty: Option<bool>,
    pub active_pty_view: Option<String>,
    pub scroll_offset: Option<u32>,
    pub hub_identifier: Option<String>,
}

/// Worktree info for list response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub path: String,
    pub branch: String,
    pub issue_number: Option<u64>,
}

/// Browser command types (browser -> CLI)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BrowserCommand {
    /// Terminal input from browser
    #[serde(rename = "input")]
    Input { data: String },
    /// Set display mode (tui/gui)
    #[serde(rename = "set_mode")]
    SetMode { mode: String },
    /// List all agents
    #[serde(rename = "list_agents")]
    ListAgents,
    /// List available worktrees
    #[serde(rename = "list_worktrees")]
    ListWorktrees,
    /// Select an agent
    #[serde(rename = "select_agent")]
    SelectAgent { id: String },
    /// Create a new agent
    #[serde(rename = "create_agent")]
    CreateAgent {
        issue_or_branch: Option<String>,
        prompt: Option<String>,
    },
    /// Reopen an existing worktree
    #[serde(rename = "reopen_worktree")]
    ReopenWorktree {
        path: String,
        branch: String,
        prompt: Option<String>,
    },
    /// Delete an agent
    #[serde(rename = "delete_agent")]
    DeleteAgent {
        id: String,
        delete_worktree: Option<bool>,
    },
    /// Toggle PTY view (CLI/Server)
    #[serde(rename = "toggle_pty_view")]
    TogglePtyView,
    /// Scroll terminal
    #[serde(rename = "scroll")]
    Scroll { direction: String, lines: Option<u32> },
    /// Scroll to bottom (return to live)
    #[serde(rename = "scroll_to_bottom")]
    ScrollToBottom,
    /// Scroll to top
    #[serde(rename = "scroll_to_top")]
    ScrollToTop,
}

/// Encrypted message envelope (sent via Action Cable)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedEnvelope {
    pub blob: String,  // Base64 encrypted data
    pub nonce: String, // Base64 nonce
}

/// Action Cable message format
#[derive(Debug, Serialize, Deserialize)]
struct CableMessage {
    command: String,
    identifier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<String>,
}

/// Action Cable subscription identifier
#[derive(Debug, Serialize, Deserialize)]
struct ChannelIdentifier {
    channel: String,
    hub_identifier: String,
    device_type: String,
}

/// Incoming Action Cable message
#[derive(Debug, Deserialize)]
struct IncomingCableMessage {
    #[serde(rename = "type")]
    msg_type: Option<String>,
    message: Option<serde_json::Value>,
}

/// Browser resize event
#[derive(Debug, Clone)]
pub struct BrowserResize {
    pub cols: u16,
    pub rows: u16,
}

/// Events received from the browser via the relay
#[derive(Debug, Clone)]
pub enum BrowserEvent {
    /// Browser connected and sent its public key
    Connected { public_key: String, device_name: String },
    /// Browser disconnected
    Disconnected,
    /// Terminal input from browser (already decrypted)
    Input(String),
    /// Browser resized terminal
    Resize(BrowserResize),
    /// Set display mode (tui/gui)
    SetMode { mode: String },
    /// List all agents
    ListAgents,
    /// List available worktrees
    ListWorktrees,
    /// Select an agent
    SelectAgent { id: String },
    /// Create a new agent
    CreateAgent {
        issue_or_branch: Option<String>,
        prompt: Option<String>,
    },
    /// Reopen an existing worktree
    ReopenWorktree {
        path: String,
        branch: String,
        prompt: Option<String>,
    },
    /// Delete an agent
    DeleteAgent {
        id: String,
        delete_worktree: bool,
    },
    /// Toggle PTY view (CLI/Server)
    TogglePtyView,
    /// Scroll terminal
    Scroll { direction: String, lines: u32 },
    /// Scroll to bottom (return to live)
    ScrollToBottom,
    /// Scroll to top
    ScrollToTop,
}

/// Shared state for the terminal relay
struct RelayState {
    secret_key: SecretKey,
    shared_box: Option<SalsaBox>,
    browser_connected: bool,
}

impl RelayState {
    fn new(secret_key: SecretKey) -> Self {
        Self {
            secret_key,
            shared_box: None,
            browser_connected: false,
        }
    }

    /// Set the peer's public key and compute shared secret
    fn set_peer_public_key(&mut self, peer_public_key_base64: &str) -> Result<()> {
        let peer_key_bytes = BASE64.decode(peer_public_key_base64)
            .context("Invalid peer public key encoding")?;

        let peer_public_key = PublicKey::from_slice(&peer_key_bytes)
            .map_err(|e| anyhow::anyhow!("Invalid peer public key: {}", e))?;

        // Compute shared secret using Diffie-Hellman
        let shared_box = SalsaBox::new(&peer_public_key, &self.secret_key);
        self.shared_box = Some(shared_box);
        self.browser_connected = true;

        log::info!("Computed shared secret for E2E encryption with browser");
        Ok(())
    }

    /// Encrypt a message for the peer
    fn encrypt(&self, message: &TerminalMessage) -> Result<EncryptedEnvelope> {
        let shared_box = self.shared_box.as_ref()
            .context("No shared secret - browser not connected")?;

        let plaintext = serde_json::to_vec(message)
            .context("Failed to serialize message")?;

        let nonce = crypto_box::Nonce::from(rand::random::<[u8; 24]>());
        let ciphertext = shared_box.encrypt(&nonce, plaintext.as_slice())
            .map_err(|e| anyhow::anyhow!("Encryption failed: {}", e))?;

        Ok(EncryptedEnvelope {
            blob: BASE64.encode(&ciphertext),
            nonce: BASE64.encode(&nonce),
        })
    }

    /// Decrypt a command from the browser
    fn decrypt_command(&self, envelope: &EncryptedEnvelope) -> Result<BrowserCommand> {
        let shared_box = self.shared_box.as_ref()
            .context("No shared secret - browser not connected")?;

        let ciphertext = BASE64.decode(&envelope.blob)
            .context("Invalid ciphertext encoding")?;
        let nonce_bytes = BASE64.decode(&envelope.nonce)
            .context("Invalid nonce encoding")?;

        let nonce = crypto_box::Nonce::from_slice(&nonce_bytes);
        let plaintext = shared_box.decrypt(nonce, ciphertext.as_slice())
            .map_err(|e| anyhow::anyhow!("Decryption failed: {}", e))?;

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
    fn decrypt(&self, envelope: &EncryptedEnvelope) -> Result<TerminalMessage> {
        let shared_box = self.shared_box.as_ref()
            .context("No shared secret - browser not connected")?;

        let ciphertext = BASE64.decode(&envelope.blob)
            .context("Invalid ciphertext encoding")?;
        let nonce_bytes = BASE64.decode(&envelope.nonce)
            .context("Invalid nonce encoding")?;

        let nonce = crypto_box::Nonce::from_slice(&nonce_bytes);
        let plaintext = shared_box.decrypt(nonce, ciphertext.as_slice())
            .map_err(|e| anyhow::anyhow!("Decryption failed: {}", e))?;

        let message: TerminalMessage = serde_json::from_slice(&plaintext)
            .context("Failed to parse decrypted message")?;

        Ok(message)
    }

    fn is_ready(&self) -> bool {
        self.browser_connected && self.shared_box.is_some()
    }
}

/// Handle for sending terminal output to the browser
#[derive(Clone)]
pub struct TerminalOutputSender {
    tx: mpsc::Sender<String>,
    state: Arc<RwLock<RelayState>>,
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
        let ws_url = self.build_ws_url()?;
        let hub_identifier = self.hub_identifier.clone();

        // Create shared state (consumes secret_key)
        let state = Arc::new(RwLock::new(RelayState::new(self.secret_key)));

        // Create channels
        let (output_tx, mut output_rx) = mpsc::channel::<String>(100);
        let (event_tx, event_rx) = mpsc::channel::<BrowserEvent>(100);

        log::info!("Connecting to Action Cable: {}", ws_url);

        // Build request with Origin header (required by ActionCable)
        let mut request = ws_url
            .into_client_request()
            .context("Failed to build WebSocket request")?;
        // Set Origin header to match the server URL (ActionCable requires this)
        request.headers_mut().insert(
            "Origin",
            self.server_url
                .parse()
                .unwrap_or_else(|_| "http://localhost".parse().unwrap()),
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
            state: state.clone(),
        };

        // Wrap write in Arc<Mutex> for sharing
        let write = Arc::new(Mutex::new(write));

        // Spawn task to handle outgoing messages (CLI -> browser)
        let state_out = state.clone();
        let identifier_out = identifier_json.clone();
        let write_out = write.clone();
        tokio::spawn(async move {
            while let Some(output) = output_rx.recv().await {
                let state = state_out.read().await;
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

                        let data = serde_json::json!({
                            "action": "relay",
                            "blob": envelope.blob,
                            "nonce": envelope.nonce,
                        });
                        let cable_msg = CableMessage {
                            command: "message".to_string(),
                            identifier: identifier_out.clone(),
                            data: Some(serde_json::to_string(&data).unwrap()),
                        };

                        let mut write = write_out.lock().await;
                        if let Err(e) = write.send(Message::Text(serde_json::to_string(&cable_msg).unwrap())).await {
                            log::error!("Failed to send output: {}", e);
                            break;
                        }
                    }
                }
            }
        });

        // Spawn task to handle incoming messages (browser -> CLI)
        let state_in = state.clone();
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
                                                if let (Some(blob), Some(nonce)) = (
                                                    message.get("blob").and_then(|v| v.as_str()),
                                                    message.get("nonce").and_then(|v| v.as_str()),
                                                ) {
                                                    let envelope = EncryptedEnvelope {
                                                        blob: blob.to_string(),
                                                        nonce: nonce.to_string(),
                                                    };
                                                    let state = state_in.read().await;
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
                                                            // Browser joined - extract public key for key exchange
                                                            if let Some(public_key) = message.get("public_key").and_then(|v| v.as_str()) {
                                                                let device_name = message.get("device_name")
                                                                    .and_then(|v| v.as_str())
                                                                    .unwrap_or("Browser")
                                                                    .to_string();

                                                                log::info!("Browser connected: {} - setting up E2E encryption", device_name);

                                                                // Set up shared secret
                                                                let mut state = state_in.write().await;
                                                                if let Err(e) = state.set_peer_public_key(public_key) {
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
                                                            state.shared_box = None;
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
                                                    message.get("cols").and_then(|v| v.as_i64()),
                                                    message.get("rows").and_then(|v| v.as_i64()),
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

    /// Build WebSocket URL for Action Cable
    fn build_ws_url(&self) -> Result<String> {
        let base = self.server_url
            .replace("https://", "wss://")
            .replace("http://", "ws://");

        // Action Cable endpoint with API key for authentication
        Ok(format!("{}/cable?api_key={}", base, self.api_key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        // Generate keypairs for CLI and browser
        let cli_secret = SecretKey::generate(&mut OsRng);
        let browser_secret = SecretKey::generate(&mut OsRng);
        let browser_public = browser_secret.public_key();

        // Create relay state with CLI keypair
        let mut state = RelayState::new(cli_secret);

        // Set browser's public key (compute shared secret)
        state.set_peer_public_key(&BASE64.encode(browser_public.as_bytes())).unwrap();

        // Encrypt a message
        let message = TerminalMessage::Output { data: "Hello, browser!".to_string() };
        let envelope = state.encrypt(&message).unwrap();

        // Decrypt the message
        let decrypted = state.decrypt(&envelope).unwrap();

        match decrypted {
            TerminalMessage::Output { data } => {
                assert_eq!(data, "Hello, browser!");
            }
            _ => panic!("Wrong message type"),
        }
    }

    // ========== TerminalMessage Serialization Tests ==========

    #[test]
    fn test_terminal_message_output_serialization() {
        let msg = TerminalMessage::Output { data: "hello".to_string() };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"output""#));
        assert!(json.contains(r#""data":"hello""#));
    }

    #[test]
    fn test_terminal_message_agents_serialization() {
        let msg = TerminalMessage::Agents {
            agents: vec![AgentInfo {
                id: "test-id".to_string(),
                repo: Some("owner/repo".to_string()),
                issue_number: Some(42),
                branch_name: Some("botster-issue-42".to_string()),
                name: None,
                status: Some("Running".to_string()),
                tunnel_port: Some(3000),
                server_running: Some(true),
                has_server_pty: Some(true),
                active_pty_view: Some("cli".to_string()),
                scroll_offset: Some(0),
                hub_identifier: Some("hub-123".to_string()),
            }],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"agents""#));
        assert!(json.contains(r#""id":"test-id""#));
        assert!(json.contains(r#""issue_number":42"#));
    }

    #[test]
    fn test_terminal_message_worktrees_serialization() {
        let msg = TerminalMessage::Worktrees {
            worktrees: vec![WorktreeInfo {
                path: "/path/to/worktree".to_string(),
                branch: "feature-branch".to_string(),
                issue_number: None,
            }],
            repo: Some("owner/repo".to_string()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"worktrees""#));
        assert!(json.contains(r#""path":"/path/to/worktree""#));
    }

    #[test]
    fn test_terminal_message_agent_selected_serialization() {
        let msg = TerminalMessage::AgentSelected { id: "agent-123".to_string() };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"agent_selected""#));
        assert!(json.contains(r#""id":"agent-123""#));
    }

    #[test]
    fn test_terminal_message_error_serialization() {
        let msg = TerminalMessage::Error { message: "Something went wrong".to_string() };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"error""#));
        assert!(json.contains(r#""message":"Something went wrong""#));
    }

    // ========== Structured Message Detection Tests ==========
    // Tests the logic that detects if a string is already a TerminalMessage

    #[test]
    fn test_structured_message_detection_output() {
        let json = r#"{"type":"output","data":"hello"}"#;
        let parsed: Result<TerminalMessage, _> = serde_json::from_str(json);
        assert!(parsed.is_ok());
        match parsed.unwrap() {
            TerminalMessage::Output { data } => assert_eq!(data, "hello"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_structured_message_detection_agents() {
        let json = r#"{"type":"agents","agents":[]}"#;
        let parsed: Result<TerminalMessage, _> = serde_json::from_str(json);
        assert!(parsed.is_ok());
        match parsed.unwrap() {
            TerminalMessage::Agents { agents } => assert!(agents.is_empty()),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_structured_message_detection_worktrees() {
        let json = r#"{"type":"worktrees","worktrees":[],"repo":"test/repo"}"#;
        let parsed: Result<TerminalMessage, _> = serde_json::from_str(json);
        assert!(parsed.is_ok());
        match parsed.unwrap() {
            TerminalMessage::Worktrees { worktrees, repo } => {
                assert!(worktrees.is_empty());
                assert_eq!(repo, Some("test/repo".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_raw_output_not_detected_as_structured() {
        // Raw terminal output should NOT parse as TerminalMessage
        let raw_output = "Hello, this is terminal output with special chars: \x1b[32mgreen\x1b[0m";
        let parsed: Result<TerminalMessage, _> = serde_json::from_str(raw_output);
        assert!(parsed.is_err(), "Raw output should not parse as TerminalMessage");
    }

    #[test]
    fn test_partial_json_not_detected_as_structured() {
        // Partial/invalid JSON should not parse
        let partial = r#"{"type":"output""#;
        let parsed: Result<TerminalMessage, _> = serde_json::from_str(partial);
        assert!(parsed.is_err());
    }

    // ========== BrowserCommand Parsing Tests ==========

    #[test]
    fn test_browser_command_input_parsing() {
        let json = r#"{"type":"input","data":"ls -la"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();
        match cmd {
            BrowserCommand::Input { data } => assert_eq!(data, "ls -la"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_browser_command_set_mode_parsing() {
        let json = r#"{"type":"set_mode","mode":"gui"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();
        match cmd {
            BrowserCommand::SetMode { mode } => assert_eq!(mode, "gui"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_browser_command_list_agents_parsing() {
        let json = r#"{"type":"list_agents"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();
        assert!(matches!(cmd, BrowserCommand::ListAgents));
    }

    #[test]
    fn test_browser_command_list_worktrees_parsing() {
        let json = r#"{"type":"list_worktrees"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();
        assert!(matches!(cmd, BrowserCommand::ListWorktrees));
    }

    #[test]
    fn test_browser_command_select_agent_parsing() {
        let json = r#"{"type":"select_agent","id":"agent-abc-123"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();
        match cmd {
            BrowserCommand::SelectAgent { id } => assert_eq!(id, "agent-abc-123"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_browser_command_create_agent_parsing() {
        let json = r#"{"type":"create_agent","issue_or_branch":"42","prompt":"Fix the bug"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();
        match cmd {
            BrowserCommand::CreateAgent { issue_or_branch, prompt } => {
                assert_eq!(issue_or_branch, Some("42".to_string()));
                assert_eq!(prompt, Some("Fix the bug".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_browser_command_create_agent_minimal_parsing() {
        // With null/missing optional fields
        let json = r#"{"type":"create_agent","issue_or_branch":"feature-branch","prompt":null}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();
        match cmd {
            BrowserCommand::CreateAgent { issue_or_branch, prompt } => {
                assert_eq!(issue_or_branch, Some("feature-branch".to_string()));
                assert_eq!(prompt, None);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_browser_command_reopen_worktree_parsing() {
        let json = r#"{"type":"reopen_worktree","path":"/path/to/wt","branch":"test-branch","prompt":"Continue work"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();
        match cmd {
            BrowserCommand::ReopenWorktree { path, branch, prompt } => {
                assert_eq!(path, "/path/to/wt");
                assert_eq!(branch, "test-branch");
                assert_eq!(prompt, Some("Continue work".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_browser_command_delete_agent_parsing() {
        let json = r#"{"type":"delete_agent","id":"agent-to-delete","delete_worktree":true}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();
        match cmd {
            BrowserCommand::DeleteAgent { id, delete_worktree } => {
                assert_eq!(id, "agent-to-delete");
                assert_eq!(delete_worktree, Some(true));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_browser_command_toggle_pty_view_parsing() {
        let json = r#"{"type":"toggle_pty_view"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();
        assert!(matches!(cmd, BrowserCommand::TogglePtyView));
    }

    #[test]
    fn test_browser_command_scroll_parsing() {
        let json = r#"{"type":"scroll","direction":"up","lines":5}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();
        match cmd {
            BrowserCommand::Scroll { direction, lines } => {
                assert_eq!(direction, "up");
                assert_eq!(lines, Some(5));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_browser_command_scroll_to_bottom_parsing() {
        let json = r#"{"type":"scroll_to_bottom"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();
        assert!(matches!(cmd, BrowserCommand::ScrollToBottom));
    }

    #[test]
    fn test_browser_command_scroll_to_top_parsing() {
        let json = r#"{"type":"scroll_to_top"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();
        assert!(matches!(cmd, BrowserCommand::ScrollToTop));
    }

    // ========== BrowserEvent Mapping Tests ==========

    #[test]
    fn test_browser_command_to_event_input() {
        let cmd = BrowserCommand::Input { data: "test".to_string() };
        let event = match cmd {
            BrowserCommand::Input { data } => BrowserEvent::Input(data),
            _ => panic!("Wrong type"),
        };
        assert!(matches!(event, BrowserEvent::Input(s) if s == "test"));
    }

    #[test]
    fn test_browser_command_to_event_set_mode() {
        let cmd = BrowserCommand::SetMode { mode: "tui".to_string() };
        let event = match cmd {
            BrowserCommand::SetMode { mode } => BrowserEvent::SetMode { mode },
            _ => panic!("Wrong type"),
        };
        assert!(matches!(event, BrowserEvent::SetMode { mode } if mode == "tui"));
    }
}
