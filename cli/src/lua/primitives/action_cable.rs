//! ActionCable Lua primitives for managing WebSocket connections.
//!
//! Exposes ActionCable connection management to Lua scripts via a request
//! queue pattern. Lua enqueues requests (connect, subscribe, perform, close)
//! which the Hub processes in its tick loop. Incoming channel messages are
//! polled each tick and dispatched to registered Lua callbacks.
//!
//! # Architecture
//!
//! ```text
//! Lua script                    Hub tick loop
//!     │                              │
//!     │ action_cable.connect()       │
//!     │ ───────────────────────►     │ process_lua_action_cable_requests()
//!     │                              │   → creates ActionCableConnection
//!     │ action_cable.subscribe()     │
//!     │ ───────────────────────►     │   → calls connection.subscribe()
//!     │                              │
//!     │ action_cable.perform()       │
//!     │ ───────────────────────►     │   → calls handle.perform()
//!     │                              │
//!     │                              │ poll_lua_action_cable_channels()
//!     │   ◄──────────────────────    │   → drains handle.try_recv()
//!     │   callback(message)          │   → auto-decrypts if crypto=true
//!     │                              │
//!     │ action_cable.close()         │
//!     │ ───────────────────────►     │   → shuts down connection
//! ```
//!
//! # Crypto
//!
//! When `crypto = true` is passed to `action_cable.connect()`, incoming
//! messages with `type == "signal"` have their `envelope` field automatically
//! decrypted via the hub's `CryptoService` before the Lua callback fires.
//!
//! # Usage in Lua
//!
//! ```lua
//! -- Connect with encryption
//! local conn = action_cable.connect({ crypto = true })
//!
//! -- Subscribe to a channel with a message callback
//! local ch = action_cable.subscribe(conn, "HubCommandChannel",
//!     { hub_id = hub.server_id(), start_from = 0 },
//!     function(message) log.info("Got: " .. json.encode(message)) end)
//!
//! -- Perform an action on the channel
//! action_cable.perform(ch, "ack", { sequence = 42 })
//!
//! -- Close the connection
//! action_cable.close(conn)
//! ```

// Rust guideline compliant 2026-02

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use mlua::{Lua, LuaSerdeExt, Table, Value};

use crate::hub::action_cable_connection::{ActionCableConnection, ChannelHandle};
use crate::relay::{CryptoService, OlmEnvelope};

// =============================================================================
// Request queue types (Lua -> Hub)
// =============================================================================

/// Request from Lua to the Hub for ActionCable operations.
///
/// Enqueued by Lua primitive functions, drained by the Hub tick loop
/// via [`process_lua_action_cable_requests`].
#[derive(Debug)]
pub enum ActionCableRequest {
    /// Open a new ActionCable WebSocket connection.
    Connect {
        /// Unique connection identifier (e.g., "ac_conn_0").
        connection_id: String,
        /// Whether to auto-decrypt signal envelopes on this connection.
        crypto: bool,
    },
    /// Subscribe to a channel on an existing connection.
    Subscribe {
        /// Connection to subscribe on.
        connection_id: String,
        /// Unique channel identifier (e.g., "ac_ch_0").
        channel_id: String,
        /// ActionCable channel class name (e.g., "HubCommandChannel").
        channel_name: String,
        /// Subscription parameters merged into the identifier JSON.
        params: serde_json::Value,
        /// Lua registry key for the message callback function.
        callback_key: mlua::RegistryKey,
    },
    /// Perform an action on a subscribed channel.
    Perform {
        /// Channel to perform on.
        channel_id: String,
        /// Action name (e.g., "ack", "signal").
        action: String,
        /// Action data payload.
        data: serde_json::Value,
    },
    /// Unsubscribe from a channel (drop the handle).
    Unsubscribe {
        /// Channel to unsubscribe from.
        channel_id: String,
    },
    /// Close an ActionCable connection and all its channels.
    Close {
        /// Connection to close.
        connection_id: String,
    },
}

/// Thread-safe queue for ActionCable requests from Lua.
pub type ActionCableRequestQueue = Arc<Mutex<Vec<ActionCableRequest>>>;

/// Create a new ActionCable request queue.
#[must_use]
pub fn new_request_queue() -> ActionCableRequestQueue {
    Arc::new(Mutex::new(Vec::new()))
}

// =============================================================================
// Hub-owned state
// =============================================================================

/// A Lua-managed ActionCable connection with its crypto preference.
///
/// Owned by the Hub, keyed by connection_id in a `HashMap`.
#[derive(Debug)]
pub struct LuaAcConnection {
    /// The underlying WebSocket connection.
    pub connection: ActionCableConnection,
    /// Whether signal envelopes should be auto-decrypted.
    pub crypto_enabled: bool,
}

/// A Lua-managed channel subscription with its callback.
///
/// Owned by the Hub, keyed by channel_id in a `HashMap`.
pub struct LuaAcChannel {
    /// The channel handle for receiving messages and performing actions.
    pub handle: ChannelHandle,
    /// Lua registry key for the message callback function.
    pub callback_key: mlua::RegistryKey,
    /// The connection this channel belongs to (for crypto lookup).
    pub connection_id: String,
}

impl std::fmt::Debug for LuaAcChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LuaAcChannel")
            .field("handle", &self.handle)
            .field("connection_id", &self.connection_id)
            .finish_non_exhaustive()
    }
}

// =============================================================================
// Processing functions (called by Hub in tick loop)
// =============================================================================

/// Process queued ActionCable requests from Lua.
///
/// Drains the request queue and executes each request:
/// - `Connect`: creates a new `ActionCableConnection` via tokio runtime
/// - `Subscribe`: subscribes to a channel on an existing connection
/// - `Perform`: sends an action on a subscribed channel
/// - `Unsubscribe`: drops a channel handle
/// - `Close`: shuts down a connection and removes all its channels
///
/// # Arguments
///
/// * `requests` - The request queue to drain
/// * `connections` - Hub-owned map of active connections
/// * `channels` - Hub-owned map of active channel subscriptions
/// * `server_url` - Server URL for new connections
/// * `api_key` - API key for authentication
/// * `tokio_runtime` - Tokio runtime handle for spawning async connection tasks
pub fn process_lua_action_cable_requests(
    requests: &ActionCableRequestQueue,
    connections: &mut HashMap<String, LuaAcConnection>,
    channels: &mut HashMap<String, LuaAcChannel>,
    server_url: &str,
    api_key: &str,
    tokio_runtime: &tokio::runtime::Handle,
) {
    let reqs: Vec<ActionCableRequest> = {
        let mut queue = requests.lock().expect("ActionCable request queue mutex poisoned");
        queue.drain(..).collect()
    };

    for req in reqs {
        match req {
            ActionCableRequest::Connect {
                connection_id,
                crypto,
            } => {
                let _guard = tokio_runtime.enter();
                let connection = ActionCableConnection::connect(server_url, api_key);
                connections.insert(
                    connection_id.clone(),
                    LuaAcConnection {
                        connection,
                        crypto_enabled: crypto,
                    },
                );
                log::info!(
                    "[ActionCable-Lua] Connection '{}' opened (crypto={})",
                    connection_id,
                    crypto
                );
            }

            ActionCableRequest::Subscribe {
                connection_id,
                channel_id,
                channel_name,
                params,
                callback_key,
            } => {
                if let Some(conn) = connections.get(&connection_id) {
                    // Build the ActionCable identifier JSON with channel name and params
                    let mut identifier = serde_json::json!({ "channel": channel_name });
                    if let serde_json::Value::Object(map) = params {
                        if let serde_json::Value::Object(ref mut id_map) = identifier {
                            for (k, v) in map {
                                id_map.insert(k, v);
                            }
                        }
                    }

                    let handle = conn.connection.subscribe(identifier);
                    channels.insert(
                        channel_id.clone(),
                        LuaAcChannel {
                            handle,
                            callback_key,
                            connection_id,
                        },
                    );
                    log::info!(
                        "[ActionCable-Lua] Channel '{}' subscribed to '{}'",
                        channel_id,
                        channel_name
                    );
                } else {
                    log::warn!(
                        "[ActionCable-Lua] Subscribe failed: connection '{}' not found",
                        connection_id
                    );
                }
            }

            ActionCableRequest::Perform {
                channel_id,
                action,
                data,
            } => {
                if let Some(ch) = channels.get(&channel_id) {
                    ch.handle.perform(&action, data);
                    log::trace!(
                        "[ActionCable-Lua] Performed '{}' on channel '{}'",
                        action,
                        channel_id
                    );
                } else {
                    log::warn!(
                        "[ActionCable-Lua] Perform failed: channel '{}' not found",
                        channel_id
                    );
                }
            }

            ActionCableRequest::Unsubscribe { channel_id } => {
                if channels.remove(&channel_id).is_some() {
                    log::info!(
                        "[ActionCable-Lua] Channel '{}' unsubscribed",
                        channel_id
                    );
                } else {
                    log::warn!(
                        "[ActionCable-Lua] Unsubscribe failed: channel '{}' not found",
                        channel_id
                    );
                }
            }

            ActionCableRequest::Close { connection_id } => {
                // Remove all channels belonging to this connection
                let orphaned: Vec<String> = channels
                    .iter()
                    .filter(|(_, ch)| ch.connection_id == connection_id)
                    .map(|(id, _)| id.clone())
                    .collect();

                for ch_id in &orphaned {
                    channels.remove(ch_id);
                }

                if let Some(conn) = connections.remove(&connection_id) {
                    conn.connection.shutdown();
                    log::info!(
                        "[ActionCable-Lua] Connection '{}' closed ({} channels removed)",
                        connection_id,
                        orphaned.len()
                    );
                } else {
                    log::warn!(
                        "[ActionCable-Lua] Close failed: connection '{}' not found",
                        connection_id
                    );
                }
            }
        }
    }
}

/// Poll all Lua ActionCable channels for incoming messages and fire callbacks.
///
/// For each channel, drains `handle.try_recv()`. If the channel's connection
/// has `crypto_enabled` and the message has `type == "signal"`, the `envelope`
/// field is automatically decrypted via `CryptoService` before the callback fires.
///
/// # Deadlock Prevention
///
/// Messages are collected first, then callbacks are fired without holding any
/// locks on the channel map. Crypto decryption acquires the `CryptoService`
/// mutex briefly per envelope.
///
/// # Returns
///
/// The number of callbacks fired.
pub fn poll_lua_action_cable_channels(
    lua: &Lua,
    channels: &mut HashMap<String, LuaAcChannel>,
    connections: &HashMap<String, LuaAcConnection>,
    crypto_service: Option<&CryptoService>,
) -> usize {
    // Phase 1: collect all pending messages with their callback keys
    let mut pending: Vec<(mlua::RegistryKey, serde_json::Value)> = Vec::new();

    for (channel_id, channel) in channels.iter_mut() {
        // Look up crypto status for this channel's connection
        let crypto_enabled = connections
            .get(&channel.connection_id)
            .map_or(false, |c| c.crypto_enabled);

        while let Some(mut msg) = channel.handle.try_recv() {
            // Auto-decrypt signal envelopes when crypto is enabled
            if crypto_enabled {
                if let Some(msg_type) = msg.get("type").and_then(|t| t.as_str()) {
                    if msg_type == "signal" {
                        if let Some(envelope_val) = msg.get("envelope").cloned() {
                            msg = decrypt_signal_envelope(
                                &msg,
                                &envelope_val,
                                crypto_service,
                                channel_id,
                            );
                        }
                    }
                }
            }

            // Clone the registry key reference for the callback.
            // We re-use the same key for every message on this channel.
            // Collect as (key_ref, message) pairs for firing.
            pending.push((
                lua.create_registry_value(
                    lua.registry_value::<mlua::Function>(&channel.callback_key)
                        .expect("ActionCable callback registry key should be valid"),
                )
                .expect("Failed to clone callback registry key"),
                msg,
            ));
        }
    }

    // Phase 2: fire callbacks
    let count = pending.len();

    for (callback_key, msg) in &pending {
        let result: mlua::Result<()> = (|| {
            let callback: mlua::Function = lua.registry_value(callback_key)?;
            let lua_msg = lua.to_value(msg)?;
            callback.call::<()>(lua_msg)?;
            Ok(())
        })();

        if let Err(e) = result {
            log::warn!("[ActionCable-Lua] Callback error: {e}");
        }
    }

    // Phase 3: clean up temporary registry keys
    for (callback_key, _) in pending {
        let _ = lua.remove_registry_value(callback_key);
    }

    count
}

/// Decrypt a signal envelope and replace it in the message.
///
/// On success, replaces the `envelope` field with the decrypted JSON payload.
/// On failure, logs a warning and returns the original message unmodified.
fn decrypt_signal_envelope(
    msg: &serde_json::Value,
    envelope_val: &serde_json::Value,
    crypto_service: Option<&CryptoService>,
    channel_id: &str,
) -> serde_json::Value {
    let Some(crypto) = crypto_service else {
        log::warn!(
            "[ActionCable-Lua] Channel '{}': crypto enabled but no CryptoService available",
            channel_id
        );
        return msg.clone();
    };

    // Parse the envelope JSON into an OlmEnvelope
    let envelope: OlmEnvelope = match serde_json::from_value(envelope_val.clone()) {
        Ok(e) => e,
        Err(e) => {
            log::warn!(
                "[ActionCable-Lua] Channel '{}': failed to parse OlmEnvelope: {e}",
                channel_id
            );
            return msg.clone();
        }
    };

    // Decrypt via CryptoService (brief mutex lock)
    let plaintext = match crypto.lock() {
        Ok(mut guard) => match guard.decrypt(&envelope) {
            Ok(pt) => pt,
            Err(e) => {
                log::warn!(
                    "[ActionCable-Lua] Channel '{}': decryption failed: {e}",
                    channel_id
                );
                return msg.clone();
            }
        },
        Err(e) => {
            log::error!(
                "[ActionCable-Lua] CryptoService mutex poisoned: {e}"
            );
            return msg.clone();
        }
    };

    // Parse decrypted plaintext as JSON and replace the envelope field
    match serde_json::from_slice::<serde_json::Value>(&plaintext) {
        Ok(decrypted_payload) => {
            let mut result = msg.clone();
            if let Some(obj) = result.as_object_mut() {
                obj.insert("envelope".to_string(), decrypted_payload);
            }
            result
        }
        Err(e) => {
            log::warn!(
                "[ActionCable-Lua] Channel '{}': failed to parse decrypted payload as JSON: {e}",
                channel_id
            );
            msg.clone()
        }
    }
}

// =============================================================================
// Lua registration
// =============================================================================

/// Register the `action_cable` global table with Lua.
///
/// Creates functions:
/// - `action_cable.connect(opts?)` - Open a new ActionCable connection
/// - `action_cable.subscribe(conn_id, channel_name, params, callback)` - Subscribe to a channel
/// - `action_cable.perform(channel_id, action, data)` - Perform an action
/// - `action_cable.unsubscribe(channel_id)` - Unsubscribe from a channel
/// - `action_cable.close(conn_id)` - Close a connection
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub fn register_action_cable(lua: &Lua, queue: ActionCableRequestQueue) -> Result<()> {
    let ac_table = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create action_cable table: {e}"))?;

    // Shared ID counters for connection and channel IDs
    let conn_counter: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));
    let ch_counter: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));

    // action_cable.connect(opts?) -> connection_id
    //
    // Options table:
    //   crypto: boolean (default false) - enable auto-decryption of signal envelopes
    let connect_queue = Arc::clone(&queue);
    let connect_counter = Arc::clone(&conn_counter);
    let connect_fn = lua
        .create_function(move |_, opts: Option<Table>| {
            let crypto = opts
                .as_ref()
                .and_then(|t| t.get::<bool>("crypto").ok())
                .unwrap_or(false);

            let connection_id = {
                let mut counter = connect_counter
                    .lock()
                    .expect("ActionCable connection counter mutex poisoned");
                let id = format!("ac_conn_{counter}");
                *counter += 1;
                id
            };

            {
                let mut q = connect_queue
                    .lock()
                    .expect("ActionCable request queue mutex poisoned");
                q.push(ActionCableRequest::Connect {
                    connection_id: connection_id.clone(),
                    crypto,
                });
            }

            Ok(connection_id)
        })
        .map_err(|e| anyhow!("Failed to create action_cable.connect function: {e}"))?;

    ac_table
        .set("connect", connect_fn)
        .map_err(|e| anyhow!("Failed to set action_cable.connect: {e}"))?;

    // action_cable.subscribe(conn_id, channel_name, params, callback) -> channel_id
    let subscribe_queue = Arc::clone(&queue);
    let subscribe_counter = Arc::clone(&ch_counter);
    let subscribe_fn = lua
        .create_function(
            move |lua,
                  (conn_id, channel_name, params, callback): (
                String,
                String,
                Value,
                mlua::Function,
            )| {
                let params_json: serde_json::Value = lua.from_value(params).map_err(|e| {
                    mlua::Error::external(format!(
                        "action_cable.subscribe: failed to serialize params: {e}"
                    ))
                })?;

                let callback_key = lua.create_registry_value(callback).map_err(|e| {
                    mlua::Error::external(format!(
                        "action_cable.subscribe: failed to store callback: {e}"
                    ))
                })?;

                let channel_id = {
                    let mut counter = subscribe_counter
                        .lock()
                        .expect("ActionCable channel counter mutex poisoned");
                    let id = format!("ac_ch_{counter}");
                    *counter += 1;
                    id
                };

                {
                    let mut q = subscribe_queue
                        .lock()
                        .expect("ActionCable request queue mutex poisoned");
                    q.push(ActionCableRequest::Subscribe {
                        connection_id: conn_id,
                        channel_id: channel_id.clone(),
                        channel_name,
                        params: params_json,
                        callback_key,
                    });
                }

                Ok(channel_id)
            },
        )
        .map_err(|e| anyhow!("Failed to create action_cable.subscribe function: {e}"))?;

    ac_table
        .set("subscribe", subscribe_fn)
        .map_err(|e| anyhow!("Failed to set action_cable.subscribe: {e}"))?;

    // action_cable.perform(channel_id, action, data)
    let perform_queue = Arc::clone(&queue);
    let perform_fn = lua
        .create_function(
            move |lua, (channel_id, action, data): (String, String, Value)| {
                let data_json: serde_json::Value = lua.from_value(data).map_err(|e| {
                    mlua::Error::external(format!(
                        "action_cable.perform: failed to serialize data: {e}"
                    ))
                })?;

                {
                    let mut q = perform_queue
                        .lock()
                        .expect("ActionCable request queue mutex poisoned");
                    q.push(ActionCableRequest::Perform {
                        channel_id,
                        action,
                        data: data_json,
                    });
                }

                Ok(())
            },
        )
        .map_err(|e| anyhow!("Failed to create action_cable.perform function: {e}"))?;

    ac_table
        .set("perform", perform_fn)
        .map_err(|e| anyhow!("Failed to set action_cable.perform: {e}"))?;

    // action_cable.unsubscribe(channel_id)
    let unsub_queue = Arc::clone(&queue);
    let unsubscribe_fn = lua
        .create_function(move |_, channel_id: String| {
            let mut q = unsub_queue
                .lock()
                .expect("ActionCable request queue mutex poisoned");
            q.push(ActionCableRequest::Unsubscribe { channel_id });
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create action_cable.unsubscribe function: {e}"))?;

    ac_table
        .set("unsubscribe", unsubscribe_fn)
        .map_err(|e| anyhow!("Failed to set action_cable.unsubscribe: {e}"))?;

    // action_cable.close(conn_id)
    let close_queue = Arc::clone(&queue);
    let close_fn = lua
        .create_function(move |_, connection_id: String| {
            let mut q = close_queue
                .lock()
                .expect("ActionCable request queue mutex poisoned");
            q.push(ActionCableRequest::Close { connection_id });
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create action_cable.close function: {e}"))?;

    ac_table
        .set("close", close_fn)
        .map_err(|e| anyhow!("Failed to set action_cable.close: {e}"))?;

    lua.globals()
        .set("action_cable", ac_table)
        .map_err(|e| anyhow!("Failed to register action_cable table globally: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action_cable_table_created() {
        let lua = Lua::new();
        let queue = new_request_queue();
        register_action_cable(&lua, queue).expect("Should register action_cable primitives");

        let globals = lua.globals();
        let ac_table: Table = globals
            .get("action_cable")
            .expect("action_cable table should exist");

        let _: mlua::Function = ac_table.get("connect").expect("connect should exist");
        let _: mlua::Function = ac_table.get("subscribe").expect("subscribe should exist");
        let _: mlua::Function = ac_table.get("perform").expect("perform should exist");
        let _: mlua::Function = ac_table
            .get("unsubscribe")
            .expect("unsubscribe should exist");
        let _: mlua::Function = ac_table.get("close").expect("close should exist");
    }

    #[test]
    fn test_connect_returns_id_and_queues_request() {
        let lua = Lua::new();
        let queue = new_request_queue();
        register_action_cable(&lua, Arc::clone(&queue))
            .expect("Should register action_cable primitives");

        let conn_id: String = lua
            .load(r#"return action_cable.connect({ crypto = true })"#)
            .eval()
            .expect("connect should return a string");

        assert!(
            conn_id.starts_with("ac_conn_"),
            "Connection ID should start with 'ac_conn_', got: {conn_id}"
        );

        let reqs = queue.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        match &reqs[0] {
            ActionCableRequest::Connect {
                connection_id,
                crypto,
            } => {
                assert_eq!(connection_id, &conn_id);
                assert!(crypto);
            }
            _ => panic!("Expected Connect request"),
        }
    }

    #[test]
    fn test_connect_default_no_crypto() {
        let lua = Lua::new();
        let queue = new_request_queue();
        register_action_cable(&lua, Arc::clone(&queue))
            .expect("Should register action_cable primitives");

        let _: String = lua
            .load(r#"return action_cable.connect()"#)
            .eval()
            .expect("connect without opts should work");

        let reqs = queue.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        match &reqs[0] {
            ActionCableRequest::Connect { crypto, .. } => {
                assert!(!crypto, "Default crypto should be false");
            }
            _ => panic!("Expected Connect request"),
        }
    }

    #[test]
    fn test_subscribe_returns_channel_id_and_queues_request() {
        let lua = Lua::new();
        let queue = new_request_queue();
        register_action_cable(&lua, Arc::clone(&queue))
            .expect("Should register action_cable primitives");

        let ch_id: String = lua
            .load(
                r#"
                return action_cable.subscribe(
                    "ac_conn_0",
                    "HubCommandChannel",
                    { hub_id = "test-hub", start_from = 0 },
                    function(msg) end
                )
                "#,
            )
            .eval()
            .expect("subscribe should return a string");

        assert!(
            ch_id.starts_with("ac_ch_"),
            "Channel ID should start with 'ac_ch_', got: {ch_id}"
        );

        let reqs = queue.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        match &reqs[0] {
            ActionCableRequest::Subscribe {
                connection_id,
                channel_id,
                channel_name,
                params,
                ..
            } => {
                assert_eq!(connection_id, "ac_conn_0");
                assert_eq!(channel_id, &ch_id);
                assert_eq!(channel_name, "HubCommandChannel");
                assert_eq!(params["hub_id"], "test-hub");
                assert_eq!(params["start_from"], 0);
            }
            _ => panic!("Expected Subscribe request"),
        }
    }

    #[test]
    fn test_perform_queues_request() {
        let lua = Lua::new();
        let queue = new_request_queue();
        register_action_cable(&lua, Arc::clone(&queue))
            .expect("Should register action_cable primitives");

        lua.load(r#"action_cable.perform("ac_ch_0", "ack", { sequence = 42 })"#)
            .exec()
            .expect("perform should succeed");

        let reqs = queue.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        match &reqs[0] {
            ActionCableRequest::Perform {
                channel_id,
                action,
                data,
            } => {
                assert_eq!(channel_id, "ac_ch_0");
                assert_eq!(action, "ack");
                assert_eq!(data["sequence"], 42);
            }
            _ => panic!("Expected Perform request"),
        }
    }

    #[test]
    fn test_unsubscribe_queues_request() {
        let lua = Lua::new();
        let queue = new_request_queue();
        register_action_cable(&lua, Arc::clone(&queue))
            .expect("Should register action_cable primitives");

        lua.load(r#"action_cable.unsubscribe("ac_ch_0")"#)
            .exec()
            .expect("unsubscribe should succeed");

        let reqs = queue.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        assert!(matches!(&reqs[0], ActionCableRequest::Unsubscribe { channel_id } if channel_id == "ac_ch_0"));
    }

    #[test]
    fn test_close_queues_request() {
        let lua = Lua::new();
        let queue = new_request_queue();
        register_action_cable(&lua, Arc::clone(&queue))
            .expect("Should register action_cable primitives");

        lua.load(r#"action_cable.close("ac_conn_0")"#)
            .exec()
            .expect("close should succeed");

        let reqs = queue.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        assert!(matches!(&reqs[0], ActionCableRequest::Close { connection_id } if connection_id == "ac_conn_0"));
    }

    #[test]
    fn test_sequential_ids_increment() {
        let lua = Lua::new();
        let queue = new_request_queue();
        register_action_cable(&lua, Arc::clone(&queue))
            .expect("Should register action_cable primitives");

        let id1: String = lua
            .load(r#"return action_cable.connect()"#)
            .eval()
            .unwrap();
        let id2: String = lua
            .load(r#"return action_cable.connect()"#)
            .eval()
            .unwrap();

        assert_eq!(id1, "ac_conn_0");
        assert_eq!(id2, "ac_conn_1");
    }

    #[test]
    fn test_decrypt_signal_envelope_no_crypto_service() {
        let msg = serde_json::json!({
            "type": "signal",
            "envelope": { "t": 0, "b": "dGVzdA==", "k": "abc" }
        });
        let envelope_val = msg.get("envelope").unwrap().clone();

        let result = decrypt_signal_envelope(&msg, &envelope_val, None, "test_ch");

        // Without crypto service, message should be returned unmodified
        assert_eq!(result, msg);
    }

    #[test]
    fn test_poll_empty_channels_returns_zero() {
        let lua = Lua::new();
        let mut channels: HashMap<String, LuaAcChannel> = HashMap::new();
        let connections: HashMap<String, LuaAcConnection> = HashMap::new();

        let count = poll_lua_action_cable_channels(&lua, &mut channels, &connections, None);
        assert_eq!(count, 0);
    }
}
