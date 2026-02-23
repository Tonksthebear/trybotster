//! Socket IPC primitives for Lua scripts.
//!
//! Exposes Unix domain socket client connection handling to Lua,
//! allowing scripts to receive socket client events and send messages.
//! Follows the WebRTC multi-peer pattern with `client_id` routing.
//!
//! # Usage in Lua
//!
//! ```lua
//! socket.on_client_connected(function(client_id)
//!     log.info("Socket client connected: " .. client_id)
//! end)
//!
//! socket.on_client_disconnected(function(client_id)
//!     log.info("Socket client disconnected: " .. client_id)
//! end)
//!
//! socket.on_message(function(client_id, message)
//!     if message.type == "subscribe" then
//!         -- handle subscription
//!     end
//! end)
//!
//! socket.send(client_id, { type = "agent_list", agents = {} })
//! socket.send_binary(client_id, binary_data)
//! ```
//!
//! # Event-Driven Delivery
//!
//! Messages sent via `socket.send()` are delivered to the Hub event loop
//! as `HubEvent::SocketSend` events via the shared `HubEventSender`.

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use mlua::prelude::*;

use super::HubEventSender;
use super::pty::{CreateSocketForwarderRequest, PtyForwarder, PtyRequest};
use crate::hub::events::HubEvent;

/// Request to send a message to a socket client.
///
/// Sent from Lua's `socket.send()` as `HubEvent::SocketSend` and processed by Hub.
#[derive(Debug, Clone)]
pub enum SocketSendRequest {
    /// Send JSON data to a socket client.
    Json {
        /// Target client identifier.
        client_id: String,
        /// JSON data to send.
        data: serde_json::Value,
    },
    /// Send binary data to a socket client.
    Binary {
        /// Target client identifier.
        client_id: String,
        /// Binary data to send.
        data: Vec<u8>,
    },
}

/// Registry keys for socket callbacks.
pub mod registry_keys {
    /// Registry key for the client connected callback.
    pub const ON_CLIENT_CONNECTED: &str = "socket_on_client_connected";
    /// Registry key for the client disconnected callback.
    pub const ON_CLIENT_DISCONNECTED: &str = "socket_on_client_disconnected";
    /// Registry key for the message received callback.
    pub const ON_MESSAGE: &str = "socket_on_message";
}

/// Register the `socket` table with socket IPC primitives.
///
/// Creates a global `socket` table with methods:
/// - `socket.on_client_connected(callback)` - Register client connected callback
/// - `socket.on_client_disconnected(callback)` - Register client disconnected callback
/// - `socket.on_message(callback)` - Register message received callback
/// - `socket.send(client_id, table)` - Send JSON message to client
/// - `socket.send_binary(client_id, data)` - Send binary data to client
pub(crate) fn register(lua: &Lua, hub_event_tx: HubEventSender) -> Result<()> {
    let socket_table = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create socket table: {e}"))?;

    // socket.on_client_connected(callback)
    let on_connected_fn = lua
        .create_function(|lua, callback: LuaFunction| {
            lua.set_named_registry_value(registry_keys::ON_CLIENT_CONNECTED, callback)?;
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create socket.on_client_connected function: {e}"))?;
    socket_table
        .set("on_client_connected", on_connected_fn)
        .map_err(|e| anyhow!("Failed to set socket.on_client_connected: {e}"))?;

    // socket.on_client_disconnected(callback)
    let on_disconnected_fn = lua
        .create_function(|lua, callback: LuaFunction| {
            lua.set_named_registry_value(registry_keys::ON_CLIENT_DISCONNECTED, callback)?;
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create socket.on_client_disconnected function: {e}"))?;
    socket_table
        .set("on_client_disconnected", on_disconnected_fn)
        .map_err(|e| anyhow!("Failed to set socket.on_client_disconnected: {e}"))?;

    // socket.on_message(callback)
    let on_message_fn = lua
        .create_function(|lua, callback: LuaFunction| {
            lua.set_named_registry_value(registry_keys::ON_MESSAGE, callback)?;
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create socket.on_message function: {e}"))?;
    socket_table
        .set("on_message", on_message_fn)
        .map_err(|e| anyhow!("Failed to set socket.on_message: {e}"))?;

    // socket.send(client_id, table) — with peer_id routing
    let tx = hub_event_tx.clone();
    let send_fn = lua
        .create_function(move |lua, (client_id, value): (String, LuaValue)| {
            let json: serde_json::Value = lua.from_value(value)?;
            let guard = tx.lock().expect("HubEventSender mutex poisoned");
            if let Some(ref sender) = *guard {
                let _ = sender.send(HubEvent::SocketSend(SocketSendRequest::Json {
                    client_id,
                    data: json,
                }));
            } else {
                ::log::warn!("[Socket] send() called before hub_event_tx set — event dropped");
            }
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create socket.send function: {e}"))?;
    socket_table
        .set("send", send_fn)
        .map_err(|e| anyhow!("Failed to set socket.send: {e}"))?;

    // socket.send_binary(client_id, data) — with peer_id routing
    let hub_event_tx_for_pty = hub_event_tx.clone();
    let tx = hub_event_tx;
    let send_binary_fn = lua
        .create_function(move |_, (client_id, data): (String, LuaString)| {
            let bytes = data.as_bytes().to_vec();
            let guard = tx.lock().expect("HubEventSender mutex poisoned");
            if let Some(ref sender) = *guard {
                let _ = sender.send(HubEvent::SocketSend(SocketSendRequest::Binary {
                    client_id,
                    data: bytes,
                }));
            } else {
                ::log::warn!("[Socket] send_binary() called before hub_event_tx set — event dropped");
            }
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create socket.send_binary function: {e}"))?;
    socket_table
        .set("send_binary", send_binary_fn)
        .map_err(|e| anyhow!("Failed to set socket.send_binary: {e}"))?;

    // socket.create_pty_forwarder({ client_id, agent_index, pty_index, subscription_id })
    //
    // Creates a PTY forwarder that streams output as Frame::PtyOutput to a socket client.
    let tx_fwd = hub_event_tx_for_pty.clone();
    let create_forwarder_fn = lua
        .create_function(move |_lua, opts: LuaTable| {
            let client_id: String = opts
                .get("client_id")
                .map_err(|_| LuaError::runtime("client_id is required"))?;
            let agent_index: usize = opts
                .get("agent_index")
                .map_err(|_| LuaError::runtime("agent_index is required"))?;
            let pty_index: usize = opts
                .get("pty_index")
                .map_err(|_| LuaError::runtime("pty_index is required"))?;
            let subscription_id: String = opts
                .get("subscription_id")
                .map_err(|_| LuaError::runtime("subscription_id is required"))?;

            let forwarder_id = format!("{}:{}:{}", client_id, agent_index, pty_index);
            let active_flag = Arc::new(Mutex::new(true));

            let guard = tx_fwd.lock().expect("HubEventSender mutex poisoned");
            if let Some(ref sender) = *guard {
                let _ = sender.send(HubEvent::LuaPtyRequest(
                    PtyRequest::CreateSocketForwarder(CreateSocketForwarderRequest {
                        client_id: client_id.clone(),
                        agent_index,
                        pty_index,
                        subscription_id,
                        active_flag: Arc::clone(&active_flag),
                    }),
                ));
            }

            Ok(PtyForwarder {
                id: forwarder_id,
                peer_id: client_id,
                agent_index,
                pty_index,
                active: active_flag,
            })
        })
        .map_err(|e| anyhow!("Failed to create socket.create_pty_forwarder function: {e}"))?;
    socket_table
        .set("create_pty_forwarder", create_forwarder_fn)
        .map_err(|e| anyhow!("Failed to set socket.create_pty_forwarder: {e}"))?;

    // Register the table globally
    lua.globals()
        .set("socket", socket_table)
        .map_err(|e| anyhow!("Failed to register socket table globally: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::new_hub_event_sender;

    fn setup() -> (Lua, HubEventSender) {
        let lua = Lua::new();
        let tx = new_hub_event_sender();
        register(&lua, tx.clone()).expect("Should register socket primitives");
        (lua, tx)
    }

    fn setup_with_channel() -> (Lua, tokio::sync::mpsc::UnboundedReceiver<HubEvent>) {
        let lua = Lua::new();
        let tx = new_hub_event_sender();
        register(&lua, tx.clone()).expect("Should register socket primitives");
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        *tx.lock().unwrap() = Some(sender);
        (lua, receiver)
    }

    #[test]
    fn test_socket_table_created() {
        let (lua, _tx) = setup();

        let globals = lua.globals();
        let socket_table: mlua::Table =
            globals.get("socket").expect("socket table should exist");

        let _: mlua::Function = socket_table.get("on_client_connected").expect("on_client_connected should exist");
        let _: mlua::Function = socket_table.get("on_client_disconnected").expect("on_client_disconnected should exist");
        let _: mlua::Function = socket_table.get("on_message").expect("on_message should exist");
        let _: mlua::Function = socket_table.get("send").expect("send should exist");
        let _: mlua::Function = socket_table.get("send_binary").expect("send_binary should exist");
    }

    #[test]
    fn test_send_delivers_json_event() {
        let (lua, mut rx) = setup_with_channel();

        lua.load(r#"
            socket.send("socket:abc123", { type = "agent_list", count = 3 })
        "#).exec().expect("Should send message");

        let event = rx.try_recv().expect("Should have received event");
        match event {
            HubEvent::SocketSend(SocketSendRequest::Json { client_id, data }) => {
                assert_eq!(client_id, "socket:abc123");
                assert_eq!(data["type"], "agent_list");
                assert_eq!(data["count"], 3);
            }
            _ => panic!("Expected SocketSend Json event"),
        }
    }

    #[test]
    fn test_send_binary_delivers_binary_event() {
        let (lua, mut rx) = setup_with_channel();

        lua.load(r#"
            socket.send_binary("socket:abc123", "hello bytes")
        "#).exec().expect("Should send binary");

        let event = rx.try_recv().expect("Should have received event");
        match event {
            HubEvent::SocketSend(SocketSendRequest::Binary { client_id, data }) => {
                assert_eq!(client_id, "socket:abc123");
                assert_eq!(data, b"hello bytes");
            }
            _ => panic!("Expected SocketSend Binary event"),
        }
    }
}
