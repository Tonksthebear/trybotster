//! HTTP channel for preview proxying.
//!
//! `HttpChannel` handles E2E encrypted HTTP request/response communication
//! between a browser and an agent's dev server. It handles HTTP request/response
//! for dev server preview functionality.
//!
//! # Architecture
//!
//! ```text
//! HttpChannel
//!   ├── channel (ActionCableChannel subscribed to PreviewChannel, agent side)
//!   ├── http_proxy (HttpProxy configured with port from PtyHandle)
//!   ├── input_task (receives encrypted HTTP requests, proxies, sends responses)
//!   └── output_task (placeholder for future bidirectional needs)
//! ```
//!
//! # Message Flow
//!
//! ```text
//! Browser ── PreviewChannel ──> HttpChannel ──> HttpProxy ──> localhost:PORT
//!                                   │
//!                                   └── Decrypt request
//!                                   └── Proxy to dev server
//!                                   └── Encrypt response
//!                                   └── Send back via channel
//! ```
//!
//! # Security
//!
//! - All HTTP traffic is E2E encrypted via Signal Protocol
//! - Rails server cannot inspect HTTP content (blind relay)
//! - HttpProxy only forwards to localhost (agent's own server)

// Rust guideline compliant 2026-01

use tokio::task::JoinHandle;

use crate::channel::{
    ActionCableChannel, Channel, ChannelConfig, ChannelReceiverHandle, ChannelSenderHandle, PeerId,
};
use crate::hub::agent_handle::PtyHandle;
use crate::relay::http_proxy::HttpProxy;
use crate::relay::preview_types::{PreviewCommand, PreviewMessage, ProxyConfig};

use crate::relay::crypto_service::CryptoServiceHandle;

/// Configuration needed for ActionCable channel connections.
///
/// Used by HttpChannel for preview proxying. Contains the connection
/// details needed to establish ActionCable channels with the server.
#[derive(Debug, Clone)]
pub struct HttpChannelConfig {
    /// Crypto service handle for E2E encryption.
    pub crypto_service: CryptoServiceHandle,
    /// Server URL for ActionCable WebSocket connections.
    pub server_url: String,
    /// API key for authentication.
    pub api_key: String,
    /// Server-assigned hub ID for channel routing.
    pub server_hub_id: String,
}

/// HTTP channel for preview proxying.
///
/// Handles HTTP request/response for dev server preview functionality.
/// Owns an `ActionCableChannel` subscribed to the `PreviewChannel` (agent side)
/// and an `HttpProxy` configured with the port from `PtyHandle`.
///
/// # Lifecycle
///
/// Created when handling `HubEvent::HttpConnectionRequested`.
/// Dropped when the browser disconnects or the agent is deleted, which aborts
/// the background tasks and disconnects the channel.
#[derive(Debug)]
pub struct HttpChannel {
    /// ActionCable channel subscribed to PreviewChannel (agent side).
    ///
    /// Provides E2E encrypted communication with the browser. The channel
    /// subscribes without `browser_identity` to indicate this is the agent
    /// (CLI) side of the preview tunnel.
    #[expect(dead_code, reason = "Channel held for lifetime, tasks use handles")]
    channel: ActionCableChannel,

    /// HTTP proxy configured with port from PtyHandle.
    ///
    /// Forwards decrypted HTTP requests to localhost:PORT and returns responses.
    #[expect(dead_code, reason = "Proxy used by input_task, kept for Debug")]
    http_proxy: HttpProxy,

    /// Task handling incoming HTTP requests from browser.
    ///
    /// Receives encrypted requests via the channel, decrypts them, proxies
    /// to the dev server, encrypts the response, and sends it back.
    /// Aborted on drop.
    input_task: JoinHandle<()>,

    /// Task for sending responses back to browser.
    ///
    /// Currently a placeholder that completes immediately. Future versions
    /// may use this for server-initiated messages (e.g., WebSocket upgrade).
    /// Aborted on drop.
    output_task: JoinHandle<()>,
}

impl HttpChannel {
    /// Create a new HttpChannel for a specific agent/pty combo.
    ///
    /// Queries `pty_handle` for the port, creates an `HttpProxy`, subscribes
    /// to the `PreviewChannel` (agent side), and spawns background tasks for
    /// request/response handling.
    ///
    /// # Arguments
    ///
    /// * `agent_index` - Index of the agent in the Hub's ordered list
    /// * `pty_index` - Index of the PTY within the agent (typically 1 for server PTY)
    /// * `pty_handle` - Handle to query for the HTTP forwarding port
    /// * `config` - Connection config for ActionCable channels
    /// * `browser_identity` - Browser's Signal identity key for routing
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The PTY has no port assigned (`pty_handle.port()` returns `None`)
    /// - The ActionCable channel fails to connect
    pub async fn new(
        agent_index: usize,
        pty_index: usize,
        pty_handle: &PtyHandle,
        config: &HttpChannelConfig,
        browser_identity: String,
    ) -> Result<Self, String> {
        // Query the port from PtyHandle
        let port = pty_handle
            .port()
            .ok_or_else(|| format!("PTY ({}, {}) has no port assigned", agent_index, pty_index))?;

        log::info!(
            "HttpChannel: creating for agent {} pty {} on port {}",
            agent_index,
            pty_index,
            port
        );

        // Create HttpProxy with the port
        let http_proxy = HttpProxy::with_config(ProxyConfig {
            port,
            ..Default::default()
        });

        // Create ActionCableChannel with E2E encryption and reliable delivery
        let mut channel = ActionCableChannel::builder()
            .server_url(&config.server_url)
            .api_key(&config.api_key)
            .crypto_service(config.crypto_service.clone())
            .reliable(true)
            .cli_subscription(true) // Agent side of preview stream
            .build();

        // Connect to PreviewChannel (agent side - with browser_identity but cli_subscription=true)
        // This subscribes to: preview:{hub}:{agent}:{pty}:{browser}:cli
        channel
            .connect(ChannelConfig {
                channel_name: "PreviewChannel".into(),
                hub_id: config.server_hub_id.clone(),
                agent_index: Some(agent_index),
                pty_index: Some(pty_index),
                browser_identity: Some(browser_identity.clone()),
                encrypt: true,
                compression_threshold: Some(4096),
                cli_subscription: true,
            })
            .await
            .map_err(|e| format!("Failed to connect preview channel: {}", e))?;

        // Get sender and receiver handles
        let sender_handle = channel
            .get_sender_handle()
            .ok_or_else(|| "Failed to get channel sender handle".to_string())?;
        let receiver_handle = channel
            .take_receiver_handle()
            .ok_or_else(|| "Failed to get channel receiver handle".to_string())?;

        // Pre-register the browser as a peer
        sender_handle.register_peer(PeerId(browser_identity.clone()));

        // Create a new proxy for the input task (proxy is not Clone, so create fresh)
        let task_proxy = HttpProxy::with_config(ProxyConfig {
            port,
            ..Default::default()
        });

        // Spawn input task: receives encrypted requests, proxies, sends encrypted responses
        let input_task = tokio::spawn(spawn_http_request_handler(
            receiver_handle,
            sender_handle.clone(),
            task_proxy,
            browser_identity.clone(),
            agent_index,
            pty_index,
        ));

        // Send "ready" message so browser knows we're listening
        // This prevents message loss from requests sent before we subscribed
        let ready_msg = PreviewMessage::Ready;
        if let Ok(json) = serde_json::to_string(&ready_msg) {
            if let Err(e) = sender_handle.send(json.as_bytes()).await {
                log::warn!("HttpChannel: failed to send ready message: {}", e);
            } else {
                log::info!("HttpChannel: sent preview_ready to browser");
            }
        }

        // Output task is a placeholder - no server-initiated messages yet
        let output_task = tokio::spawn(async {
            // Future: handle WebSocket upgrade or server push
        });

        log::info!(
            "HttpChannel: created for browser {} agent {} pty {} port {}",
            &browser_identity[..8.min(browser_identity.len())],
            agent_index,
            pty_index,
            port
        );

        Ok(Self {
            channel,
            http_proxy,
            input_task,
            output_task,
        })
    }
}

impl Drop for HttpChannel {
    fn drop(&mut self) {
        self.input_task.abort();
        self.output_task.abort();
    }
}

/// Background task that handles incoming HTTP requests from the browser.
///
/// Receives encrypted `PreviewCommand` messages via the channel, decrypts them,
/// proxies `HttpRequest` commands to the dev server via `HttpProxy`, encrypts
/// the response, and sends it back through the channel.
///
/// Exits when the channel closes or an unrecoverable error occurs.
async fn spawn_http_request_handler(
    mut receiver: ChannelReceiverHandle,
    sender: ChannelSenderHandle,
    proxy: HttpProxy,
    browser_identity: String,
    agent_index: usize,
    pty_index: usize,
) {
    log::info!(
        "Started HTTP request handler for browser {} agent {} pty {}",
        &browser_identity[..8.min(browser_identity.len())],
        agent_index,
        pty_index
    );

    while let Some(incoming) = receiver.recv().await {
        // Parse the incoming payload as JSON PreviewCommand
        let payload_str = match String::from_utf8(incoming.payload.clone()) {
            Ok(s) => s,
            Err(e) => {
                log::warn!(
                    "HttpChannel: non-UTF8 payload from browser {}: {}",
                    &browser_identity[..8.min(browser_identity.len())],
                    e
                );
                continue;
            }
        };

        let command: PreviewCommand = match serde_json::from_str(&payload_str) {
            Ok(cmd) => cmd,
            Err(e) => {
                log::warn!(
                    "HttpChannel: failed to parse preview command from browser {}: {}",
                    &browser_identity[..8.min(browser_identity.len())],
                    e
                );
                continue;
            }
        };

        match command {
            PreviewCommand::HttpRequest(request) => {
                let request_id = request.request_id;

                log::debug!(
                    "HttpChannel: proxying {} {} (request_id={})",
                    request.method,
                    request.url,
                    request_id
                );

                // Proxy the request to the dev server
                let result = proxy.proxy(&request).await;

                // Convert result to PreviewMessage
                let message = result.into_message(request_id);

                // Serialize and send response
                match serde_json::to_string(&message) {
                    Ok(json) => {
                        if let Err(e) = sender.send(json.as_bytes()).await {
                            log::debug!(
                                "HttpChannel: failed to send response (channel closed?): {}",
                                e
                            );
                            break;
                        }
                    }
                    Err(e) => {
                        log::error!("HttpChannel: failed to serialize response: {}", e);
                    }
                }
            }
            PreviewCommand::GetStatus => {
                log::debug!("HttpChannel: received GetStatus request");

                // Respond with status (server is assumed running if we have a port)
                let message = PreviewMessage::Status {
                    server_running: true,
                    port: Some(proxy.port()),
                };

                match serde_json::to_string(&message) {
                    Ok(json) => {
                        if let Err(e) = sender.send(json.as_bytes()).await {
                            log::debug!(
                                "HttpChannel: failed to send status (channel closed?): {}",
                                e
                            );
                            break;
                        }
                    }
                    Err(e) => {
                        log::error!("HttpChannel: failed to serialize status: {}", e);
                    }
                }
            }
        }
    }

    log::info!(
        "Stopped HTTP request handler for browser {} agent {} pty {}",
        &browser_identity[..8.min(browser_identity.len())],
        agent_index,
        pty_index
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_channel_debug() {
        // HttpChannel requires async construction, but we can test the Debug derive
        // by inspecting that the struct fields are defined correctly.
        // This is a compile-time check that Debug is derived.
        fn assert_debug<T: std::fmt::Debug>() {}
        assert_debug::<HttpChannel>();
    }

    #[test]
    fn test_preview_command_parsing() {
        // Test that we can parse PreviewCommand correctly
        let json = r#"{"type": "http_request", "request_id": 1, "method": "GET", "url": "/"}"#;
        let cmd: PreviewCommand = serde_json::from_str(json).unwrap();
        assert!(matches!(cmd, PreviewCommand::HttpRequest(_)));

        let json = r#"{"type": "get_status"}"#;
        let cmd: PreviewCommand = serde_json::from_str(json).unwrap();
        assert!(matches!(cmd, PreviewCommand::GetStatus));
    }

    #[test]
    fn test_preview_message_serialization() {
        // Test that we can serialize PreviewMessage correctly
        let msg = PreviewMessage::Status {
            server_running: true,
            port: Some(3000),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"preview_status""#));
        assert!(json.contains(r#""server_running":true"#));
        assert!(json.contains(r#""port":3000"#));
    }
}
