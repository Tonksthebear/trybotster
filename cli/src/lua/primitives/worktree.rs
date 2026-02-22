//! Worktree primitives for Lua scripts.
//!
//! Exposes git worktree queries and operations to Lua, allowing scripts to
//! list, find, create, and delete worktrees.
//!
//! # Design Principle: "Query freely. Mutate via event."
//!
//! - **Queries** (`list`, `exists`, `find`, `repo_root`) read directly from
//!   `HandleCache` - non-blocking, thread-safe snapshots
//! - **Create** (`create`) runs synchronously (legacy, blocks Hub event loop)
//! - **Create async** (`create_async`) sends `HubEvent::LuaWorktreeRequest` for
//!   Hub to process on a blocking threadpool. Hub fires `worktree_created` or
//!   `worktree_create_failed` Lua events when done.
//! - **Delete** (`delete`) sends `HubEvent::LuaWorktreeRequest` for Hub to
//!   process asynchronously
//!
//! # Usage in Lua
//!
//! ```lua
//! -- List available worktrees (reads from cache)
//! local trees = worktree.list()
//! for i, wt in ipairs(trees) do
//!     log.info(string.format("Worktree: %s -> %s", wt.branch, wt.path))
//! end
//!
//! -- Check if worktree exists for branch
//! if worktree.exists("feature-branch") then
//!     log.info("Worktree exists!")
//! end
//!
//! -- Find worktree path for branch (nil if not found)
//! local path = worktree.find("feature-branch")
//! if path then
//!     log.info("Found at: " .. path)
//! end
//!
//! -- Create worktree synchronously (returns path or errors)
//! local path = worktree.create("feature-branch")
//!
//! -- Create worktree asynchronously (returns immediately, fires event on completion)
//! worktree.create_async({ agent_key = "key", branch = "feature-branch", prompt = "..." })
//!
//! -- Delete worktree (sends event for async processing)
//! worktree.delete("/path/to/worktree", "feature-branch")
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use mlua::prelude::*;

use super::HubEventSender;
use crate::git::WorktreeManager;
use crate::hub::events::HubEvent;
use crate::hub::handle_cache::HandleCache;

/// Worktree operation requests sent from Lua via `HubEvent::LuaWorktreeRequest`.
///
/// These are delivered directly to the Hub event loop.
/// `Create` requests are dispatched to a blocking threadpool via
/// `tokio::task::spawn_blocking` so the Hub event loop stays responsive.
#[derive(Debug, Clone)]
pub enum WorktreeRequest {
    /// Create a worktree asynchronously.
    ///
    /// Hub spawns the git operation on a blocking thread and fires
    /// `worktree_created` or `worktree_create_failed` Lua events on completion.
    /// All context fields are carried through so Lua can resume agent spawning.
    Create {
        /// Agent key for lifecycle broadcasts.
        agent_key: String,
        /// Git branch name for the worktree.
        branch: String,
        /// GitHub issue number (if launched from an issue).
        issue_number: Option<u32>,
        /// Task prompt for the agent.
        prompt: String,
        /// Profile name for config resolution.
        profile_name: Option<String>,
        /// Terminal rows from the requesting client.
        client_rows: u16,
        /// Terminal cols from the requesting client.
        client_cols: u16,
    },
    /// Delete a worktree by path.
    Delete {
        /// Filesystem path of the worktree to delete.
        path: String,
        /// Branch name associated with the worktree.
        branch: String,
    },
}

/// Result of an async worktree creation, sent back to Hub via channel.
///
/// Carries all the context needed for Lua to resume agent spawning
/// after the blocking git operation completes.
#[derive(Debug)]
pub struct WorktreeCreateResult {
    /// Agent key for lifecycle broadcasts.
    pub agent_key: String,
    /// Git branch name.
    pub branch: String,
    /// `Ok(path)` on success, `Err(message)` on failure.
    pub result: Result<std::path::PathBuf, String>,
    /// GitHub issue number (carried forward from request).
    pub issue_number: Option<u32>,
    /// Task prompt (carried forward from request).
    pub prompt: String,
    /// Profile name (carried forward from request).
    pub profile_name: Option<String>,
    /// Terminal rows (carried forward from request).
    pub client_rows: u16,
    /// Terminal cols (carried forward from request).
    pub client_cols: u16,
}

/// Channel type for receiving async worktree creation results.
pub type WorktreeResultReceiver = tokio::sync::mpsc::UnboundedReceiver<WorktreeCreateResult>;

/// Channel type for sending async worktree creation results.
pub type WorktreeResultSender = tokio::sync::mpsc::UnboundedSender<WorktreeCreateResult>;

/// Register worktree primitives with the Lua state.
///
/// Adds the following functions to the `worktree` table:
/// - `worktree.list()` - Get all worktrees as a table of {branch, path}
/// - `worktree.exists(branch)` - Check if worktree exists for branch
/// - `worktree.find(branch)` - Find worktree path for branch (nil if not found)
/// - `worktree.create(branch)` - Synchronously create worktree, returns path
/// - `worktree.delete(path, branch)` - Request worktree deletion (async)
/// - `worktree.repo_root()` - Get the main repository root path (nil if not in repo)
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `hub_event_tx` - Shared sender for Hub events (filled in later by `set_hub_event_tx`)
/// * `handle_cache` - Thread-safe cache for worktree queries
/// * `worktree_base` - Base directory for worktree storage
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub(crate) fn register(
    lua: &Lua,
    hub_event_tx: HubEventSender,
    handle_cache: Arc<HandleCache>,
    worktree_base: PathBuf,
) -> Result<()> {
    let worktree = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create worktree table: {e}"))?;

    // worktree.list() -> table of {branch = "...", path = "..."}
    //
    // Returns all available worktrees from HandleCache.
    // Uses serde serialization to ensure proper JSON array format.
    let cache = Arc::clone(&handle_cache);
    let list_fn = lua
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

            // Convert to Lua via json_to_lua (null-safe, unlike lua.to_value)
            super::json::json_to_lua(lua, &serde_json::Value::Array(worktrees_data))
        })
        .map_err(|e| anyhow!("Failed to create worktree.list function: {e}"))?;

    worktree
        .set("list", list_fn)
        .map_err(|e| anyhow!("Failed to set worktree.list: {e}"))?;

    // worktree.exists(branch) -> boolean
    //
    // Checks if a worktree exists for the given branch name.
    let cache2 = Arc::clone(&handle_cache);
    let exists_fn = lua
        .create_function(move |_, branch: String| {
            let worktrees = cache2.get_worktrees();
            let found = worktrees.iter().any(|(_, b)| b == &branch);
            Ok(found)
        })
        .map_err(|e| anyhow!("Failed to create worktree.exists function: {e}"))?;

    worktree
        .set("exists", exists_fn)
        .map_err(|e| anyhow!("Failed to set worktree.exists: {e}"))?;

    // worktree.find(branch) -> path string or nil
    //
    // Finds the filesystem path for a worktree by branch name.
    // Returns nil if no worktree exists for the branch.
    let cache3 = Arc::clone(&handle_cache);
    let find_fn = lua
        .create_function(move |_, branch: String| {
            let worktrees = cache3.get_worktrees();
            let path = worktrees
                .iter()
                .find(|(_, b)| b == &branch)
                .map(|(p, _)| p.clone());
            Ok(path)
        })
        .map_err(|e| anyhow!("Failed to create worktree.find function: {e}"))?;

    worktree
        .set("find", find_fn)
        .map_err(|e| anyhow!("Failed to set worktree.find: {e}"))?;

    // worktree.create(branch) -> path string or error
    //
    // Synchronously creates a worktree for the given branch name.
    // Returns the filesystem path on success, raises Lua error on failure.
    // Also updates HandleCache so worktree.find() sees it immediately.
    let create_base = worktree_base.clone();
    let create_cache = Arc::clone(&handle_cache);
    let create_fn = lua
        .create_function(move |_, branch: String| {
            let manager = WorktreeManager::new(create_base.clone());
            match manager.create_worktree_with_branch(&branch) {
                Ok(path) => {
                    let path_str = path.to_string_lossy().to_string();
                    log::info!("Created worktree for branch '{}' at {}", branch, path_str);

                    // Update HandleCache so worktree.find() sees it immediately
                    let mut worktrees = create_cache.get_worktrees();
                    worktrees.push((path_str.clone(), branch));
                    create_cache.set_worktrees(worktrees);

                    Ok(path_str)
                }
                Err(e) => Err(mlua::Error::runtime(format!(
                    "Failed to create worktree for branch '{}': {}",
                    branch, e
                ))),
            }
        })
        .map_err(|e| anyhow!("Failed to create worktree.create function: {e}"))?;

    worktree
        .set("create", create_fn)
        .map_err(|e| anyhow!("Failed to set worktree.create: {e}"))?;

    // worktree.create_async(params) - Queue async worktree creation
    //
    // Queues a worktree creation request for Hub to process on a blocking
    // threadpool. Returns immediately. Hub fires `worktree_created` or
    // `worktree_create_failed` Lua events on completion.
    //
    // params is a table with: agent_key, branch, issue_number, prompt,
    // profile_name, client_rows, client_cols
    let tx = hub_event_tx.clone();
    let create_async_fn = lua
        .create_function(move |_, params: LuaTable| {
            let agent_key: String = params.get("agent_key").map_err(|e| {
                mlua::Error::runtime(format!("create_async: missing agent_key: {e}"))
            })?;
            let branch: String = params.get("branch").map_err(|e| {
                mlua::Error::runtime(format!("create_async: missing branch: {e}"))
            })?;
            let prompt: String = params.get::<Option<String>>("prompt")
                .unwrap_or(None)
                .unwrap_or_default();
            let issue_number: Option<u32> = params.get("issue_number").unwrap_or(None);
            let profile_name: Option<String> = params.get("profile_name").unwrap_or(None);
            let client_rows: u16 = params.get("client_rows").unwrap_or(24);
            let client_cols: u16 = params.get("client_cols").unwrap_or(80);

            let guard = tx.lock().expect("HubEventSender mutex poisoned");
            if let Some(ref sender) = *guard {
                let _ = sender.send(HubEvent::LuaWorktreeRequest(WorktreeRequest::Create {
                    agent_key,
                    branch,
                    issue_number,
                    prompt,
                    profile_name,
                    client_rows,
                    client_cols,
                }));
            } else {
                ::log::warn!("[Worktree] create_async() called before hub_event_tx set — event dropped");
            }
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create worktree.create_async function: {e}"))?;

    worktree
        .set("create_async", create_async_fn)
        .map_err(|e| anyhow!("Failed to set worktree.create_async: {e}"))?;

    // Detect git repo once at registration time — shared by is_git_repo() and repo_root().
    let detected = WorktreeManager::detect_current_repo();
    let is_git = detected.is_ok();
    let repo_root: Option<String> = match detected {
        Ok((path, _name)) => Some(path.to_string_lossy().to_string()),
        Err(_) => std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string()),
    };

    // worktree.is_git_repo() -> boolean
    //
    // Returns true if running inside a git repository, false otherwise.
    let is_git_fn = lua
        .create_function(move |_, ()| Ok(is_git))
        .map_err(|e| anyhow!("Failed to create worktree.is_git_repo function: {e}"))?;

    worktree
        .set("is_git_repo", is_git_fn)
        .map_err(|e| anyhow!("Failed to set worktree.is_git_repo: {e}"))?;

    // worktree.repo_root() -> path string or nil
    //
    // Returns the git repo root, or cwd if not in a git repo.
    let repo_root_fn = lua
        .create_function(move |_, ()| Ok(repo_root.clone()))
        .map_err(|e| anyhow!("Failed to create worktree.repo_root function: {e}"))?;

    worktree
        .set("repo_root", repo_root_fn)
        .map_err(|e| anyhow!("Failed to set worktree.repo_root: {e}"))?;

    // worktree.copy_from_patterns(repo_root, dest, patterns_file) -> true | error
    //
    // Copies files from repo_root to dest matching glob patterns in patterns_file.
    // Raises a Lua error on failure (callers use pcall for error handling).
    let copy_fn = lua
        .create_function(
            |_, (repo_root, dest, patterns_file): (String, String, String)| {
                use std::path::Path;
                WorktreeManager::copy_from_patterns(
                    Path::new(&repo_root),
                    Path::new(&dest),
                    Path::new(&patterns_file),
                )
                .map(|()| true)
                .map_err(|e| mlua::Error::runtime(format!(
                    "Failed to copy patterns from '{}': {}",
                    patterns_file, e
                )))
            },
        )
        .map_err(|e| anyhow!("Failed to create worktree.copy_from_patterns function: {e}"))?;

    worktree
        .set("copy_from_patterns", copy_fn)
        .map_err(|e| anyhow!("Failed to set worktree.copy_from_patterns: {e}"))?;

    // worktree.delete(path, branch) - Queue worktree deletion
    //
    // Queues a request to delete a worktree. Hub processes it asynchronously.
    let tx = hub_event_tx;
    let delete_fn = lua
        .create_function(move |_, (path, branch): (String, String)| {
            let guard = tx.lock().expect("HubEventSender mutex poisoned");
            if let Some(ref sender) = *guard {
                let _ = sender.send(HubEvent::LuaWorktreeRequest(WorktreeRequest::Delete { path, branch }));
            } else {
                ::log::warn!("[Worktree] delete() called before hub_event_tx set — event dropped");
            }
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create worktree.delete function: {e}"))?;

    worktree
        .set("delete", delete_fn)
        .map_err(|e| anyhow!("Failed to set worktree.delete: {e}"))?;

    // Register globally
    lua.globals()
        .set("worktree", worktree)
        .map_err(|e| anyhow!("Failed to register worktree table globally: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::new_hub_event_sender;

    fn create_test_queue_and_cache() -> (HubEventSender, Arc<HandleCache>, PathBuf) {
        (
            new_hub_event_sender(),
            Arc::new(HandleCache::new()),
            PathBuf::from("/tmp/test-worktrees"),
        )
    }

    #[test]
    fn test_register_creates_worktree_table() {
        let lua = Lua::new();
        let (tx, cache, base) = create_test_queue_and_cache();

        register(&lua, tx, cache, base).expect("Should register worktree primitives");

        let wt: LuaTable = lua
            .globals()
            .get("worktree")
            .expect("worktree table should exist");
        assert!(wt.contains_key("list").unwrap());
        assert!(wt.contains_key("exists").unwrap());
        assert!(wt.contains_key("find").unwrap());
        assert!(wt.contains_key("create").unwrap());
        assert!(wt.contains_key("create_async").unwrap());
        assert!(wt.contains_key("copy_from_patterns").unwrap());
        assert!(wt.contains_key("delete").unwrap());
        assert!(wt.contains_key("repo_root").unwrap());
    }

    #[test]
    fn test_list_returns_empty_table() {
        let lua = Lua::new();
        let (tx, cache, base) = create_test_queue_and_cache();

        register(&lua, tx, cache, base).expect("Should register");

        let worktrees: LuaTable = lua.load("return worktree.list()").eval().unwrap();
        assert_eq!(worktrees.len().unwrap(), 0);
    }

    /// Empty worktree list returns an empty Lua table (iterable, length 0).
    #[test]
    fn test_list_empty_returns_table() {
        let lua = Lua::new();
        let (tx, cache, base) = create_test_queue_and_cache();

        register(&lua, tx, cache, base).expect("Should register");

        let worktrees: LuaTable = lua.load("return worktree.list()").eval().unwrap();
        assert_eq!(worktrees.len().unwrap(), 0, "Empty worktree list should have length 0");
    }

    #[test]
    fn test_list_returns_cached_worktrees() {
        let lua = Lua::new();
        let (tx, cache, base) = create_test_queue_and_cache();

        // Pre-populate the cache
        cache.set_worktrees(vec![
            ("/path/to/wt1".to_string(), "feature-a".to_string()),
            ("/path/to/wt2".to_string(), "feature-b".to_string()),
        ]);

        register(&lua, tx, cache, base).expect("Should register");

        let worktrees: LuaTable = lua.load("return worktree.list()").eval().unwrap();
        assert_eq!(worktrees.len().unwrap(), 2);
    }

    #[test]
    fn test_exists_returns_false_when_empty() {
        let lua = Lua::new();
        let (tx, cache, base) = create_test_queue_and_cache();

        register(&lua, tx, cache, base).expect("Should register");

        let exists: bool = lua
            .load(r#"return worktree.exists("feature-a")"#)
            .eval()
            .unwrap();
        assert!(!exists);
    }

    #[test]
    fn test_exists_returns_true_when_found() {
        let lua = Lua::new();
        let (tx, cache, base) = create_test_queue_and_cache();

        cache.set_worktrees(vec![(
            "/path/to/wt1".to_string(),
            "feature-a".to_string(),
        )]);

        register(&lua, tx, cache, base).expect("Should register");

        let exists: bool = lua
            .load(r#"return worktree.exists("feature-a")"#)
            .eval()
            .unwrap();
        assert!(exists);
    }

    #[test]
    fn test_exists_returns_false_for_wrong_branch() {
        let lua = Lua::new();
        let (tx, cache, base) = create_test_queue_and_cache();

        cache.set_worktrees(vec![(
            "/path/to/wt1".to_string(),
            "feature-a".to_string(),
        )]);

        register(&lua, tx, cache, base).expect("Should register");

        let exists: bool = lua
            .load(r#"return worktree.exists("feature-b")"#)
            .eval()
            .unwrap();
        assert!(!exists);
    }

    #[test]
    fn test_find_returns_nil_when_not_found() {
        let lua = Lua::new();
        let (tx, cache, base) = create_test_queue_and_cache();

        register(&lua, tx, cache, base).expect("Should register");

        let result: LuaValue = lua
            .load(r#"return worktree.find("feature-a")"#)
            .eval()
            .unwrap();
        assert!(matches!(result, LuaValue::Nil));
    }

    #[test]
    fn test_find_returns_path_when_found() {
        let lua = Lua::new();
        let (tx, cache, base) = create_test_queue_and_cache();

        cache.set_worktrees(vec![(
            "/path/to/wt1".to_string(),
            "feature-a".to_string(),
        )]);

        register(&lua, tx, cache, base).expect("Should register");

        let path: String = lua
            .load(r#"return worktree.find("feature-a")"#)
            .eval()
            .unwrap();
        assert_eq!(path, "/path/to/wt1");
    }

    #[test]
    fn test_delete_sends_event() {
        let lua = Lua::new();
        let tx = new_hub_event_sender();
        let (sender, mut rx) = tokio::sync::mpsc::unbounded_channel();
        *tx.lock().unwrap() = Some(sender);
        let cache = Arc::new(HandleCache::new());
        let base = PathBuf::from("/tmp/test-worktrees");

        register(&lua, tx, cache, base).expect("Should register");

        lua.load(r#"worktree.delete("/path/to/wt", "feature-branch")"#)
            .exec()
            .expect("Should delete worktree");

        let event = rx.try_recv().expect("Should have received event");
        match event {
            HubEvent::LuaWorktreeRequest(WorktreeRequest::Delete { path, branch }) => {
                assert_eq!(path, "/path/to/wt");
                assert_eq!(branch, "feature-branch");
            }
            _ => panic!("Expected LuaWorktreeRequest(Delete) event"),
        }
    }

    #[test]
    fn test_multiple_delete_requests_send_events_in_order() {
        let lua = Lua::new();
        let tx = new_hub_event_sender();
        let (sender, mut rx) = tokio::sync::mpsc::unbounded_channel();
        *tx.lock().unwrap() = Some(sender);
        let cache = Arc::new(HandleCache::new());
        let base = PathBuf::from("/tmp/test-worktrees");

        register(&lua, tx, cache, base).expect("Should register");

        lua.load(
            r#"
            worktree.delete("/path/to/wt1", "branch-1")
            worktree.delete("/path/to/wt2", "branch-2")
        "#,
        )
        .exec()
        .expect("Should send multiple events");

        match rx.try_recv().unwrap() {
            HubEvent::LuaWorktreeRequest(WorktreeRequest::Delete { .. }) => {}
            _ => panic!("Expected LuaWorktreeRequest(Delete)"),
        }
        match rx.try_recv().unwrap() {
            HubEvent::LuaWorktreeRequest(WorktreeRequest::Delete { .. }) => {}
            _ => panic!("Expected LuaWorktreeRequest(Delete)"),
        }
    }

    #[test]
    fn test_repo_root_returns_value_or_nil() {
        let lua = Lua::new();
        let (tx, cache, base) = create_test_queue_and_cache();

        register(&lua, tx, cache, base).expect("Should register");

        // repo_root() should return a string or nil depending on whether
        // we're running in a git repo. In tests, this is typically a git repo.
        let result: LuaValue = lua
            .load("return worktree.repo_root()")
            .eval()
            .expect("repo_root should not error");

        // Either nil or a string path
        match result {
            LuaValue::Nil => {
                // OK - not in a git repo
            }
            LuaValue::String(s) => {
                // OK - should be a valid path
                assert!(!s.to_str().unwrap().is_empty(), "repo_root should not be empty string");
            }
            _ => panic!("repo_root should return nil or string, got {:?}", result),
        }
    }

    #[test]
    fn test_create_async_sends_event() {
        let lua = Lua::new();
        let tx = new_hub_event_sender();
        let (sender, mut rx) = tokio::sync::mpsc::unbounded_channel();
        *tx.lock().unwrap() = Some(sender);
        let cache = Arc::new(HandleCache::new());
        let base = PathBuf::from("/tmp/test-worktrees");

        register(&lua, tx, cache, base).expect("Should register");

        lua.load(
            r#"worktree.create_async({
                agent_key = "test-repo-42",
                branch = "feature-branch",
                issue_number = 42,
                prompt = "Fix the bug",
                profile_name = "default",
                client_rows = 30,
                client_cols = 120,
            })"#,
        )
        .exec()
        .expect("Should send create_async event");

        let event = rx.try_recv().expect("Should have received event");
        match event {
            HubEvent::LuaWorktreeRequest(WorktreeRequest::Create {
                agent_key,
                branch,
                issue_number,
                prompt,
                profile_name,
                client_rows,
                client_cols,
            }) => {
                assert_eq!(agent_key, "test-repo-42");
                assert_eq!(branch, "feature-branch");
                assert_eq!(issue_number, Some(42));
                assert_eq!(prompt, "Fix the bug");
                assert_eq!(profile_name.as_deref(), Some("default"));
                assert_eq!(client_rows, 30);
                assert_eq!(client_cols, 120);
            }
            _ => panic!("Expected LuaWorktreeRequest(Create) event"),
        }
    }

    #[test]
    fn test_create_returns_error_for_invalid_branch() {
        let lua = Lua::new();
        let (tx, cache, base) = create_test_queue_and_cache();

        register(&lua, tx, cache, base).expect("Should register");

        // Calling create() with a branch name when not in a repo (or invalid setup)
        // should raise a Lua error rather than panic
        let result: LuaResult<String> = lua
            .load(r#"return worktree.create("test-branch-that-will-fail")"#)
            .eval();

        // The call might succeed (if running in a valid git repo) or fail gracefully.
        // Either way, it shouldn't panic.
        match result {
            Ok(path) => {
                // If it succeeded, it should return a path string
                assert!(!path.is_empty());
            }
            Err(e) => {
                // If it failed, the error should be descriptive
                let err_msg = e.to_string();
                assert!(
                    err_msg.contains("Failed to create worktree") || err_msg.contains("git"),
                    "Error should mention worktree creation failure: {}",
                    err_msg
                );
            }
        }
    }

    /// Proves that `worktree.list()` converts data to proper Lua tables
    /// (not userdata). The conversion path must use `json_to_lua` for safety.
    #[test]
    fn test_list_returns_proper_lua_tables() {
        let lua = Lua::new();
        let (tx, cache, base) = create_test_queue_and_cache();

        // Inject a worktree so list returns data
        cache.set_worktrees(vec![("/tmp/wt".to_string(), "main".to_string())]);

        register(&lua, tx, cache, base).expect("Should register");

        let result: LuaValue = lua.load("return worktree.list()").eval().unwrap();
        assert!(
            result.is_table(),
            "worktree.list() should return a table, got: {:?}",
            result
        );
        let tbl = result.as_table().unwrap();
        assert_eq!(tbl.len().unwrap(), 1);

        // Verify nested entry is a proper table (not userdata)
        let entry: LuaValue = tbl.get(1).unwrap();
        assert!(
            entry.is_table(),
            "worktree entry should be a table, got: {:?}",
            entry
        );

        // Verify fields are accessible as strings
        let entry_tbl = entry.as_table().unwrap();
        let path: String = entry_tbl.get("path").unwrap();
        let branch: String = entry_tbl.get("branch").unwrap();
        assert_eq!(path, "/tmp/wt");
        assert_eq!(branch, "main");
    }
}
