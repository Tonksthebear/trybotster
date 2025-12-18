//! WebRTC P2P handler for browser connections
//!
//! Allows users to view their running agents directly in the browser
//! via peer-to-peer WebRTC data channels. The Rails server only handles
//! signaling - actual data never passes through the server.

use anyhow::Result;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
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

/// Information about a running agent
#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub id: String,
    pub repo: String,
    pub issue: u32,
    pub status: String,
}

/// Messages from browser to CLI
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrowserMessage {
    GetAgents,
    Subscribe { agent_id: String },
    Unsubscribe { agent_id: String },
    // Phase 2: Interactive mode
    // Input { agent_id: String, data: String },
}

/// Messages from CLI to browser
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CLIMessage {
    Agents { agents: Vec<AgentInfo> },
    Output { agent_id: String, data: String },
    Status { agent_id: String, status: String },
    Error { message: String },
}

/// Handles WebRTC peer connections with browsers
pub struct WebRTCHandler {
    /// Active peer connection (only one browser connection at a time for now)
    peer_connection: Option<Arc<RTCPeerConnection>>,
    /// Active data channel
    data_channel: Option<Arc<RTCDataChannel>>,
    /// Agent IDs the browser is subscribed to for output streaming
    subscriptions: Arc<Mutex<HashSet<String>>>,
    /// Callback to get current agent list
    get_agents: Arc<dyn Fn() -> Vec<AgentInfo> + Send + Sync>,
}

impl WebRTCHandler {
    /// Create a new WebRTC handler
    pub fn new<F>(get_agents: F) -> Self
    where
        F: Fn() -> Vec<AgentInfo> + Send + Sync + 'static,
    {
        Self {
            peer_connection: None,
            data_channel: None,
            subscriptions: Arc::new(Mutex::new(HashSet::new())),
            get_agents: Arc::new(get_agents),
        }
    }

    /// Handle an incoming WebRTC offer from a browser (via signaling server)
    /// Returns the SDP answer to send back
    pub async fn handle_offer(&mut self, offer_sdp: &str) -> Result<String> {
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

        // ICE configuration with public STUN servers
        let config = RTCConfiguration {
            ice_servers: vec![
                RTCIceServer {
                    urls: vec!["stun:stun.l.google.com:19302".to_owned()],
                    ..Default::default()
                },
                RTCIceServer {
                    urls: vec!["stun:stun1.l.google.com:19302".to_owned()],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        // Create peer connection
        let peer_connection = Arc::new(api.new_peer_connection(config).await?);

        // Set up connection state handler
        peer_connection.on_peer_connection_state_change(Box::new(move |state| {
            log::info!("WebRTC connection state changed: {:?}", state);
            if state == RTCPeerConnectionState::Failed
                || state == RTCPeerConnectionState::Disconnected
            {
                log::warn!("WebRTC connection failed or disconnected");
            }
            Box::pin(async {})
        }));

        // Set up data channel handler
        let subscriptions = Arc::clone(&self.subscriptions);
        let get_agents = Arc::clone(&self.get_agents);

        peer_connection.on_data_channel(Box::new(move |dc| {
            let dc_label = dc.label().to_owned();
            log::info!("New data channel: {}", dc_label);

            let subscriptions = Arc::clone(&subscriptions);
            let get_agents = Arc::clone(&get_agents);
            let dc_clone = Arc::clone(&dc);

            // Handle incoming messages
            dc.on_message(Box::new(move |msg: DataChannelMessage| {
                let subscriptions = Arc::clone(&subscriptions);
                let get_agents = Arc::clone(&get_agents);
                let dc = Arc::clone(&dc_clone);

                Box::pin(async move {
                    if let Ok(text) = String::from_utf8(msg.data.to_vec()) {
                        if let Err(e) =
                            handle_browser_message(&text, &dc, &subscriptions, &get_agents).await
                        {
                            log::error!("Error handling browser message: {}", e);
                        }
                    }
                })
            }));

            dc.on_open(Box::new(move || {
                log::info!("Data channel '{}' opened", dc_label);
                Box::pin(async {})
            }));

            Box::pin(async {})
        }));

        // Parse and set remote description (the offer)
        let offer = RTCSessionDescription::offer(offer_sdp.to_owned())?;
        peer_connection.set_remote_description(offer).await?;

        // Create answer
        let answer = peer_connection.create_answer(None).await?;

        // Set local description
        peer_connection
            .set_local_description(answer.clone())
            .await?;

        // Wait for ICE gathering to complete
        let (ice_done_tx, ice_done_rx) = tokio::sync::oneshot::channel::<()>();
        let ice_done_tx = Arc::new(Mutex::new(Some(ice_done_tx)));

        peer_connection.on_ice_gathering_state_change(Box::new(move |state| {
            log::debug!("ICE gathering state: {:?}", state);
            if state == webrtc::ice_transport::ice_gatherer_state::RTCIceGathererState::Complete {
                let tx = Arc::clone(&ice_done_tx);
                Box::pin(async move {
                    if let Some(tx) = tx.lock().await.take() {
                        let _ = tx.send(());
                    }
                })
            } else {
                Box::pin(async {})
            }
        }));

        // Wait for ICE gathering with timeout
        tokio::select! {
            _ = ice_done_rx => {
                log::info!("ICE gathering complete");
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {
                log::warn!("ICE gathering timed out, proceeding with available candidates");
            }
        }

        // Get the final local description with ICE candidates
        let local_desc = peer_connection
            .local_description()
            .await
            .ok_or_else(|| anyhow::anyhow!("No local description available"))?;

        // Store peer connection
        self.peer_connection = Some(peer_connection);

        log::info!("WebRTC answer created successfully");
        Ok(local_desc.sdp)
    }

    /// Send terminal output to subscribed browsers
    pub async fn send_output(&self, agent_id: &str, data: &[u8]) -> Result<()> {
        let subscriptions = self.subscriptions.lock().await;
        if !subscriptions.contains(agent_id) {
            return Ok(()); // Not subscribed
        }
        drop(subscriptions);

        if let Some(dc) = &self.data_channel {
            let msg = CLIMessage::Output {
                agent_id: agent_id.to_string(),
                data: BASE64.encode(data),
            };
            let json = serde_json::to_string(&msg)?;
            dc.send_text(json).await?;
        }
        Ok(())
    }

    /// Send agent status update to browser
    pub async fn send_status(&self, agent_id: &str, status: &str) -> Result<()> {
        if let Some(dc) = &self.data_channel {
            let msg = CLIMessage::Status {
                agent_id: agent_id.to_string(),
                status: status.to_string(),
            };
            let json = serde_json::to_string(&msg)?;
            dc.send_text(json).await?;
        }
        Ok(())
    }

    /// Check if there's an active P2P connection
    pub fn is_connected(&self) -> bool {
        self.peer_connection.is_some()
    }

    /// Close the peer connection
    pub async fn close(&mut self) -> Result<()> {
        if let Some(pc) = self.peer_connection.take() {
            pc.close().await?;
        }
        self.data_channel = None;
        self.subscriptions.lock().await.clear();
        Ok(())
    }
}

/// Handle a message from the browser
async fn handle_browser_message(
    msg: &str,
    dc: &Arc<RTCDataChannel>,
    subscriptions: &Arc<Mutex<HashSet<String>>>,
    get_agents: &Arc<dyn Fn() -> Vec<AgentInfo> + Send + Sync>,
) -> Result<()> {
    let message: BrowserMessage = serde_json::from_str(msg)?;

    match message {
        BrowserMessage::GetAgents => {
            let agents = get_agents();
            let response = CLIMessage::Agents { agents };
            let json = serde_json::to_string(&response)?;
            dc.send_text(json).await?;
        }
        BrowserMessage::Subscribe { agent_id } => {
            log::info!("Browser subscribed to agent: {}", agent_id);
            subscriptions.lock().await.insert(agent_id);
        }
        BrowserMessage::Unsubscribe { agent_id } => {
            log::info!("Browser unsubscribed from agent: {}", agent_id);
            subscriptions.lock().await.remove(&agent_id);
        } // Phase 2: Handle input
          // BrowserMessage::Input { agent_id, data } => {
          //     // Send to agent's PTY
          // }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_browser_message_parsing() {
        let msg = r#"{"type": "get_agents"}"#;
        let parsed: BrowserMessage = serde_json::from_str(msg).unwrap();
        assert!(matches!(parsed, BrowserMessage::GetAgents));

        let msg = r#"{"type": "subscribe", "agent_id": "test-123"}"#;
        let parsed: BrowserMessage = serde_json::from_str(msg).unwrap();
        assert!(matches!(parsed, BrowserMessage::Subscribe { agent_id } if agent_id == "test-123"));
    }

    #[test]
    fn test_cli_message_serialization() {
        let msg = CLIMessage::Agents {
            agents: vec![AgentInfo {
                id: "test-123".to_string(),
                repo: "owner/repo".to_string(),
                issue: 42,
                status: "running".to_string(),
            }],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"agents\""));
        assert!(json.contains("\"id\":\"test-123\""));
    }

    #[test]
    fn test_output_base64_encoding() {
        let msg = CLIMessage::Output {
            agent_id: "test".to_string(),
            data: BASE64.encode(b"hello\x1b[32mworld\x1b[0m"),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"output\""));
    }
}
