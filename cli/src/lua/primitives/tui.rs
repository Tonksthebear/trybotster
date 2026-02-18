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
//!         tui.send({ type = "agent_list", agents = Agent.all_info() })
//!     end
//! end)
//! ```
//!
//! # Event-Driven Delivery
//!
//! Messages sent via `tui.send()` are delivered directly to the Hub event
//! loop as `HubEvent::TuiSend` events via the shared `HubEventSender`.

use anyhow::{anyhow, Result};
use mlua::prelude::*;

use super::HubEventSender;
use crate::hub::events::HubEvent;

/// Request to send a message to the TUI.
///
/// Sent from Lua's `tui.send()` as `HubEvent::TuiSend` and processed by Hub.
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
/// * `hub_event_tx` - Shared sender for Hub events (filled in later by `set_hub_event_tx`)
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub(crate) fn register(lua: &Lua, hub_event_tx: HubEventSender) -> Result<()> {
    let tui_table = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create tui table: {e}"))?;

    // tui.on_connected(callback)
    let on_connected_fn = lua
        .create_function(|lua, callback: LuaFunction| {
            lua.set_named_registry_value(registry_keys::ON_CONNECTED, callback)?;
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create tui.on_connected function: {e}"))?;
    tui_table
        .set("on_connected", on_connected_fn)
        .map_err(|e| anyhow!("Failed to set tui.on_connected: {e}"))?;

    // tui.on_disconnected(callback)
    let on_disconnected_fn = lua
        .create_function(|lua, callback: LuaFunction| {
            lua.set_named_registry_value(registry_keys::ON_DISCONNECTED, callback)?;
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create tui.on_disconnected function: {e}"))?;
    tui_table
        .set("on_disconnected", on_disconnected_fn)
        .map_err(|e| anyhow!("Failed to set tui.on_disconnected: {e}"))?;

    // tui.on_message(callback)
    let on_message_fn = lua
        .create_function(|lua, callback: LuaFunction| {
            lua.set_named_registry_value(registry_keys::ON_MESSAGE, callback)?;
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create tui.on_message function: {e}"))?;
    tui_table
        .set("on_message", on_message_fn)
        .map_err(|e| anyhow!("Failed to set tui.on_message: {e}"))?;

    // tui.send(table) — no peer_id, single TUI client
    let tx = hub_event_tx.clone();
    let send_fn = lua
        .create_function(move |lua, value: LuaValue| {
            let json: serde_json::Value = lua.from_value(value)?;
            let guard = tx.lock().expect("HubEventSender mutex poisoned");
            if let Some(ref sender) = *guard {
                let _ = sender.send(HubEvent::TuiSend(TuiSendRequest::Json { data: json }));
            } else {
                ::log::warn!("[TUI] send() called before hub_event_tx set — event dropped");
            }
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create tui.send function: {e}"))?;
    tui_table
        .set("send", send_fn)
        .map_err(|e| anyhow!("Failed to set tui.send: {e}"))?;

    // tui.send_binary(data) — no peer_id, single TUI client
    let tx = hub_event_tx;
    let send_binary_fn = lua
        .create_function(move |_, data: LuaString| {
            let bytes = data.as_bytes().to_vec();
            let guard = tx.lock().expect("HubEventSender mutex poisoned");
            if let Some(ref sender) = *guard {
                let _ = sender.send(HubEvent::TuiSend(TuiSendRequest::Binary { data: bytes }));
            } else {
                ::log::warn!("[TUI] send_binary() called before hub_event_tx set — event dropped");
            }
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
    use super::super::new_hub_event_sender;

    fn setup() -> (Lua, HubEventSender) {
        let lua = Lua::new();
        let tx = new_hub_event_sender();
        register(&lua, tx.clone()).expect("Should register tui primitives");
        (lua, tx)
    }

    fn setup_with_channel() -> (Lua, tokio::sync::mpsc::UnboundedReceiver<HubEvent>) {
        let lua = Lua::new();
        let tx = new_hub_event_sender();
        register(&lua, tx.clone()).expect("Should register tui primitives");
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        *tx.lock().unwrap() = Some(sender);
        (lua, receiver)
    }

    #[test]
    fn test_tui_table_created() {
        let (lua, _tx) = setup();

        let globals = lua.globals();
        let tui_table: mlua::Table =
            globals.get("tui").expect("tui table should exist");

        let _: mlua::Function = tui_table.get("on_connected").expect("on_connected should exist");
        let _: mlua::Function = tui_table.get("on_disconnected").expect("on_disconnected should exist");
        let _: mlua::Function = tui_table.get("on_message").expect("on_message should exist");
        let _: mlua::Function = tui_table.get("send").expect("send should exist");
        let _: mlua::Function = tui_table.get("send_binary").expect("send_binary should exist");
    }

    #[test]
    fn test_on_connected_stores_callback() {
        let (lua, _tx) = setup();

        lua.load(r#"
            tui.on_connected(function()
                connected_called = true
            end)
        "#).exec().expect("Should register callback");

        let key: mlua::RegistryKey = lua
            .named_registry_value(registry_keys::ON_CONNECTED)
            .expect("Callback should be stored in registry");

        let callback: mlua::Function =
            lua.registry_value(&key).expect("Should retrieve callback");

        lua.globals().set("connected_called", LuaValue::Nil).unwrap();
        callback.call::<()>(()).expect("Should call callback");

        let result: bool = lua.globals().get("connected_called").expect("connected_called should be set");
        assert!(result);
    }

    #[test]
    fn test_on_message_stores_callback() {
        let (lua, _tx) = setup();

        lua.load(r#"
            tui.on_message(function(msg)
                received_type = msg.type
            end)
        "#).exec().expect("Should register callback");

        let key: mlua::RegistryKey = lua
            .named_registry_value(registry_keys::ON_MESSAGE)
            .expect("Callback should be stored in registry");

        let callback: mlua::Function =
            lua.registry_value(&key).expect("Should retrieve callback");

        lua.globals().set("received_type", LuaValue::Nil).unwrap();
        let msg = lua.create_table().unwrap();
        msg.set("type", "test_msg").unwrap();
        callback.call::<()>(msg).expect("Should call callback");

        let result: String = lua.globals().get("received_type").expect("received_type should be set");
        assert_eq!(result, "test_msg");
    }

    #[test]
    fn test_send_delivers_json_event() {
        let (lua, mut rx) = setup_with_channel();

        lua.load(r#"
            tui.send({ type = "agent_list", count = 3 })
        "#).exec().expect("Should send message");

        let event = rx.try_recv().expect("Should have received event");
        match event {
            HubEvent::TuiSend(TuiSendRequest::Json { data }) => {
                assert_eq!(data["type"], "agent_list");
                assert_eq!(data["count"], 3);
            }
            _ => panic!("Expected TuiSend Json event"),
        }
    }

    #[test]
    fn test_send_binary_delivers_binary_event() {
        let (lua, mut rx) = setup_with_channel();

        lua.load(r#"
            tui.send_binary("hello bytes")
        "#).exec().expect("Should send binary");

        let event = rx.try_recv().expect("Should have received event");
        match event {
            HubEvent::TuiSend(TuiSendRequest::Binary { data }) => {
                assert_eq!(data, b"hello bytes");
            }
            _ => panic!("Expected TuiSend Binary event"),
        }
    }

    #[test]
    fn test_multiple_sends_deliver_in_order() {
        let (lua, mut rx) = setup_with_channel();

        lua.load(r#"
            tui.send({ msg = "first" })
            tui.send({ msg = "second" })
            tui.send_binary("third")
        "#).exec().expect("Should send messages");

        match rx.try_recv().unwrap() {
            HubEvent::TuiSend(TuiSendRequest::Json { data }) => assert_eq!(data["msg"], "first"),
            _ => panic!("Expected Json"),
        }
        match rx.try_recv().unwrap() {
            HubEvent::TuiSend(TuiSendRequest::Json { data }) => assert_eq!(data["msg"], "second"),
            _ => panic!("Expected Json"),
        }
        match rx.try_recv().unwrap() {
            HubEvent::TuiSend(TuiSendRequest::Binary { .. }) => {}
            _ => panic!("Expected Binary"),
        }
    }
}
