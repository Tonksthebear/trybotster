//! WebRTC P2P handler for browser connections
//!
//! Streams the entire TUI interface to the browser and receives keyboard input.
//! The browser sees exactly what the local terminal sees.

use anyhow::Result;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;

/// Information about a running agent (kept for compatibility)
#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub id: String,
    pub repo: String,
    pub issue: u32,
    pub status: String,
}

/// ICE server configuration from signaling server (metered.ca format)
/// The server sends ICE servers directly so CLI doesn't need API keys
#[derive(Debug, Clone, Deserialize)]
pub struct IceServerConfig {
    /// URL(s) - can be a single string or array of strings
    pub urls: IceUrls,
    /// TURN username (optional, only for TURN servers)
    #[serde(default)]
    pub username: Option<String>,
    /// TURN credential (optional, only for TURN servers)
    #[serde(default)]
    pub credential: Option<String>,
}

/// Helper to deserialize urls which can be string or array
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum IceUrls {
    Single(String),
    Multiple(Vec<String>),
}

impl IceUrls {
    pub fn to_vec(&self) -> Vec<String> {
        match self {
            IceUrls::Single(s) => vec![s.clone()],
            IceUrls::Multiple(v) => v.clone(),
        }
    }
}

/// Browser display mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BrowserMode {
    /// TUI mode - shows full hub terminal interface (terminal widgets are 70% width)
    #[default]
    Tui,
    /// GUI mode - shows individual agent terminal directly (full width)
    Gui,
}

/// Messages from browser to CLI
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrowserMessage {
    /// Keyboard input for selected agent
    KeyPress {
        key: String,
        ctrl: bool,
        alt: bool,
        shift: bool,
    },
    /// Browser terminal resize
    Resize { rows: u16, cols: u16 },
    /// Set browser display mode (tui or gui)
    SetMode { mode: String },
    /// Request list of agents
    ListAgents,
    /// Request list of available worktrees (for reopening)
    ListWorktrees,
    /// Select an agent to view
    SelectAgent { id: String },
    /// Create a new agent (from issue number or branch name, with optional prompt)
    CreateAgent {
        /// Issue number or branch name
        issue_or_branch: String,
        /// Optional prompt for the agent
        prompt: Option<String>,
    },
    /// Reopen an existing worktree
    ReopenWorktree {
        /// Path to the worktree
        path: String,
        /// Branch name
        branch: String,
        /// Optional prompt for the agent
        prompt: Option<String>,
    },
    /// Delete an agent
    DeleteAgent {
        id: String,
        delete_worktree: bool,
    },
    /// Send raw input to selected agent
    SendInput { data: String },
}

/// Agent info sent to browser
#[derive(Debug, Clone, Serialize)]
pub struct WebAgentInfo {
    pub id: String,
    pub repo: String,
    pub issue_number: Option<u32>,
    pub branch_name: String,
    pub status: String,
    pub selected: bool,
}

/// Worktree info sent to browser
#[derive(Debug, Clone, Serialize)]
pub struct WebWorktreeInfo {
    pub path: String,
    pub branch: String,
    pub issue_number: Option<u32>,
}

/// Messages from CLI to browser
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CLIMessage {
    /// Full TUI screen content (base64 encoded) - legacy, keeping for now
    Screen { data: String, rows: u16, cols: u16 },
    /// List of agents
    Agents { agents: Vec<WebAgentInfo> },
    /// List of available worktrees for reopening
    Worktrees {
        repo: String,
        worktrees: Vec<WebWorktreeInfo>,
    },
    /// Terminal output from selected agent (base64 encoded)
    AgentOutput { id: String, data: String },
    /// Agent selection confirmed
    AgentSelected { id: String },
    /// Agent created
    AgentCreated { id: String },
    /// Agent deleted
    AgentDeleted { id: String },
    /// Error message
    Error { message: String },
}

/// Pending keyboard input from browser
#[derive(Debug, Clone)]
pub struct KeyInput {
    pub key: String,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// Browser's terminal dimensions
#[derive(Debug, Clone, Copy)]
pub struct BrowserDimensions {
    pub rows: u16,
    pub cols: u16,
    pub mode: BrowserMode,
}

/// Commands from browser to be processed by main loop
#[derive(Debug, Clone)]
pub enum BrowserCommand {
    ListAgents,
    ListWorktrees,
    SelectAgent { id: String },
    CreateAgent { issue_or_branch: String, prompt: Option<String> },
    ReopenWorktree { path: String, branch: String, prompt: Option<String> },
    DeleteAgent { id: String, delete_worktree: bool },
    SendInput { data: String },
}

/// Handles WebRTC peer connections with browsers
pub struct WebRTCHandler {
    /// Active peer connection
    peer_connection: Option<Arc<RTCPeerConnection>>,
    /// Active data channel for sending/receiving
    data_channel: Arc<Mutex<Option<Arc<RTCDataChannel>>>>,
    /// Queue of keyboard inputs received from browser
    input_queue: Arc<Mutex<Vec<KeyInput>>>,
    /// Queue of commands received from browser
    command_queue: Arc<Mutex<Vec<BrowserCommand>>>,
    /// Browser's terminal dimensions (for rendering at correct size)
    browser_dimensions: Arc<Mutex<Option<BrowserDimensions>>>,
    /// Browser's display mode (TUI or GUI)
    browser_mode: Arc<Mutex<BrowserMode>>,
}

impl WebRTCHandler {
    /// Create a new WebRTC handler
    pub fn new() -> Self {
        Self {
            peer_connection: None,
            data_channel: Arc::new(Mutex::new(None)),
            input_queue: Arc::new(Mutex::new(Vec::new())),
            command_queue: Arc::new(Mutex::new(Vec::new())),
            browser_dimensions: Arc::new(Mutex::new(None)),
            browser_mode: Arc::new(Mutex::new(BrowserMode::default())),
        }
    }

    /// Handle an incoming WebRTC offer from a browser (via signaling server)
    /// Returns the SDP answer to send back
    ///
    /// # Arguments
    /// * `offer_sdp` - The SDP offer from the browser
    /// * `ice_server_configs` - ICE servers from signaling server (STUN and TURN)
    pub async fn handle_offer(
        &mut self,
        offer_sdp: &str,
        ice_server_configs: &[IceServerConfig],
    ) -> Result<String> {
        log::info!("Received WebRTC offer, creating answer...");

        // Create media engine and interceptor registry
        let mut media_engine = MediaEngine::default();
        media_engine.register_default_codecs()?;

        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut media_engine)?;

        // Create API
        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .build();

        // Convert ICE server configs to webrtc-rs format
        let ice_servers: Vec<RTCIceServer> = if ice_server_configs.is_empty() {
            log::warn!("No ICE servers provided - using default STUN servers");
            vec![
                RTCIceServer {
                    urls: vec!["stun:stun.l.google.com:19302".to_string()],
                    ..Default::default()
                },
                RTCIceServer {
                    urls: vec!["stun:stun1.l.google.com:19302".to_string()],
                    ..Default::default()
                },
            ]
        } else {
            let mut has_turn = false;
            let servers: Vec<RTCIceServer> = ice_server_configs
                .iter()
                .map(|config| {
                    let urls = config.urls.to_vec();
                    if urls.iter().any(|u| u.starts_with("turn:") || u.starts_with("turns:")) {
                        has_turn = true;
                    }
                    RTCIceServer {
                        urls,
                        username: config.username.clone().unwrap_or_default(),
                        credential: config.credential.clone().unwrap_or_default(),
                        ..Default::default()
                    }
                })
                .collect();

            log::info!(
                "Using {} ICE server(s) from signaling server (TURN: {})",
                servers.len(),
                if has_turn { "yes" } else { "no" }
            );
            servers
        };

        // ICE servers for NAT traversal
        let config = RTCConfiguration {
            ice_servers,
            ..Default::default()
        };

        // Create peer connection
        let peer_connection = Arc::new(api.new_peer_connection(config).await?);

        // Set up connection state handler
        peer_connection.on_peer_connection_state_change(Box::new(move |state| {
            log::info!("WebRTC connection state: {:?}", state);
            if state == RTCPeerConnectionState::Failed
                || state == RTCPeerConnectionState::Disconnected
            {
                log::warn!("WebRTC connection failed or disconnected");
            }
            Box::pin(async {})
        }));

        // Set up data channel handler
        let data_channel_store = Arc::clone(&self.data_channel);
        let input_queue = Arc::clone(&self.input_queue);
        let command_queue = Arc::clone(&self.command_queue);
        let browser_dimensions = Arc::clone(&self.browser_dimensions);
        let browser_mode = Arc::clone(&self.browser_mode);

        peer_connection.on_data_channel(Box::new(move |dc| {
            let dc_label = dc.label().to_owned();
            log::info!("New data channel: {}", dc_label);

            let data_channel_store = Arc::clone(&data_channel_store);
            let input_queue = Arc::clone(&input_queue);
            let command_queue = Arc::clone(&command_queue);
            let browser_dimensions = Arc::clone(&browser_dimensions);
            let browser_mode = Arc::clone(&browser_mode);
            let dc_for_store = Arc::clone(&dc);

            // Handle incoming messages
            dc.on_message(Box::new(move |msg: DataChannelMessage| {
                let input_queue = Arc::clone(&input_queue);
                let command_queue = Arc::clone(&command_queue);
                let browser_dimensions = Arc::clone(&browser_dimensions);
                let browser_mode = Arc::clone(&browser_mode);

                Box::pin(async move {
                    if let Ok(text) = String::from_utf8(msg.data.to_vec()) {
                        if let Ok(browser_msg) = serde_json::from_str::<BrowserMessage>(&text) {
                            match browser_msg {
                                BrowserMessage::KeyPress {
                                    key,
                                    ctrl,
                                    alt,
                                    shift,
                                } => {
                                    log::debug!(
                                        "Received key: {} (ctrl={}, alt={}, shift={})",
                                        key,
                                        ctrl,
                                        alt,
                                        shift
                                    );
                                    input_queue.lock().await.push(KeyInput {
                                        key,
                                        ctrl,
                                        alt,
                                        shift,
                                    });
                                }
                                BrowserMessage::Resize { rows, cols } => {
                                    let mode = *browser_mode.lock().await;
                                    log::info!(
                                        "Browser terminal resized to {}x{} (cols x rows), mode={:?}",
                                        cols,
                                        rows,
                                        mode
                                    );
                                    *browser_dimensions.lock().await =
                                        Some(BrowserDimensions { rows, cols, mode });
                                }
                                BrowserMessage::SetMode { mode } => {
                                    let new_mode = match mode.as_str() {
                                        "gui" => BrowserMode::Gui,
                                        _ => BrowserMode::Tui,
                                    };
                                    log::info!("Browser mode set to {:?}", new_mode);
                                    *browser_mode.lock().await = new_mode;
                                    // Update dimensions with new mode if we have them
                                    if let Some(dims) = browser_dimensions.lock().await.as_mut() {
                                        dims.mode = new_mode;
                                    }
                                }
                                BrowserMessage::ListAgents => {
                                    log::info!("Browser requested agent list");
                                    command_queue.lock().await.push(BrowserCommand::ListAgents);
                                }
                                BrowserMessage::ListWorktrees => {
                                    log::info!("Browser requested worktree list");
                                    command_queue.lock().await.push(BrowserCommand::ListWorktrees);
                                }
                                BrowserMessage::SelectAgent { id } => {
                                    log::info!("Browser selected agent: {}", id);
                                    command_queue.lock().await.push(BrowserCommand::SelectAgent { id });
                                }
                                BrowserMessage::CreateAgent { issue_or_branch, prompt } => {
                                    log::info!("Browser requested create agent: {}", issue_or_branch);
                                    command_queue.lock().await.push(BrowserCommand::CreateAgent { issue_or_branch, prompt });
                                }
                                BrowserMessage::ReopenWorktree { path, branch, prompt } => {
                                    log::info!("Browser requested reopen worktree: {} ({})", path, branch);
                                    command_queue.lock().await.push(BrowserCommand::ReopenWorktree { path, branch, prompt });
                                }
                                BrowserMessage::DeleteAgent { id, delete_worktree } => {
                                    log::info!("Browser requested delete agent: {} (delete_worktree={})", id, delete_worktree);
                                    command_queue.lock().await.push(BrowserCommand::DeleteAgent { id, delete_worktree });
                                }
                                BrowserMessage::SendInput { data } => {
                                    log::debug!("Browser sent input: {} bytes", data.len());
                                    command_queue.lock().await.push(BrowserCommand::SendInput { data });
                                }
                            }
                        }
                    }
                })
            }));

            // Store data channel when opened
            dc.on_open(Box::new(move || {
                log::info!("Data channel '{}' opened - storing for screen streaming", dc_label);
                let dc_for_store = Arc::clone(&dc_for_store);
                let data_channel_store = Arc::clone(&data_channel_store);
                Box::pin(async move {
                    *data_channel_store.lock().await = Some(dc_for_store);
                })
            }));

            Box::pin(async {})
        }));

        // Set remote description (the offer)
        let offer = RTCSessionDescription::offer(offer_sdp.to_string())?;
        peer_connection.set_remote_description(offer).await?;

        // Create answer
        let answer = peer_connection.create_answer(None).await?;

        // Set local description
        peer_connection.set_local_description(answer).await?;

        // Wait for ICE gathering to complete
        let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
        peer_connection.on_ice_gathering_state_change(Box::new(move |state| {
            log::debug!("ICE gathering state: {:?}", state);
            if state == webrtc::ice_transport::ice_gatherer_state::RTCIceGathererState::Complete {
                let _ = tx.try_send(());
            }
            Box::pin(async {})
        }));

        // Wait up to 5 seconds for ICE gathering
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await;

        // Get the local description with ICE candidates
        let local_desc = peer_connection
            .local_description()
            .await
            .ok_or_else(|| anyhow::anyhow!("No local description after ICE gathering"))?;

        // Store peer connection
        self.peer_connection = Some(peer_connection);

        log::info!("WebRTC answer created successfully");
        Ok(local_desc.sdp)
    }

    /// Send the TUI screen content to connected browser
    pub async fn send_screen(&self, screen_data: &str, rows: u16, cols: u16) -> Result<()> {
        let dc = self.data_channel.lock().await;
        if let Some(ref dc) = *dc {
            let msg = CLIMessage::Screen {
                data: BASE64.encode(screen_data.as_bytes()),
                rows,
                cols,
            };
            let json = serde_json::to_string(&msg)?;
            dc.send_text(json).await?;
        }
        Ok(())
    }

    /// Get pending keyboard inputs from browser
    pub async fn get_pending_inputs(&self) -> Vec<KeyInput> {
        let mut queue = self.input_queue.lock().await;
        std::mem::take(&mut *queue)
    }

    /// Get pending commands from browser
    pub async fn get_pending_commands(&self) -> Vec<BrowserCommand> {
        let mut queue = self.command_queue.lock().await;
        std::mem::take(&mut *queue)
    }

    /// Get the browser's terminal dimensions (if set)
    pub async fn get_browser_dimensions(&self) -> Option<BrowserDimensions> {
        *self.browser_dimensions.lock().await
    }

    /// Send agent list to browser
    pub async fn send_agents(&self, agents: Vec<WebAgentInfo>) -> Result<()> {
        let dc = self.data_channel.lock().await;
        if let Some(ref dc) = *dc {
            let msg = CLIMessage::Agents { agents };
            let json = serde_json::to_string(&msg)?;
            dc.send_text(json).await?;
        }
        Ok(())
    }

    /// Send available worktrees list to browser
    pub async fn send_worktrees(&self, repo: &str, worktrees: Vec<WebWorktreeInfo>) -> Result<()> {
        let dc = self.data_channel.lock().await;
        if let Some(ref dc) = *dc {
            let msg = CLIMessage::Worktrees {
                repo: repo.to_string(),
                worktrees,
            };
            let json = serde_json::to_string(&msg)?;
            dc.send_text(json).await?;
        }
        Ok(())
    }

    /// Send agent terminal output to browser
    pub async fn send_agent_output(&self, id: &str, data: &str) -> Result<()> {
        let dc = self.data_channel.lock().await;
        if let Some(ref dc) = *dc {
            let msg = CLIMessage::AgentOutput {
                id: id.to_string(),
                data: BASE64.encode(data.as_bytes()),
            };
            let json = serde_json::to_string(&msg)?;
            dc.send_text(json).await?;
        }
        Ok(())
    }

    /// Send agent selection confirmation to browser
    pub async fn send_agent_selected(&self, id: &str) -> Result<()> {
        let dc = self.data_channel.lock().await;
        if let Some(ref dc) = *dc {
            let msg = CLIMessage::AgentSelected { id: id.to_string() };
            let json = serde_json::to_string(&msg)?;
            dc.send_text(json).await?;
        }
        Ok(())
    }

    /// Send agent created confirmation to browser
    pub async fn send_agent_created(&self, id: &str) -> Result<()> {
        let dc = self.data_channel.lock().await;
        if let Some(ref dc) = *dc {
            let msg = CLIMessage::AgentCreated { id: id.to_string() };
            let json = serde_json::to_string(&msg)?;
            dc.send_text(json).await?;
        }
        Ok(())
    }

    /// Send agent deleted confirmation to browser
    pub async fn send_agent_deleted(&self, id: &str) -> Result<()> {
        let dc = self.data_channel.lock().await;
        if let Some(ref dc) = *dc {
            let msg = CLIMessage::AgentDeleted { id: id.to_string() };
            let json = serde_json::to_string(&msg)?;
            dc.send_text(json).await?;
        }
        Ok(())
    }

    /// Send error message to browser
    pub async fn send_error(&self, message: &str) -> Result<()> {
        let dc = self.data_channel.lock().await;
        if let Some(ref dc) = *dc {
            let msg = CLIMessage::Error { message: message.to_string() };
            let json = serde_json::to_string(&msg)?;
            dc.send_text(json).await?;
        }
        Ok(())
    }

    /// Check if there's an active P2P connection
    pub fn is_connected(&self) -> bool {
        self.peer_connection.is_some()
    }

    /// Check if data channel is ready for sending
    pub async fn is_ready(&self) -> bool {
        self.data_channel.lock().await.is_some()
    }

    /// Close the peer connection
    pub async fn close(&mut self) -> Result<()> {
        *self.data_channel.lock().await = None;
        if let Some(pc) = self.peer_connection.take() {
            pc.close().await?;
        }
        Ok(())
    }
}

impl Default for WebRTCHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_browser_message_parsing() {
        let msg = r#"{"type": "key_press", "key": "j", "ctrl": false, "alt": false, "shift": false}"#;
        let parsed: BrowserMessage = serde_json::from_str(msg).unwrap();
        assert!(matches!(
            parsed,
            BrowserMessage::KeyPress { key, ctrl: false, alt: false, shift: false } if key == "j"
        ));

        let msg = r#"{"type": "key_press", "key": "q", "ctrl": true, "alt": false, "shift": false}"#;
        let parsed: BrowserMessage = serde_json::from_str(msg).unwrap();
        assert!(matches!(
            parsed,
            BrowserMessage::KeyPress { key, ctrl: true, .. } if key == "q"
        ));
    }

    #[test]
    fn test_cli_message_serialization() {
        let msg = CLIMessage::Screen {
            data: "dGVzdA==".to_string(), // "test" base64 encoded
            rows: 24,
            cols: 80,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"screen\""));
        assert!(json.contains("\"rows\":24"));
        assert!(json.contains("\"cols\":80"));
    }

    #[test]
    fn test_browser_command_messages() {
        // Test ListAgents
        let msg = r#"{"type": "list_agents"}"#;
        let parsed: BrowserMessage = serde_json::from_str(msg).unwrap();
        assert!(matches!(parsed, BrowserMessage::ListAgents));

        // Test SelectAgent
        let msg = r#"{"type": "select_agent", "id": "my-repo-123"}"#;
        let parsed: BrowserMessage = serde_json::from_str(msg).unwrap();
        assert!(matches!(
            parsed,
            BrowserMessage::SelectAgent { id } if id == "my-repo-123"
        ));

        // Test CreateAgent with issue number
        let msg = r#"{"type": "create_agent", "issue_or_branch": "42"}"#;
        let parsed: BrowserMessage = serde_json::from_str(msg).unwrap();
        assert!(matches!(
            parsed,
            BrowserMessage::CreateAgent { issue_or_branch, prompt: None } if issue_or_branch == "42"
        ));

        // Test CreateAgent with branch name and prompt
        let msg = r#"{"type": "create_agent", "issue_or_branch": "feature-branch", "prompt": "Fix the bug"}"#;
        let parsed: BrowserMessage = serde_json::from_str(msg).unwrap();
        assert!(matches!(
            parsed,
            BrowserMessage::CreateAgent { issue_or_branch, prompt: Some(_) } if issue_or_branch == "feature-branch"
        ));

        // Test DeleteAgent
        let msg = r#"{"type": "delete_agent", "id": "my-repo-123", "delete_worktree": true}"#;
        let parsed: BrowserMessage = serde_json::from_str(msg).unwrap();
        assert!(matches!(
            parsed,
            BrowserMessage::DeleteAgent { id, delete_worktree } if id == "my-repo-123" && delete_worktree
        ));

        // Test SendInput
        let msg = r#"{"type": "send_input", "data": "hello\n"}"#;
        let parsed: BrowserMessage = serde_json::from_str(msg).unwrap();
        assert!(matches!(
            parsed,
            BrowserMessage::SendInput { data } if data == "hello\n"
        ));
    }

    #[test]
    fn test_cli_response_messages() {
        // Test Agents list
        let agents = vec![WebAgentInfo {
            id: "test-repo-123".to_string(),
            repo: "test/repo".to_string(),
            issue_number: Some(123),
            branch_name: "botster-issue-123".to_string(),
            status: "Running".to_string(),
            selected: true,
        }];
        let msg = CLIMessage::Agents { agents };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"agents\""));
        assert!(json.contains("\"id\":\"test-repo-123\""));
        assert!(json.contains("\"selected\":true"));

        // Test AgentOutput
        let msg = CLIMessage::AgentOutput {
            id: "test-agent".to_string(),
            data: BASE64.encode(b"terminal output"),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"agent_output\""));
        assert!(json.contains("\"id\":\"test-agent\""));

        // Test AgentSelected
        let msg = CLIMessage::AgentSelected {
            id: "selected-id".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"agent_selected\""));
        assert!(json.contains("\"id\":\"selected-id\""));

        // Test AgentCreated
        let msg = CLIMessage::AgentCreated {
            id: "new-agent-id".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"agent_created\""));

        // Test AgentDeleted
        let msg = CLIMessage::AgentDeleted {
            id: "deleted-id".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"agent_deleted\""));

        // Test Error
        let msg = CLIMessage::Error {
            message: "Something went wrong".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"error\""));
        assert!(json.contains("\"message\":\"Something went wrong\""));
    }

    #[test]
    fn test_browser_mode_default() {
        // BrowserMode should default to Tui
        let mode = BrowserMode::default();
        assert_eq!(mode, BrowserMode::Tui);
    }

    #[test]
    fn test_set_mode_message_parsing() {
        // Test SetMode with gui
        let msg = r#"{"type": "set_mode", "mode": "gui"}"#;
        let parsed: BrowserMessage = serde_json::from_str(msg).unwrap();
        assert!(matches!(
            parsed,
            BrowserMessage::SetMode { mode } if mode == "gui"
        ));

        // Test SetMode with tui
        let msg = r#"{"type": "set_mode", "mode": "tui"}"#;
        let parsed: BrowserMessage = serde_json::from_str(msg).unwrap();
        assert!(matches!(
            parsed,
            BrowserMessage::SetMode { mode } if mode == "tui"
        ));
    }

    #[test]
    fn test_resize_message_parsing() {
        let msg = r#"{"type": "resize", "rows": 24, "cols": 80}"#;
        let parsed: BrowserMessage = serde_json::from_str(msg).unwrap();
        assert!(matches!(
            parsed,
            BrowserMessage::Resize { rows: 24, cols: 80 }
        ));
    }

    #[test]
    fn test_browser_dimensions_with_mode() {
        // Test that BrowserDimensions properly stores mode
        let dims_gui = BrowserDimensions {
            rows: 24,
            cols: 80,
            mode: BrowserMode::Gui,
        };
        assert_eq!(dims_gui.mode, BrowserMode::Gui);
        assert_eq!(dims_gui.rows, 24);
        assert_eq!(dims_gui.cols, 80);

        let dims_tui = BrowserDimensions {
            rows: 50,
            cols: 100,
            mode: BrowserMode::Tui,
        };
        assert_eq!(dims_tui.mode, BrowserMode::Tui);
        assert_eq!(dims_tui.rows, 50);
        assert_eq!(dims_tui.cols, 100);
    }

    #[test]
    fn test_reopen_worktree_message_parsing() {
        // Test ReopenWorktree without prompt
        let msg = r#"{"type": "reopen_worktree", "path": "/path/to/worktree", "branch": "feature-123"}"#;
        let parsed: BrowserMessage = serde_json::from_str(msg).unwrap();
        assert!(matches!(
            parsed,
            BrowserMessage::ReopenWorktree { path, branch, prompt: None }
            if path == "/path/to/worktree" && branch == "feature-123"
        ));

        // Test ReopenWorktree with prompt
        let msg = r#"{"type": "reopen_worktree", "path": "/path/to/worktree", "branch": "feature-123", "prompt": "Continue work"}"#;
        let parsed: BrowserMessage = serde_json::from_str(msg).unwrap();
        assert!(matches!(
            parsed,
            BrowserMessage::ReopenWorktree { path, branch, prompt: Some(_) }
            if path == "/path/to/worktree" && branch == "feature-123"
        ));
    }

    #[test]
    fn test_list_worktrees_message_parsing() {
        let msg = r#"{"type": "list_worktrees"}"#;
        let parsed: BrowserMessage = serde_json::from_str(msg).unwrap();
        assert!(matches!(parsed, BrowserMessage::ListWorktrees));
    }

    #[test]
    fn test_worktrees_response_serialization() {
        let worktrees = vec![
            WebWorktreeInfo {
                path: "/path/to/worktree".to_string(),
                branch: "feature-123".to_string(),
                issue_number: Some(123),
            },
            WebWorktreeInfo {
                path: "/path/to/other".to_string(),
                branch: "manual-branch".to_string(),
                issue_number: None,
            },
        ];
        let msg = CLIMessage::Worktrees {
            repo: "owner/repo".to_string(),
            worktrees,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"worktrees\""));
        assert!(json.contains("\"repo\":\"owner/repo\""));
        assert!(json.contains("\"issue_number\":123"));
        assert!(json.contains("\"branch\":\"manual-branch\""));
    }
}
