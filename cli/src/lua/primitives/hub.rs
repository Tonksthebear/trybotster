//! Hub state primitives for Lua scripts.
//!
//! Exposes Hub state queries and operations to Lua, allowing scripts to
//! inspect agents, worktrees, and request agent lifecycle operations.
//!
//! # Design Principle: "Query freely. Mutate via queue."
//!
//! - **State queries** (get_agents, get_agent, get_worktrees) read directly from
//!   HandleCache - non-blocking, thread-safe snapshots
//! - **Operations** (create_agent, delete_agent) queue requests for Hub to process
//!   asynchronously after Lua callbacks return
//!
//! # Usage in Lua
//!
//! ```lua
//! -- Query agents (non-blocking, reads from cache)
//! local agents = hub.get_agents()
//! for i, agent in ipairs(agents) do
//!     log.info(string.format("Agent %d: %s (%s)", i, agent.id, agent.status))
//! end
//!
//! -- Get single agent by index
//! local agent = hub.get_agent(0)
//! if agent then
//!     log.info("First agent: " .. agent.id)
//! end
//!
//! -- Get available worktrees
//! local worktrees = hub.get_worktrees()
//!
//! -- Request agent creation (async - returns request key)
//! local key = hub.create_agent({
//!     issue_or_branch = "42",
//!     prompt = "Fix the bug",
//! })
//!
//! -- Request agent deletion (async)
//! hub.delete_agent("owner-repo-42", true)  -- true = delete worktree
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
    /// Create a new agent.
    CreateAgent {
        /// Issue number or branch name.
        issue_or_branch: String,
        /// Optional prompt for the agent.
        prompt: Option<String>,
        /// Optional worktree path for reopening existing worktree.
        from_worktree: Option<String>,
        /// Response key for tracking the request.
        response_key: String,
    },
    /// Delete an agent.
    DeleteAgent {
        /// Agent ID (session key) to delete.
        agent_id: String,
        /// Whether to also delete the worktree.
        delete_worktree: bool,
    },
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
/// - `hub.get_agents()` - Get all agents as a table
/// - `hub.get_agent(index)` - Get single agent by index
/// - `hub.get_agent_count()` - Get number of agents
/// - `hub.get_worktrees()` - Get available worktrees
/// - `hub.create_agent(opts)` - Request agent creation (async)
/// - `hub.delete_agent(agent_id, delete_worktree?)` - Request agent deletion (async)
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

    // hub.get_agents() - Returns table of agent info
    let cache = Arc::clone(&handle_cache);
    let get_agents_fn = lua
        .create_function(move |lua, ()| {
            let agents = cache.get_all_agents();
            let result = lua.create_table()?;

            for (i, agent) in agents.iter().enumerate() {
                let info = agent.info();
                let agent_table = lua.create_table()?;

                // Core identity
                agent_table.set("index", i)?;
                agent_table.set("id", info.id.clone())?;

                // Repository info
                if let Some(ref repo) = info.repo {
                    agent_table.set("repo", repo.clone())?;
                }
                if let Some(issue) = info.issue_number {
                    agent_table.set("issue_number", issue)?;
                }
                if let Some(ref branch) = info.branch_name {
                    agent_table.set("branch_name", branch.clone())?;
                }

                // Status
                if let Some(ref status) = info.status {
                    agent_table.set("status", status.clone())?;
                }

                // Server info
                if let Some(port) = info.port {
                    agent_table.set("port", port)?;
                }
                if let Some(server_running) = info.server_running {
                    agent_table.set("server_running", server_running)?;
                }
                if let Some(has_server_pty) = info.has_server_pty {
                    agent_table.set("has_server_pty", has_server_pty)?;
                }

                // PTY count from handle
                agent_table.set("pty_count", agent.pty_count())?;

                result.set(i + 1, agent_table)?;
            }

            Ok(result)
        })
        .map_err(|e| anyhow!("Failed to create hub.get_agents function: {e}"))?;

    hub.set("get_agents", get_agents_fn)
        .map_err(|e| anyhow!("Failed to set hub.get_agents: {e}"))?;

    // hub.get_agent(index) - Returns single agent info or nil
    let cache2 = Arc::clone(&handle_cache);
    let get_agent_fn = lua
        .create_function(move |lua, index: usize| {
            match cache2.get_agent(index) {
                Some(agent) => {
                    let info = agent.info();
                    let agent_table = lua.create_table()?;

                    // Core identity
                    agent_table.set("index", index)?;
                    agent_table.set("id", info.id.clone())?;

                    // Repository info
                    if let Some(ref repo) = info.repo {
                        agent_table.set("repo", repo.clone())?;
                    }
                    if let Some(issue) = info.issue_number {
                        agent_table.set("issue_number", issue)?;
                    }
                    if let Some(ref branch) = info.branch_name {
                        agent_table.set("branch_name", branch.clone())?;
                    }

                    // Status
                    if let Some(ref status) = info.status {
                        agent_table.set("status", status.clone())?;
                    }

                    // Server info
                    if let Some(port) = info.port {
                        agent_table.set("port", port)?;
                    }
                    if let Some(server_running) = info.server_running {
                        agent_table.set("server_running", server_running)?;
                    }
                    if let Some(has_server_pty) = info.has_server_pty {
                        agent_table.set("has_server_pty", has_server_pty)?;
                    }

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

    // hub.get_worktrees() - Returns available worktrees
    let cache4 = Arc::clone(&handle_cache);
    let get_worktrees_fn = lua
        .create_function(move |lua, ()| {
            let worktrees = cache4.get_worktrees();
            let result = lua.create_table()?;

            for (i, (path, branch)) in worktrees.iter().enumerate() {
                let wt = lua.create_table()?;
                wt.set("path", path.clone())?;
                wt.set("branch", branch.clone())?;
                result.set(i + 1, wt)?;
            }

            Ok(result)
        })
        .map_err(|e| anyhow!("Failed to create hub.get_worktrees function: {e}"))?;

    hub.set("get_worktrees", get_worktrees_fn)
        .map_err(|e| anyhow!("Failed to set hub.get_worktrees: {e}"))?;

    // hub.create_agent({ issue_or_branch, prompt?, from_worktree? }) -> response_key
    let queue = Arc::clone(&request_queue);
    let create_agent_fn = lua
        .create_function(move |_, opts: LuaTable| {
            let issue_or_branch: String = opts
                .get("issue_or_branch")
                .map_err(|_| LuaError::runtime("issue_or_branch is required"))?;
            let prompt: Option<String> = opts.get("prompt").ok();
            let from_worktree: Option<String> = opts.get("from_worktree").ok();

            let response_key = format!("create_agent:{}", uuid::Uuid::new_v4());

            let mut q = queue.lock()
                .expect("Hub request queue mutex poisoned");
            q.push(HubRequest::CreateAgent {
                issue_or_branch,
                prompt,
                from_worktree,
                response_key: response_key.clone(),
            });

            Ok(response_key)
        })
        .map_err(|e| anyhow!("Failed to create hub.create_agent function: {e}"))?;

    hub.set("create_agent", create_agent_fn)
        .map_err(|e| anyhow!("Failed to set hub.create_agent: {e}"))?;

    // hub.delete_agent(agent_id, delete_worktree?)
    let queue2 = request_queue;
    let delete_agent_fn = lua
        .create_function(move |_, (agent_id, delete_worktree): (String, Option<bool>)| {
            let mut q = queue2.lock()
                .expect("Hub request queue mutex poisoned");
            q.push(HubRequest::DeleteAgent {
                agent_id,
                delete_worktree: delete_worktree.unwrap_or(false),
            });
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create hub.delete_agent function: {e}"))?;

    hub.set("delete_agent", delete_agent_fn)
        .map_err(|e| anyhow!("Failed to set hub.delete_agent: {e}"))?;

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
        assert!(hub.contains_key("create_agent").unwrap());
        assert!(hub.contains_key("delete_agent").unwrap());
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
    fn test_get_worktrees_returns_empty_table() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, queue, cache).expect("Should register");

        let worktrees: LuaTable = lua.load("return hub.get_worktrees()").eval().unwrap();
        assert_eq!(worktrees.len().unwrap(), 0);
    }

    #[test]
    fn test_create_agent_queues_request() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, Arc::clone(&queue), cache).expect("Should register");

        let key: String = lua
            .load(
                r#"
            return hub.create_agent({
                issue_or_branch = "42",
                prompt = "Fix the bug",
            })
        "#,
            )
            .eval()
            .expect("Should create agent");

        assert!(key.starts_with("create_agent:"), "Key should have prefix");

        let requests = queue.lock()
            .expect("Hub request queue mutex poisoned");
        assert_eq!(requests.len(), 1);

        match &requests[0] {
            HubRequest::CreateAgent {
                issue_or_branch,
                prompt,
                from_worktree,
                response_key,
            } => {
                assert_eq!(issue_or_branch, "42");
                assert_eq!(prompt, &Some("Fix the bug".to_string()));
                assert!(from_worktree.is_none());
                assert_eq!(response_key, &key);
            }
            _ => panic!("Expected CreateAgent request"),
        }
    }

    #[test]
    fn test_create_agent_requires_issue_or_branch() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, queue, cache).expect("Should register");

        let result: mlua::Result<String> = lua
            .load(
                r#"
            return hub.create_agent({
                prompt = "No issue or branch",
            })
        "#,
            )
            .eval();

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("issue_or_branch"),
            "Error should mention issue_or_branch: {}",
            err_msg
        );
    }

    #[test]
    fn test_create_agent_with_from_worktree() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, Arc::clone(&queue), cache).expect("Should register");

        lua.load(
            r#"
            hub.create_agent({
                issue_or_branch = "feature-branch",
                from_worktree = "/path/to/worktree",
            })
        "#,
        )
        .exec()
        .expect("Should create agent");

        let requests = queue.lock()
            .expect("Hub request queue mutex poisoned");
        match &requests[0] {
            HubRequest::CreateAgent {
                from_worktree, ..
            } => {
                assert_eq!(from_worktree, &Some("/path/to/worktree".to_string()));
            }
            _ => panic!("Expected CreateAgent request"),
        }
    }

    #[test]
    fn test_delete_agent_queues_request() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, Arc::clone(&queue), cache).expect("Should register");

        lua.load(r#"hub.delete_agent("owner-repo-42", true)"#)
            .exec()
            .expect("Should delete agent");

        let requests = queue.lock()
            .expect("Hub request queue mutex poisoned");
        assert_eq!(requests.len(), 1);

        match &requests[0] {
            HubRequest::DeleteAgent {
                agent_id,
                delete_worktree,
            } => {
                assert_eq!(agent_id, "owner-repo-42");
                assert!(*delete_worktree);
            }
            _ => panic!("Expected DeleteAgent request"),
        }
    }

    #[test]
    fn test_delete_agent_defaults_delete_worktree_to_false() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, Arc::clone(&queue), cache).expect("Should register");

        lua.load(r#"hub.delete_agent("owner-repo-42")"#)
            .exec()
            .expect("Should delete agent");

        let requests = queue.lock()
            .expect("Hub request queue mutex poisoned");
        match &requests[0] {
            HubRequest::DeleteAgent {
                delete_worktree, ..
            } => {
                assert!(!*delete_worktree);
            }
            _ => panic!("Expected DeleteAgent request"),
        }
    }

    #[test]
    fn test_multiple_requests_queue_in_order() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, Arc::clone(&queue), cache).expect("Should register");

        lua.load(
            r#"
            hub.create_agent({ issue_or_branch = "1" })
            hub.delete_agent("agent-1")
            hub.create_agent({ issue_or_branch = "2" })
        "#,
        )
        .exec()
        .expect("Should queue multiple requests");

        let requests = queue.lock()
            .expect("Hub request queue mutex poisoned");
        assert_eq!(requests.len(), 3);

        assert!(matches!(requests[0], HubRequest::CreateAgent { .. }));
        assert!(matches!(requests[1], HubRequest::DeleteAgent { .. }));
        assert!(matches!(requests[2], HubRequest::CreateAgent { .. }));
    }
}
