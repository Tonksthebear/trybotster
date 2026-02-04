//! TUI primitives for Lua scripts.
//!
//! Exposes TUI connection and message handling to Lua, allowing scripts
//! to receive TUI events and send messages back to the terminal UI.
//!
//! Unlike WebRTC, there is only ever one TUI client, so send functions
//! do not require a `peer_id` parameter.
//!
//! # Usage in Lua
//!
//! ```lua
//! -- Register callback for TUI ready
//! tui.on_connected(function()
//!     log.info("TUI connected")
//! end)
//!
//! -- Register callback for TUI shutdown
//! tui.on_disconnected(function()
//!     log.info("TUI disconnected")
//! end)
//!
//! -- Register callback for messages from TUI
//! tui.on_message(function(message)
//!     log.debug("TUI message: type=" .. tostring(message.type))
//!     if message.type == "list_agents" then
//!         tui.send({ type = "agent_list", agents = hub.get_agents() })
//!     end
//! end)
//! ```
//!
//! # Send Queue
//!
//! Messages sent via `tui.send()` are queued and processed by the Hub
//! after the Lua callback returns. This avoids async complexity in Lua.

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use mlua::prelude::*;

/// Request to send a message to the TUI.
///
/// Queued by Lua's `tui.send()` and processed by Hub.
#[derive(Debug, Clone)]
pub enum TuiSendRequest {
    /// Send JSON data to the TUI.
    Json {
        /// JSON data to send.
        data: serde_json::Value,
    },
    /// Send binary data to the TUI.
    Binary {
        /// Binary data to send.
        data: Vec<u8>,
    },
}

/// Shared send queue for TUI messages from Lua.
pub type TuiSendQueue = Arc<Mutex<Vec<TuiSendRequest>>>;

/// Create a new send queue for TUI messages.
#[must_use]
pub fn new_send_queue() -> TuiSendQueue {
    Arc::new(Mutex::new(Vec::new()))
}

/// Registry keys for TUI callbacks.
///
/// These constants are used to store and retrieve Lua callback functions
/// from the mlua registry.
pub mod registry_keys {
    /// Registry key for the TUI connected callback.
    pub const ON_CONNECTED: &str = "tui_on_connected";
    /// Registry key for the TUI disconnected callback.
    pub const ON_DISCONNECTED: &str = "tui_on_disconnected";
    /// Registry key for the message received callback.
    pub const ON_MESSAGE: &str = "tui_on_message";
}

/// Register the `tui` table with TUI primitives.
///
/// Creates a global `tui` table with methods:
/// - `tui.on_connected(callback)` - Register TUI connected callback
/// - `tui.on_disconnected(callback)` - Register TUI disconnected callback
/// - `tui.on_message(callback)` - Register message received callback
/// - `tui.send(table)` - Send JSON message to TUI
/// - `tui.send_binary(data)` - Send binary data to TUI
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `send_queue` - Queue for outgoing messages (processed by Hub)
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub fn register(lua: &Lua, send_queue: TuiSendQueue) -> Result<()> {
    let tui_table = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create tui table: {e}"))?;

    // tui.on_connected(callback)
    let on_connected_fn = lua
        .create_function(|lua, callback: LuaFunction| {
            let key = lua.create_registry_value(callback)?;
            lua.set_named_registry_value(registry_keys::ON_CONNECTED, key)?;
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create tui.on_connected function: {e}"))?;
    tui_table
        .set("on_connected", on_connected_fn)
        .map_err(|e| anyhow!("Failed to set tui.on_connected: {e}"))?;

    // tui.on_disconnected(callback)
    let on_disconnected_fn = lua
        .create_function(|lua, callback: LuaFunction| {
            let key = lua.create_registry_value(callback)?;
            lua.set_named_registry_value(registry_keys::ON_DISCONNECTED, key)?;
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create tui.on_disconnected function: {e}"))?;
    tui_table
        .set("on_disconnected", on_disconnected_fn)
        .map_err(|e| anyhow!("Failed to set tui.on_disconnected: {e}"))?;

    // tui.on_message(callback)
    let on_message_fn = lua
        .create_function(|lua, callback: LuaFunction| {
            let key = lua.create_registry_value(callback)?;
            lua.set_named_registry_value(registry_keys::ON_MESSAGE, key)?;
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create tui.on_message function: {e}"))?;
    tui_table
        .set("on_message", on_message_fn)
        .map_err(|e| anyhow!("Failed to set tui.on_message: {e}"))?;

    // tui.send(table) — no peer_id, single TUI client
    let send_queue_clone = Arc::clone(&send_queue);
    let send_fn = lua
        .create_function(move |lua, value: LuaValue| {
            let json: serde_json::Value = lua.from_value(value)?;

            let mut queue = send_queue_clone
                .lock()
                .expect("TUI send queue mutex poisoned");
            queue.push(TuiSendRequest::Json { data: json });

            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create tui.send function: {e}"))?;
    tui_table
        .set("send", send_fn)
        .map_err(|e| anyhow!("Failed to set tui.send: {e}"))?;

    // tui.send_binary(data) — no peer_id, single TUI client
    let send_queue_clone = Arc::clone(&send_queue);
    let send_binary_fn = lua
        .create_function(move |_, data: LuaString| {
            let bytes = data.as_bytes().to_vec();

            let mut queue = send_queue_clone
                .lock()
                .expect("TUI send queue mutex poisoned");
            queue.push(TuiSendRequest::Binary { data: bytes });

            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create tui.send_binary function: {e}"))?;
    tui_table
        .set("send_binary", send_binary_fn)
        .map_err(|e| anyhow!("Failed to set tui.send_binary: {e}"))?;

    // Register the table globally
    lua.globals()
        .set("tui", tui_table)
        .map_err(|e| anyhow!("Failed to register tui table globally: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tui_table_created() {
        let lua = Lua::new();
        let queue = new_send_queue();
        register(&lua, queue).expect("Should register tui primitives");

        let globals = lua.globals();
        let tui_table: mlua::Table =
            globals.get("tui").expect("tui table should exist");

        // Verify all functions exist
        let _: mlua::Function = tui_table
            .get("on_connected")
            .expect("on_connected should exist");
        let _: mlua::Function = tui_table
            .get("on_disconnected")
            .expect("on_disconnected should exist");
        let _: mlua::Function = tui_table
            .get("on_message")
            .expect("on_message should exist");
        let _: mlua::Function = tui_table
            .get("send")
            .expect("send should exist");
        let _: mlua::Function = tui_table
            .get("send_binary")
            .expect("send_binary should exist");
    }

    #[test]
    fn test_on_connected_stores_callback() {
        let lua = Lua::new();
        let queue = new_send_queue();
        register(&lua, queue).expect("Should register tui primitives");

        lua.load(
            r#"
            tui.on_connected(function()
                connected_called = true
            end)
        "#,
        )
        .exec()
        .expect("Should register callback");

        // Verify callback is stored in registry
        let key: mlua::RegistryKey = lua
            .named_registry_value(registry_keys::ON_CONNECTED)
            .expect("Callback should be stored in registry");

        let callback: mlua::Function =
            lua.registry_value(&key).expect("Should retrieve callback");

        lua.globals()
            .set("connected_called", LuaValue::Nil)
            .unwrap();
        callback.call::<()>(()).expect("Should call callback");

        let result: bool = lua
            .globals()
            .get("connected_called")
            .expect("connected_called should be set");
        assert!(result);
    }

    #[test]
    fn test_on_message_stores_callback() {
        let lua = Lua::new();
        let queue = new_send_queue();
        register(&lua, queue).expect("Should register tui primitives");

        lua.load(
            r#"
            tui.on_message(function(msg)
                received_type = msg.type
            end)
        "#,
        )
        .exec()
        .expect("Should register callback");

        let key: mlua::RegistryKey = lua
            .named_registry_value(registry_keys::ON_MESSAGE)
            .expect("Callback should be stored in registry");

        let callback: mlua::Function =
            lua.registry_value(&key).expect("Should retrieve callback");

        // Call with a table argument
        lua.globals()
            .set("received_type", LuaValue::Nil)
            .unwrap();
        let msg = lua.create_table().unwrap();
        msg.set("type", "test_msg").unwrap();
        callback.call::<()>(msg).expect("Should call callback");

        let result: String = lua
            .globals()
            .get("received_type")
            .expect("received_type should be set");
        assert_eq!(result, "test_msg");
    }

    #[test]
    fn test_send_queues_json_message() {
        let lua = Lua::new();
        let queue = new_send_queue();
        register(&lua, Arc::clone(&queue)).expect("Should register tui primitives");

        lua.load(
            r#"
            tui.send({ type = "agent_list", count = 3 })
        "#,
        )
        .exec()
        .expect("Should send message");

        let pending = queue.lock().expect("TUI send queue mutex poisoned");
        assert_eq!(pending.len(), 1);

        match &pending[0] {
            TuiSendRequest::Json { data } => {
                assert_eq!(data["type"], "agent_list");
                assert_eq!(data["count"], 3);
            }
            _ => panic!("Expected Json request"),
        }
    }

    #[test]
    fn test_send_binary_queues_binary_message() {
        let lua = Lua::new();
        let queue = new_send_queue();
        register(&lua, Arc::clone(&queue)).expect("Should register tui primitives");

        lua.load(
            r#"
            tui.send_binary("hello bytes")
        "#,
        )
        .exec()
        .expect("Should send binary");

        let pending = queue.lock().expect("TUI send queue mutex poisoned");
        assert_eq!(pending.len(), 1);

        match &pending[0] {
            TuiSendRequest::Binary { data } => {
                assert_eq!(data, b"hello bytes");
            }
            _ => panic!("Expected Binary request"),
        }
    }

    #[test]
    fn test_multiple_sends_queue_in_order() {
        let lua = Lua::new();
        let queue = new_send_queue();
        register(&lua, Arc::clone(&queue)).expect("Should register tui primitives");

        lua.load(
            r#"
            tui.send({ msg = "first" })
            tui.send({ msg = "second" })
            tui.send_binary("third")
        "#,
        )
        .exec()
        .expect("Should send messages");

        let pending = queue.lock().expect("TUI send queue mutex poisoned");
        assert_eq!(pending.len(), 3);

        // Verify order is preserved
        match &pending[0] {
            TuiSendRequest::Json { data } => assert_eq!(data["msg"], "first"),
            _ => panic!("Expected Json"),
        }
        match &pending[1] {
            TuiSendRequest::Json { data } => assert_eq!(data["msg"], "second"),
            _ => panic!("Expected Json"),
        }
        match &pending[2] {
            TuiSendRequest::Binary { .. } => {}
            _ => panic!("Expected Binary"),
        }
    }
}
