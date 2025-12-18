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

/// Messages from browser to CLI
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrowserMessage {
    /// Keyboard input from browser
    KeyPress {
        key: String,
        ctrl: bool,
        alt: bool,
        shift: bool,
    },
    /// Browser terminal resize
    Resize { rows: u16, cols: u16 },
}

/// Messages from CLI to browser
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CLIMessage {
    /// Full TUI screen content (base64 encoded)
    Screen { data: String, rows: u16, cols: u16 },
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
}

/// Handles WebRTC peer connections with browsers
pub struct WebRTCHandler {
    /// Active peer connection
    peer_connection: Option<Arc<RTCPeerConnection>>,
    /// Active data channel for sending/receiving
    data_channel: Arc<Mutex<Option<Arc<RTCDataChannel>>>>,
    /// Queue of keyboard inputs received from browser
    input_queue: Arc<Mutex<Vec<KeyInput>>>,
    /// Browser's terminal dimensions (for rendering at correct size)
    browser_dimensions: Arc<Mutex<Option<BrowserDimensions>>>,
}

impl WebRTCHandler {
    /// Create a new WebRTC handler
    pub fn new() -> Self {
        Self {
            peer_connection: None,
            data_channel: Arc::new(Mutex::new(None)),
            input_queue: Arc::new(Mutex::new(Vec::new())),
            browser_dimensions: Arc::new(Mutex::new(None)),
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

        // ICE servers for NAT traversal
        let config = RTCConfiguration {
            ice_servers: vec![
                RTCIceServer {
                    urls: vec!["stun:stun.l.google.com:19302".to_string()],
                    ..Default::default()
                },
                RTCIceServer {
                    urls: vec!["stun:stun1.l.google.com:19302".to_string()],
                    ..Default::default()
                },
            ],
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
        let browser_dimensions = Arc::clone(&self.browser_dimensions);

        peer_connection.on_data_channel(Box::new(move |dc| {
            let dc_label = dc.label().to_owned();
            log::info!("New data channel: {}", dc_label);

            let data_channel_store = Arc::clone(&data_channel_store);
            let input_queue = Arc::clone(&input_queue);
            let browser_dimensions = Arc::clone(&browser_dimensions);
            let dc_for_store = Arc::clone(&dc);

            // Handle incoming messages (keyboard input and resize)
            dc.on_message(Box::new(move |msg: DataChannelMessage| {
                let input_queue = Arc::clone(&input_queue);
                let browser_dimensions = Arc::clone(&browser_dimensions);

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
                                    log::info!(
                                        "Browser terminal resized to {}x{} (cols x rows)",
                                        cols,
                                        rows
                                    );
                                    *browser_dimensions.lock().await =
                                        Some(BrowserDimensions { rows, cols });
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

    /// Get the browser's terminal dimensions (if set)
    pub async fn get_browser_dimensions(&self) -> Option<BrowserDimensions> {
        *self.browser_dimensions.lock().await
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
}
