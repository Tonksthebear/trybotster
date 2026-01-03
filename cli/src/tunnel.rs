use futures_util::{SinkExt, StreamExt};
use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite::Message, tungstenite::client::IntoClientRequest};

/// Tunnel connection status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TunnelStatus {
    Disconnected = 0,
    Connecting = 1,
    Connected = 2,
}

impl From<u8> for TunnelStatus {
    fn from(value: u8) -> Self {
        match value {
            1 => TunnelStatus::Connecting,
            2 => TunnelStatus::Connected,
            _ => TunnelStatus::Disconnected,
        }
    }
}

/// Allocate an available port for an agent's tunnel
pub fn allocate_tunnel_port() -> Option<u16> {
    // Try ports in range 4001-4999 (avoid common dev ports)
    for port in 4001..5000 {
        if TcpListener::bind(format!("127.0.0.1:{}", port)).is_ok() {
            return Some(port);
        }
    }
    None
}

/// Pending agent registration to notify Rails about
#[derive(Debug, Clone)]
pub struct PendingRegistration {
    pub session_key: String,
    pub port: u16,
}

/// Manages tunnel connections for all agents on this hub
pub struct TunnelManager {
    hub_identifier: String,
    api_key: String,
    server_url: String,
    // Map of session_key -> allocated port
    agent_ports: Arc<Mutex<HashMap<String, u16>>>,
    // Connection status (atomic for lock-free access from TUI)
    status: Arc<AtomicU8>,
    // Channel for pending agent registrations that need to be sent to Rails
    pending_tx: mpsc::UnboundedSender<PendingRegistration>,
    pending_rx: Arc<Mutex<mpsc::UnboundedReceiver<PendingRegistration>>>,
}

impl TunnelManager {
    pub fn new(hub_identifier: String, api_key: String, server_url: String) -> Self {
        let (pending_tx, pending_rx) = mpsc::unbounded_channel();
        Self {
            hub_identifier,
            api_key,
            server_url,
            agent_ports: Arc::new(Mutex::new(HashMap::new())),
            status: Arc::new(AtomicU8::new(TunnelStatus::Disconnected as u8)),
            pending_tx,
            pending_rx: Arc::new(Mutex::new(pending_rx)),
        }
    }

    /// Get the current tunnel connection status
    pub fn get_status(&self) -> TunnelStatus {
        TunnelStatus::from(self.status.load(Ordering::Relaxed))
    }

    /// Set the tunnel connection status
    fn set_status(&self, status: TunnelStatus) {
        self.status.store(status as u8, Ordering::Relaxed);
    }

    /// Register an agent's tunnel port and queue notification to Rails
    pub async fn register_agent(&self, session_key: String, port: u16) {
        let mut ports = self.agent_ports.lock().await;
        ports.insert(session_key.clone(), port);
        drop(ports); // Release lock before sending

        // Queue notification to Rails (will be sent when connection is established)
        if let Err(e) = self.pending_tx.send(PendingRegistration {
            session_key: session_key.clone(),
            port,
        }) {
            warn!("[Tunnel] Failed to queue agent registration: {}", e);
        } else {
            debug!("[Tunnel] Queued registration for agent {} on port {}", session_key, port);
        }
    }

    /// Get the port for an agent
    pub async fn get_agent_port(&self, session_key: &str) -> Option<u16> {
        let ports = self.agent_ports.lock().await;
        ports.get(session_key).copied()
    }

    pub async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let ws_url = format!(
            "{}/cable?api_key={}",
            self.server_url
                .replace("https://", "wss://")
                .replace("http://", "ws://"),
            self.api_key
        );

        self.set_status(TunnelStatus::Connecting);
        info!("[Tunnel] Connecting to {}", ws_url);

        // Build request with Origin header (required by ActionCable)
        let mut request = match ws_url.into_client_request() {
            Ok(req) => req,
            Err(e) => {
                error!("[Tunnel] Failed to build WebSocket request: {}", e);
                self.set_status(TunnelStatus::Disconnected);
                return Err(e.into());
            }
        };
        // Set Origin header to match the server URL (ActionCable requires this)
        let origin = self.server_url.clone();
        request.headers_mut().insert(
            "Origin",
            origin.parse().unwrap_or_else(|_| "http://localhost".parse().unwrap()),
        );

        let (ws_stream, _) = match connect_async(request).await {
            Ok(stream) => {
                info!("[Tunnel] WebSocket connected successfully");
                stream
            }
            Err(e) => {
                error!("[Tunnel] WebSocket connection failed: {}", e);
                self.set_status(TunnelStatus::Disconnected);
                return Err(e.into());
            }
        };
        let (mut write, mut read) = ws_stream.split();

        // Subscribe to tunnel channel for this hub
        let subscribe_msg = serde_json::json!({
            "command": "subscribe",
            "identifier": serde_json::json!({
                "channel": "TunnelChannel",
                "hub_id": self.hub_identifier
            }).to_string()
        });
        info!("[Tunnel] Sending subscribe message: {}", subscribe_msg);
        write
            .send(Message::Text(subscribe_msg.to_string().into()))
            .await?;

        info!("[Tunnel] Subscribe sent, entering message loop for hub {}", self.hub_identifier);

        // Handle incoming HTTP request messages and pending registrations
        loop {
            // Lock pending_rx for this iteration
            let mut pending_rx = self.pending_rx.lock().await;

            tokio::select! {
                // Handle incoming WebSocket messages
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            drop(pending_rx); // Release lock before handling message
                            if let Err(e) = self
                                .handle_message(&text.to_string(), &mut write)
                                .await
                            {
                                error!("[Tunnel] Message error: {}", e);
                            }
                        }
                        Some(Ok(Message::Ping(data))) => {
                            drop(pending_rx);
                            if let Err(e) = write.send(Message::Pong(data)).await {
                                warn!("[Tunnel] Failed to send pong: {}", e);
                            }
                        }
                        Some(Ok(Message::Close(frame))) => {
                            info!("[Tunnel] Connection closed by server: {:?}", frame);
                            break;
                        }
                        Some(Err(e)) => {
                            error!("[Tunnel] WebSocket error: {}", e);
                            break;
                        }
                        None => {
                            info!("[Tunnel] WebSocket stream ended (None received)");
                            break;
                        }
                        Some(Ok(other)) => {
                            debug!("[Tunnel] Received other message type: {:?}", other);
                        }
                    }
                }
                // Handle pending agent registrations
                Some(registration) = pending_rx.recv() => {
                    drop(pending_rx); // Release lock before sending
                    // Only send if we're connected
                    if self.get_status() == TunnelStatus::Connected {
                        info!("[Tunnel] Notifying Rails of agent {} on port {}",
                            registration.session_key, registration.port);
                        if let Err(e) = self.notify_agent_tunnel(
                            &mut write,
                            &registration.session_key,
                            registration.port
                        ).await {
                            warn!("[Tunnel] Failed to notify agent tunnel: {}", e);
                        }
                    } else {
                        debug!("[Tunnel] Skipping registration (not connected yet): {}",
                            registration.session_key);
                        // Re-queue for later
                        let _ = self.pending_tx.send(registration);
                    }
                }
            }
        }

        self.set_status(TunnelStatus::Disconnected);
        Ok(())
    }

    async fn handle_message<S>(
        &self,
        text: &str,
        write: &mut S,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        S: SinkExt<Message> + Unpin,
        S::Error: std::error::Error + Send + Sync + 'static,
    {
        let msg: serde_json::Value = serde_json::from_str(text)?;

        // Handle ActionCable protocol messages
        if let Some(msg_type) = msg.get("type").and_then(|t| t.as_str()) {
            match msg_type {
                "welcome" => {
                    info!("[Tunnel] ActionCable welcome received");
                }
                "confirm_subscription" => {
                    self.set_status(TunnelStatus::Connected);
                    info!("[Tunnel] Subscription confirmed - tunnel connected");

                    // Send all existing registered agents to Rails
                    let ports = self.agent_ports.lock().await;
                    for (session_key, port) in ports.iter() {
                        info!("[Tunnel] Registering existing agent {} on port {}", session_key, port);
                        if let Err(e) = self.notify_agent_tunnel(write, session_key, *port).await {
                            warn!("[Tunnel] Failed to notify agent tunnel: {}", e);
                        }
                    }
                }
                "reject_subscription" => {
                    // Hub doesn't exist yet - will retry after heartbeat creates it
                    debug!("[Tunnel] Subscription rejected - hub not yet created");
                }
                "disconnect" => {
                    self.set_status(TunnelStatus::Disconnected);
                    warn!("[Tunnel] Disconnected by server");
                }
                "ping" => {
                    // ActionCable ping, no response needed
                }
                _ => {}
            }
            return Ok(());
        }

        // Handle actual messages
        if let Some(message) = msg.get("message") {
            if message.get("type").and_then(|t| t.as_str()) == Some("http_request") {
                let request_id = message["request_id"].as_str().unwrap_or_default();
                let agent_session_key = message["agent_session_key"].as_str().unwrap_or_default();

                debug!(
                    "[Tunnel] HTTP request for agent {}: {}",
                    agent_session_key,
                    message["path"].as_str().unwrap_or("/")
                );

                // Find the port for this agent
                let port = match self.get_agent_port(agent_session_key).await {
                    Some(p) => p,
                    None => {
                        warn!("[Tunnel] Agent {} not registered", agent_session_key);
                        self.send_error_response(write, request_id, "Agent tunnel not registered")
                            .await?;
                        return Ok(());
                    }
                };

                let method = message["method"].as_str().unwrap_or("GET");
                let path = message["path"].as_str().unwrap_or("/");
                let query = message["query_string"].as_str().unwrap_or("");
                let headers: HashMap<String, String> =
                    serde_json::from_value(message["headers"].clone()).unwrap_or_default();
                let body = message["body"].as_str().unwrap_or("");

                // Forward to local server
                let response = self
                    .forward_request(port, method, path, query, &headers, body)
                    .await;

                // Send response back via ActionCable
                let response_msg = serde_json::json!({
                    "command": "message",
                    "identifier": serde_json::json!({
                        "channel": "TunnelChannel",
                        "hub_id": self.hub_identifier
                    }).to_string(),
                    "data": serde_json::json!({
                        "action": "http_response",
                        "request_id": request_id,
                        "status": response.status,
                        "headers": response.headers,
                        "body": response.body,
                        "content_type": response.content_type
                    }).to_string()
                });

                write
                    .send(Message::Text(response_msg.to_string().into()))
                    .await?;
            }
        }

        Ok(())
    }

    async fn send_error_response<S>(
        &self,
        write: &mut S,
        request_id: &str,
        error: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        S: SinkExt<Message> + Unpin,
        S::Error: std::error::Error + Send + Sync + 'static,
    {
        let response_msg = serde_json::json!({
            "command": "message",
            "identifier": serde_json::json!({
                "channel": "TunnelChannel",
                "hub_id": self.hub_identifier
            }).to_string(),
            "data": serde_json::json!({
                "action": "http_response",
                "request_id": request_id,
                "status": 502,
                "headers": {},
                "body": error,
                "content_type": "text/plain"
            }).to_string()
        });
        write
            .send(Message::Text(response_msg.to_string().into()))
            .await?;
        Ok(())
    }

    async fn forward_request(
        &self,
        port: u16,
        method: &str,
        path: &str,
        query: &str,
        headers: &HashMap<String, String>,
        body: &str,
    ) -> TunnelResponse {
        let client = reqwest::Client::new();
        let url = if query.is_empty() {
            format!("http://localhost:{}{}", port, path)
        } else {
            format!("http://localhost:{}{}?{}", port, path, query)
        };

        let mut req = match method {
            "GET" => client.get(&url),
            "POST" => client.post(&url),
            "PUT" => client.put(&url),
            "PATCH" => client.patch(&url),
            "DELETE" => client.delete(&url),
            "HEAD" => client.head(&url),
            _ => client.get(&url),
        };

        for (key, value) in headers {
            req = req.header(key, value);
        }

        if !body.is_empty() && !["GET", "HEAD"].contains(&method) {
            req = req.body(body.to_string());
        }

        match req.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let content_type = resp
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("text/html")
                    .to_string();
                let resp_headers: HashMap<String, String> = resp
                    .headers()
                    .iter()
                    .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), v.to_string())))
                    .collect();
                let body = resp.text().await.unwrap_or_default();

                TunnelResponse {
                    status,
                    headers: resp_headers,
                    body,
                    content_type,
                }
            }
            Err(e) => {
                error!("[Tunnel] Failed to forward request to port {}: {}", port, e);
                TunnelResponse {
                    status: 502,
                    headers: HashMap::new(),
                    body: format!("Failed to connect to local server on port {}: {}", port, e),
                    content_type: "text/plain".to_string(),
                }
            }
        }
    }

    /// Notify Rails that an agent's tunnel is ready
    pub async fn notify_agent_tunnel<S>(
        &self,
        write: &mut S,
        session_key: &str,
        port: u16,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        S: SinkExt<Message> + Unpin,
        S::Error: std::error::Error + Send + Sync + 'static,
    {
        let msg = serde_json::json!({
            "command": "message",
            "identifier": serde_json::json!({
                "channel": "TunnelChannel",
                "hub_id": self.hub_identifier
            }).to_string(),
            "data": serde_json::json!({
                "action": "register_agent_tunnel",
                "session_key": session_key,
                "port": port
            }).to_string()
        });
        write.send(Message::Text(msg.to_string().into())).await?;
        Ok(())
    }
}

struct TunnelResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
    content_type: String,
}
