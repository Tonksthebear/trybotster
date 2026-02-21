//! ActionCable Lua primitives for managing WebSocket connections.
//!
//! Exposes ActionCable connection management to Lua scripts via the event-driven
//! `HubEvent` channel. Lua closures send `HubEvent::LuaActionCableRequest`
//! directly to the Hub event loop, which processes connect/subscribe/perform/
//! unsubscribe/close operations. Incoming channel messages arrive via
//! `HubEvent::AcChannelMessage` from per-channel forwarding tasks.
//!
//! # Architecture
//!
//! ```text
//! Lua script                    Hub event loop
//!     │                              │
//!     │ action_cable.connect()       │
//!     │ ──── HubEvent ──────────►    │ process_single_action_cable_request()
//!     │                              │   → creates ActionCableConnection
//!     │ action_cable.subscribe()     │
//!     │ ──── HubEvent ──────────►    │   → spawns forwarding task
//!     │                              │
//!     │ action_cable.perform()       │
//!     │ ──── HubEvent ──────────►    │   → calls handle.perform()
//!     │                              │
//!     │                              │ HubEvent::AcChannelMessage
//!     │   ◄──────────────────────    │   → fire_single_ac_message()
//!     │   callback(message)          │   → auto-decrypts if crypto=true
//!     │                              │
//!     │ action_cable.close()         │
//!     │ ──── HubEvent ──────────►    │   → shuts down connection
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
//! -- The callback receives the message AND the channel_id as arguments,
//! -- so you can use channel_id directly without upvalue capture.
//! local ch = action_cable.subscribe(conn, "HubCommandChannel",
//!     { hub_id = hub.server_id(), start_from = 0 },
//!     function(message, channel_id) log.info("Got: " .. json.encode(message)) end)
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

use super::HubEventSender;
use crate::hub::action_cable_connection::{ActionCableConnection, ChannelHandle};
use crate::hub::events::HubEvent;
use crate::relay::{CryptoService, OlmEnvelope};

// =============================================================================
// Callback registry (Lua-thread-pinned, shared with Hub)
// =============================================================================

/// Thread-safe registry mapping channel IDs to Lua callback keys.
///
/// Callbacks are stored here at subscribe time (in the Lua closure) and
/// looked up by channel ID when messages arrive. This follows the same
/// pattern as HTTP, Timer, WebSocket, and Watch registries — the `RegistryKey`
/// stays pinned to the Lua thread while only `Send`-safe string IDs cross
/// the `HubEvent` channel.
pub type ActionCableCallbackRegistry = Arc<Mutex<HashMap<String, mlua::RegistryKey>>>;

/// Create a new empty callback registry.
#[must_use]
pub fn new_callback_registry() -> ActionCableCallbackRegistry {
    Arc::new(Mutex::new(HashMap::new()))
}

// =============================================================================
// Request types (Lua -> Hub via HubEvent channel)
// =============================================================================

/// Request from Lua to the Hub for ActionCable operations.
///
/// Sent directly via `HubEvent::LuaActionCableRequest` from Lua closures
/// to the Hub event loop. All variants are `Send`-safe — callback keys are
/// stored separately in [`ActionCableCallbackRegistry`].
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
    ///
    /// The callback for this channel is stored in the
    /// [`ActionCableCallbackRegistry`] at subscribe time (keyed by
    /// `channel_id`), not carried in this request.
    Subscribe {
        /// Connection to subscribe on.
        connection_id: String,
        /// Unique channel identifier (e.g., "ac_ch_0").
        channel_id: String,
        /// ActionCable channel class name (e.g., "HubCommandChannel").
        channel_name: String,
        /// Subscription parameters merged into the identifier JSON.
        params: serde_json::Value,
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

/// A Lua-managed channel subscription.
///
/// Owned by the Hub, keyed by channel_id in a `HashMap`. The Lua callback
/// for this channel is stored in the [`ActionCableCallbackRegistry`], not here.
pub struct LuaAcChannel {
    /// The channel handle for receiving messages and performing actions.
    pub handle: ChannelHandle,
    /// The connection this channel belongs to (for crypto lookup).
    pub connection_id: String,
    /// Handle for the forwarding task that reads from `message_rx` and sends
    /// [`HubEvent::AcChannelMessage`]. `None` in test mode (poll-based).
    pub(crate) forwarder_handle: Option<tokio::task::JoinHandle<()>>,
}

impl std::fmt::Debug for LuaAcChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LuaAcChannel")
            .field("handle", &self.handle)
            .field("connection_id", &self.connection_id)
            .field("has_forwarder", &self.forwarder_handle.is_some())
            .finish_non_exhaustive()
    }
}

impl Drop for LuaAcChannel {
    fn drop(&mut self) {
        // Abort the forwarding task when the channel is dropped (unsubscribe/close).
        if let Some(handle) = self.forwarder_handle.take() {
            handle.abort();
        }
    }
}


/// Poll all Lua ActionCable channels for incoming messages and fire callbacks.
///
/// For each channel, drains `handle.try_recv()`. If the channel's connection
/// has `crypto_enabled` and the message has `type == "signal"`, the `envelope`
/// field is automatically decrypted via `CryptoService` before the callback fires.
///
/// Callbacks are looked up from the [`ActionCableCallbackRegistry`] by channel ID,
/// matching the pattern used by HTTP, Timer, WebSocket, and Watch registries.
///
/// # Deadlock Prevention
///
/// Messages are collected first, then callbacks are fired without holding any
/// locks on the channel map or callback registry. Crypto decryption acquires
/// the `CryptoService` mutex briefly per envelope.
///
/// # Returns
///
/// The number of callbacks fired.
pub fn poll_lua_action_cable_channels(
    lua: &Lua,
    channels: &mut HashMap<String, LuaAcChannel>,
    connections: &HashMap<String, LuaAcConnection>,
    callback_registry: &ActionCableCallbackRegistry,
    crypto_service: Option<&CryptoService>,
) -> usize {
    // Phase 1: collect all pending messages with cloned callback keys and channel IDs.
    let mut pending: Vec<(mlua::RegistryKey, serde_json::Value, String)> = Vec::new();

    let registry = callback_registry.lock().expect("ActionCableCallbackRegistry mutex poisoned");

    for (channel_id, channel) in channels.iter_mut() {
        let Some(callback_key) = registry.get(channel_id) else {
            continue;
        };

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

            // Clone the callback key for safe firing outside the lock.
            pending.push((
                lua.create_registry_value(
                    lua.registry_value::<mlua::Function>(callback_key)
                        .expect("ActionCable callback registry key should be valid"),
                )
                .expect("Failed to clone callback registry key"),
                msg,
                channel_id.clone(),
            ));
        }
    }

    // Release the registry lock before firing callbacks.
    drop(registry);

    // Phase 2: fire callbacks
    let count = pending.len();

    for (callback_key, msg, channel_id) in &pending {
        let result: mlua::Result<()> = (|| {
            let callback: mlua::Function = lua.registry_value(callback_key)?;
            let lua_msg = super::json::json_to_lua(lua, msg)?;
            callback.call::<()>((lua_msg, channel_id.as_str()))?;
            Ok(())
        })();

        if let Err(e) = result {
            log::warn!("[ActionCable-Lua] Callback error: {e}");
        }
    }

    // Phase 3: clean up temporary registry keys
    for (callback_key, _, _) in pending {
        let _ = lua.remove_registry_value(callback_key);
    }

    count
}

/// Fire the Lua callback for a single ActionCable channel message.
///
/// Called from [`handle_hub_event`] for [`HubEvent::AcChannelMessage`] events.
/// Looks up the callback from the [`ActionCableCallbackRegistry`] by channel ID,
/// performs crypto decryption if enabled, then fires the callback with
/// `(message, channel_id)`.
///
/// Does nothing if the channel or callback has been removed (unsubscribed
/// between send and receive — benign race).
pub(crate) fn fire_single_ac_message(
    lua: &Lua,
    channels: &HashMap<String, LuaAcChannel>,
    connections: &HashMap<String, LuaAcConnection>,
    callback_registry: &ActionCableCallbackRegistry,
    crypto_service: Option<&CryptoService>,
    channel_id: &str,
    mut message: serde_json::Value,
) {
    let Some(channel) = channels.get(channel_id) else {
        // Channel was unsubscribed between send and receive — benign race.
        return;
    };

    // Phase 1: Look up and clone the callback key under the registry lock.
    let callback_key = {
        let registry = callback_registry
            .lock()
            .expect("ActionCableCallbackRegistry mutex poisoned");
        let Some(key) = registry.get(channel_id) else {
            // Callback removed (unsubscribed) — benign race.
            return;
        };
        match lua.registry_value::<mlua::Function>(key) {
            Ok(cb) => match lua.create_registry_value(cb) {
                Ok(cloned) => cloned,
                Err(e) => {
                    log::warn!("[ActionCable-Lua] Failed to clone callback key for {channel_id}: {e}");
                    return;
                }
            },
            Err(e) => {
                log::warn!("[ActionCable-Lua] Failed to retrieve callback for {channel_id}: {e}");
                return;
            }
        }
    };
    // Registry lock released — safe to call Lua.

    // Auto-decrypt signal envelopes when crypto is enabled for this connection.
    let crypto_enabled = connections
        .get(&channel.connection_id)
        .map_or(false, |c| c.crypto_enabled);

    if crypto_enabled {
        if let Some(msg_type) = message.get("type").and_then(|t| t.as_str()) {
            if msg_type == "signal" {
                if let Some(envelope_val) = message.get("envelope").cloned() {
                    message = decrypt_signal_envelope(
                        &message,
                        &envelope_val,
                        crypto_service,
                        channel_id,
                    );
                }
            }
        }
    }

    // Phase 2: Fire callback.
    let result: mlua::Result<()> = (|| {
        let callback: mlua::Function = lua.registry_value(&callback_key)?;
        let lua_msg = super::json::json_to_lua(lua, &message)?;
        callback.call::<()>((lua_msg, channel_id))?;
        Ok(())
    })();

    if let Err(e) = result {
        log::warn!("[ActionCable-Lua] Callback error for {channel_id}: {e}");
    }

    // Phase 3: Clean up temporary registry key.
    let _ = lua.remove_registry_value(callback_key);
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
        Ok(mut guard) => match guard.decrypt(&envelope, envelope.sender_key.as_deref()) {
            Ok(pt) => pt,
            Err(e) => {
                log::warn!(
                    "[ActionCable-Lua] Channel '{}': decryption failed: {e}",
                    channel_id
                );
                let mut result = msg.clone();
                if let Some(obj) = result.as_object_mut() {
                    obj.insert(
                        "decrypt_failed".to_string(),
                        serde_json::Value::Bool(true),
                    );
                }
                return result;
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

/// Send an ActionCable request via the shared `HubEventSender`.
///
/// Helper used by all Lua closure registrations to send requests to the Hub
/// event loop. Silently drops the request if the sender is not yet set
/// (during early init before `set_hub_event_tx()`).
fn send_ac_event(tx: &HubEventSender, request: ActionCableRequest) {
    let guard = tx.lock().expect("HubEventSender mutex poisoned");
    if let Some(ref sender) = *guard {
        let _ = sender.send(HubEvent::LuaActionCableRequest(request));
    } else {
        ::log::warn!(
            "[ActionCable] request sent before hub_event_tx set — event dropped"
        );
    }
}

/// Register the `action_cable` global table with Lua.
///
/// Creates functions:
/// - `action_cable.connect(opts?)` - Open a new ActionCable connection
/// - `action_cable.subscribe(conn_id, channel_name, params, callback(msg, ch_id))` - Subscribe to a channel
/// - `action_cable.perform(channel_id, action, data)` - Perform an action
/// - `action_cable.unsubscribe(channel_id)` - Unsubscribe from a channel
/// - `action_cable.close(conn_id)` - Close a connection
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub(crate) fn register_action_cable(
    lua: &Lua,
    hub_event_tx: HubEventSender,
    callback_registry: ActionCableCallbackRegistry,
) -> Result<()> {
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
    let tx = Arc::clone(&hub_event_tx);
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

            send_ac_event(&tx, ActionCableRequest::Connect {
                connection_id: connection_id.clone(),
                crypto,
            });

            Ok(connection_id)
        })
        .map_err(|e| anyhow!("Failed to create action_cable.connect function: {e}"))?;

    ac_table
        .set("connect", connect_fn)
        .map_err(|e| anyhow!("Failed to set action_cable.connect: {e}"))?;

    // action_cable.subscribe(conn_id, channel_name, params, callback) -> channel_id
    //
    // Stores the callback in the ActionCableCallbackRegistry (keyed by channel_id)
    // and sends a Subscribe request (without the callback) via HubEvent channel.
    // This matches the pattern used by HTTP, Timer, WebSocket, and Watch registries.
    let tx = Arc::clone(&hub_event_tx);
    let subscribe_counter = Arc::clone(&ch_counter);
    let cb_registry = Arc::clone(&callback_registry);
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

                // Store callback in registry (Lua-thread-pinned).
                {
                    let mut registry = cb_registry
                        .lock()
                        .expect("ActionCableCallbackRegistry mutex poisoned");
                    registry.insert(channel_id.clone(), callback_key);
                }

                // Send request without callback — only Send-safe data crosses the channel.
                send_ac_event(&tx, ActionCableRequest::Subscribe {
                    connection_id: conn_id,
                    channel_id: channel_id.clone(),
                    channel_name,
                    params: params_json,
                });

                Ok(channel_id)
            },
        )
        .map_err(|e| anyhow!("Failed to create action_cable.subscribe function: {e}"))?;

    ac_table
        .set("subscribe", subscribe_fn)
        .map_err(|e| anyhow!("Failed to set action_cable.subscribe: {e}"))?;

    // action_cable.perform(channel_id, action, data)
    let tx = Arc::clone(&hub_event_tx);
    let perform_fn = lua
        .create_function(
            move |lua, (channel_id, action, data): (String, String, Value)| {
                let data_json: serde_json::Value = lua.from_value(data).map_err(|e| {
                    mlua::Error::external(format!(
                        "action_cable.perform: failed to serialize data: {e}"
                    ))
                })?;

                send_ac_event(&tx, ActionCableRequest::Perform {
                    channel_id,
                    action,
                    data: data_json,
                });

                Ok(())
            },
        )
        .map_err(|e| anyhow!("Failed to create action_cable.perform function: {e}"))?;

    ac_table
        .set("perform", perform_fn)
        .map_err(|e| anyhow!("Failed to set action_cable.perform: {e}"))?;

    // action_cable.unsubscribe(channel_id)
    let tx = Arc::clone(&hub_event_tx);
    let unsubscribe_fn = lua
        .create_function(move |_, channel_id: String| {
            send_ac_event(&tx, ActionCableRequest::Unsubscribe { channel_id });
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create action_cable.unsubscribe function: {e}"))?;

    ac_table
        .set("unsubscribe", unsubscribe_fn)
        .map_err(|e| anyhow!("Failed to set action_cable.unsubscribe: {e}"))?;

    // action_cable.close(conn_id)
    let tx = Arc::clone(&hub_event_tx);
    let close_fn = lua
        .create_function(move |_, connection_id: String| {
            send_ac_event(&tx, ActionCableRequest::Close { connection_id });
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
    fn test_action_cable_table_created() {
        let lua = Lua::new();
        let tx = new_hub_event_sender();
        let registry = new_callback_registry();
        register_action_cable(&lua, tx, registry).expect("Should register action_cable primitives");

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
    fn test_connect_sends_event() {
        let lua = Lua::new();
        let (tx, mut rx) = setup_with_channel();
        let registry = new_callback_registry();
        register_action_cable(&lua, tx, registry)
            .expect("Should register action_cable primitives");

        let conn_id: String = lua
            .load(r#"return action_cable.connect({ crypto = true })"#)
            .eval()
            .expect("connect should return a string");

        assert!(
            conn_id.starts_with("ac_conn_"),
            "Connection ID should start with 'ac_conn_', got: {conn_id}"
        );

        let event = rx.try_recv().expect("Should have received an event");
        match event {
            HubEvent::LuaActionCableRequest(ActionCableRequest::Connect {
                connection_id,
                crypto,
            }) => {
                assert_eq!(connection_id, conn_id);
                assert!(crypto);
            }
            _ => panic!("Expected LuaActionCableRequest(Connect), got: {event:?}"),
        }
    }

    #[test]
    fn test_connect_default_no_crypto() {
        let lua = Lua::new();
        let (tx, mut rx) = setup_with_channel();
        let registry = new_callback_registry();
        register_action_cable(&lua, tx, registry)
            .expect("Should register action_cable primitives");

        let _: String = lua
            .load(r#"return action_cable.connect()"#)
            .eval()
            .expect("connect without opts should work");

        let event = rx.try_recv().expect("Should have received an event");
        match event {
            HubEvent::LuaActionCableRequest(ActionCableRequest::Connect { crypto, .. }) => {
                assert!(!crypto, "Default crypto should be false");
            }
            _ => panic!("Expected LuaActionCableRequest(Connect)"),
        }
    }

    #[test]
    fn test_subscribe_sends_event_and_stores_callback() {
        let lua = Lua::new();
        let (tx, mut rx) = setup_with_channel();
        let registry = new_callback_registry();
        register_action_cable(&lua, tx, Arc::clone(&registry))
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

        let event = rx.try_recv().expect("Should have received an event");
        match event {
            HubEvent::LuaActionCableRequest(ActionCableRequest::Subscribe {
                connection_id,
                channel_id,
                channel_name,
                params,
            }) => {
                assert_eq!(connection_id, "ac_conn_0");
                assert_eq!(channel_id, ch_id);
                assert_eq!(channel_name, "HubCommandChannel");
                assert_eq!(params["hub_id"], "test-hub");
                assert_eq!(params["start_from"], 0);
            }
            _ => panic!("Expected LuaActionCableRequest(Subscribe)"),
        }

        // Verify callback was stored in the registry (not in the event)
        let reg = registry.lock().unwrap();
        assert!(reg.contains_key(&ch_id), "Callback should be stored in registry");
    }

    #[test]
    fn test_perform_sends_event() {
        let lua = Lua::new();
        let (tx, mut rx) = setup_with_channel();
        let registry = new_callback_registry();
        register_action_cable(&lua, tx, registry)
            .expect("Should register action_cable primitives");

        lua.load(r#"action_cable.perform("ac_ch_0", "ack", { sequence = 42 })"#)
            .exec()
            .expect("perform should succeed");

        let event = rx.try_recv().expect("Should have received an event");
        match event {
            HubEvent::LuaActionCableRequest(ActionCableRequest::Perform {
                channel_id,
                action,
                data,
            }) => {
                assert_eq!(channel_id, "ac_ch_0");
                assert_eq!(action, "ack");
                assert_eq!(data["sequence"], 42);
            }
            _ => panic!("Expected LuaActionCableRequest(Perform)"),
        }
    }

    #[test]
    fn test_unsubscribe_sends_event() {
        let lua = Lua::new();
        let (tx, mut rx) = setup_with_channel();
        let registry = new_callback_registry();
        register_action_cable(&lua, tx, registry)
            .expect("Should register action_cable primitives");

        lua.load(r#"action_cable.unsubscribe("ac_ch_0")"#)
            .exec()
            .expect("unsubscribe should succeed");

        let event = rx.try_recv().expect("Should have received an event");
        assert!(matches!(
            event,
            HubEvent::LuaActionCableRequest(ActionCableRequest::Unsubscribe { channel_id }) if channel_id == "ac_ch_0"
        ));
    }

    #[test]
    fn test_close_sends_event() {
        let lua = Lua::new();
        let (tx, mut rx) = setup_with_channel();
        let registry = new_callback_registry();
        register_action_cable(&lua, tx, registry)
            .expect("Should register action_cable primitives");

        lua.load(r#"action_cable.close("ac_conn_0")"#)
            .exec()
            .expect("close should succeed");

        let event = rx.try_recv().expect("Should have received an event");
        assert!(matches!(
            event,
            HubEvent::LuaActionCableRequest(ActionCableRequest::Close { connection_id }) if connection_id == "ac_conn_0"
        ));
    }

    #[test]
    fn test_sequential_ids_increment() {
        let lua = Lua::new();
        let (tx, _rx) = setup_with_channel();
        let registry = new_callback_registry();
        register_action_cable(&lua, tx, registry)
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

        let registry = new_callback_registry();
        let count = poll_lua_action_cable_channels(&lua, &mut channels, &connections, &registry, None);
        assert_eq!(count, 0);
    }
}
