//! Hub state primitives for Lua scripts.
//!
//! Exposes Hub state queries and operations to Lua, allowing scripts to
//! inspect worktrees, register/unregister agents, and request lifecycle operations.
//!
//! # Design Principle: "Query freely. Mutate via queue."
//!
//! - **State queries** (get_worktrees) read directly from HandleCache
//! - **Registration** (register_agent, unregister_agent) manages PTY handles
//! - **Operations** (quit) queue requests for Hub to process asynchronously
//!
//! # Usage in Lua
//!
//! ```lua
//! -- Get available worktrees
//! local worktrees = hub.get_worktrees()
//!
//! -- Register agent PTY handles
//! local index = hub.register_agent("owner-repo-42", sessions)
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
/// - `hub.get_worktrees()` - Get available worktrees
/// - `hub.register_agent(key, sessions)` - Register agent PTY handles
/// - `hub.unregister_agent(key)` - Unregister agent PTY handles
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

    // hub.get_worktrees() - Returns array of available worktrees
    // Uses serde serialization to ensure proper JSON array format
    let cache = Arc::clone(&handle_cache);
    let get_worktrees_fn = lua
        .create_function(move |lua, ()| {
            let worktrees = cache.get_worktrees();

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
    //   sessions: array - Ordered Lua array of PtySessionHandle userdata
    //                     Index order determines PTY index (agent=0, then alphabetical)
    let cache2 = Arc::clone(&handle_cache);
    let register_agent_fn = lua
        .create_function(move |_, (agent_key, sessions): (String, LuaTable)| {
            use crate::hub::agent_handle::{AgentPtys, PtyHandle};
            use crate::lua::primitives::pty::PtySessionHandle;

            let mut pty_handles: Vec<PtyHandle> = Vec::new();

            // Iterate ordered Lua array (1-based indices)
            for i in 1..=sessions.raw_len() {
                if let Ok(ud) = sessions.get::<LuaAnyUserData>(i) {
                    match ud.borrow::<PtySessionHandle>() {
                        Ok(handle) => {
                            pty_handles.push(handle.to_pty_handle());
                            log::debug!(
                                "[Lua] Extracted PTY handle at index {} for '{}'",
                                i - 1, agent_key
                            );
                        }
                        Err(e) => {
                            log::warn!(
                                "[Lua] Failed to borrow PTY session at index {} for '{}': {}",
                                i, agent_key, e
                            );
                        }
                    }
                }
            }

            if pty_handles.is_empty() {
                log::error!(
                    "[Lua] register_agent '{}' failed: no valid PTY sessions found in array",
                    agent_key
                );
                return Err(LuaError::runtime(
                    "register_agent requires at least one PTY session"
                ));
            }

            let pty_count = pty_handles.len();
            let agent_index = cache2.len();
            let handle = AgentPtys::new(agent_key.clone(), pty_handles, agent_index);

            match cache2.add_agent(handle) {
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
    let cache3 = Arc::clone(&handle_cache);
    let unregister_agent_fn = lua
        .create_function(move |_, agent_key: String| {
            let removed = cache3.remove_agent(&agent_key);
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
        assert!(hub.contains_key("get_worktrees").unwrap());
        assert!(hub.contains_key("register_agent").unwrap());
        assert!(hub.contains_key("unregister_agent").unwrap());
        assert!(hub.contains_key("quit").unwrap());
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
