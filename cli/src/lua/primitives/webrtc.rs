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
//! # Send Queue
//!
//! Messages sent via `webrtc.send()` are queued and processed by the Hub
//! after the Lua callback returns. This avoids async complexity in Lua.

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use mlua::prelude::*;

/// Request to send a message via WebRTC.
///
/// Queued by Lua's `webrtc.send()` and processed by Hub.
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

/// Shared send queue for WebRTC messages from Lua.
pub type WebRtcSendQueue = Arc<Mutex<Vec<WebRtcSendRequest>>>;

/// Create a new send queue for WebRTC messages.
#[must_use]
pub fn new_send_queue() -> WebRtcSendQueue {
    Arc::new(Mutex::new(Vec::new()))
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
/// * `send_queue` - Queue for outgoing messages (processed by Hub)
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub fn register(lua: &Lua, send_queue: WebRtcSendQueue) -> Result<()> {
    let webrtc_table = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create webrtc table: {e}"))?;

    // webrtc.on_peer_connected(callback)
    let on_connected_fn = lua
        .create_function(|lua, callback: LuaFunction| {
            let key = lua.create_registry_value(callback)?;
            lua.set_named_registry_value(registry_keys::ON_PEER_CONNECTED, key)?;
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create webrtc.on_peer_connected function: {e}"))?;
    webrtc_table
        .set("on_peer_connected", on_connected_fn)
        .map_err(|e| anyhow!("Failed to set webrtc.on_peer_connected: {e}"))?;

    // webrtc.on_peer_disconnected(callback)
    let on_disconnected_fn = lua
        .create_function(|lua, callback: LuaFunction| {
            let key = lua.create_registry_value(callback)?;
            lua.set_named_registry_value(registry_keys::ON_PEER_DISCONNECTED, key)?;
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create webrtc.on_peer_disconnected function: {e}"))?;
    webrtc_table
        .set("on_peer_disconnected", on_disconnected_fn)
        .map_err(|e| anyhow!("Failed to set webrtc.on_peer_disconnected: {e}"))?;

    // webrtc.on_message(callback)
    let on_message_fn = lua
        .create_function(|lua, callback: LuaFunction| {
            let key = lua.create_registry_value(callback)?;
            lua.set_named_registry_value(registry_keys::ON_MESSAGE, key)?;
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create webrtc.on_message function: {e}"))?;
    webrtc_table
        .set("on_message", on_message_fn)
        .map_err(|e| anyhow!("Failed to set webrtc.on_message: {e}"))?;

    // webrtc.send(peer_id, table)
    let send_queue_clone = Arc::clone(&send_queue);
    let send_fn = lua
        .create_function(move |lua, (peer_id, value): (String, LuaValue)| {
            // Convert Lua value to JSON
            let json: serde_json::Value = lua.from_value(value)?;

            // Queue the send request
            let mut queue = send_queue_clone.lock()
                .expect("WebRTC send queue mutex poisoned");
            queue.push(WebRtcSendRequest::Json {
                peer_id,
                data: json,
            });

            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create webrtc.send function: {e}"))?;
    webrtc_table
        .set("send", send_fn)
        .map_err(|e| anyhow!("Failed to set webrtc.send: {e}"))?;

    // webrtc.send_binary(peer_id, data)
    let send_queue_clone = Arc::clone(&send_queue);
    let send_binary_fn = lua
        .create_function(move |_, (peer_id, data): (String, LuaString)| {
            let bytes = data.as_bytes().to_vec();

            // Queue the send request
            let mut queue = send_queue_clone.lock()
                .expect("WebRTC send queue mutex poisoned");
            queue.push(WebRtcSendRequest::Binary {
                peer_id,
                data: bytes,
            });

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

    #[test]
    fn test_webrtc_table_created() {
        let lua = Lua::new();
        let queue = new_send_queue();
        register(&lua, queue).expect("Should register webrtc primitives");

        let globals = lua.globals();
        let webrtc_table: mlua::Table = globals.get("webrtc").expect("webrtc table should exist");

        // Verify all functions exist
        let _: mlua::Function = webrtc_table.get("on_peer_connected").expect("on_peer_connected should exist");
        let _: mlua::Function = webrtc_table.get("on_peer_disconnected").expect("on_peer_disconnected should exist");
        let _: mlua::Function = webrtc_table.get("on_message").expect("on_message should exist");
        let _: mlua::Function = webrtc_table.get("send").expect("send should exist");
        let _: mlua::Function = webrtc_table.get("send_binary").expect("send_binary should exist");
    }

    #[test]
    fn test_on_peer_connected_stores_callback() {
        let lua = Lua::new();
        let queue = new_send_queue();
        register(&lua, queue).expect("Should register webrtc primitives");

        lua.load(r#"
            webrtc.on_peer_connected(function(peer_id)
                log_result = "connected: " .. peer_id
            end)
        "#).exec().expect("Should register callback");

        // Verify callback is stored in registry
        let key: mlua::RegistryKey = lua
            .named_registry_value(registry_keys::ON_PEER_CONNECTED)
            .expect("Callback should be stored in registry");

        let callback: mlua::Function = lua.registry_value(&key).expect("Should retrieve callback");

        // Test that we can call it
        lua.globals().set("log_result", LuaValue::Nil).unwrap();
        callback.call::<()>("test-peer").expect("Should call callback");

        let result: String = lua.globals().get("log_result").expect("log_result should be set");
        assert_eq!(result, "connected: test-peer");
    }

    #[test]
    fn test_send_queues_json_message() {
        let lua = Lua::new();
        let queue = new_send_queue();
        register(&lua, Arc::clone(&queue)).expect("Should register webrtc primitives");

        lua.load(r#"
            webrtc.send("peer-123", { type = "ping", value = 42 })
        "#).exec().expect("Should send message");

        // Verify message is queued
        let pending = queue.lock()
            .expect("WebRTC send queue mutex poisoned");
        assert_eq!(pending.len(), 1);

        match &pending[0] {
            WebRtcSendRequest::Json { peer_id, data } => {
                assert_eq!(peer_id, "peer-123");
                assert_eq!(data["type"], "ping");
                assert_eq!(data["value"], 42);
            }
            _ => panic!("Expected Json request"),
        }
    }

    #[test]
    fn test_send_binary_queues_binary_message() {
        let lua = Lua::new();
        let queue = new_send_queue();
        register(&lua, Arc::clone(&queue)).expect("Should register webrtc primitives");

        lua.load(r#"
            webrtc.send_binary("peer-456", "hello bytes")
        "#).exec().expect("Should send binary");

        // Verify message is queued
        let pending = queue.lock()
            .expect("WebRTC send queue mutex poisoned");
        assert_eq!(pending.len(), 1);

        match &pending[0] {
            WebRtcSendRequest::Binary { peer_id, data } => {
                assert_eq!(peer_id, "peer-456");
                assert_eq!(data, b"hello bytes");
            }
            _ => panic!("Expected Binary request"),
        }
    }

    #[test]
    fn test_multiple_sends_queue_in_order() {
        let lua = Lua::new();
        let queue = new_send_queue();
        register(&lua, Arc::clone(&queue)).expect("Should register webrtc primitives");

        lua.load(r#"
            webrtc.send("peer-1", { msg = "first" })
            webrtc.send("peer-2", { msg = "second" })
            webrtc.send_binary("peer-3", "third")
        "#).exec().expect("Should send messages");

        let pending = queue.lock()
            .expect("WebRTC send queue mutex poisoned");
        assert_eq!(pending.len(), 3);

        // Verify order is preserved
        match &pending[0] {
            WebRtcSendRequest::Json { peer_id, .. } => assert_eq!(peer_id, "peer-1"),
            _ => panic!("Expected Json"),
        }
        match &pending[1] {
            WebRtcSendRequest::Json { peer_id, .. } => assert_eq!(peer_id, "peer-2"),
            _ => panic!("Expected Json"),
        }
        match &pending[2] {
            WebRtcSendRequest::Binary { peer_id, .. } => assert_eq!(peer_id, "peer-3"),
            _ => panic!("Expected Binary"),
        }
    }
}
