//! Event system primitives for Lua scripts.
//!
//! Provides a pub/sub event system allowing Lua scripts to register callbacks
//! for Hub events like agent creation, deletion, and status changes.
//!
//! # Design Principle: "Subscribe once. React always."
//!
//! Lua scripts register callbacks that are invoked synchronously when events occur.
//! Callbacks should be fast - expensive operations should queue work for later.
//!
//! # Usage in Lua
//!
//! ```lua
//! -- Subscribe to agent creation events
//! local sub_id = events.on("agent_created", function(info)
//!     log.info("Agent created: " .. info.id .. " (repo: " .. info.repo .. ")")
//! end)
//!
//! -- Subscribe to agent deletion events
//! events.on("agent_deleted", function(agent_id)
//!     log.info("Agent deleted: " .. agent_id)
//! end)
//!
//! -- Subscribe to status changes
//! events.on("agent_status_changed", function(info)
//!     log.info("Agent " .. info.agent_id .. " status: " .. info.status)
//! end)
//!
//! -- Unsubscribe if needed
//! events.off(sub_id)
//! ```
//!
//! # Available Events
//!
//! - `agent_created` - Called with agent info table when agent is created
//! - `agent_deleted` - Called with agent_id string when agent is deleted
//! - `agent_status_changed` - Called with {agent_id, status} when status changes
//! - `shutdown` - Called with no arguments when Hub is shutting down

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use mlua::prelude::*;

/// Unique identifier for an event subscription.
pub type EventCallbackId = String;

/// Storage for event callbacks registered by Lua scripts.
///
/// Callbacks are stored in the Lua registry to prevent garbage collection.
/// Each event name maps to a list of (callback_id, registry_key) pairs.
#[derive(Default)]
pub struct EventCallbacks {
    /// Map of event name -> list of (callback_id, registry_key).
    callbacks: HashMap<String, Vec<(EventCallbackId, LuaRegistryKey)>>,
    /// Counter for generating unique callback IDs.
    next_id: u64,
}

impl std::fmt::Debug for EventCallbacks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventCallbacks")
            .field("event_count", &self.callbacks.len())
            .field("total_callbacks", &self.callback_count())
            .field("next_id", &self.next_id)
            .finish()
    }
}

impl EventCallbacks {
    /// Create a new empty callback registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a callback for an event.
    ///
    /// # Arguments
    ///
    /// * `lua` - The Lua state (needed to create registry key)
    /// * `event` - Event name to subscribe to
    /// * `callback` - Lua function to call when event fires
    ///
    /// # Returns
    ///
    /// A unique callback ID that can be used to unsubscribe.
    ///
    /// # Errors
    ///
    /// Returns an error if registry key creation fails.
    pub fn register(
        &mut self,
        lua: &Lua,
        event: &str,
        callback: LuaFunction,
    ) -> Result<EventCallbackId> {
        let id = format!("evt_{}", self.next_id);
        self.next_id += 1;

        let key = lua
            .create_registry_value(callback)
            .map_err(|e| anyhow!("Failed to create registry value: {e}"))?;

        self.callbacks
            .entry(event.to_string())
            .or_default()
            .push((id.clone(), key));

        log::debug!("Registered event callback '{}' for '{}'", id, event);
        Ok(id)
    }

    /// Unregister a callback by its ID.
    ///
    /// Searches all events for the callback and removes it.
    /// Safe to call with an ID that doesn't exist (no-op).
    ///
    /// # Arguments
    ///
    /// * `lua` - The Lua state (needed to remove registry key)
    /// * `subscription_id` - The callback ID to remove
    pub fn unregister(&mut self, lua: &Lua, subscription_id: &str) {
        // First, find and collect keys to remove
        let mut keys_to_remove = Vec::new();
        for callbacks in self.callbacks.values_mut() {
            // Find index of matching callback
            if let Some(idx) = callbacks.iter().position(|(id, _)| id == subscription_id) {
                // Remove from Vec and collect the key
                let (id, key) = callbacks.remove(idx);
                keys_to_remove.push((id, key));
            }
        }

        // Now remove from registry
        for (id, key) in keys_to_remove {
            if let Err(e) = lua.remove_registry_value(key) {
                log::warn!("Failed to remove registry value for {}: {}", id, e);
            }
            log::debug!("Unregistered event callback '{}'", id);
        }
    }

    /// Get all registry keys for callbacks registered to an event.
    ///
    /// Returns an empty vec if no callbacks are registered.
    #[must_use]
    pub fn get_callbacks(&self, event: &str) -> Vec<&LuaRegistryKey> {
        self.callbacks
            .get(event)
            .map(|v| v.iter().map(|(_, k)| k).collect())
            .unwrap_or_default()
    }

    /// Check if any callbacks are registered for an event.
    #[must_use]
    pub fn has_callbacks(&self, event: &str) -> bool {
        self.callbacks
            .get(event)
            .map_or(false, |v| !v.is_empty())
    }

    /// Get total number of registered callbacks across all events.
    #[must_use]
    pub fn callback_count(&self) -> usize {
        self.callbacks.values().map(Vec::len).sum()
    }

    /// Get the names of all events with registered callbacks.
    #[must_use]
    pub fn registered_events(&self) -> Vec<&str> {
        self.callbacks
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, _)| k.as_str())
            .collect()
    }
}

/// Shared reference to event callbacks for thread-safe access.
pub type SharedEventCallbacks = Arc<Mutex<EventCallbacks>>;

/// Create a new shared event callback registry.
#[must_use]
pub fn new_event_callbacks() -> SharedEventCallbacks {
    Arc::new(Mutex::new(EventCallbacks::new()))
}

/// Register event primitives with the Lua state.
///
/// Adds the following functions to the global `events` table:
/// - `events.on(event_name, callback)` -> subscription_id
/// - `events.off(subscription_id)` - Unsubscribe
/// - `events.has(event_name)` -> bool - Check if callbacks exist
/// - `events.emit(event_name, data)` -> number - Fire event, invoke all callbacks
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `callbacks` - Shared callback storage
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub fn register(lua: &Lua, callbacks: SharedEventCallbacks) -> Result<()> {
    let events = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create events table: {e}"))?;

    // events.on(event_name, callback) -> subscription_id
    let cb = Arc::clone(&callbacks);
    let on_fn = lua
        .create_function(move |lua, (event, callback): (String, LuaFunction)| {
            let mut cbs = cb.lock()
                .expect("Event callbacks mutex poisoned");
            let id = cbs
                .register(lua, &event, callback)
                .map_err(LuaError::external)?;
            Ok(id)
        })
        .map_err(|e| anyhow!("Failed to create events.on function: {e}"))?;

    events
        .set("on", on_fn)
        .map_err(|e| anyhow!("Failed to set events.on: {e}"))?;

    // events.off(subscription_id)
    let cb2 = Arc::clone(&callbacks);
    let off_fn = lua
        .create_function(move |lua, subscription_id: String| {
            let mut cbs = cb2.lock()
                .expect("Event callbacks mutex poisoned");
            cbs.unregister(lua, &subscription_id);
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create events.off function: {e}"))?;

    events
        .set("off", off_fn)
        .map_err(|e| anyhow!("Failed to set events.off: {e}"))?;

    // events.has(event_name) -> bool
    let cb3 = Arc::clone(&callbacks);
    let has_fn = lua
        .create_function(move |_, event: String| {
            let cbs = cb3.lock()
                .expect("Event callbacks mutex poisoned");
            Ok(cbs.has_callbacks(&event))
        })
        .map_err(|e| anyhow!("Failed to create events.has function: {e}"))?;

    events
        .set("has", has_fn)
        .map_err(|e| anyhow!("Failed to set events.has: {e}"))?;

    // events.emit(event_name, data) -> number of callbacks invoked
    let cb4 = callbacks;
    let emit_fn = lua
        .create_function(move |lua, (event, data): (String, LuaValue)| {
            // Get callback functions while holding the lock
            let functions: Vec<LuaFunction> = {
                let cbs = cb4.lock().expect("Event callbacks mutex poisoned");
                cbs.get_callbacks(&event)
                    .iter()
                    .filter_map(|key| lua.registry_value::<LuaFunction>(*key).ok())
                    .collect()
            };
            // Lock released here

            let mut invoked = 0;
            for callback in functions {
                match callback.call::<()>(data.clone()) {
                    Ok(()) => invoked += 1,
                    Err(e) => log::warn!("Event callback for '{}' failed: {}", event, e),
                }
            }

            Ok(invoked)
        })
        .map_err(|e| anyhow!("Failed to create events.emit function: {e}"))?;

    events
        .set("emit", emit_fn)
        .map_err(|e| anyhow!("Failed to set events.emit: {e}"))?;

    lua.globals()
        .set("events", events)
        .map_err(|e| anyhow!("Failed to register events table globally: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_callbacks_new() {
        let callbacks = EventCallbacks::new();
        assert_eq!(callbacks.callback_count(), 0);
        assert!(!callbacks.has_callbacks("test_event"));
    }

    #[test]
    fn test_event_callbacks_register() {
        let lua = Lua::new();
        let mut callbacks = EventCallbacks::new();

        let func = lua.create_function(|_, ()| Ok(())).unwrap();
        let id = callbacks.register(&lua, "test_event", func).unwrap();

        assert!(id.starts_with("evt_"));
        assert_eq!(callbacks.callback_count(), 1);
        assert!(callbacks.has_callbacks("test_event"));
        assert!(!callbacks.has_callbacks("other_event"));
    }

    #[test]
    fn test_event_callbacks_unregister() {
        let lua = Lua::new();
        let mut callbacks = EventCallbacks::new();

        let func = lua.create_function(|_, ()| Ok(())).unwrap();
        let id = callbacks.register(&lua, "test_event", func).unwrap();

        assert_eq!(callbacks.callback_count(), 1);

        callbacks.unregister(&lua, &id);

        assert_eq!(callbacks.callback_count(), 0);
        assert!(!callbacks.has_callbacks("test_event"));
    }

    #[test]
    fn test_event_callbacks_unregister_nonexistent() {
        let lua = Lua::new();
        let mut callbacks = EventCallbacks::new();

        // Should not panic
        callbacks.unregister(&lua, "nonexistent_id");

        assert_eq!(callbacks.callback_count(), 0);
    }

    #[test]
    fn test_event_callbacks_multiple_events() {
        let lua = Lua::new();
        let mut callbacks = EventCallbacks::new();

        let func1 = lua.create_function(|_, ()| Ok(())).unwrap();
        let func2 = lua.create_function(|_, ()| Ok(())).unwrap();
        let func3 = lua.create_function(|_, ()| Ok(())).unwrap();

        callbacks.register(&lua, "event_a", func1).unwrap();
        callbacks.register(&lua, "event_b", func2).unwrap();
        callbacks.register(&lua, "event_a", func3).unwrap();

        assert_eq!(callbacks.callback_count(), 3);
        assert_eq!(callbacks.get_callbacks("event_a").len(), 2);
        assert_eq!(callbacks.get_callbacks("event_b").len(), 1);
        assert_eq!(callbacks.get_callbacks("event_c").len(), 0);
    }

    #[test]
    fn test_event_callbacks_registered_events() {
        let lua = Lua::new();
        let mut callbacks = EventCallbacks::new();

        let func1 = lua.create_function(|_, ()| Ok(())).unwrap();
        let func2 = lua.create_function(|_, ()| Ok(())).unwrap();

        callbacks.register(&lua, "event_a", func1).unwrap();
        callbacks.register(&lua, "event_b", func2).unwrap();

        let events = callbacks.registered_events();
        assert!(events.contains(&"event_a"));
        assert!(events.contains(&"event_b"));
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_register_creates_events_table() {
        let lua = Lua::new();
        let callbacks = new_event_callbacks();

        register(&lua, callbacks).expect("Should register events primitives");

        let events: LuaTable = lua.globals().get("events").expect("events table should exist");
        assert!(events.contains_key("on").unwrap());
        assert!(events.contains_key("off").unwrap());
        assert!(events.contains_key("has").unwrap());
        assert!(events.contains_key("emit").unwrap());
    }

    #[test]
    fn test_events_on_registers_callback() {
        let lua = Lua::new();
        let callbacks = new_event_callbacks();

        register(&lua, Arc::clone(&callbacks)).expect("Should register");

        let id: String = lua
            .load(
                r#"
            return events.on("test_event", function() end)
        "#,
            )
            .eval()
            .unwrap();

        assert!(id.starts_with("evt_"));

        let cbs = callbacks.lock()
            .expect("Event callbacks mutex poisoned");
        assert!(cbs.has_callbacks("test_event"));
    }

    #[test]
    fn test_events_off_unregisters_callback() {
        let lua = Lua::new();
        let callbacks = new_event_callbacks();

        register(&lua, Arc::clone(&callbacks)).expect("Should register");

        lua.load(
            r#"
            sub_id = events.on("test_event", function() end)
            events.off(sub_id)
        "#,
        )
        .exec()
        .unwrap();

        let cbs = callbacks.lock()
            .expect("Event callbacks mutex poisoned");
        assert!(!cbs.has_callbacks("test_event"));
    }

    #[test]
    fn test_events_has_returns_correct_value() {
        let lua = Lua::new();
        let callbacks = new_event_callbacks();

        register(&lua, callbacks).expect("Should register");

        let has_before: bool = lua.load(r#"return events.has("test_event")"#).eval().unwrap();
        assert!(!has_before);

        lua.load(r#"events.on("test_event", function() end)"#)
            .exec()
            .unwrap();

        let has_after: bool = lua.load(r#"return events.has("test_event")"#).eval().unwrap();
        assert!(has_after);

        let has_other: bool = lua.load(r#"return events.has("other_event")"#).eval().unwrap();
        assert!(!has_other);
    }

    #[test]
    fn test_multiple_callbacks_same_event() {
        let lua = Lua::new();
        let callbacks = new_event_callbacks();

        register(&lua, Arc::clone(&callbacks)).expect("Should register");

        lua.load(
            r#"
            events.on("test_event", function() end)
            events.on("test_event", function() end)
            events.on("test_event", function() end)
        "#,
        )
        .exec()
        .unwrap();

        let cbs = callbacks.lock()
            .expect("Event callbacks mutex poisoned");
        assert_eq!(cbs.get_callbacks("test_event").len(), 3);
    }

    #[test]
    fn test_callback_invocation() {
        let lua = Lua::new();
        let callbacks = new_event_callbacks();

        register(&lua, Arc::clone(&callbacks)).expect("Should register");

        // Register a callback that sets a global variable
        lua.load(
            r#"
            callback_called = false
            callback_arg = nil
            events.on("test_event", function(arg)
                callback_called = true
                callback_arg = arg
            end)
        "#,
        )
        .exec()
        .unwrap();

        // Get the callback and invoke it
        let cbs = callbacks.lock()
            .expect("Event callbacks mutex poisoned");
        let keys = cbs.get_callbacks("test_event");
        assert_eq!(keys.len(), 1);

        let callback: LuaFunction = lua.registry_value(keys[0]).unwrap();
        callback.call::<()>("test_value").unwrap();

        // Verify callback was called with correct argument
        let called: bool = lua.globals().get("callback_called").unwrap();
        let arg: String = lua.globals().get("callback_arg").unwrap();

        assert!(called);
        assert_eq!(arg, "test_value");
    }

    #[test]
    fn test_events_emit_invokes_callbacks() {
        let lua = Lua::new();
        let callbacks = new_event_callbacks();

        register(&lua, callbacks).expect("Should register");

        let count: i32 = lua
            .load(
                r#"
            callback_called = false
            callback_arg = nil
            events.on("test_event", function(arg)
                callback_called = true
                callback_arg = arg
            end)
            return events.emit("test_event", "hello")
        "#,
            )
            .eval()
            .unwrap();

        assert_eq!(count, 1);

        let called: bool = lua.globals().get("callback_called").unwrap();
        let arg: String = lua.globals().get("callback_arg").unwrap();

        assert!(called);
        assert_eq!(arg, "hello");
    }

    #[test]
    fn test_events_emit_invokes_multiple_callbacks() {
        let lua = Lua::new();
        let callbacks = new_event_callbacks();

        register(&lua, callbacks).expect("Should register");

        let count: i32 = lua
            .load(
                r#"
            call_count = 0
            events.on("test_event", function() call_count = call_count + 1 end)
            events.on("test_event", function() call_count = call_count + 1 end)
            events.on("test_event", function() call_count = call_count + 1 end)
            return events.emit("test_event", nil)
        "#,
            )
            .eval()
            .unwrap();

        assert_eq!(count, 3);

        let call_count: i32 = lua.globals().get("call_count").unwrap();
        assert_eq!(call_count, 3);
    }

    #[test]
    fn test_events_emit_returns_zero_for_no_callbacks() {
        let lua = Lua::new();
        let callbacks = new_event_callbacks();

        register(&lua, callbacks).expect("Should register");

        let count: i32 = lua
            .load(r#"return events.emit("nonexistent_event", "data")"#)
            .eval()
            .unwrap();

        assert_eq!(count, 0);
    }

    #[test]
    fn test_events_emit_with_table_data() {
        let lua = Lua::new();
        let callbacks = new_event_callbacks();

        register(&lua, callbacks).expect("Should register");

        let count: i32 = lua
            .load(
                r#"
            received_data = nil
            events.on("test_event", function(data)
                received_data = data
            end)
            return events.emit("test_event", { id = "agent-1", status = "running" })
        "#,
            )
            .eval()
            .unwrap();

        assert_eq!(count, 1);

        let id: String = lua.load(r#"return received_data.id"#).eval().unwrap();
        let status: String = lua.load(r#"return received_data.status"#).eval().unwrap();

        assert_eq!(id, "agent-1");
        assert_eq!(status, "running");
    }

    #[test]
    fn test_events_emit_continues_on_callback_error() {
        let lua = Lua::new();
        let callbacks = new_event_callbacks();

        register(&lua, callbacks).expect("Should register");

        // First callback errors, second should still run
        let count: i32 = lua
            .load(
                r#"
            second_called = false
            events.on("test_event", function() error("intentional error") end)
            events.on("test_event", function() second_called = true end)
            return events.emit("test_event", nil)
        "#,
            )
            .eval()
            .unwrap();

        // First failed, second succeeded
        assert_eq!(count, 1);

        let second_called: bool = lua.globals().get("second_called").unwrap();
        assert!(second_called);
    }
}
