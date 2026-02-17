//! WebSocket client primitives for Lua scripts.
//!
//! Provides persistent WebSocket connections with event-driven callbacks.
//! Each connection runs on a dedicated OS thread with a mini tokio runtime,
//! following the same pattern as `http.rs` for background work.
//!
//! # Lua API
//!
//! ```lua
//! -- Connect to a WebSocket server
//! local ws = websocket.connect("wss://example.com/ws", {
//!     headers = { ["Authorization"] = "Bearer token" },
//!     on_open = function() log.info("connected!") end,
//!     on_message = function(data) log.info("got: " .. data) end,
//!     on_close = function(code, reason) log.info("closed") end,
//!     on_error = function(err) log.error("ws error: " .. err) end,
//! })
//!
//! -- Send a text message
//! websocket.send(ws, "hello")
//!
//! -- Close the connection
//! websocket.close(ws)
//! ```
//!
//! # Threading Model
//!
//! Each `websocket.connect()` spawns one OS thread that owns a single-threaded
//! tokio runtime. The thread reads from the WebSocket and pushes `WsEvent`
//! values into the shared registry. It also listens on an `mpsc` channel for
//! outgoing messages (`send` / `close`). In production, background threads
//! send `HubEvent::WebSocketEvent` directly to the Hub event loop.
//!
//! # Deadlock Prevention
//!
//! Events are collected under the registry lock, then the lock is released
//! before invoking any Lua callbacks. This allows callbacks to call
//! `websocket.send()` or `websocket.close()` without deadlocking.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use mlua::{Lua, RegistryKey, Table, Value};
use tokio::sync::mpsc;

/// Maximum number of concurrent WebSocket connections.
/// Prevents thread exhaustion from rapid-fire `websocket.connect()` calls.
const MAX_CONCURRENT_CONNECTIONS: usize = 16;

// =============================================================================
// Registry types
// =============================================================================

/// Outgoing command from Lua to a WebSocket connection thread.
#[derive(Debug)]
enum WsOutgoing {
    /// Send a UTF-8 text frame.
    Text(String),
    /// Initiate a graceful close.
    Close,
}

/// An event produced by a background WebSocket thread.
///
/// Sent through `HubEvent::WebSocketEvent` for instant delivery (production)
/// or pushed to `pending_events` vec (tests without event channel).
pub(crate) struct WsEvent {
    /// Which connection produced this event.
    pub(crate) connection_id: String,
    /// The event payload.
    pub(crate) kind: WsEventKind,
}

impl std::fmt::Debug for WsEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WsEvent")
            .field("connection_id", &self.connection_id)
            .field("kind", &self.kind)
            .finish()
    }
}

/// Discriminant for WebSocket events delivered to Lua.
#[derive(Debug)]
pub(crate) enum WsEventKind {
    /// Connection successfully established.
    Open,
    /// A text message was received.
    Message(String),
    /// Connection closed (possibly by the remote).
    Close {
        /// WebSocket close code (1000 = normal).
        code: u16,
        /// Human-readable close reason.
        reason: String,
    },
    /// A transport or protocol error occurred.
    Error(String),
}

/// Per-connection state held in the registry.
pub(crate) struct WsConnectionState {
    /// Lua callback for `on_message` events.
    on_message_key: Option<RegistryKey>,
    /// Lua callback for `on_open` events.
    on_open_key: Option<RegistryKey>,
    /// Lua callback for `on_close` events.
    on_close_key: Option<RegistryKey>,
    /// Lua callback for `on_error` events.
    on_error_key: Option<RegistryKey>,
    /// Channel for sending outgoing messages to the background thread.
    send_tx: mpsc::UnboundedSender<WsOutgoing>,
}

/// Inner state of the WebSocket registry, protected by a mutex.
pub struct WebSocketRegistryInner {
    /// Active connections keyed by connection ID.
    pub(crate) connections: HashMap<String, WsConnectionState>,
    /// Events waiting to be delivered to Lua on the next tick (test-only fallback).
    pending_events: Vec<WsEvent>,
    /// Monotonic counter for generating unique connection IDs.
    next_id: u64,
    /// Event channel sender for instant delivery to the Hub event loop.
    /// `None` in tests that don't wire up the full event bus.
    hub_event_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::hub::events::HubEvent>>,
}

impl Default for WebSocketRegistryInner {
    fn default() -> Self {
        Self {
            connections: HashMap::new(),
            pending_events: Vec::new(),
            next_id: 0,
            hub_event_tx: None,
        }
    }
}

impl WebSocketRegistryInner {
    /// Number of active connections.
    #[must_use]
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    /// Number of events waiting to be delivered.
    #[must_use]
    pub fn pending_event_count(&self) -> usize {
        self.pending_events.len()
    }

    /// Set the Hub event channel sender for event-driven delivery.
    pub(crate) fn set_hub_event_tx(
        &mut self,
        tx: tokio::sync::mpsc::UnboundedSender<crate::hub::events::HubEvent>,
    ) {
        self.hub_event_tx = Some(tx);
    }

    /// Emit a WebSocket event through the event channel or shared vec.
    ///
    /// If `hub_event_tx` is set (production), sends via the channel for
    /// instant delivery. Otherwise falls back to the shared vec (tests).
    fn emit_event(&mut self, event: WsEvent) {
        if let Some(ref tx) = self.hub_event_tx {
            let _ = tx.send(crate::hub::events::HubEvent::WebSocketEvent(event));
        } else {
            self.pending_events.push(event);
        }
    }
}

impl std::fmt::Debug for WebSocketRegistryInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSocketRegistryInner")
            .field("connections", &self.connections.len())
            .field("pending_events", &self.pending_events.len())
            .field("next_id", &self.next_id)
            .finish()
    }
}

/// Thread-safe handle to the WebSocket registry.
pub type WebSocketRegistry = Arc<Mutex<WebSocketRegistryInner>>;

/// Create a new shared WebSocket registry.
#[must_use]
pub fn new_websocket_registry() -> WebSocketRegistry {
    Arc::new(Mutex::new(WebSocketRegistryInner::default()))
}

// =============================================================================
// Background thread
// =============================================================================

/// Run a single WebSocket connection on a dedicated tokio runtime.
///
/// Reads frames from the WebSocket and pushes `WsEvent` to the registry.
/// Listens on `send_rx` for outgoing messages from Lua. Exits when the
/// connection closes or an unrecoverable error occurs.
fn run_ws_thread(
    url: String,
    headers: Vec<(String, String)>,
    connection_id: String,
    registry: WebSocketRegistry,
    mut send_rx: mpsc::UnboundedReceiver<WsOutgoing>,
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let mut inner = registry.lock().expect("WebSocketRegistry mutex poisoned");
            inner.emit_event(WsEvent {
                connection_id,
                kind: WsEventKind::Error(format!("Failed to create tokio runtime: {e}")),
            });
            return;
        }
    };

    rt.block_on(async {
        let header_refs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let (mut writer, mut reader) = match crate::ws::connect(&url, &header_refs).await {
            Ok(pair) => pair,
            Err(e) => {
                let mut inner = registry.lock().expect("WebSocketRegistry mutex poisoned");
                inner.emit_event(WsEvent {
                    connection_id,
                    kind: WsEventKind::Error(format!("{e}")),
                });
                return;
            }
        };

        // Notify Lua that the connection is open
        {
            let mut inner = registry.lock().expect("WebSocketRegistry mutex poisoned");
            inner.emit_event(WsEvent {
                connection_id: connection_id.clone(),
                kind: WsEventKind::Open,
            });
        }

        loop {
            tokio::select! {
                // Incoming WebSocket frame
                frame = reader.recv() => {
                    match frame {
                        Some(Ok(crate::ws::WsMessage::Text(text))) => {
                            let mut inner = registry.lock().expect("WebSocketRegistry mutex poisoned");
                            inner.emit_event(WsEvent {
                                connection_id: connection_id.clone(),
                                kind: WsEventKind::Message(text),
                            });
                        }
                        Some(Ok(crate::ws::WsMessage::Binary(data))) => {
                            // Deliver binary as a lossy UTF-8 string for Lua compatibility
                            let text = String::from_utf8_lossy(&data).into_owned();
                            let mut inner = registry.lock().expect("WebSocketRegistry mutex poisoned");
                            inner.emit_event(WsEvent {
                                connection_id: connection_id.clone(),
                                kind: WsEventKind::Message(text),
                            });
                        }
                        Some(Ok(crate::ws::WsMessage::Close { code, reason })) => {
                            let mut inner = registry.lock().expect("WebSocketRegistry mutex poisoned");
                            inner.emit_event(WsEvent {
                                connection_id: connection_id.clone(),
                                kind: WsEventKind::Close { code, reason },
                            });
                            return;
                        }
                        Some(Ok(crate::ws::WsMessage::Ping(_) | crate::ws::WsMessage::Pong(_))) => {
                            // Pings are auto-replied by tungstenite; ignore pongs
                        }
                        Some(Err(e)) => {
                            let mut inner = registry.lock().expect("WebSocketRegistry mutex poisoned");
                            inner.emit_event(WsEvent {
                                connection_id: connection_id.clone(),
                                kind: WsEventKind::Error(format!("{e}")),
                            });
                            return;
                        }
                        None => {
                            // Stream ended without a Close frame
                            let mut inner = registry.lock().expect("WebSocketRegistry mutex poisoned");
                            inner.emit_event(WsEvent {
                                connection_id: connection_id.clone(),
                                kind: WsEventKind::Close { code: 1006, reason: "stream ended".to_string() },
                            });
                            return;
                        }
                    }
                }
                // Outgoing message from Lua
                outgoing = send_rx.recv() => {
                    match outgoing {
                        Some(WsOutgoing::Text(text)) => {
                            if let Err(e) = writer.send_text(&text).await {
                                let mut inner = registry.lock().expect("WebSocketRegistry mutex poisoned");
                                inner.emit_event(WsEvent {
                                    connection_id: connection_id.clone(),
                                    kind: WsEventKind::Error(format!("WebSocket send failed: {e}")),
                                });
                                return;
                            }
                        }
                        Some(WsOutgoing::Close) => {
                            let _ = writer.send_close().await;
                            let mut inner = registry.lock().expect("WebSocketRegistry mutex poisoned");
                            inner.emit_event(WsEvent {
                                connection_id: connection_id.clone(),
                                kind: WsEventKind::Close { code: 1000, reason: "client requested close".to_string() },
                            });
                            return;
                        }
                        None => {
                            // send_tx dropped — Lua side forgot about us
                            return;
                        }
                    }
                }
            }
        }
    });
}

// =============================================================================
// Event dispatch
// =============================================================================

/// Drain pending WebSocket events and fire Lua callbacks.
///
/// Called from the Hub tick loop each tick. For each event:
/// - Retrieves the matching connection's callback keys
/// - Fires the appropriate Lua callback (`on_open`, `on_message`, etc.)
/// - On `Close` or `Error`, removes the connection and cleans up registry keys
///
/// # Deadlock Prevention
///
/// Events and callback keys are collected under the lock, then the lock is
/// released before calling Lua. This allows callbacks to issue new
/// `websocket.connect()` / `websocket.send()` calls without deadlocking.
///
/// # Returns
///
/// The number of WebSocket events processed.
pub fn poll_websocket_events(lua: &Lua, registry: &WebSocketRegistry) -> usize {
    // Phase 1: drain events under the lock, collecting callback info
    let events_with_keys: Vec<(WsEvent, CallbackKeys)> = {
        let mut inner = registry.lock().expect("WebSocketRegistry mutex poisoned");

        if inner.pending_events.is_empty() {
            return 0;
        }

        let events: Vec<WsEvent> = inner.pending_events.drain(..).collect();
        let mut collected = Vec::with_capacity(events.len());

        for event in events {
            let is_terminal = matches!(
                event.kind,
                WsEventKind::Close { .. } | WsEventKind::Error(_)
            );

            if is_terminal {
                // Remove connection on terminal events; take ownership of keys
                if let Some(conn) = inner.connections.remove(&event.connection_id) {
                    collected.push((
                        event,
                        CallbackKeys {
                            on_open: conn.on_open_key,
                            on_message: conn.on_message_key,
                            on_close: conn.on_close_key,
                            on_error: conn.on_error_key,
                            owned: true,
                        },
                    ));
                } else {
                    log::warn!(
                        "[websocket] Terminal event for unknown connection: {}",
                        event.connection_id
                    );
                }
            } else if inner.connections.contains_key(&event.connection_id) {
                // Borrow callback keys (not owned, do not clean up)
                collected.push((
                    event,
                    CallbackKeys {
                        on_open: None,
                        on_message: None,
                        on_close: None,
                        on_error: None,
                        owned: false,
                    },
                ));
            } else {
                log::warn!(
                    "[websocket] Event for unknown connection: {}",
                    event.connection_id
                );
            }
        }

        collected
    };
    // Lock released here — callbacks can safely call websocket.send/connect/close.

    let count = events_with_keys.len();

    // Phase 2: fire callbacks without holding the lock
    for (event, keys) in &events_with_keys {
        let callback_result: mlua::Result<()> = (|| {
            match &event.kind {
                WsEventKind::Open => {
                    // Look up on_open key from the still-alive connection
                    let inner = registry.lock().expect("WebSocketRegistry mutex poisoned");
                    if let Some(conn) = inner.connections.get(&event.connection_id) {
                        if let Some(ref key) = conn.on_open_key {
                            let callback: mlua::Function = lua.registry_value(key)?;
                            drop(inner);
                            callback.call::<()>(())?;
                        }
                    }
                }
                WsEventKind::Message(data) => {
                    let inner = registry.lock().expect("WebSocketRegistry mutex poisoned");
                    if let Some(conn) = inner.connections.get(&event.connection_id) {
                        if let Some(ref key) = conn.on_message_key {
                            let callback: mlua::Function = lua.registry_value(key)?;
                            drop(inner);
                            callback.call::<()>(data.as_str())?;
                        }
                    }
                }
                WsEventKind::Close { code, reason } => {
                    // Connection already removed; use owned keys
                    if let Some(ref key) = keys.on_close {
                        let callback: mlua::Function = lua.registry_value(key)?;
                        callback.call::<()>((*code, reason.as_str()))?;
                    }
                }
                WsEventKind::Error(err) => {
                    if let Some(ref key) = keys.on_error {
                        let callback: mlua::Function = lua.registry_value(key)?;
                        callback.call::<()>(err.as_str())?;
                    }
                }
            }
            Ok(())
        })();

        if let Err(e) = callback_result {
            log::warn!(
                "[websocket] Callback error for {}: {e}",
                event.connection_id
            );
        }
    }

    // Phase 3: clean up owned registry keys from terminal events
    for (_event, keys) in events_with_keys {
        if keys.owned {
            if let Some(key) = keys.on_open {
                let _ = lua.remove_registry_value(key);
            }
            if let Some(key) = keys.on_message {
                let _ = lua.remove_registry_value(key);
            }
            if let Some(key) = keys.on_close {
                let _ = lua.remove_registry_value(key);
            }
            if let Some(key) = keys.on_error {
                let _ = lua.remove_registry_value(key);
            }
        }
    }

    count
}

/// Temporary holder for callback keys during event dispatch.
struct CallbackKeys {
    on_open: Option<RegistryKey>,
    on_message: Option<RegistryKey>,
    on_close: Option<RegistryKey>,
    on_error: Option<RegistryKey>,
    /// Whether these keys are owned (taken from a removed connection)
    /// and should be cleaned up after dispatch.
    owned: bool,
}

/// Fire the Lua callback for a single WebSocket event.
///
/// Called from `handle_hub_event()` when an `HubEvent::WebSocketEvent` arrives
/// via the event channel. Looks up the connection's callback keys, fires the
/// appropriate callback, and cleans up on terminal events.
///
/// This is the event-driven counterpart of [`poll_websocket_events`], which
/// batch-drains the shared vec.
///
/// # Deadlock Prevention
///
/// The registry lock is released before any Lua callback is invoked, matching
/// the same pattern used in [`poll_websocket_events`].
pub(crate) fn fire_single_websocket_event(
    lua: &Lua,
    registry: &WebSocketRegistry,
    event: WsEvent,
) {
    let is_terminal = matches!(
        event.kind,
        WsEventKind::Close { .. } | WsEventKind::Error(_)
    );

    if is_terminal {
        fire_terminal_ws_event(lua, registry, event);
    } else {
        fire_nonterminal_ws_event(lua, registry, event);
    }
}

/// Handle a non-terminal WebSocket event (Open, Message).
///
/// Locks the registry to resolve the Lua callback `Function` from the
/// stored `RegistryKey`, drops the lock, then fires the callback.
fn fire_nonterminal_ws_event(
    lua: &Lua,
    registry: &WebSocketRegistry,
    event: WsEvent,
) {
    // Resolve callback Function under the lock, then drop the lock.
    let callback_result: mlua::Result<()> = (|| {
        match &event.kind {
            WsEventKind::Open => {
                let inner = registry.lock().expect("WebSocketRegistry mutex poisoned");
                if let Some(conn) = inner.connections.get(&event.connection_id) {
                    if let Some(ref key) = conn.on_open_key {
                        let callback: mlua::Function = lua.registry_value(key)?;
                        drop(inner);
                        callback.call::<()>(())?;
                    }
                }
            }
            WsEventKind::Message(data) => {
                let inner = registry.lock().expect("WebSocketRegistry mutex poisoned");
                if let Some(conn) = inner.connections.get(&event.connection_id) {
                    if let Some(ref key) = conn.on_message_key {
                        let callback: mlua::Function = lua.registry_value(key)?;
                        drop(inner);
                        callback.call::<()>(data.as_str())?;
                    }
                }
            }
            _ => {} // Terminal events handled separately.
        }
        Ok(())
    })();

    if let Err(e) = callback_result {
        log::warn!(
            "[websocket] Event callback error for {}: {e}",
            event.connection_id
        );
    }
}

/// Handle a terminal WebSocket event (Close, Error).
///
/// Removes the connection under the lock (taking ownership of callback keys),
/// drops the lock, fires the callback, then cleans up all owned keys.
fn fire_terminal_ws_event(
    lua: &Lua,
    registry: &WebSocketRegistry,
    event: WsEvent,
) {
    // Phase 1: remove connection and take ownership of keys under the lock.
    let conn = {
        let mut inner = registry.lock().expect("WebSocketRegistry mutex poisoned");
        if let Some(conn) = inner.connections.remove(&event.connection_id) {
            conn
        } else {
            log::warn!(
                "[websocket] Terminal event for unknown connection: {}",
                event.connection_id
            );
            return;
        }
    };
    // Lock released.

    // Phase 2: fire the appropriate callback.
    let callback_result: mlua::Result<()> = (|| {
        match &event.kind {
            WsEventKind::Close { code, reason } => {
                if let Some(ref key) = conn.on_close_key {
                    let callback: mlua::Function = lua.registry_value(key)?;
                    callback.call::<()>((*code, reason.as_str()))?;
                }
            }
            WsEventKind::Error(err) => {
                if let Some(ref key) = conn.on_error_key {
                    let callback: mlua::Function = lua.registry_value(key)?;
                    callback.call::<()>(err.as_str())?;
                }
            }
            _ => {} // Non-terminal events handled separately.
        }
        Ok(())
    })();

    if let Err(e) = callback_result {
        log::warn!(
            "[websocket] Event callback error for {}: {e}",
            event.connection_id
        );
    }

    // Phase 3: clean up all owned callback keys.
    if let Some(key) = conn.on_open_key {
        let _ = lua.remove_registry_value(key);
    }
    if let Some(key) = conn.on_message_key {
        let _ = lua.remove_registry_value(key);
    }
    if let Some(key) = conn.on_close_key {
        let _ = lua.remove_registry_value(key);
    }
    if let Some(key) = conn.on_error_key {
        let _ = lua.remove_registry_value(key);
    }
}

// =============================================================================
// Lua registration
// =============================================================================

/// Register the `websocket` global table with connect/send/close functions.
///
/// Creates a global `websocket` table with methods:
/// - `websocket.connect(url, opts)` - Open a WebSocket connection
/// - `websocket.send(id, data)` - Send a text message
/// - `websocket.close(id)` - Close a connection
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub fn register(lua: &Lua, registry: WebSocketRegistry) -> Result<()> {
    let ws_table = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create websocket table: {e}"))?;

    // websocket.connect(url, opts) -> connection_id or (nil, error)
    let connect_registry = Arc::clone(&registry);
    let connect_fn = lua
        .create_function(
            move |lua, (url, opts): (String, Option<Table>)| {
                let mut headers = Vec::new();
                let mut on_open_key = None;
                let mut on_message_key = None;
                let mut on_close_key = None;
                let mut on_error_key = None;

                if let Some(ref opts) = opts {
                    // Parse headers
                    if let Ok(h) = opts.get::<Table>("headers") {
                        for pair in h.pairs::<String, String>() {
                            let (k, v) = pair?;
                            headers.push((k, v));
                        }
                    }

                    // Store callback functions in the Lua registry
                    if let Ok(f) = opts.get::<mlua::Function>("on_open") {
                        on_open_key =
                            Some(lua.create_registry_value(f).map_err(|e| {
                                mlua::Error::external(format!(
                                    "websocket.connect: failed to store on_open: {e}"
                                ))
                            })?);
                    }
                    if let Ok(f) = opts.get::<mlua::Function>("on_message") {
                        on_message_key =
                            Some(lua.create_registry_value(f).map_err(|e| {
                                mlua::Error::external(format!(
                                    "websocket.connect: failed to store on_message: {e}"
                                ))
                            })?);
                    }
                    if let Ok(f) = opts.get::<mlua::Function>("on_close") {
                        on_close_key =
                            Some(lua.create_registry_value(f).map_err(|e| {
                                mlua::Error::external(format!(
                                    "websocket.connect: failed to store on_close: {e}"
                                ))
                            })?);
                    }
                    if let Ok(f) = opts.get::<mlua::Function>("on_error") {
                        on_error_key =
                            Some(lua.create_registry_value(f).map_err(|e| {
                                mlua::Error::external(format!(
                                    "websocket.connect: failed to store on_error: {e}"
                                ))
                            })?);
                    }
                }

                let (send_tx, send_rx) = mpsc::unbounded_channel();

                // Allocate a connection ID and check the concurrency cap
                let connection_id = {
                    let mut inner =
                        connect_registry.lock().expect("WebSocketRegistry mutex poisoned");

                    if inner.connections.len() >= MAX_CONCURRENT_CONNECTIONS {
                        // Clean up stored callback keys
                        if let Some(k) = on_open_key { let _ = lua.remove_registry_value(k); }
                        if let Some(k) = on_message_key { let _ = lua.remove_registry_value(k); }
                        if let Some(k) = on_close_key { let _ = lua.remove_registry_value(k); }
                        if let Some(k) = on_error_key { let _ = lua.remove_registry_value(k); }
                        return Ok((
                            Value::Nil,
                            Some(format!(
                                "Too many concurrent WebSocket connections (limit: {MAX_CONCURRENT_CONNECTIONS})"
                            )),
                        ));
                    }

                    let id = format!("ws_{}", inner.next_id);
                    inner.next_id += 1;
                    inner.connections.insert(
                        id.clone(),
                        WsConnectionState {
                            on_message_key,
                            on_open_key,
                            on_close_key,
                            on_error_key,
                            send_tx,
                        },
                    );
                    id
                };

                // Spawn a background OS thread for this connection
                let thread_id = connection_id.clone();
                let thread_registry = Arc::clone(&connect_registry);
                let spawn_result = std::thread::Builder::new()
                    .name(format!("ws-{thread_id}"))
                    .spawn(move || {
                        run_ws_thread(url, headers, thread_id, thread_registry, send_rx);
                    });

                if let Err(e) = spawn_result {
                    // Roll back: remove the connection we just inserted
                    let mut inner =
                        connect_registry.lock().expect("WebSocketRegistry mutex poisoned");
                    if let Some(conn) = inner.connections.remove(&connection_id) {
                        if let Some(k) = conn.on_open_key { let _ = lua.remove_registry_value(k); }
                        if let Some(k) = conn.on_message_key { let _ = lua.remove_registry_value(k); }
                        if let Some(k) = conn.on_close_key { let _ = lua.remove_registry_value(k); }
                        if let Some(k) = conn.on_error_key { let _ = lua.remove_registry_value(k); }
                    }
                    return Ok((
                        Value::Nil,
                        Some(format!("Failed to spawn WebSocket thread: {e}")),
                    ));
                }

                Ok((Value::String(lua.create_string(&connection_id)?), None::<String>))
            },
        )
        .map_err(|e| anyhow!("Failed to create websocket.connect function: {e}"))?;

    ws_table
        .set("connect", connect_fn)
        .map_err(|e| anyhow!("Failed to set websocket.connect: {e}"))?;

    // websocket.send(connection_id, data) -> (true, nil) or (nil, error)
    let send_registry = Arc::clone(&registry);
    let send_fn = lua
        .create_function(move |_lua, (id, data): (String, String)| {
            let inner = send_registry.lock().expect("WebSocketRegistry mutex poisoned");
            if let Some(conn) = inner.connections.get(&id) {
                if conn.send_tx.send(WsOutgoing::Text(data)).is_err() {
                    Ok((Value::Nil, Some("WebSocket connection thread has exited".to_string())))
                } else {
                    Ok((Value::Boolean(true), None::<String>))
                }
            } else {
                Ok((Value::Nil, Some(format!("Unknown WebSocket connection: {id}"))))
            }
        })
        .map_err(|e| anyhow!("Failed to create websocket.send function: {e}"))?;

    ws_table
        .set("send", send_fn)
        .map_err(|e| anyhow!("Failed to set websocket.send: {e}"))?;

    // websocket.close(connection_id) -> (true, nil) or (nil, error)
    let close_registry = Arc::clone(&registry);
    let close_fn = lua
        .create_function(move |_lua, id: String| {
            let inner = close_registry.lock().expect("WebSocketRegistry mutex poisoned");
            if let Some(conn) = inner.connections.get(&id) {
                if conn.send_tx.send(WsOutgoing::Close).is_err() {
                    Ok((Value::Nil, Some("WebSocket connection thread has exited".to_string())))
                } else {
                    Ok((Value::Boolean(true), None::<String>))
                }
            } else {
                Ok((Value::Nil, Some(format!("Unknown WebSocket connection: {id}"))))
            }
        })
        .map_err(|e| anyhow!("Failed to create websocket.close function: {e}"))?;

    ws_table
        .set("close", close_fn)
        .map_err(|e| anyhow!("Failed to set websocket.close: {e}"))?;

    lua.globals()
        .set("websocket", ws_table)
        .map_err(|e| anyhow!("Failed to register websocket table globally: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_websocket_table_created() {
        let lua = Lua::new();
        let registry = new_websocket_registry();
        register(&lua, registry).expect("Should register websocket primitives");

        let globals = lua.globals();
        let ws_table: Table = globals.get("websocket").expect("websocket table should exist");

        let _: mlua::Function = ws_table.get("connect").expect("websocket.connect should exist");
        let _: mlua::Function = ws_table.get("send").expect("websocket.send should exist");
        let _: mlua::Function = ws_table.get("close").expect("websocket.close should exist");
    }

    #[test]
    fn test_send_unknown_connection_returns_error() {
        let lua = Lua::new();
        let registry = new_websocket_registry();
        register(&lua, registry).expect("Should register websocket primitives");

        let (result, err): (Value, Option<String>) = lua
            .load(r#"return websocket.send("ws_999", "hello")"#)
            .eval()
            .expect("websocket.send should be callable");

        assert_eq!(result, Value::Nil);
        assert!(err.is_some());
        assert!(
            err.unwrap().contains("Unknown WebSocket connection"),
            "Error should mention unknown connection"
        );
    }

    #[test]
    fn test_close_unknown_connection_returns_error() {
        let lua = Lua::new();
        let registry = new_websocket_registry();
        register(&lua, registry).expect("Should register websocket primitives");

        let (result, err): (Value, Option<String>) = lua
            .load(r#"return websocket.close("ws_999")"#)
            .eval()
            .expect("websocket.close should be callable");

        assert_eq!(result, Value::Nil);
        assert!(err.is_some());
        assert!(
            err.unwrap().contains("Unknown WebSocket connection"),
            "Error should mention unknown connection"
        );
    }

    #[test]
    fn test_connect_concurrency_cap() {
        let lua = Lua::new();
        let registry = new_websocket_registry();
        register(&lua, Arc::clone(&registry)).expect("Should register websocket primitives");

        // Artificially fill connections to the cap
        {
            let mut inner = registry.lock().unwrap();
            for i in 0..MAX_CONCURRENT_CONNECTIONS {
                let (tx, _rx) = mpsc::unbounded_channel();
                inner.connections.insert(
                    format!("ws_fake_{i}"),
                    WsConnectionState {
                        on_message_key: None,
                        on_open_key: None,
                        on_close_key: None,
                        on_error_key: None,
                        send_tx: tx,
                    },
                );
            }
        }

        // Next connect should be rejected
        let (id, err): (Value, Option<String>) = lua
            .load(r#"return websocket.connect("wss://example.com/ws", {})"#)
            .eval()
            .expect("websocket.connect should be callable");

        assert_eq!(id, Value::Nil, "Should not return an ID when at capacity");
        assert!(err.is_some(), "Should return an error");
        assert!(
            err.unwrap().contains("Too many concurrent"),
            "Error should mention concurrency limit"
        );
    }

    #[test]
    fn test_poll_no_events_returns_zero() {
        let lua = Lua::new();
        let registry = new_websocket_registry();
        register(&lua, Arc::clone(&registry)).expect("Should register websocket primitives");

        let fired = poll_websocket_events(&lua, &registry);
        assert_eq!(fired, 0);
    }

    #[test]
    fn test_connect_to_invalid_url_fires_error_callback() {
        let lua = Lua::new();
        let registry = new_websocket_registry();
        register(&lua, Arc::clone(&registry)).expect("Should register websocket primitives");

        // Set up globals to capture callback results
        lua.load(
            r#"
            _ws_error = nil
            websocket.connect("wss://127.0.0.1:1/invalid", {
                on_error = function(err) _ws_error = err end,
            })
            "#,
        )
        .exec()
        .expect("websocket.connect should be callable");

        // Wait for the background thread to push an error event
        let max_wait = std::time::Duration::from_secs(10);
        let start = std::time::Instant::now();
        loop {
            let has_events = {
                let inner = registry.lock().unwrap();
                !inner.pending_events.is_empty()
            };
            if has_events {
                break;
            }
            if start.elapsed() > max_wait {
                panic!("Background WebSocket thread did not produce an event in time");
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        let fired = poll_websocket_events(&lua, &registry);
        assert!(fired >= 1, "Should have fired at least 1 event");

        let err: Option<String> = lua
            .load(r#"return _ws_error"#)
            .eval()
            .expect("Should read _ws_error");
        assert!(err.is_some(), "Should have received an error callback");
    }

    #[test]
    fn test_registry_default_state() {
        let registry = new_websocket_registry();
        let inner = registry.lock().unwrap();
        assert_eq!(inner.connection_count(), 0);
        assert_eq!(inner.pending_event_count(), 0);
    }
}
