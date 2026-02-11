//! Multi-subscription ActionCable WebSocket connection.
//!
//! Provides a shared WebSocket connection to Rails ActionCable that supports
//! multiple concurrent channel subscriptions. Each subscription gets its own
//! [`ChannelHandle`] for non-blocking message reception and action invocation.
//!
//! # Architecture
//!
//! ```text
//!   ActionCableConnection        ChannelHandle (HubCommandChannel)
//!         │                              │
//!         │  subscribe(identifier)       │  try_recv() → Value
//!         │ ──────────────────────────►  │  perform("ack", data)
//!         │                              │
//!         │                      ChannelHandle (Github::EventsChannel)
//!         │                              │
//!         │  subscribe(identifier)       │  try_recv() → Value
//!         │ ──────────────────────────►  │  perform("ack", data)
//!         │                              │
//!         ▼                              │
//!   Background WebSocket task            │
//!   (reconnect, route, ping)             │
//! ```
//!
//! # Protocol
//!
//! - Connects via WebSocket to ActionCable with Bearer token auth
//! - Supports runtime `subscribe()` calls that register new channels
//! - Routes incoming messages to the correct [`ChannelHandle`] by `identifier`
//! - On reconnect, re-subscribes all active channels automatically
//! - Handles welcome, confirm_subscription, reject_subscription, ping, disconnect

// Rust guideline compliant 2026-02

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Deserialize;
use tokio::sync::mpsc;

/// How long to wait for a `confirm_subscription` response before re-sending
/// the subscribe command. Covers transient timing races where the confirmation
/// is lost between the re-subscribe loop and the message loop.
const CONFIRM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// How often to check for unconfirmed subscriptions that have timed out.
const CONFIRM_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Message received from the hub command channel.
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

/// Shared ActionCable WebSocket connection.
///
/// Owns a background task that manages the WebSocket lifecycle (connect,
/// reconnect, ping/pong). Supports multiple concurrent channel subscriptions
/// via [`Self::subscribe`].
#[derive(Debug)]
pub struct ActionCableConnection {
    subscribe_tx: mpsc::UnboundedSender<SubscribeRequest>,
    perform_tx: mpsc::UnboundedSender<ChannelPerform>,
    shutdown: Arc<AtomicBool>,
}

/// Handle for a single channel subscription.
///
/// Provides non-blocking message reception and action invocation for one
/// ActionCable channel. Messages arrive as raw `serde_json::Value` -- the
/// consumer is responsible for parsing into domain types.
pub struct ChannelHandle {
    message_rx: mpsc::Receiver<serde_json::Value>,
    identifier: String,
    perform_tx: mpsc::UnboundedSender<ChannelPerform>,
}

impl std::fmt::Debug for ChannelHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelHandle")
            .field("identifier", &self.identifier)
            .finish_non_exhaustive()
    }
}

/// Outgoing action request for a specific channel.
#[derive(Debug)]
struct ChannelPerform {
    identifier: String,
    action: String,
    data: serde_json::Value,
}

/// Request to subscribe to a new channel at runtime.
#[derive(Debug)]
struct SubscribeRequest {
    identifier: String,
    message_tx: mpsc::Sender<serde_json::Value>,
}

impl ActionCableConnection {
    /// Connect to ActionCable and spawn the background WebSocket task.
    ///
    /// The connection authenticates with a Bearer token and automatically
    /// reconnects with exponential backoff on disconnection. All active
    /// channel subscriptions are re-established after reconnection.
    #[must_use]
    pub fn connect(server_url: &str, api_key: &str) -> Self {
        let (subscribe_tx, subscribe_rx) = mpsc::unbounded_channel();
        let (perform_tx, perform_rx) = mpsc::unbounded_channel();
        let shutdown = Arc::new(AtomicBool::new(false));

        let config = ConnectionConfig {
            server_url: server_url.to_string(),
            api_key: api_key.to_string(),
            shutdown: Arc::clone(&shutdown),
        };

        tokio::spawn(run_connection_loop(config, subscribe_rx, perform_rx));

        Self {
            subscribe_tx,
            perform_tx,
            shutdown,
        }
    }

    /// Subscribe to an ActionCable channel.
    ///
    /// Sends a subscribe command to the WebSocket and returns a
    /// [`ChannelHandle`] for receiving messages and performing actions.
    /// The `identifier` is a JSON value matching ActionCable's channel
    /// identifier format (e.g., `{"channel": "FooChannel", "id": 1}`).
    #[must_use]
    pub fn subscribe(&self, identifier: serde_json::Value) -> ChannelHandle {
        // Buffer of 256 messages per channel before backpressure
        let (message_tx, message_rx) = mpsc::channel(256);
        let identifier_str = identifier.to_string();

        let request = SubscribeRequest {
            identifier: identifier_str.clone(),
            message_tx,
        };

        // Send subscribe request to background task (fire-and-forget)
        let _ = self.subscribe_tx.send(request);

        ChannelHandle {
            message_rx,
            identifier: identifier_str,
            perform_tx: self.perform_tx.clone(),
        }
    }

    /// Shut down the connection and background task.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

impl Drop for ActionCableConnection {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl ChannelHandle {
    /// Try to receive the next message (non-blocking).
    ///
    /// Returns `None` if no messages are pending.
    pub fn try_recv(&mut self) -> Option<serde_json::Value> {
        self.message_rx.try_recv().ok()
    }

    /// Send an ActionCable perform action on this channel.
    ///
    /// Queues the action for the background WebSocket task to send.
    pub fn perform(&self, action: &str, data: serde_json::Value) {
        let _ = self.perform_tx.send(ChannelPerform {
            identifier: self.identifier.clone(),
            action: action.to_string(),
            data,
        });
    }

    /// Get the channel identifier string.
    #[must_use]
    pub fn identifier(&self) -> &str {
        &self.identifier
    }
}

/// Internal configuration for the connection loop.
struct ConnectionConfig {
    server_url: String,
    api_key: String,
    shutdown: Arc<AtomicBool>,
}

/// Build the WebSocket URL from the server URL.
///
/// Converts `https://` to `wss://` and `http://` to `ws://`, then appends `/cable`.
fn build_ws_url(server_url: &str) -> String {
    format!("{}/cable", crate::ws::http_to_ws_scheme(server_url))
}

/// Main connection loop with reconnection and multi-subscription routing.
///
/// Owns the WebSocket and routes messages to subscribed channels by matching
/// the `identifier` field in ActionCable frames. On reconnect, re-subscribes
/// all active channels.
async fn run_connection_loop(
    config: ConnectionConfig,
    mut subscribe_rx: mpsc::UnboundedReceiver<SubscribeRequest>,
    mut perform_rx: mpsc::UnboundedReceiver<ChannelPerform>,
) {
    /// Initial reconnection delay in seconds.
    const INITIAL_BACKOFF_SECS: u64 = 1;
    /// Maximum reconnection delay in seconds.
    const MAX_BACKOFF_SECS: u64 = 60;

    let mut backoff_secs: u64 = INITIAL_BACKOFF_SECS;

    // Active subscriptions: identifier -> message sender
    // Persisted across reconnections for automatic re-subscribe
    let mut subscriptions: HashMap<String, mpsc::Sender<serde_json::Value>> = HashMap::new();

    loop {
        if config.shutdown.load(Ordering::SeqCst) {
            log::info!("[ActionCable] Shutdown requested, exiting connection loop");
            break;
        }

        // Drain any pending subscribe requests before connecting
        while let Ok(req) = subscribe_rx.try_recv() {
            subscriptions.insert(req.identifier, req.message_tx);
        }

        let ws_url = build_ws_url(&config.server_url);

        log::info!("[ActionCable] Connecting to {}", ws_url);

        let bearer = format!("Bearer {}", config.api_key);
        let (mut writer, mut reader) = match crate::ws::connect(
            &ws_url,
            &[("Authorization", &bearer)],
        )
        .await
        {
            Ok(pair) => {
                log::info!("[ActionCable] WebSocket connected");
                backoff_secs = INITIAL_BACKOFF_SECS;
                pair
            }
            Err(e) => {
                log::warn!(
                    "[ActionCable] Connection failed: {} (retry in {}s)",
                    e,
                    backoff_secs
                );
                tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                continue;
            }
        };

        // Wait for ActionCable welcome message
        if !wait_for_welcome(&mut writer, &mut reader).await {
            log::warn!("[ActionCable] Did not receive welcome, reconnecting...");
            tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
            backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
            continue;
        }

        // Re-subscribe all active channels after (re)connect
        for identifier in subscriptions.keys() {
            let subscribe_cmd = serde_json::json!({
                "command": "subscribe",
                "identifier": identifier
            });
            if let Err(e) = writer.send_text(&subscribe_cmd.to_string()).await {
                log::error!(
                    "[ActionCable] Failed to send subscribe for {}: {}",
                    identifier,
                    e
                );
            } else {
                log::debug!("[ActionCable] Sent subscribe for channel");
            }
        }

        // Run the main message loop for this connection
        let loop_result = run_message_loop(
            &config,
            &mut subscriptions,
            &mut writer,
            &mut reader,
            &mut subscribe_rx,
            &mut perform_rx,
        )
        .await;

        if let ConnectionLoopExit::Shutdown = loop_result {
            return;
        }

        // Disconnected -- will reconnect after backoff
        log::info!(
            "[ActionCable] Disconnected, reconnecting in {}s",
            backoff_secs
        );
        tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
    }
}

/// Result of the inner message loop.
enum ConnectionLoopExit {
    /// Shutdown was requested.
    Shutdown,
    /// Connection was lost -- should reconnect.
    Disconnected,
}

/// Wait for the ActionCable welcome message after connecting.
///
/// Returns `true` if welcome was received, `false` on error or unexpected close.
async fn wait_for_welcome(
    writer: &mut crate::ws::WsWriter,
    reader: &mut crate::ws::WsReader,
) -> bool {
    while let Some(msg_result) = reader.recv().await {
        match msg_result {
            Ok(crate::ws::WsMessage::Text(text)) => {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    if json.get("type").and_then(|t| t.as_str()) == Some("welcome") {
                        log::debug!("[ActionCable] Received welcome");
                        return true;
                    }
                }
            }
            Ok(crate::ws::WsMessage::Ping(data)) => {
                let _ = writer.send_pong(data).await;
            }
            Err(e) => {
                log::warn!("[ActionCable] Error waiting for welcome: {}", e);
                return false;
            }
            _ => {}
        }
    }

    false
}

/// Inner message loop for a single WebSocket connection.
///
/// Routes incoming messages to subscribed channels, processes outgoing perform
/// requests, and handles runtime subscribe requests. Returns when the connection
/// is lost or shutdown is requested.
///
/// # Confirmation Timeout
///
/// If a subscription is not confirmed within [`CONFIRM_TIMEOUT`], the subscribe
/// command is re-sent. This handles edge cases where a confirmation message is
/// lost due to timing races between the re-subscribe loop and the message loop.
#[expect(
    clippy::too_many_arguments,
    reason = "connection loop needs all channel endpoints"
)]
async fn run_message_loop(
    config: &ConnectionConfig,
    subscriptions: &mut HashMap<String, mpsc::Sender<serde_json::Value>>,
    writer: &mut crate::ws::WsWriter,
    reader: &mut crate::ws::WsReader,
    subscribe_rx: &mut mpsc::UnboundedReceiver<SubscribeRequest>,
    perform_rx: &mut mpsc::UnboundedReceiver<ChannelPerform>,
) -> ConnectionLoopExit {
    // Track which channels have been confirmed by the server
    let mut confirmed: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Track when each subscription was sent, for confirmation timeout.
    // Entries are removed once confirmed. Re-subscribe fires if the entry
    // exceeds CONFIRM_TIMEOUT without confirmation.
    let mut pending_confirm: HashMap<String, tokio::time::Instant> = subscriptions
        .keys()
        .map(|id| (id.clone(), tokio::time::Instant::now()))
        .collect();

    // Tick interval for checking confirmation timeouts.
    // Uses a moderate interval so we don't spin — the timeout itself is what
    // matters, not sub-second precision on the retry.
    let mut confirm_check = tokio::time::interval(CONFIRM_CHECK_INTERVAL);
    confirm_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        if config.shutdown.load(Ordering::SeqCst) {
            log::info!("[ActionCable] Shutdown requested, closing connection");
            let _ = writer.close().await;
            return ConnectionLoopExit::Shutdown;
        }

        tokio::select! {
            // Receive from WebSocket
            msg = reader.recv() => {
                match msg {
                    Some(Ok(crate::ws::WsMessage::Text(text))) => {
                        match handle_text_message(&text, subscriptions, &mut confirmed, &mut pending_confirm).await {
                            TextMessageResult::Continue => {}
                            TextMessageResult::Disconnected => return ConnectionLoopExit::Disconnected,
                        }
                    }
                    Some(Ok(crate::ws::WsMessage::Ping(data))) => {
                        let _ = writer.send_pong(data).await;
                    }
                    Some(Ok(crate::ws::WsMessage::Close { .. })) => {
                        log::info!("[ActionCable] Connection closed by server");
                        return ConnectionLoopExit::Disconnected;
                    }
                    Some(Err(e)) => {
                        log::warn!("[ActionCable] WebSocket error: {}", e);
                        return ConnectionLoopExit::Disconnected;
                    }
                    None => {
                        log::info!("[ActionCable] WebSocket stream ended");
                        return ConnectionLoopExit::Disconnected;
                    }
                    _ => {}
                }
            }

            // Process runtime subscribe requests
            Some(req) = subscribe_rx.recv() => {
                let identifier = req.identifier.clone();
                subscriptions.insert(req.identifier, req.message_tx);

                // Send subscribe command to WebSocket
                let subscribe_cmd = serde_json::json!({
                    "command": "subscribe",
                    "identifier": identifier
                });
                if let Err(e) = writer.send_text(&subscribe_cmd.to_string()).await {
                    log::warn!("[ActionCable] Failed to send subscribe: {}", e);
                    return ConnectionLoopExit::Disconnected;
                }
                log::debug!("[ActionCable] Sent runtime subscribe command");
                pending_confirm.insert(identifier, tokio::time::Instant::now());
            }

            // Process outgoing perform requests
            Some(request) = perform_rx.recv() => {
                if confirmed.contains(&request.identifier) {
                    // Build data object: merge action name with caller-provided fields.
                    // ActionCable expects data as a JSON string containing action + fields.
                    let mut data_obj = serde_json::json!({
                        "action": request.action,
                    });
                    if let serde_json::Value::Object(map) = request.data {
                        for (k, v) in map {
                            data_obj[&k] = v;
                        }
                    }

                    let perform_cmd = serde_json::json!({
                        "command": "message",
                        "identifier": request.identifier,
                        "data": data_obj.to_string()
                    });

                    if let Err(e) = writer.send_text(&perform_cmd.to_string()).await {
                        log::warn!("[ActionCable] Failed to send perform '{}': {}", request.action, e);
                        return ConnectionLoopExit::Disconnected;
                    }
                    log::trace!("[ActionCable] Sent perform '{}'", request.action);
                } else {
                    log::debug!(
                        "[ActionCable] Dropping perform '{}' -- channel not yet confirmed (identifier={}, confirmed={:?})",
                        request.action,
                        &request.identifier[..request.identifier.len().min(80)],
                        confirmed
                    );
                }
            }

            // Check for unconfirmed subscriptions that have timed out
            _ = confirm_check.tick() => {
                let now = tokio::time::Instant::now();
                let timed_out: Vec<String> = pending_confirm
                    .iter()
                    .filter(|(_, sent_at)| now.duration_since(**sent_at) >= CONFIRM_TIMEOUT)
                    .map(|(id, _)| id.clone())
                    .collect();

                for identifier in timed_out {
                    log::warn!(
                        "[ActionCable] Subscription not confirmed after {}s, re-sending subscribe (identifier={})",
                        CONFIRM_TIMEOUT.as_secs(),
                        &identifier[..identifier.len().min(80)]
                    );

                    let subscribe_cmd = serde_json::json!({
                        "command": "subscribe",
                        "identifier": identifier
                    });
                    if let Err(e) = writer.send_text(&subscribe_cmd.to_string()).await {
                        log::warn!("[ActionCable] Failed to re-send subscribe: {}", e);
                        return ConnectionLoopExit::Disconnected;
                    }

                    // Reset the timer for this subscription
                    pending_confirm.insert(identifier, tokio::time::Instant::now());
                }
            }
        }
    }
}

/// Result of processing a text message from the WebSocket.
enum TextMessageResult {
    /// Continue the message loop.
    Continue,
    /// Connection should be dropped (reconnect).
    Disconnected,
}

/// Handle an incoming ActionCable text message.
///
/// Routes data messages to the appropriate channel by matching the `identifier`
/// field. Handles subscription confirmations, rejections, pings, and disconnects.
/// Clears the `pending_confirm` timer on successful confirmation.
async fn handle_text_message(
    text: &str,
    subscriptions: &mut HashMap<String, mpsc::Sender<serde_json::Value>>,
    confirmed: &mut std::collections::HashSet<String>,
    pending_confirm: &mut HashMap<String, tokio::time::Instant>,
) -> TextMessageResult {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        log::warn!("[ActionCable] Failed to parse message as JSON: {}", &text[..text.len().min(100)]);
        return TextMessageResult::Continue;
    };

    let msg_type = json.get("type").and_then(|t| t.as_str());
    log::debug!(
        "[ActionCable] Received message type={:?} identifier={}",
        msg_type,
        json.get("identifier")
            .and_then(|i| i.as_str())
            .map(|s| &s[..s.len().min(60)])
            .unwrap_or("none")
    );

    match msg_type {
        Some("confirm_subscription") => {
            if let Some(identifier) = json.get("identifier").and_then(|i| i.as_str()) {
                confirmed.insert(identifier.to_string());
                pending_confirm.remove(identifier);
                log::info!(
                    "[ActionCable] Subscription confirmed for channel (identifier={})",
                    &identifier[..identifier.len().min(80)]
                );
            } else {
                log::warn!(
                    "[ActionCable] Subscription confirmed but no identifier field in: {}",
                    &text[..text.len().min(200)]
                );
            }
            TextMessageResult::Continue
        }
        Some("reject_subscription") => {
            if let Some(identifier) = json.get("identifier").and_then(|i| i.as_str()) {
                log::error!(
                    "[ActionCable] Subscription rejected for channel: {}",
                    identifier
                );
                subscriptions.remove(identifier);
                pending_confirm.remove(identifier);
            }
            TextMessageResult::Continue
        }
        Some("ping") => {
            // ActionCable server-side ping -- no response needed
            TextMessageResult::Continue
        }
        Some("disconnect") => {
            log::warn!("[ActionCable] Server requested disconnect");
            TextMessageResult::Disconnected
        }
        _ if json.get("message").is_some() => {
            // Data message -- route to correct channel by identifier
            if let (Some(identifier), Some(message)) = (
                json.get("identifier").and_then(|i| i.as_str()),
                json.get("message"),
            ) {
                if let Some(tx) = subscriptions.get(identifier) {
                    if tx.send(message.clone()).await.is_err() {
                        log::warn!(
                            "[ActionCable] Channel receiver dropped, removing subscription"
                        );
                        subscriptions.remove(identifier);
                        confirmed.remove(identifier);
                    }
                } else {
                    log::trace!(
                        "[ActionCable] Message for unknown channel: {}",
                        identifier
                    );
                }
            }
            TextMessageResult::Continue
        }
        _ => {
            log::trace!("[ActionCable] Unhandled message: {}", text);
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
    fn test_channel_handle_identifier() {
        let (perform_tx, _perform_rx) = mpsc::unbounded_channel();
        let (_message_tx, message_rx) = mpsc::channel(16);

        let identifier = serde_json::json!({
            "channel": "HubCommandChannel",
            "hub_id": "test-hub",
            "start_from": 0
        });

        let handle = ChannelHandle {
            message_rx,
            identifier: identifier.to_string(),
            perform_tx,
        };

        let parsed: serde_json::Value =
            serde_json::from_str(handle.identifier()).expect("valid JSON identifier");
        assert_eq!(parsed["channel"], "HubCommandChannel");
        assert_eq!(parsed["hub_id"], "test-hub");
    }

    #[test]
    fn test_subscribe_request_format() {
        let identifier = serde_json::json!({
            "channel": "Github::EventsChannel",
            "repo": "owner/repo"
        });

        let identifier_str = identifier.to_string();

        // Verify the subscribe command JSON format
        let subscribe_cmd = serde_json::json!({
            "command": "subscribe",
            "identifier": identifier_str
        });

        assert_eq!(subscribe_cmd["command"], "subscribe");
        // The identifier should be a string (ActionCable expects stringified JSON)
        assert!(subscribe_cmd["identifier"].is_string());

        // Parse the identifier back to verify it round-trips
        let parsed_id: serde_json::Value =
            serde_json::from_str(subscribe_cmd["identifier"].as_str().unwrap())
                .expect("identifier should be valid JSON string");
        assert_eq!(parsed_id["channel"], "Github::EventsChannel");
        assert_eq!(parsed_id["repo"], "owner/repo");
    }

    #[test]
    fn test_perform_command_format() {
        let identifier = serde_json::json!({
            "channel": "HubCommandChannel",
            "hub_id": "test-hub"
        })
        .to_string();

        // Simulate what the message loop builds for a perform
        let action = "ack";
        let data = serde_json::json!({ "sequence": 42 });

        let mut data_obj = serde_json::json!({ "action": action });
        if let serde_json::Value::Object(map) = data {
            for (k, v) in map {
                data_obj[&k] = v;
            }
        }

        let perform_cmd = serde_json::json!({
            "command": "message",
            "identifier": identifier,
            "data": data_obj.to_string()
        });

        assert_eq!(perform_cmd["command"], "message");
        assert!(perform_cmd["identifier"].is_string());

        // Parse the data string back
        let data_parsed: serde_json::Value =
            serde_json::from_str(perform_cmd["data"].as_str().unwrap())
                .expect("data should be valid JSON string");
        assert_eq!(data_parsed["action"], "ack");
        assert_eq!(data_parsed["sequence"], 42);
    }
}
