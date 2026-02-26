//! Hub client Lua primitives for connecting to remote hubs via Unix sockets.
//!
//! Exposes outgoing hub-to-hub socket connections to Lua scripts via the
//! event-driven `HubEvent` channel. Lua closures send
//! `HubEvent::LuaHubClientRequest` directly to the Hub event loop, which
//! processes connect/send/close operations. Incoming JSON frames arrive via
//! `HubEvent::HubClientMessage` from per-connection read tasks.
//!
//! # Architecture
//!
//! ```text
//! Lua script                    Hub event loop
//!     │                              │
//!     │ hub_client.connect()        │
//!     │ ──── HubEvent ──────────►   │ process_hub_client_request()
//!     │                              │   → connects UnixStream, spawns R/W tasks
//!     │ hub_client.send()           │
//!     │ ──── HubEvent ──────────►   │   → writes frame via mpsc
//!     │                              │
//!     │                              │ HubEvent::HubClientMessage
//!     │   ◄──────────────────────   │   → fire_hub_client_message()
//!     │   callback(message)         │
//!     │                              │
//!     │ hub_client.close()          │
//!     │ ──── HubEvent ──────────►   │   → aborts tasks, drops conn
//! ```
//!
//! # Usage in Lua
//!
//! ```lua
//! -- Connect to another hub's Unix socket
//! local conn = hub_client.connect("/tmp/botster-other.sock")
//!
//! -- Register callback for incoming JSON messages
//! hub_client.on_message(conn, function(message, connection_id)
//!     log.info("Got: " .. json.encode(message))
//! end)
//!
//! -- Send a JSON message to the remote hub
//! hub_client.send(conn, { type = "ping" })
//!
//! -- Disconnect
//! hub_client.close(conn)
//! ```

// Rust guideline compliant 2026-02

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use mlua::{Lua, LuaSerdeExt, Value};

use super::HubEventSender;
use crate::hub::events::HubEvent;

// =============================================================================
// Callback registry (Lua-thread-pinned, shared with Hub)
// =============================================================================

/// Thread-safe registry mapping connection IDs to Lua callback keys.
///
/// Callbacks are stored here at `on_message` time (in the Lua closure) and
/// looked up by connection ID when messages arrive. This follows the same
/// pattern as ActionCable, HTTP, Timer, WebSocket, and Watch registries —
/// the `RegistryKey` stays pinned to the Lua thread while only `Send`-safe
/// string IDs cross the `HubEvent` channel.
pub type HubClientCallbackRegistry = Arc<Mutex<HashMap<String, mlua::RegistryKey>>>;

/// Create a new empty callback registry.
#[must_use]
pub fn new_hub_client_callback_registry() -> HubClientCallbackRegistry {
    Arc::new(Mutex::new(HashMap::new()))
}

// =============================================================================
// Pending requests (blocking request/response, shared with Hub)
// =============================================================================

/// Thread-safe map of correlation IDs to one-shot response channels.
///
/// When `hub_client.request()` is called, it stores a `SyncSender` here keyed
/// by a unique correlation ID. The read task checks incoming messages for the
/// `_mcp_rid` field and delivers the response directly through this channel,
/// unblocking the Lua caller without going through the Hub event loop.
pub type HubClientPendingRequests =
    Arc<Mutex<HashMap<String, std::sync::mpsc::SyncSender<serde_json::Value>>>>;

/// Create a new empty pending requests map.
#[must_use]
pub fn new_hub_client_pending_requests() -> HubClientPendingRequests {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Thread-safe map of connection IDs to frame senders.
///
/// Allows `hub_client.request()` to write frames directly to the connection's
/// write task without going through the Hub event loop. This is required because
/// `request()` blocks the Hub event loop thread via `recv_timeout()` — the event
/// loop cannot process `HubClientRequest::Send` while Lua is blocked.
///
/// Populated by Hub when a connection is established, cleared on close/disconnect.
pub type HubClientFrameSenders =
    Arc<Mutex<HashMap<String, tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>;

/// Create a new empty frame senders map.
#[must_use]
pub fn new_hub_client_frame_senders() -> HubClientFrameSenders {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Global counter for generating unique correlation IDs.
static REQUEST_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

// =============================================================================
// Request types (Lua -> Hub via HubEvent channel)
// =============================================================================

/// Request from Lua to the Hub for hub client operations.
///
/// Sent directly via `HubEvent::LuaHubClientRequest` from Lua closures
/// to the Hub event loop. All variants are `Send`-safe — callback keys are
/// stored separately in [`HubClientCallbackRegistry`].
#[derive(Debug)]
pub enum HubClientRequest {
    /// Connect to a remote hub's Unix socket.
    Connect {
        /// Unique connection identifier (e.g., "hc_conn_0").
        connection_id: String,
        /// Path to the remote hub's Unix socket.
        socket_path: String,
    },
    /// Send a JSON message to a connected remote hub.
    Send {
        /// Connection to send on.
        connection_id: String,
        /// JSON message payload.
        data: serde_json::Value,
    },
    /// Close a hub client connection and abort its tasks.
    Close {
        /// Connection to close.
        connection_id: String,
    },
}

// =============================================================================
// Hub-owned state
// =============================================================================

/// A Lua-managed outgoing hub client connection.
///
/// Owned by the Hub, keyed by connection_id in a `HashMap`. The read and
/// write tasks are aborted on drop (close or Hub shutdown).
pub struct LuaHubClientConn {
    /// Sender for outgoing frames to the remote hub.
    pub frame_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    /// Handle for the read task (aborted on close).
    pub read_handle: tokio::task::JoinHandle<()>,
    /// Handle for the write task (aborted on close).
    pub write_handle: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for LuaHubClientConn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LuaHubClientConn")
            .field("frame_tx_closed", &self.frame_tx.is_closed())
            .field("read_finished", &self.read_handle.is_finished())
            .field("write_finished", &self.write_handle.is_finished())
            .finish()
    }
}

impl Drop for LuaHubClientConn {
    fn drop(&mut self) {
        // Abort both tasks when the connection is dropped (close/shutdown).
        self.read_handle.abort();
        self.write_handle.abort();
    }
}

// =============================================================================
// Lua callback firing (Hub → Lua)
// =============================================================================

/// Fire the Lua callback for a single hub client message.
///
/// Called from [`handle_hub_event`] for [`HubEvent::HubClientMessage`] events.
///
/// If the message contains an `_mcp_rid` field matching a pending request,
/// the response is delivered to the blocking `hub_client.request()` caller
/// instead of firing the on_message callback.
///
/// Otherwise, looks up the callback from the [`HubClientCallbackRegistry`]
/// by connection ID, then fires the callback with `(message, connection_id)`.
///
/// Does nothing if the connection or callback has been removed (closed
/// between send and receive — benign race).
pub(crate) fn fire_hub_client_message(
    lua: &Lua,
    callback_registry: &HubClientCallbackRegistry,
    pending_requests: &HubClientPendingRequests,
    connection_id: &str,
    message: serde_json::Value,
) {
    // Check if this message is a response to a pending request.
    if let Some(rid) = message.get("_mcp_rid").and_then(|v| v.as_str()) {
        let sender = {
            let mut pending = pending_requests
                .lock()
                .expect("HubClientPendingRequests mutex poisoned");
            pending.remove(rid)
        };
        if let Some(tx) = sender {
            let _ = tx.send(message);
            return;
        }
        // No pending request for this rid — fall through to normal callback.
    }
    // Phase 1: Look up and clone the callback key under the registry lock.
    let callback_key = {
        let registry = callback_registry
            .lock()
            .expect("HubClientCallbackRegistry mutex poisoned");
        let Some(key) = registry.get(connection_id) else {
            // Callback removed (closed) — benign race.
            return;
        };
        match lua.registry_value::<mlua::Function>(key) {
            Ok(cb) => match lua.create_registry_value(cb) {
                Ok(cloned) => cloned,
                Err(e) => {
                    log::warn!("[HubClient-Lua] Failed to clone callback key for {connection_id}: {e}");
                    return;
                }
            },
            Err(e) => {
                log::warn!("[HubClient-Lua] Failed to retrieve callback for {connection_id}: {e}");
                return;
            }
        }
    };
    // Registry lock released — safe to call Lua.

    // Phase 2: Fire callback.
    let result: mlua::Result<()> = (|| {
        let callback: mlua::Function = lua.registry_value(&callback_key)?;
        let lua_msg = super::json::json_to_lua(lua, &message)?;
        callback.call::<()>((lua_msg, connection_id))?;
        Ok(())
    })();

    if let Err(e) = result {
        log::warn!("[HubClient-Lua] Callback error for {connection_id}: {e}");
    }

    // Phase 3: Clean up temporary registry key.
    let _ = lua.remove_registry_value(callback_key);
}

// =============================================================================
// Lua registration
// =============================================================================

/// Send a hub client request via the shared `HubEventSender`.
///
/// Helper used by all Lua closure registrations to send requests to the Hub
/// event loop. Silently drops the request if the sender is not yet set
/// (during early init before `set_hub_event_tx()`).
fn send_hc_event(tx: &HubEventSender, request: HubClientRequest) {
    let guard = tx.lock().expect("HubEventSender mutex poisoned");
    if let Some(ref sender) = *guard {
        let _ = sender.send(HubEvent::LuaHubClientRequest(request));
    } else {
        ::log::warn!(
            "[HubClient] request sent before hub_event_tx set — event dropped"
        );
    }
}

/// Register the `hub_client` global table with Lua.
///
/// Creates functions:
/// - `hub_client.connect(socket_path)` - Connect to a remote hub's Unix socket
/// - `hub_client.on_message(conn_id, callback(msg, conn_id))` - Register message callback
/// - `hub_client.send(conn_id, data)` - Send a JSON message
/// - `hub_client.request(conn_id, data, timeout_ms?)` - Send and block for response
/// - `hub_client.close(conn_id)` - Close a connection
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub(crate) fn register(
    lua: &Lua,
    hub_event_tx: HubEventSender,
    callback_registry: HubClientCallbackRegistry,
    pending_requests: HubClientPendingRequests,
    frame_senders: HubClientFrameSenders,
) -> Result<()> {
    let hc_table = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create hub_client table: {e}"))?;

    // Shared ID counter for connection IDs
    let conn_counter: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));

    // hub_client.connect(socket_path) -> connection_id
    let tx = Arc::clone(&hub_event_tx);
    let connect_counter = Arc::clone(&conn_counter);
    let connect_fn = lua
        .create_function(move |_, socket_path: String| {
            let connection_id = {
                let mut counter = connect_counter
                    .lock()
                    .expect("HubClient connection counter mutex poisoned");
                let id = format!("hc_conn_{counter}");
                *counter += 1;
                id
            };

            send_hc_event(&tx, HubClientRequest::Connect {
                connection_id: connection_id.clone(),
                socket_path,
            });

            Ok(connection_id)
        })
        .map_err(|e| anyhow!("Failed to create hub_client.connect function: {e}"))?;

    hc_table
        .set("connect", connect_fn)
        .map_err(|e| anyhow!("Failed to set hub_client.connect: {e}"))?;

    // hub_client.on_message(conn_id, callback)
    //
    // Stores the callback in the HubClientCallbackRegistry (keyed by connection_id).
    // Messages arriving via HubEvent::HubClientMessage will look up and fire this callback.
    let cb_registry = Arc::clone(&callback_registry);
    let on_message_fn = lua
        .create_function(
            move |lua, (conn_id, callback): (String, mlua::Function)| {
                let callback_key = lua.create_registry_value(callback).map_err(|e| {
                    mlua::Error::external(format!(
                        "hub_client.on_message: failed to store callback: {e}"
                    ))
                })?;

                // Store callback in registry (Lua-thread-pinned).
                {
                    let mut registry = cb_registry
                        .lock()
                        .expect("HubClientCallbackRegistry mutex poisoned");
                    registry.insert(conn_id, callback_key);
                }

                Ok(())
            },
        )
        .map_err(|e| anyhow!("Failed to create hub_client.on_message function: {e}"))?;

    hc_table
        .set("on_message", on_message_fn)
        .map_err(|e| anyhow!("Failed to set hub_client.on_message: {e}"))?;

    // hub_client.send(conn_id, data)
    let tx = Arc::clone(&hub_event_tx);
    let send_fn = lua
        .create_function(
            move |lua, (connection_id, data): (String, Value)| {
                let data_json: serde_json::Value = lua.from_value(data).map_err(|e| {
                    mlua::Error::external(format!(
                        "hub_client.send: failed to serialize data: {e}"
                    ))
                })?;

                send_hc_event(&tx, HubClientRequest::Send {
                    connection_id,
                    data: data_json,
                });

                Ok(())
            },
        )
        .map_err(|e| anyhow!("Failed to create hub_client.send function: {e}"))?;

    hc_table
        .set("send", send_fn)
        .map_err(|e| anyhow!("Failed to set hub_client.send: {e}"))?;

    // hub_client.request(conn_id, data, timeout_ms?)
    //
    // Blocking request/response: injects a correlation ID (`_mcp_rid`) into
    // the outgoing message, writes it DIRECTLY to the connection's frame sender
    // (bypassing the Hub event loop), then blocks until a response with the same
    // `_mcp_rid` arrives or the timeout expires. Default timeout: 30s.
    //
    // Must NOT use send_hc_event() here — request() blocks the Hub event loop
    // thread via recv_timeout(), so HubClientRequest::Send would never be
    // processed while Lua is waiting. Writing directly to frame_senders avoids
    // this by writing to the tokio write task (running on a worker thread).
    let frames = Arc::clone(&frame_senders);
    let pending = Arc::clone(&pending_requests);
    let request_fn = lua
        .create_function(
            move |lua, (connection_id, data, timeout_ms): (String, Value, Option<u64>)| {
                let timeout = Duration::from_millis(timeout_ms.unwrap_or(30_000));

                let mut data_json: serde_json::Value =
                    lua.from_value(data).map_err(|e| {
                        mlua::Error::external(format!(
                            "hub_client.request: failed to serialize data: {e}"
                        ))
                    })?;

                // Generate a unique correlation ID and inject it.
                let rid = format!(
                    "hcr_{}",
                    REQUEST_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
                );
                if let Some(obj) = data_json.as_object_mut() {
                    obj.insert(
                        "_mcp_rid".to_string(),
                        serde_json::Value::String(rid.clone()),
                    );
                } else {
                    return Err(mlua::Error::external(
                        "hub_client.request: data must be a JSON object (table)",
                    ));
                }

                // Create a bounded sync channel for the response.
                let (resp_tx, resp_rx) = std::sync::mpsc::sync_channel::<serde_json::Value>(1);

                // Store the sender in the pending map before sending the request
                // to avoid a race where the response arrives before we start waiting.
                {
                    let mut map = pending
                        .lock()
                        .expect("HubClientPendingRequests mutex poisoned");
                    map.insert(rid.clone(), resp_tx);
                }

                // Write directly to the connection's frame sender, bypassing the
                // Hub event loop (which is blocked waiting for this Lua call to return).
                let frame_bytes = {
                    use crate::socket::framing::Frame;
                    Frame::Json(data_json).encode()
                };
                let sent = {
                    let senders = frames
                        .lock()
                        .expect("HubClientFrameSenders mutex poisoned");
                    if let Some(tx) = senders.get(&connection_id) {
                        tx.send(frame_bytes).is_ok()
                    } else {
                        false
                    }
                };
                if !sent {
                    // Clean up pending entry before returning the error.
                    let mut map = pending
                        .lock()
                        .expect("HubClientPendingRequests mutex poisoned");
                    map.remove(&rid);
                    return Err(mlua::Error::external(format!(
                        "hub_client.request: connection '{}' not found or closed",
                        connection_id
                    )));
                }

                // Block until response or timeout.
                let result = resp_rx.recv_timeout(timeout);

                // Clean up the pending entry (may already be removed by the read task).
                {
                    let mut map = pending
                        .lock()
                        .expect("HubClientPendingRequests mutex poisoned");
                    map.remove(&rid);
                }

                match result {
                    Ok(response) => {
                        let lua_val = super::json::json_to_lua(lua, &response)?;
                        Ok(lua_val)
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        Err(mlua::Error::external(format!(
                            "hub_client.request: timeout after {}ms",
                            timeout.as_millis()
                        )))
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        Err(mlua::Error::external(
                            "hub_client.request: response channel closed (connection dropped?)",
                        ))
                    }
                }
            },
        )
        .map_err(|e| anyhow!("Failed to create hub_client.request function: {e}"))?;

    hc_table
        .set("request", request_fn)
        .map_err(|e| anyhow!("Failed to set hub_client.request: {e}"))?;

    // hub_client.close(conn_id)
    let tx = Arc::clone(&hub_event_tx);
    let close_fn = lua
        .create_function(move |_, connection_id: String| {
            send_hc_event(&tx, HubClientRequest::Close { connection_id });
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create hub_client.close function: {e}"))?;

    hc_table
        .set("close", close_fn)
        .map_err(|e| anyhow!("Failed to set hub_client.close: {e}"))?;

    lua.globals()
        .set("hub_client", hc_table)
        .map_err(|e| anyhow!("Failed to register hub_client table globally: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::new_hub_event_sender;

    /// Create a test sender with a wired-up channel for event capture.
    fn setup_with_channel() -> (
        HubEventSender,
        tokio::sync::mpsc::UnboundedReceiver<HubEvent>,
    ) {
        let tx = new_hub_event_sender();
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        *tx.lock().unwrap() = Some(sender);
        (tx, receiver)
    }

    #[test]
    fn test_hub_client_table_created() {
        let lua = Lua::new();
        let tx = new_hub_event_sender();
        let registry = new_hub_client_callback_registry();
        register(&lua, tx, registry, new_hub_client_pending_requests(), new_hub_client_frame_senders()).expect("Should register hub_client primitives");

        let globals = lua.globals();
        let hc_table: mlua::Table = globals
            .get("hub_client")
            .expect("hub_client table should exist");

        let _: mlua::Function = hc_table.get("connect").expect("connect should exist");
        let _: mlua::Function = hc_table.get("on_message").expect("on_message should exist");
        let _: mlua::Function = hc_table.get("send").expect("send should exist");
        let _: mlua::Function = hc_table.get("request").expect("request should exist");
        let _: mlua::Function = hc_table.get("close").expect("close should exist");
    }

    #[test]
    fn test_connect_returns_connection_id() {
        let lua = Lua::new();
        let (tx, _rx) = setup_with_channel();
        let registry = new_hub_client_callback_registry();
        register(&lua, tx, registry, new_hub_client_pending_requests(), new_hub_client_frame_senders()).expect("Should register hub_client primitives");

        let conn_id: String = lua
            .load(r#"return hub_client.connect("/tmp/test.sock")"#)
            .eval()
            .expect("connect should return a string");

        assert!(
            conn_id.starts_with("hc_conn_"),
            "Connection ID should start with 'hc_conn_', got: {conn_id}"
        );
    }

    #[test]
    fn test_connect_sends_event() {
        let lua = Lua::new();
        let (tx, mut rx) = setup_with_channel();
        let registry = new_hub_client_callback_registry();
        register(&lua, tx, registry, new_hub_client_pending_requests(), new_hub_client_frame_senders()).expect("Should register hub_client primitives");

        let conn_id: String = lua
            .load(r#"return hub_client.connect("/tmp/other-hub.sock")"#)
            .eval()
            .expect("connect should return a string");

        let event = rx.try_recv().expect("Should have received an event");
        match event {
            HubEvent::LuaHubClientRequest(HubClientRequest::Connect {
                connection_id,
                socket_path,
            }) => {
                assert_eq!(connection_id, conn_id);
                assert_eq!(socket_path, "/tmp/other-hub.sock");
            }
            _ => panic!("Expected LuaHubClientRequest(Connect), got: {event:?}"),
        }
    }

    #[test]
    fn test_send_sends_event() {
        let lua = Lua::new();
        let (tx, mut rx) = setup_with_channel();
        let registry = new_hub_client_callback_registry();
        register(&lua, tx, registry, new_hub_client_pending_requests(), new_hub_client_frame_senders()).expect("Should register hub_client primitives");

        lua.load(r#"hub_client.send("hc_conn_0", { type = "ping", seq = 42 })"#)
            .exec()
            .expect("send should succeed");

        let event = rx.try_recv().expect("Should have received an event");
        match event {
            HubEvent::LuaHubClientRequest(HubClientRequest::Send {
                connection_id,
                data,
            }) => {
                assert_eq!(connection_id, "hc_conn_0");
                assert_eq!(data["type"], "ping");
                assert_eq!(data["seq"], 42);
            }
            _ => panic!("Expected LuaHubClientRequest(Send), got: {event:?}"),
        }
    }

    #[test]
    fn test_close_sends_event() {
        let lua = Lua::new();
        let (tx, mut rx) = setup_with_channel();
        let registry = new_hub_client_callback_registry();
        register(&lua, tx, registry, new_hub_client_pending_requests(), new_hub_client_frame_senders()).expect("Should register hub_client primitives");

        lua.load(r#"hub_client.close("hc_conn_0")"#)
            .exec()
            .expect("close should succeed");

        let event = rx.try_recv().expect("Should have received an event");
        assert!(matches!(
            event,
            HubEvent::LuaHubClientRequest(HubClientRequest::Close { connection_id }) if connection_id == "hc_conn_0"
        ));
    }

    #[test]
    fn test_on_message_stores_callback() {
        let lua = Lua::new();
        let tx = new_hub_event_sender();
        let registry = new_hub_client_callback_registry();
        register(&lua, tx, Arc::clone(&registry), new_hub_client_pending_requests(), new_hub_client_frame_senders())
            .expect("Should register hub_client primitives");

        lua.load(
            r#"
            hub_client.on_message("hc_conn_0", function(msg, conn_id) end)
            "#,
        )
        .exec()
        .expect("on_message should succeed");

        let reg = registry.lock().unwrap();
        assert!(
            reg.contains_key("hc_conn_0"),
            "Callback should be stored in registry"
        );
    }

    #[test]
    fn test_sequential_ids_increment() {
        let lua = Lua::new();
        let (tx, _rx) = setup_with_channel();
        let registry = new_hub_client_callback_registry();
        register(&lua, tx, registry, new_hub_client_pending_requests(), new_hub_client_frame_senders()).expect("Should register hub_client primitives");

        let id1: String = lua
            .load(r#"return hub_client.connect("/tmp/a.sock")"#)
            .eval()
            .unwrap();
        let id2: String = lua
            .load(r#"return hub_client.connect("/tmp/b.sock")"#)
            .eval()
            .unwrap();

        assert_eq!(id1, "hc_conn_0");
        assert_eq!(id2, "hc_conn_1");
    }

    #[test]
    fn test_request_injects_rid_and_sends_frame() {
        use crate::socket::framing::{Frame, FrameDecoder};

        let lua = Lua::new();
        let (tx, _rx) = setup_with_channel();
        let registry = new_hub_client_callback_registry();
        let pending = new_hub_client_pending_requests();
        let frames = new_hub_client_frame_senders();

        // Pre-populate a fake frame channel for "hc_conn_0".
        let (frame_tx, mut frame_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        frames.lock().unwrap().insert("hc_conn_0".to_string(), frame_tx);

        register(&lua, tx, registry, Arc::clone(&pending), Arc::clone(&frames))
            .expect("Should register hub_client primitives");

        // Spawn a thread: read the frame from the channel, extract _mcp_rid,
        // and deliver the response directly to pending (simulating the read task).
        let pending_clone = Arc::clone(&pending);
        let handle = std::thread::spawn(move || {
            // Wait for the frame written by request().
            let frame_bytes = frame_rx
                .blocking_recv()
                .expect("Should have received a frame");

            // Decode the frame to extract _mcp_rid from the JSON.
            let mut decoder = FrameDecoder::new();
            let frames = decoder.feed(&frame_bytes).expect("Should decode frame");
            let frame = frames.into_iter().next().expect("Should have one frame");
            let data = match frame {
                Frame::Json(v) => v,
                _ => panic!("Expected JSON frame"),
            };
            let rid = data["_mcp_rid"].as_str().expect("Should have _mcp_rid").to_string();
            assert_eq!(data["method"], "test", "Frame should contain original data");

            // Deliver the response to pending (as the read task would).
            let map = pending_clone.lock().unwrap();
            if let Some(sender) = map.get(&rid) {
                let _ = sender.send(serde_json::json!({
                    "_mcp_rid": rid,
                    "result": "ok"
                }));
            }
        });

        // Call request from Lua (blocks until response).
        let result: mlua::Result<Value> = lua
            .load(
                r#"return hub_client.request("hc_conn_0", { method = "test" }, 5000)"#,
            )
            .eval();

        handle.join().unwrap();

        let val = result.expect("request should succeed");
        let result_field: String = lua
            .load("return ...")
            .call(val)
            .and_then(|v: mlua::Table| v.get("result"))
            .expect("Should have result field");
        assert_eq!(result_field, "ok");
    }

    #[test]
    fn test_request_timeout() {
        let lua = Lua::new();
        let (tx, _rx) = setup_with_channel();
        let registry = new_hub_client_callback_registry();
        let pending = new_hub_client_pending_requests();
        let frames = new_hub_client_frame_senders();

        // Pre-populate a frame sender so request() can write — but nobody reads,
        // so no response is delivered and recv_timeout() fires.
        let (frame_tx, _frame_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        frames.lock().unwrap().insert("hc_conn_0".to_string(), frame_tx);

        register(&lua, tx, registry, pending, frames)
            .expect("Should register hub_client primitives");

        // Call with a very short timeout — no one will respond.
        let result: mlua::Result<Value> = lua
            .load(
                r#"return hub_client.request("hc_conn_0", { method = "test" }, 50)"#,
            )
            .eval();

        assert!(result.is_err(), "Should timeout");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("timeout"),
            "Error should mention timeout, got: {err_msg}"
        );
    }

    #[test]
    fn test_fire_delivers_to_pending_request() {
        let lua = Lua::new();
        let cb_registry = new_hub_client_callback_registry();
        let pending = new_hub_client_pending_requests();

        let (resp_tx, resp_rx) = std::sync::mpsc::sync_channel::<serde_json::Value>(1);
        {
            let mut map = pending.lock().unwrap();
            map.insert("hcr_test_1".to_string(), resp_tx);
        }

        // Fire a message with matching _mcp_rid — should deliver to channel.
        let msg = serde_json::json!({
            "_mcp_rid": "hcr_test_1",
            "result": "delivered"
        });
        fire_hub_client_message(&lua, &cb_registry, &pending, "hc_conn_0", msg);

        let response = resp_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("Should receive response");
        assert_eq!(response["result"], "delivered");

        // Pending entry should be cleaned up.
        let map = pending.lock().unwrap();
        assert!(
            !map.contains_key("hcr_test_1"),
            "Pending entry should be removed after delivery"
        );
    }
}
