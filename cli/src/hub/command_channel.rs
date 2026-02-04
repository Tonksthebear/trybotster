//! Command channel client for reliable message delivery from Rails.
//!
//! Implements a lightweight ActionCable WebSocket client for the HubCommandChannel.
//! This is a plaintext channel (no encryption) — separate from the E2E encrypted
//! HubChannel used for browser relay.
//!
//! # Protocol
//!
//! - CLI connects via WebSocket to ActionCable
//! - Subscribes to HubCommandChannel with hub_id and start_from sequence
//! - Receives messages in order with sequence numbers
//! - Acknowledges each message via perform("ack")
//! - Sends periodic heartbeat via perform("heartbeat")
//!
//! # Reconnection
//!
//! On disconnect, reconnects with start_from set to last_acked_sequence.
//! Rails replays unacked messages, ensuring no message loss.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

use serde::Deserialize;
use tokio::sync::mpsc;

/// Message received from the command channel.
#[derive(Debug, Clone, Deserialize)]
pub struct CommandMessage {
    /// Per-hub sequence number for ordering and replay.
    pub sequence: i64,
    /// Database ID for the message.
    pub id: i64,
    /// Event type (e.g., "github_mention", "browser_connected").
    pub event_type: String,
    /// Event payload (JSON object).
    pub payload: serde_json::Value,
    /// When the message was created.
    pub created_at: Option<String>,
}

/// Signal envelope received from browser via Rails ActionCable relay.
///
/// Rails is a dumb pipe — it relays opaque envelopes without inspecting content.
/// The CLI decrypts the envelope to discover the signal type (offer, ice, etc.).
#[derive(Debug, Clone, Deserialize)]
pub struct SignalMessage {
    /// Browser tab identity (`identityKey:tabId`).
    pub browser_identity: String,
    /// Opaque encrypted envelope from browser.
    pub envelope: serde_json::Value,
}

/// An outgoing ActionCable perform request.
///
/// Sent from the main thread to the background WebSocket task to execute
/// an ActionCable channel action (e.g., `signal` on `HubCommandChannel`).
#[derive(Debug)]
pub struct PerformRequest {
    /// ActionCable action name (e.g., "signal").
    pub action: String,
    /// Action data (serialized as JSON string in the perform command).
    pub data: serde_json::Value,
}

/// Handle for interacting with the command channel from the main thread.
///
/// Provides non-blocking message reception, acknowledgment, heartbeat,
/// and shutdown control. Dropping the handle triggers automatic shutdown
/// of the background connection task.
pub struct CommandChannelHandle {
    /// Receiver for incoming messages.
    message_rx: mpsc::Receiver<CommandMessage>,
    /// Receiver for incoming signal envelopes (from browser via Rails relay).
    signal_rx: mpsc::Receiver<SignalMessage>,
    /// Last acknowledged sequence (shared with background task for reconnection).
    last_acked_sequence: Arc<AtomicI64>,
    /// Sender for ack requests to the background task.
    ack_tx: mpsc::UnboundedSender<i64>,
    /// Sender for heartbeat data to the background task.
    heartbeat_tx: mpsc::UnboundedSender<serde_json::Value>,
    /// Sender for outgoing ActionCable perform commands.
    perform_tx: mpsc::UnboundedSender<PerformRequest>,
    /// Shutdown flag.
    shutdown: Arc<AtomicBool>,
}

impl std::fmt::Debug for CommandChannelHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandChannelHandle")
            .field(
                "last_acked_sequence",
                &self.last_acked_sequence.load(Ordering::SeqCst),
            )
            .field("shutdown", &self.shutdown.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl CommandChannelHandle {
    /// Try to receive the next message (non-blocking).
    pub fn try_recv(&mut self) -> Option<CommandMessage> {
        self.message_rx.try_recv().ok()
    }

    /// Acknowledge a message by sequence number.
    pub fn acknowledge(&self, sequence: i64) {
        self.last_acked_sequence.store(sequence, Ordering::SeqCst);
        let _ = self.ack_tx.send(sequence);
    }

    /// Try to receive the next signal message (non-blocking).
    pub fn try_recv_signal(&mut self) -> Option<SignalMessage> {
        self.signal_rx.try_recv().ok()
    }

    /// Send heartbeat data (agent list).
    pub fn send_heartbeat(&self, agents: serde_json::Value) {
        let _ = self.heartbeat_tx.send(agents);
    }

    /// Send an ActionCable perform command to the channel.
    ///
    /// Used to relay encrypted signal envelopes back to browsers via Rails.
    pub fn perform(&self, action: &str, data: serde_json::Value) {
        let _ = self.perform_tx.send(PerformRequest {
            action: action.to_string(),
            data,
        });
    }

    /// Shut down the command channel.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

impl Drop for CommandChannelHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Connect to the HubCommandChannel and return a handle for interacting with it.
///
/// Spawns a background tokio task that:
/// 1. Connects to the ActionCable WebSocket endpoint
/// 2. Subscribes to HubCommandChannel with the given hub_id
/// 3. Forwards received messages to the returned handle
/// 4. Processes ack and heartbeat requests from the handle
/// 5. Reconnects on disconnection with last_acked_sequence
///
/// # Arguments
///
/// * `server_url` - Rails server URL (e.g., "<https://trybotster.com>")
/// * `api_key` - DeviceToken for Bearer authentication
/// * `hub_id` - Hub identifier for channel subscription
/// * `start_from` - Initial sequence number to start from
pub fn connect(
    server_url: &str,
    api_key: &str,
    hub_id: &str,
    start_from: i64,
) -> CommandChannelHandle {
    let (message_tx, message_rx) = mpsc::channel(256);
    let (signal_tx, signal_rx) = mpsc::channel(64);
    let (ack_tx, ack_rx) = mpsc::unbounded_channel();
    let (heartbeat_tx, heartbeat_rx) = mpsc::unbounded_channel();
    let (perform_tx, perform_rx) = mpsc::unbounded_channel();
    let last_acked_sequence = Arc::new(AtomicI64::new(start_from));
    let shutdown = Arc::new(AtomicBool::new(false));

    let config = ConnectionConfig {
        server_url: server_url.to_string(),
        api_key: api_key.to_string(),
        hub_id: hub_id.to_string(),
        last_acked_sequence: Arc::clone(&last_acked_sequence),
        shutdown: Arc::clone(&shutdown),
    };

    tokio::spawn(run_connection_loop(
        config, message_tx, signal_tx, ack_rx, heartbeat_rx, perform_rx,
    ));

    CommandChannelHandle {
        message_rx,
        signal_rx,
        last_acked_sequence,
        ack_tx,
        heartbeat_tx,
        perform_tx,
        shutdown,
    }
}

/// Internal configuration for the connection loop.
struct ConnectionConfig {
    /// Rails server URL.
    server_url: String,
    /// Bearer token for authentication.
    api_key: String,
    /// Hub identifier for channel subscription.
    hub_id: String,
    /// Shared last-acked sequence for reconnection replay.
    last_acked_sequence: Arc<AtomicI64>,
    /// Shared shutdown flag.
    shutdown: Arc<AtomicBool>,
}

/// Build the WebSocket URL from the server URL.
///
/// Converts `https://` to `wss://` and `http://` to `ws://`, then appends `/cable`.
fn build_ws_url(server_url: &str) -> String {
    format!(
        "{}/cable",
        server_url
            .replace("https://", "wss://")
            .replace("http://", "ws://")
    )
}

/// Build the ActionCable channel identifier JSON.
fn channel_identifier(hub_id: &str, start_from: i64) -> String {
    serde_json::json!({
        "channel": "HubCommandChannel",
        "hub_id": hub_id,
        "start_from": start_from
    })
    .to_string()
}

/// Main connection loop with reconnection logic.
async fn run_connection_loop(
    config: ConnectionConfig,
    message_tx: mpsc::Sender<CommandMessage>,
    signal_tx: mpsc::Sender<SignalMessage>,
    mut ack_rx: mpsc::UnboundedReceiver<i64>,
    mut heartbeat_rx: mpsc::UnboundedReceiver<serde_json::Value>,
    mut perform_rx: mpsc::UnboundedReceiver<PerformRequest>,
) {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite;
    use tungstenite::client::IntoClientRequest;

    let mut backoff_secs: u64 = 1;
    const MAX_BACKOFF_SECS: u64 = 60;

    loop {
        if config.shutdown.load(Ordering::SeqCst) {
            log::info!("[CommandChannel] Shutdown requested, exiting connection loop");
            break;
        }

        let start_from = config.last_acked_sequence.load(Ordering::SeqCst);
        let ws_url = build_ws_url(&config.server_url);

        log::info!(
            "[CommandChannel] Connecting to {} (start_from={})",
            ws_url,
            start_from
        );

        // Build request with Bearer token auth using IntoClientRequest
        let request = match ws_url.into_client_request() {
            Ok(mut req) => {
                req.headers_mut().insert(
                    "Authorization",
                    format!("Bearer {}", config.api_key)
                        .parse()
                        .expect("valid header"),
                );
                req
            }
            Err(e) => {
                log::error!("[CommandChannel] Failed to build WebSocket request: {}", e);
                tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                continue;
            }
        };

        let ws_stream = match tokio_tungstenite::connect_async(request).await {
            Ok((stream, _)) => {
                log::info!("[CommandChannel] WebSocket connected");
                backoff_secs = 1; // Reset backoff on successful connect
                stream
            }
            Err(e) => {
                log::warn!(
                    "[CommandChannel] Connection failed: {} (retry in {}s)",
                    e,
                    backoff_secs
                );
                tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                continue;
            }
        };

        let (mut ws_sink, mut ws_stream_rx) = ws_stream.split();

        // Wait for ActionCable welcome message
        let welcomed = wait_for_welcome(&mut ws_sink, &mut ws_stream_rx).await;

        if !welcomed {
            log::warn!("[CommandChannel] Did not receive welcome, reconnecting...");
            tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
            backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
            continue;
        }

        // Subscribe to HubCommandChannel
        let identifier = channel_identifier(&config.hub_id, start_from);
        let subscribe_cmd = serde_json::json!({
            "command": "subscribe",
            "identifier": identifier
        });

        if let Err(e) = ws_sink
            .send(tungstenite::Message::Text(subscribe_cmd.to_string()))
            .await
        {
            log::error!("[CommandChannel] Failed to send subscribe: {}", e);
            continue;
        }

        log::debug!("[CommandChannel] Sent subscribe command");

        // Run the main message loop for this connection
        let loop_result = run_message_loop(
            &config,
            &identifier,
            &mut ws_sink,
            &mut ws_stream_rx,
            &message_tx,
            &signal_tx,
            &mut ack_rx,
            &mut heartbeat_rx,
            &mut perform_rx,
        )
        .await;

        if let ConnectionLoopExit::Shutdown = loop_result {
            return;
        }

        // Disconnected -- will reconnect after backoff
        log::info!(
            "[CommandChannel] Disconnected, reconnecting in {}s (last_acked={})",
            backoff_secs,
            config.last_acked_sequence.load(Ordering::SeqCst)
        );
        tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
    }
}

/// Result of the inner message loop.
enum ConnectionLoopExit {
    /// Shutdown was requested -- exit entirely.
    Shutdown,
    /// Connection was lost -- should reconnect.
    Disconnected,
}

/// Wait for the ActionCable welcome message after connecting.
///
/// Returns `true` if welcome was received, `false` on error or unexpected close.
async fn wait_for_welcome<S, St>(ws_sink: &mut S, ws_stream_rx: &mut St) -> bool
where
    S: futures_util::Sink<tokio_tungstenite::tungstenite::Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin,
    St: futures_util::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite;

    while let Some(msg_result) = ws_stream_rx.next().await {
        match msg_result {
            Ok(tungstenite::Message::Text(text)) => {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    if json.get("type").and_then(|t| t.as_str()) == Some("welcome") {
                        log::debug!("[CommandChannel] Received welcome");
                        return true;
                    }
                }
            }
            Ok(tungstenite::Message::Ping(data)) => {
                let _ = ws_sink.send(tungstenite::Message::Pong(data)).await;
            }
            Err(e) => {
                log::warn!("[CommandChannel] Error waiting for welcome: {}", e);
                return false;
            }
            _ => {}
        }
    }

    false
}

/// Inner message loop for a single WebSocket connection.
///
/// Handles incoming channel messages, ack/heartbeat sends, and WebSocket pings.
/// Returns when the connection is lost or shutdown is requested.
#[allow(clippy::too_many_arguments)]
async fn run_message_loop<S, St>(
    config: &ConnectionConfig,
    identifier: &str,
    ws_sink: &mut S,
    ws_stream_rx: &mut St,
    message_tx: &mpsc::Sender<CommandMessage>,
    signal_tx: &mpsc::Sender<SignalMessage>,
    ack_rx: &mut mpsc::UnboundedReceiver<i64>,
    heartbeat_rx: &mut mpsc::UnboundedReceiver<serde_json::Value>,
    perform_rx: &mut mpsc::UnboundedReceiver<PerformRequest>,
) -> ConnectionLoopExit
where
    S: futures_util::Sink<tokio_tungstenite::tungstenite::Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin,
    St: futures_util::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite;

    let mut subscribed = false;

    loop {
        if config.shutdown.load(Ordering::SeqCst) {
            log::info!("[CommandChannel] Shutdown requested, closing connection");
            let _ = ws_sink.close().await;
            return ConnectionLoopExit::Shutdown;
        }

        tokio::select! {
            // Receive from WebSocket
            msg = ws_stream_rx.next() => {
                match msg {
                    Some(Ok(tungstenite::Message::Text(text))) => {
                        match handle_text_message(&text, &mut subscribed, message_tx, signal_tx).await {
                            TextMessageResult::Continue => {}
                            TextMessageResult::Break => return ConnectionLoopExit::Disconnected,
                            TextMessageResult::ReceiverDropped => return ConnectionLoopExit::Shutdown,
                        }
                    }
                    Some(Ok(tungstenite::Message::Ping(data))) => {
                        let _ = ws_sink.send(tungstenite::Message::Pong(data)).await;
                    }
                    Some(Ok(tungstenite::Message::Close(_))) => {
                        log::info!("[CommandChannel] Connection closed by server");
                        return ConnectionLoopExit::Disconnected;
                    }
                    Some(Err(e)) => {
                        log::warn!("[CommandChannel] WebSocket error: {}", e);
                        return ConnectionLoopExit::Disconnected;
                    }
                    None => {
                        log::info!("[CommandChannel] WebSocket stream ended");
                        return ConnectionLoopExit::Disconnected;
                    }
                    _ => {}
                }
            }

            // Process ack requests
            Some(sequence) = ack_rx.recv() => {
                if subscribed {
                    let perform_cmd = serde_json::json!({
                        "command": "message",
                        "identifier": identifier,
                        "data": serde_json::json!({
                            "action": "ack",
                            "sequence": sequence
                        }).to_string()
                    });
                    if let Err(e) = ws_sink.send(tungstenite::Message::Text(perform_cmd.to_string())).await {
                        log::warn!("[CommandChannel] Failed to send ack: {}", e);
                        return ConnectionLoopExit::Disconnected;
                    }
                }
            }

            // Process heartbeat requests
            Some(agents) = heartbeat_rx.recv() => {
                if subscribed {
                    let perform_cmd = serde_json::json!({
                        "command": "message",
                        "identifier": identifier,
                        "data": serde_json::json!({
                            "action": "heartbeat",
                            "agents": agents
                        }).to_string()
                    });
                    if let Err(e) = ws_sink.send(tungstenite::Message::Text(perform_cmd.to_string())).await {
                        log::warn!("[CommandChannel] Failed to send heartbeat: {}", e);
                        return ConnectionLoopExit::Disconnected;
                    }
                }
            }

            // Process outgoing perform requests (e.g., signal relay to browser)
            Some(request) = perform_rx.recv() => {
                if subscribed {
                    let perform_cmd = serde_json::json!({
                        "command": "message",
                        "identifier": identifier,
                        "data": serde_json::json!({
                            "action": request.action,
                            "browser_identity": request.data["browser_identity"],
                            "envelope": request.data["envelope"],
                        }).to_string()
                    });
                    if let Err(e) = ws_sink.send(tungstenite::Message::Text(perform_cmd.to_string())).await {
                        log::warn!("[CommandChannel] Failed to send perform '{}': {}", request.action, e);
                        return ConnectionLoopExit::Disconnected;
                    }
                    log::debug!("[CommandChannel] Sent perform '{}'", request.action);
                }
            }
        }
    }
}

/// Result of processing a text message from the WebSocket.
enum TextMessageResult {
    /// Continue the message loop.
    Continue,
    /// Break out of the message loop (reconnect).
    Break,
    /// The message receiver was dropped (shutdown).
    ReceiverDropped,
}

/// Handle an incoming ActionCable text message.
///
/// Dispatches based on message type: subscription confirmations, pings,
/// disconnects, and data messages from the channel.
async fn handle_text_message(
    text: &str,
    subscribed: &mut bool,
    message_tx: &mpsc::Sender<CommandMessage>,
    signal_tx: &mpsc::Sender<SignalMessage>,
) -> TextMessageResult {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return TextMessageResult::Continue;
    };

    let msg_type = json.get("type").and_then(|t| t.as_str());

    match msg_type {
        Some("confirm_subscription") => {
            *subscribed = true;
            log::info!("[CommandChannel] Subscription confirmed");
            TextMessageResult::Continue
        }
        Some("reject_subscription") => {
            log::error!("[CommandChannel] Subscription rejected");
            TextMessageResult::Break
        }
        Some("ping") => {
            // ActionCable ping -- no response needed
            TextMessageResult::Continue
        }
        Some("disconnect") => {
            log::warn!("[CommandChannel] Server requested disconnect");
            TextMessageResult::Break
        }
        _ if json.get("message").is_some() && *subscribed => {
            // Data message from channel
            if let Some(message) = json.get("message") {
                let msg_type = message.get("type").and_then(|t| t.as_str());

                match msg_type {
                    Some("message") => {
                        // Command channel message (Bot::Message)
                        match serde_json::from_value::<CommandMessage>(message.clone()) {
                            Ok(cmd_msg) => {
                                log::info!("[CommandChannel] Received message seq={} type={}", cmd_msg.sequence, cmd_msg.event_type);
                                if message_tx.send(cmd_msg).await.is_err() {
                                    log::warn!("[CommandChannel] Message receiver dropped");
                                    return TextMessageResult::ReceiverDropped;
                                }
                            }
                            Err(e) => {
                                log::warn!("[CommandChannel] Failed to parse message: {}", e);
                            }
                        }
                    }
                    Some("signal") => {
                        // Encrypted signal envelope from browser (relayed by Rails)
                        match serde_json::from_value::<SignalMessage>(message.clone()) {
                            Ok(signal_msg) => {
                                log::debug!(
                                    "[CommandChannel] Received signal from browser={}",
                                    signal_msg.browser_identity
                                );
                                if signal_tx.send(signal_msg).await.is_err() {
                                    log::warn!("[CommandChannel] Signal receiver dropped");
                                    return TextMessageResult::ReceiverDropped;
                                }
                            }
                            Err(e) => {
                                log::warn!("[CommandChannel] Failed to parse signal: {}", e);
                            }
                        }
                    }
                    _ => {
                        log::trace!("[CommandChannel] Unknown message type: {:?}", msg_type);
                    }
                }
            }
            TextMessageResult::Continue
        }
        _ => {
            log::trace!("[CommandChannel] Unhandled message: {}", text);
            TextMessageResult::Continue
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_ws_url_https() {
        let url = build_ws_url("https://trybotster.com");
        assert_eq!(url, "wss://trybotster.com/cable");
    }

    #[test]
    fn test_build_ws_url_http() {
        let url = build_ws_url("http://localhost:3000");
        assert_eq!(url, "ws://localhost:3000/cable");
    }

    #[test]
    fn test_channel_identifier() {
        let id = channel_identifier("hub-123", 42);
        let parsed: serde_json::Value = serde_json::from_str(&id).expect("valid JSON");
        assert_eq!(parsed["channel"], "HubCommandChannel");
        assert_eq!(parsed["hub_id"], "hub-123");
        assert_eq!(parsed["start_from"], 42);
    }

    #[test]
    fn test_command_message_deserialize() {
        let json = serde_json::json!({
            "type": "message",
            "sequence": 5,
            "id": 42,
            "event_type": "github_mention",
            "payload": { "repo": "owner/repo", "issue_number": 123 },
            "created_at": "2026-01-27T12:00:00Z"
        });

        let msg: CommandMessage = serde_json::from_value(json).expect("valid CommandMessage");
        assert_eq!(msg.sequence, 5);
        assert_eq!(msg.id, 42);
        assert_eq!(msg.event_type, "github_mention");
        assert_eq!(msg.payload["repo"], "owner/repo");
    }

    #[test]
    fn test_signal_message_deserialize() {
        let json = serde_json::json!({
            "type": "signal",
            "browser_identity": "abc123key:tab42",
            "envelope": { "t": 3, "c": "encrypted_blob", "s": "sender_key", "d": 1 }
        });

        let msg: SignalMessage = serde_json::from_value(json).expect("valid SignalMessage");
        assert_eq!(msg.browser_identity, "abc123key:tab42");
        assert_eq!(msg.envelope["t"], 3);
        assert_eq!(msg.envelope["c"], "encrypted_blob");
    }

    #[test]
    fn test_command_message_deserialize_without_optional_fields() {
        let json = serde_json::json!({
            "sequence": 1,
            "id": 1,
            "event_type": "browser_connected",
            "payload": { "browser_identity": "abc123" }
        });

        let msg: CommandMessage = serde_json::from_value(json).expect("valid CommandMessage");
        assert_eq!(msg.sequence, 1);
        assert_eq!(msg.event_type, "browser_connected");
        assert!(msg.created_at.is_none());
    }
}
