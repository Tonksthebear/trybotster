//! Hub state primitives for Lua scripts.
//!
//! Exposes Hub state queries and operations to Lua, allowing scripts to
//! inspect agent PTY handles, worktrees, and request agent lifecycle operations.
//!
//! # Design Principle: "Query freely. Mutate via queue."
//!
//! - **State queries** (get_agents, get_agent, get_worktrees) read directly from
//!   HandleCache - non-blocking, thread-safe snapshots
//! - **Operations** (quit) queue requests for Hub to process asynchronously
//!   after Lua callbacks return
//!
//! # Lua Migration
//!
//! Agent metadata (repo, issue, status, etc.) is now managed by Lua.
//! The `hub.get_agents()` and `hub.get_agent()` functions return PTY-level
//! information (agent_key, pty_count, index). Lua enriches this with its
//! own agent registry data.
//!
//! # Usage in Lua
//!
//! ```lua
//! -- Query agent handles (non-blocking, reads from cache)
//! local agents = hub.get_agents()
//! for i, agent in ipairs(agents) do
//!     log.info(string.format("Agent %d: %s (ptys: %d)", i, agent.key, agent.pty_count))
//! end
//!
//! -- Get single agent handle by index
//! local agent = hub.get_agent(0)
//! if agent then
//!     log.info("First agent: " .. agent.key)
//! end
//!
//! -- Get available worktrees
//! local worktrees = hub.get_worktrees()
//!
//! -- Request Hub shutdown
//! hub.quit()
//! ```

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use mlua::prelude::*;

use crate::hub::handle_cache::HandleCache;

/// Hub operation requests queued from Lua.
///
/// These are processed by Hub in its event loop after Lua callbacks return.
#[derive(Debug, Clone)]
pub enum HubRequest {
    /// Request Hub shutdown.
    Quit,
}

/// Shared request queue for Hub operations from Lua.
pub type HubRequestQueue = Arc<Mutex<Vec<HubRequest>>>;

/// Create a new Hub request queue.
#[must_use]
pub fn new_request_queue() -> HubRequestQueue {
    Arc::new(Mutex::new(Vec::new()))
}

/// Register Hub state primitives with the Lua state.
///
/// Adds the following functions to the `hub` table:
/// - `hub.get_agents()` - Get all agent PTY handles (key, index, pty_count)
/// - `hub.get_agent(index)` - Get single agent PTY handle by index
/// - `hub.get_agent_count()` - Get number of agents
/// - `hub.get_worktrees()` - Get available worktrees
/// - `hub.quit()` - Request Hub shutdown
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `request_queue` - Shared queue for Hub operations (processed by Hub)
/// * `handle_cache` - Thread-safe cache of agent handles for queries
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub fn register(
    lua: &Lua,
    request_queue: HubRequestQueue,
    handle_cache: Arc<HandleCache>,
) -> Result<()> {
    // Get or create the hub table
    let hub: LuaTable = lua
        .globals()
        .get("hub")
        .unwrap_or_else(|_| lua.create_table().unwrap());

    // hub.get_agents() - Returns array of agent PTY handle info
    // Agent metadata (repo, issue, status) is managed by Lua's own registry.
    // This returns PTY-level data: key, index, pty_count.
    let cache = Arc::clone(&handle_cache);
    let get_agents_fn = lua
        .create_function(move |lua, ()| {
            let agents = cache.get_all_agents();

            // Build as Vec for proper array serialization
            let agents_data: Vec<serde_json::Value> = agents
                .iter()
                .enumerate()
                .map(|(i, agent)| {
                    let mut obj = serde_json::Map::new();

                    // Core identity from handle
                    obj.insert("index".to_string(), serde_json::json!(i));
                    obj.insert("key".to_string(), serde_json::json!(agent.agent_key()));

                    // PTY count from handle
                    obj.insert("pty_count".to_string(), serde_json::json!(agent.pty_count()));

                    serde_json::Value::Object(obj)
                })
                .collect();

            // Convert to Lua - Vec serializes as array
            lua.to_value(&agents_data)
        })
        .map_err(|e| anyhow!("Failed to create hub.get_agents function: {e}"))?;

    hub.set("get_agents", get_agents_fn)
        .map_err(|e| anyhow!("Failed to set hub.get_agents: {e}"))?;

    // hub.get_agent(index) - Returns single agent PTY handle info or nil
    // Agent metadata is managed by Lua's own registry.
    let cache2 = Arc::clone(&handle_cache);
    let get_agent_fn = lua
        .create_function(move |lua, index: usize| {
            match cache2.get_agent(index) {
                Some(agent) => {
                    let agent_table = lua.create_table()?;

                    // Core identity from handle
                    agent_table.set("index", index)?;
                    agent_table.set("key", agent.agent_key().to_string())?;

                    // PTY count from handle
                    agent_table.set("pty_count", agent.pty_count())?;

                    Ok(LuaValue::Table(agent_table))
                }
                None => Ok(LuaValue::Nil),
            }
        })
        .map_err(|e| anyhow!("Failed to create hub.get_agent function: {e}"))?;

    hub.set("get_agent", get_agent_fn)
        .map_err(|e| anyhow!("Failed to set hub.get_agent: {e}"))?;

    // hub.get_agent_count() - Returns number of agents
    let cache3 = Arc::clone(&handle_cache);
    let get_agent_count_fn = lua
        .create_function(move |_, ()| Ok(cache3.len()))
        .map_err(|e| anyhow!("Failed to create hub.get_agent_count function: {e}"))?;

    hub.set("get_agent_count", get_agent_count_fn)
        .map_err(|e| anyhow!("Failed to set hub.get_agent_count: {e}"))?;

    // hub.get_worktrees() - Returns array of available worktrees
    // Uses serde serialization to ensure proper JSON array format
    let cache4 = Arc::clone(&handle_cache);
    let get_worktrees_fn = lua
        .create_function(move |lua, ()| {
            let worktrees = cache4.get_worktrees();

            // Build as Vec for proper array serialization
            let worktrees_data: Vec<serde_json::Value> = worktrees
                .iter()
                .map(|(path, branch)| {
                    serde_json::json!({
                        "path": path,
                        "branch": branch
                    })
                })
                .collect();

            // Convert to Lua - Vec serializes as array
            lua.to_value(&worktrees_data)
        })
        .map_err(|e| anyhow!("Failed to create hub.get_worktrees function: {e}"))?;

    hub.set("get_worktrees", get_worktrees_fn)
        .map_err(|e| anyhow!("Failed to set hub.get_worktrees: {e}"))?;

    // hub.register_agent(agent_key, sessions) - Register agent PTY handles
    //
    // Called by Lua Agent class to register PTY session handles with
    // HandleCache, enabling Rust-side PTY operations (forwarders, write, resize).
    //
    // Arguments:
    //   agent_key: string - Agent key (e.g., "owner-repo-42")
    //   sessions: table - Map of session name to PtySessionHandle userdata
    //                     e.g., { cli = <PtySessionHandle>, server = <PtySessionHandle> }
    //
    // The order of PTYs in HandleCache is: cli first (index 0), then server (index 1).
    let cache5 = Arc::clone(&handle_cache);
    let register_agent_fn = lua
        .create_function(move |_, (agent_key, sessions): (String, LuaTable)| {
            use crate::hub::agent_handle::{AgentHandle, PtyHandle};
            use crate::lua::primitives::pty::PtySessionHandle;

            let mut pty_handles: Vec<PtyHandle> = Vec::new();

            // Extract CLI session first (index 0)
            // Use AnyUserData and borrow for more robust type handling
            if let Ok(cli_ud) = sessions.get::<LuaAnyUserData>("cli") {
                match cli_ud.borrow::<PtySessionHandle>() {
                    Ok(cli_handle) => {
                        pty_handles.push(cli_handle.to_pty_handle());
                        log::debug!("[Lua] Extracted CLI PTY handle for '{}'", agent_key);
                    }
                    Err(e) => {
                        log::warn!("[Lua] Failed to borrow CLI session as PtySessionHandle: {}", e);
                    }
                }
            } else {
                log::debug!("[Lua] No 'cli' session found for agent '{}'", agent_key);
            }

            // Extract server session second (index 1)
            if let Ok(server_ud) = sessions.get::<LuaAnyUserData>("server") {
                match server_ud.borrow::<PtySessionHandle>() {
                    Ok(server_handle) => {
                        pty_handles.push(server_handle.to_pty_handle());
                        log::debug!("[Lua] Extracted server PTY handle for '{}'", agent_key);
                    }
                    Err(e) => {
                        log::warn!("[Lua] Failed to borrow server session as PtySessionHandle: {}", e);
                    }
                }
            }

            if pty_handles.is_empty() {
                log::error!(
                    "[Lua] register_agent '{}' failed: no valid PTY sessions found in table",
                    agent_key
                );
                return Err(LuaError::runtime(
                    "register_agent requires at least one PTY session (cli or server)"
                ));
            }

            // Capture count before moving
            let pty_count = pty_handles.len();

            // Determine index (will be updated by add_agent)
            let agent_index = cache5.len();
            let handle = AgentHandle::new(agent_key.clone(), pty_handles, agent_index);

            match cache5.add_agent(handle) {
                Some(idx) => {
                    log::info!("[Lua] Registered agent '{}' at index {} with {} PTY(s)",
                        agent_key, idx, pty_count);
                    Ok(idx)
                }
                None => Err(LuaError::runtime("Failed to register agent with HandleCache")),
            }
        })
        .map_err(|e| anyhow!("Failed to create hub.register_agent function: {e}"))?;

    hub.set("register_agent", register_agent_fn)
        .map_err(|e| anyhow!("Failed to set hub.register_agent: {e}"))?;

    // hub.unregister_agent(agent_key) - Unregister agent PTY handles
    //
    // Called by Lua when an agent is closed to remove it from HandleCache.
    let cache6 = Arc::clone(&handle_cache);
    let unregister_agent_fn = lua
        .create_function(move |_, agent_key: String| {
            let removed = cache6.remove_agent(&agent_key);
            if removed {
                log::info!("[Lua] Unregistered agent '{}'", agent_key);
            }
            Ok(removed)
        })
        .map_err(|e| anyhow!("Failed to create hub.unregister_agent function: {e}"))?;

    hub.set("unregister_agent", unregister_agent_fn)
        .map_err(|e| anyhow!("Failed to set hub.unregister_agent: {e}"))?;

    // hub.quit() - Request Hub shutdown
    let queue3 = request_queue;
    let quit_fn = lua
        .create_function(move |_, ()| {
            let mut q = queue3.lock()
                .expect("Hub request queue mutex poisoned");
            q.push(HubRequest::Quit);
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create hub.quit function: {e}"))?;

    hub.set("quit", quit_fn)
        .map_err(|e| anyhow!("Failed to set hub.quit: {e}"))?;

    // Ensure hub table is globally registered
    lua.globals()
        .set("hub", hub)
        .map_err(|e| anyhow!("Failed to register hub table globally: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_queue_and_cache() -> (HubRequestQueue, Arc<HandleCache>) {
        (new_request_queue(), Arc::new(HandleCache::new()))
    }

    #[test]
    fn test_register_creates_hub_table() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, queue, cache).expect("Should register hub primitives");

        let hub: LuaTable = lua.globals().get("hub").expect("hub table should exist");
        assert!(hub.contains_key("get_agents").unwrap());
        assert!(hub.contains_key("get_agent").unwrap());
        assert!(hub.contains_key("get_agent_count").unwrap());
        assert!(hub.contains_key("get_worktrees").unwrap());
        assert!(hub.contains_key("quit").unwrap());
    }

    #[test]
    fn test_get_agents_returns_empty_table() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, queue, cache).expect("Should register");

        let agents: LuaTable = lua.load("return hub.get_agents()").eval().unwrap();
        assert_eq!(agents.len().unwrap(), 0);
    }

    #[test]
    fn test_get_agent_returns_nil_for_invalid_index() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, queue, cache).expect("Should register");

        let result: LuaValue = lua.load("return hub.get_agent(0)").eval().unwrap();
        assert!(matches!(result, LuaValue::Nil));

        let result: LuaValue = lua.load("return hub.get_agent(100)").eval().unwrap();
        assert!(matches!(result, LuaValue::Nil));
    }

    #[test]
    fn test_get_agent_count_returns_zero_initially() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, queue, cache).expect("Should register");

        let count: usize = lua.load("return hub.get_agent_count()").eval().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_get_agents_returns_key_and_pty_count() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        // Populate cache with a test handle
        use crate::agent::pty::PtySession;
        use crate::hub::agent_handle::{AgentHandle, PtyHandle};

        let pty_session = PtySession::new(24, 80);
        let (shared_state, scrollback, event_tx) = pty_session.get_direct_access();
        std::mem::forget(pty_session);
        let pty = PtyHandle::new(event_tx, shared_state, scrollback, None);
        let handle = AgentHandle::new("test-key", vec![pty], 0);
        cache.set_all(vec![handle]);

        register(&lua, queue, cache).expect("Should register");

        let agents: LuaTable = lua.load("return hub.get_agents()").eval().unwrap();
        assert_eq!(agents.len().unwrap(), 1);

        let first: LuaTable = agents.get(1).unwrap();
        let key: String = first.get("key").unwrap();
        let pty_count: usize = first.get("pty_count").unwrap();
        assert_eq!(key, "test-key");
        assert_eq!(pty_count, 1);
    }

    #[test]
    fn test_get_worktrees_returns_empty_array() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, queue, cache).expect("Should register");

        let worktrees: LuaTable = lua.load("return hub.get_worktrees()").eval().unwrap();
        assert_eq!(worktrees.len().unwrap(), 0);
    }

    #[test]
    fn test_get_agents_serializes_as_json_array() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, queue, cache).expect("Should register");

        // Get agents and convert back to JSON to verify array format
        let agents: LuaValue = lua.load("return hub.get_agents()").eval().unwrap();
        let json: serde_json::Value = lua.from_value(agents).unwrap();

        // Empty agents should be an array [], not an object {}
        assert!(json.is_array(), "Empty agents should serialize as JSON array, got: {}", json);
        assert_eq!(json.as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_get_worktrees_serializes_as_json_array() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, queue, cache).expect("Should register");

        // Get worktrees and convert back to JSON to verify array format
        let worktrees: LuaValue = lua.load("return hub.get_worktrees()").eval().unwrap();
        let json: serde_json::Value = lua.from_value(worktrees).unwrap();

        // Empty worktrees should be an array [], not an object {}
        assert!(json.is_array(), "Empty worktrees should serialize as JSON array, got: {}", json);
        assert_eq!(json.as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_quit_queues_request() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, Arc::clone(&queue), cache).expect("Should register");

        lua.load("hub.quit()").exec().expect("Should queue quit");

        let requests = queue.lock()
            .expect("Hub request queue mutex poisoned");
        assert_eq!(requests.len(), 1);
        assert!(matches!(requests[0], HubRequest::Quit));
    }
}
