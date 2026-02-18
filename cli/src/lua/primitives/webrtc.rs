//! WebRTC primitives for Lua scripts.
//!
//! Exposes WebRTC connection and message handling to Lua, allowing scripts
//! to receive peer events and messages, and send responses.
//!
//! # Usage in Lua
//!
//! ```lua
//! -- Register callbacks for peer events
//! webrtc.on_peer_connected(function(peer_id)
//!     log.info("Peer connected: " .. peer_id)
//! end)
//!
//! webrtc.on_peer_disconnected(function(peer_id)
//!     log.info("Peer disconnected: " .. peer_id)
//! end)
//!
//! -- Register callback for messages
//! webrtc.on_message(function(peer_id, message)
//!     log.debug("Message from " .. peer_id .. ": type=" .. tostring(message.type))
//!     if message.type == "ping" then
//!         webrtc.send(peer_id, { type = "pong" })
//!     end
//! end)
//! ```
//!
//! # Event-Driven Delivery
//!
//! Messages sent via `webrtc.send()` are delivered directly to the Hub event
//! loop as `HubEvent::WebRtcSend` events via the shared `HubEventSender`.

use anyhow::{anyhow, Result};
use mlua::prelude::*;

use super::HubEventSender;
use crate::hub::events::HubEvent;

/// Request to send a message via WebRTC.
///
/// Sent from Lua's `webrtc.send()` as `HubEvent::WebRtcSend` and processed by Hub.
#[derive(Debug, Clone)]
pub enum WebRtcSendRequest {
    /// Send JSON data to a peer.
    Json {
        /// Target peer identifier.
        peer_id: String,
        /// JSON data to send.
        data: serde_json::Value,
    },
    /// Send binary data to a peer (still encrypted).
    Binary {
        /// Target peer identifier.
        peer_id: String,
        /// Binary data to send.
        data: Vec<u8>,
    },
}

/// Registry keys for WebRTC callbacks.
///
/// These constants are used to store and retrieve Lua callback functions
/// from the mlua registry.
pub mod registry_keys {
    /// Registry key for the peer connected callback.
    pub const ON_PEER_CONNECTED: &str = "webrtc_on_peer_connected";
    /// Registry key for the peer disconnected callback.
    pub const ON_PEER_DISCONNECTED: &str = "webrtc_on_peer_disconnected";
    /// Registry key for the message received callback.
    pub const ON_MESSAGE: &str = "webrtc_on_message";
}

/// Register the `webrtc` table with WebRTC primitives.
///
/// Creates a global `webrtc` table with methods:
/// - `webrtc.on_peer_connected(callback)` - Register peer connected callback
/// - `webrtc.on_peer_disconnected(callback)` - Register peer disconnected callback
/// - `webrtc.on_message(callback)` - Register message received callback
/// - `webrtc.send(peer_id, table)` - Send JSON message to peer
/// - `webrtc.send_binary(peer_id, data)` - Send binary data to peer
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `hub_event_tx` - Shared sender for Hub events (filled in later by `set_hub_event_tx`)
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub(crate) fn register(lua: &Lua, hub_event_tx: HubEventSender) -> Result<()> {
    let webrtc_table = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create webrtc table: {e}"))?;

    // webrtc.on_peer_connected(callback)
    let on_connected_fn = lua
        .create_function(|lua, callback: LuaFunction| {
            lua.set_named_registry_value(registry_keys::ON_PEER_CONNECTED, callback)?;
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create webrtc.on_peer_connected function: {e}"))?;
    webrtc_table
        .set("on_peer_connected", on_connected_fn)
        .map_err(|e| anyhow!("Failed to set webrtc.on_peer_connected: {e}"))?;

    // webrtc.on_peer_disconnected(callback)
    let on_disconnected_fn = lua
        .create_function(|lua, callback: LuaFunction| {
            lua.set_named_registry_value(registry_keys::ON_PEER_DISCONNECTED, callback)?;
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create webrtc.on_peer_disconnected function: {e}"))?;
    webrtc_table
        .set("on_peer_disconnected", on_disconnected_fn)
        .map_err(|e| anyhow!("Failed to set webrtc.on_peer_disconnected: {e}"))?;

    // webrtc.on_message(callback)
    let on_message_fn = lua
        .create_function(|lua, callback: LuaFunction| {
            lua.set_named_registry_value(registry_keys::ON_MESSAGE, callback)?;
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create webrtc.on_message function: {e}"))?;
    webrtc_table
        .set("on_message", on_message_fn)
        .map_err(|e| anyhow!("Failed to set webrtc.on_message: {e}"))?;

    // webrtc.send(peer_id, table)
    let tx = hub_event_tx.clone();
    let send_fn = lua
        .create_function(move |lua, (peer_id, value): (String, LuaValue)| {
            let json: serde_json::Value = lua.from_value(value)?;
            let guard = tx.lock().expect("HubEventSender mutex poisoned");
            if let Some(ref sender) = *guard {
                let _ = sender.send(HubEvent::WebRtcSend(WebRtcSendRequest::Json {
                    peer_id,
                    data: json,
                }));
            } else {
                ::log::warn!("[WebRTC] send() called before hub_event_tx set — event dropped");
            }
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create webrtc.send function: {e}"))?;
    webrtc_table
        .set("send", send_fn)
        .map_err(|e| anyhow!("Failed to set webrtc.send: {e}"))?;

    // webrtc.send_binary(peer_id, data)
    let tx = hub_event_tx;
    let send_binary_fn = lua
        .create_function(move |_, (peer_id, data): (String, LuaString)| {
            let bytes = data.as_bytes().to_vec();
            let guard = tx.lock().expect("HubEventSender mutex poisoned");
            if let Some(ref sender) = *guard {
                let _ = sender.send(HubEvent::WebRtcSend(WebRtcSendRequest::Binary {
                    peer_id,
                    data: bytes,
                }));
            } else {
                ::log::warn!("[WebRTC] send_binary() called before hub_event_tx set — event dropped");
            }
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create webrtc.send_binary function: {e}"))?;
    webrtc_table
        .set("send_binary", send_binary_fn)
        .map_err(|e| anyhow!("Failed to set webrtc.send_binary: {e}"))?;

    // Register the table globally
    lua.globals()
        .set("webrtc", webrtc_table)
        .map_err(|e| anyhow!("Failed to register webrtc table globally: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::new_hub_event_sender;

    fn setup() -> (Lua, HubEventSender) {
        let lua = Lua::new();
        let tx = new_hub_event_sender();
        register(&lua, tx.clone()).expect("Should register webrtc primitives");
        (lua, tx)
    }

    /// Wire up an actual channel so send() events are captured.
    fn setup_with_channel() -> (Lua, tokio::sync::mpsc::UnboundedReceiver<HubEvent>) {
        let lua = Lua::new();
        let tx = new_hub_event_sender();
        register(&lua, tx.clone()).expect("Should register webrtc primitives");
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        *tx.lock().unwrap() = Some(sender);
        (lua, receiver)
    }

    #[test]
    fn test_webrtc_table_created() {
        let (lua, _tx) = setup();

        let globals = lua.globals();
        let webrtc_table: mlua::Table = globals.get("webrtc").expect("webrtc table should exist");

        let _: mlua::Function = webrtc_table.get("on_peer_connected").expect("on_peer_connected should exist");
        let _: mlua::Function = webrtc_table.get("on_peer_disconnected").expect("on_peer_disconnected should exist");
        let _: mlua::Function = webrtc_table.get("on_message").expect("on_message should exist");
        let _: mlua::Function = webrtc_table.get("send").expect("send should exist");
        let _: mlua::Function = webrtc_table.get("send_binary").expect("send_binary should exist");
    }

    #[test]
    fn test_on_peer_connected_stores_callback() {
        let (lua, _tx) = setup();

        lua.load(r#"
            webrtc.on_peer_connected(function(peer_id)
                log_result = "connected: " .. peer_id
            end)
        "#).exec().expect("Should register callback");

        let key: mlua::RegistryKey = lua
            .named_registry_value(registry_keys::ON_PEER_CONNECTED)
            .expect("Callback should be stored in registry");

        let callback: mlua::Function = lua.registry_value(&key).expect("Should retrieve callback");

        lua.globals().set("log_result", LuaValue::Nil).unwrap();
        callback.call::<()>("test-peer").expect("Should call callback");

        let result: String = lua.globals().get("log_result").expect("log_result should be set");
        assert_eq!(result, "connected: test-peer");
    }

    #[test]
    fn test_send_delivers_json_event() {
        let (lua, mut rx) = setup_with_channel();

        lua.load(r#"
            webrtc.send("peer-123", { type = "ping", value = 42 })
        "#).exec().expect("Should send message");

        let event = rx.try_recv().expect("Should have received event");
        match event {
            HubEvent::WebRtcSend(WebRtcSendRequest::Json { peer_id, data }) => {
                assert_eq!(peer_id, "peer-123");
                assert_eq!(data["type"], "ping");
                assert_eq!(data["value"], 42);
            }
            _ => panic!("Expected WebRtcSend Json event"),
        }
    }

    #[test]
    fn test_send_binary_delivers_binary_event() {
        let (lua, mut rx) = setup_with_channel();

        lua.load(r#"
            webrtc.send_binary("peer-456", "hello bytes")
        "#).exec().expect("Should send binary");

        let event = rx.try_recv().expect("Should have received event");
        match event {
            HubEvent::WebRtcSend(WebRtcSendRequest::Binary { peer_id, data }) => {
                assert_eq!(peer_id, "peer-456");
                assert_eq!(data, b"hello bytes");
            }
            _ => panic!("Expected WebRtcSend Binary event"),
        }
    }

    #[test]
    fn test_multiple_sends_deliver_in_order() {
        let (lua, mut rx) = setup_with_channel();

        lua.load(r#"
            webrtc.send("peer-1", { msg = "first" })
            webrtc.send("peer-2", { msg = "second" })
            webrtc.send_binary("peer-3", "third")
        "#).exec().expect("Should send messages");

        match rx.try_recv().unwrap() {
            HubEvent::WebRtcSend(WebRtcSendRequest::Json { peer_id, .. }) => assert_eq!(peer_id, "peer-1"),
            _ => panic!("Expected Json"),
        }
        match rx.try_recv().unwrap() {
            HubEvent::WebRtcSend(WebRtcSendRequest::Json { peer_id, .. }) => assert_eq!(peer_id, "peer-2"),
            _ => panic!("Expected Json"),
        }
        match rx.try_recv().unwrap() {
            HubEvent::WebRtcSend(WebRtcSendRequest::Binary { peer_id, .. }) => assert_eq!(peer_id, "peer-3"),
            _ => panic!("Expected Binary"),
        }
    }

    #[test]
    fn test_send_before_tx_set_does_not_panic() {
        let (lua, _tx) = setup();
        // tx is None — send should silently drop, not panic
        lua.load(r#"
            webrtc.send("peer-1", { msg = "dropped" })
        "#).exec().expect("Should not panic when tx is None");
    }
}
