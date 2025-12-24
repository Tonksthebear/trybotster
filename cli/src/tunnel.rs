use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite::Message};

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

/// Manages tunnel connections for all agents on this hub
pub struct TunnelManager {
    hub_identifier: String,
    api_key: String,
    server_url: String,
    // Map of session_key -> allocated port
    agent_ports: Arc<Mutex<HashMap<String, u16>>>,
}

impl TunnelManager {
    pub fn new(hub_identifier: String, api_key: String, server_url: String) -> Self {
        Self {
            hub_identifier,
            api_key,
            server_url,
            agent_ports: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register an agent's tunnel port
    pub async fn register_agent(&self, session_key: String, port: u16) {
        let mut ports = self.agent_ports.lock().await;
        ports.insert(session_key, port);
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

        eprintln!("[Tunnel] Connecting to {}", ws_url);

        let (ws_stream, _) = connect_async(&ws_url).await?;
        let (mut write, mut read) = ws_stream.split();

        // Subscribe to tunnel channel for this hub
        let subscribe_msg = serde_json::json!({
            "command": "subscribe",
            "identifier": serde_json::json!({
                "channel": "TunnelChannel",
                "hub_id": self.hub_identifier
            }).to_string()
        });
        write
            .send(Message::Text(subscribe_msg.to_string().into()))
            .await?;

        eprintln!("[Tunnel] Subscribed to TunnelChannel for hub {}", self.hub_identifier);

        // Handle incoming HTTP request messages
        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Err(e) = self
                        .handle_message(&text.to_string(), &mut write)
                        .await
                    {
                        eprintln!("[Tunnel] Message error: {}", e);
                    }
                }
                Ok(Message::Ping(data)) => {
                    if let Err(e) = write.send(Message::Pong(data)).await {
                        eprintln!("[Tunnel] Failed to send pong: {}", e);
                    }
                }
                Ok(Message::Close(_)) => {
                    eprintln!("[Tunnel] Connection closed by server");
                    break;
                }
                Err(e) => {
                    eprintln!("[Tunnel] WebSocket error: {}", e);
                    break;
                }
                _ => {}
            }
        }

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
                    eprintln!("[Tunnel] ActionCable connection established");
                }
                "confirm_subscription" => {
                    eprintln!("[Tunnel] Subscription confirmed");
                }
                "disconnect" => {
                    eprintln!("[Tunnel] Disconnected by server");
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

                eprintln!(
                    "[Tunnel] HTTP request for agent {}: {}",
                    agent_session_key,
                    message["path"].as_str().unwrap_or("/")
                );

                // Find the port for this agent
                let port = match self.get_agent_port(agent_session_key).await {
                    Some(p) => p,
                    None => {
                        eprintln!("[Tunnel] Agent {} not registered", agent_session_key);
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
            Err(e) => TunnelResponse {
                status: 502,
                headers: HashMap::new(),
                body: format!("Failed to connect to local server on port {}: {}", port, e),
                content_type: "text/plain".to_string(),
            },
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
