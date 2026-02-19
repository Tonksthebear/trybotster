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
    let tx = hub_event_tx;
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

    lua.globals()
        .set("push", push_table)
        .map_err(|e| anyhow!("Failed to set push global: {e}"))?;

    Ok(())
}
