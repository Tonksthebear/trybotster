//! Web push notification primitive for Lua scripts.
//!
//! Exposes push notification sending to Lua, allowing scripts to trigger
//! customized browser notifications from any hook or handler.
//!
//! # Usage in Lua
//!
//! ```lua
//! -- Send a push notification with full customization
//! push.send({
//!     kind = "agent_alert",              -- required: stored in IndexedDB for history
//!     title = "Agent completed",         -- optional: notification title (default: "Botster")
//!     body = "PR #42 is ready",          -- optional: notification body text
//!     url = "/hubs/128",                 -- optional: click destination (relative or absolute)
//!     icon = "/custom-icon.png",         -- optional: notification icon (relative or absolute)
//!     tag = "agent-42",                  -- optional: replaces same-tag notifications
//! })
//! ```
//!
//! Browser subscription controls are intentionally routed through Lua command
//! handlers:
//!
//! ```lua
//! push.control(client.peer_id, command)
//! ```
//!
//! Rust still performs VAPID/subscription persistence, but the browser-origin
//! command first passes through the same `client.lua` dispatch path as other
//! hub UI commands.
//!
//! # Event-Driven Delivery
//!
//! Notifications sent via `push.send()` are delivered to the Hub event loop
//! as `HubEvent::LuaPushRequest` events via the shared `HubEventSender`.
//! The Hub wraps Lua fields into a Declarative Web Push payload (RFC 8030,
//! `web_push: 8030`) with absolute URLs for the `navigate` field, and
//! broadcasts to all subscribed browsers.

// Rust guideline compliant 2026-02

use anyhow::{anyhow, Result};
use mlua::prelude::*;

use super::HubEventSender;
use crate::hub::events::HubEvent;

/// Register the `push` table with web push notification primitives.
///
/// Creates a global `push` table with methods:
/// - `push.send(opts)` — Send a push notification to all subscribed browsers.
/// - `push.control(peer_id, command)` — Route browser push controls to Rust.
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
    let push_table = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create push table: {e}"))?;

    // push.send(opts) — Send a web push notification
    let tx = hub_event_tx.clone();
    let send_fn = lua
        .create_function(move |lua, value: LuaValue| {
            let LuaValue::Table(ref table) = value else {
                return Err(LuaError::external(anyhow!(
                    "push.send() expects a table argument"
                )));
            };

            // Validate required field: kind
            let kind: Option<String> = table.get("kind")?;
            if kind.as_deref().unwrap_or("").is_empty() {
                return Err(LuaError::external(anyhow!(
                    "push.send() requires a 'kind' field (e.g. 'agent_alert')"
                )));
            }

            // Convert the full Lua table to a serde_json::Value
            let payload: serde_json::Value = lua.from_value(value)?;

            let guard = tx.lock().expect("HubEventSender mutex poisoned");
            if let Some(ref sender) = *guard {
                let _ = sender.send(HubEvent::LuaPushRequest { payload });
            } else {
                log::warn!("[Push] send() called before hub_event_tx set — notification dropped");
            }

            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create push.send function: {e}"))?;

    push_table
        .set("send", send_fn)
        .map_err(|e| anyhow!("Failed to set push.send: {e}"))?;

    let tx = hub_event_tx.clone();
    let control_fn = lua
        .create_function(move |lua, (browser_identity, value): (String, LuaValue)| {
            if browser_identity.is_empty() {
                return Err(LuaError::external(anyhow!(
                    "push.control() requires a browser identity"
                )));
            }
            let LuaValue::Table(_) = value else {
                return Err(LuaError::external(anyhow!(
                    "push.control() expects a command table"
                )));
            };

            let payload: serde_json::Value = lua.from_value(value)?;
            let guard = tx.lock().expect("HubEventSender mutex poisoned");
            if let Some(ref sender) = *guard {
                let _ = sender.send(HubEvent::BrowserPushControl {
                    browser_identity,
                    payload,
                });
            } else {
                log::warn!("[Push] control() called before hub_event_tx set — command dropped");
            }

            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create push.control function: {e}"))?;

    push_table
        .set("control", control_fn)
        .map_err(|e| anyhow!("Failed to set push.control: {e}"))?;

    lua.globals()
        .set("push", push_table)
        .map_err(|e| anyhow!("Failed to set push global: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::new_hub_event_sender;
    use super::*;

    fn register_with_receiver() -> (Lua, tokio::sync::mpsc::UnboundedReceiver<HubEvent>) {
        let lua = Lua::new();
        let tx = new_hub_event_sender();
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        *tx.lock().unwrap() = Some(sender.into());
        register(&lua, tx).expect("push primitive registers");
        (lua, receiver)
    }

    #[test]
    fn control_sends_browser_push_control_event() {
        let (lua, mut receiver) = register_with_receiver();

        lua.load(
            r#"
            push.control("browser-1", {
                type = "push_status_req",
                browser_id = "stable-browser",
            })
            "#,
        )
        .exec()
        .expect("push.control should send");

        let event = receiver.try_recv().expect("event queued");
        match event {
            HubEvent::BrowserPushControl {
                browser_identity,
                payload,
            } => {
                assert_eq!(browser_identity, "browser-1");
                assert_eq!(payload["type"], "push_status_req");
                assert_eq!(payload["browser_id"], "stable-browser");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn send_still_sends_lua_push_request_event() {
        let (lua, mut receiver) = register_with_receiver();

        lua.load(
            r#"
            push.send({
                kind = "agent_alert",
                title = "Done",
            })
            "#,
        )
        .exec()
        .expect("push.send should send");

        let event = receiver.try_recv().expect("event queued");
        match event {
            HubEvent::LuaPushRequest { payload } => {
                assert_eq!(payload["kind"], "agent_alert");
                assert_eq!(payload["title"], "Done");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
