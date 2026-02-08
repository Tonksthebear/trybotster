//! Worktree primitives for Lua scripts.
//!
//! Exposes git worktree queries and operations to Lua, allowing scripts to
//! list, find, create, and delete worktrees.
//!
//! # Design Principle: "Query freely. Create synchronously. Delete via queue."
//!
//! - **Queries** (`list`, `exists`, `find`, `repo_root`) read directly from
//!   `HandleCache` - non-blocking, thread-safe snapshots
//! - **Create** (`create`) runs synchronously (git worktree add is fast)
//! - **Delete** (`delete`) queues request for Hub to process asynchronously
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
//! -- Delete worktree (queues request for async processing)
//! worktree.delete("/path/to/worktree", "feature-branch")
//! ```

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use mlua::prelude::*;

use crate::git::WorktreeManager;
use crate::hub::handle_cache::HandleCache;

/// Worktree operation requests queued from Lua.
///
/// These are processed by Hub in its event loop after Lua callbacks return.
#[derive(Debug, Clone)]
pub enum WorktreeRequest {
    /// Delete a worktree by path.
    Delete {
        /// Filesystem path of the worktree to delete.
        path: String,
        /// Branch name associated with the worktree.
        branch: String,
    },
}

/// Shared request queue for worktree operations from Lua.
pub type WorktreeRequestQueue = Arc<Mutex<Vec<WorktreeRequest>>>;

/// Create a new worktree request queue.
#[must_use]
pub fn new_request_queue() -> WorktreeRequestQueue {
    Arc::new(Mutex::new(Vec::new()))
}

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
/// * `request_queue` - Shared queue for worktree operations (processed by Hub)
/// * `handle_cache` - Thread-safe cache for worktree queries
/// * `worktree_base` - Base directory for worktree storage
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub fn register(
    lua: &Lua,
    request_queue: WorktreeRequestQueue,
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

            // Convert to Lua - Vec serializes as array
            lua.to_value(&worktrees_data)
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

    // worktree.repo_root() -> path string or nil
    //
    // Returns the main repository root path, or nil if not in a git repository.
    // Detected once at registration time for efficiency.
    let repo_root: Option<String> = match WorktreeManager::detect_current_repo() {
        Ok((path, _name)) => Some(path.to_string_lossy().to_string()),
        Err(_) => None,
    };
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
    let delete_queue = request_queue;
    let delete_fn = lua
        .create_function(move |_, (path, branch): (String, String)| {
            let mut q = delete_queue
                .lock()
                .expect("Worktree request queue mutex poisoned");
            q.push(WorktreeRequest::Delete { path, branch });
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

    fn create_test_queue_and_cache() -> (WorktreeRequestQueue, Arc<HandleCache>, PathBuf) {
        (
            new_request_queue(),
            Arc::new(HandleCache::new()),
            PathBuf::from("/tmp/test-worktrees"),
        )
    }

    #[test]
    fn test_register_creates_worktree_table() {
        let lua = Lua::new();
        let (queue, cache, base) = create_test_queue_and_cache();

        register(&lua, queue, cache, base).expect("Should register worktree primitives");

        let wt: LuaTable = lua
            .globals()
            .get("worktree")
            .expect("worktree table should exist");
        assert!(wt.contains_key("list").unwrap());
        assert!(wt.contains_key("exists").unwrap());
        assert!(wt.contains_key("find").unwrap());
        assert!(wt.contains_key("create").unwrap());
        assert!(wt.contains_key("copy_from_patterns").unwrap());
        assert!(wt.contains_key("delete").unwrap());
        assert!(wt.contains_key("repo_root").unwrap());
    }

    #[test]
    fn test_list_returns_empty_table() {
        let lua = Lua::new();
        let (queue, cache, base) = create_test_queue_and_cache();

        register(&lua, queue, cache, base).expect("Should register");

        let worktrees: LuaTable = lua.load("return worktree.list()").eval().unwrap();
        assert_eq!(worktrees.len().unwrap(), 0);
    }

    #[test]
    fn test_list_serializes_as_json_array() {
        let lua = Lua::new();
        let (queue, cache, base) = create_test_queue_and_cache();

        register(&lua, queue, cache, base).expect("Should register");

        let worktrees: LuaValue = lua.load("return worktree.list()").eval().unwrap();
        let json: serde_json::Value = lua.from_value(worktrees).unwrap();

        assert!(
            json.is_array(),
            "Empty worktrees should serialize as JSON array, got: {}",
            json
        );
        assert_eq!(json.as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_list_returns_cached_worktrees() {
        let lua = Lua::new();
        let (queue, cache, base) = create_test_queue_and_cache();

        // Pre-populate the cache
        cache.set_worktrees(vec![
            ("/path/to/wt1".to_string(), "feature-a".to_string()),
            ("/path/to/wt2".to_string(), "feature-b".to_string()),
        ]);

        register(&lua, queue, cache, base).expect("Should register");

        let worktrees: LuaTable = lua.load("return worktree.list()").eval().unwrap();
        assert_eq!(worktrees.len().unwrap(), 2);
    }

    #[test]
    fn test_exists_returns_false_when_empty() {
        let lua = Lua::new();
        let (queue, cache, base) = create_test_queue_and_cache();

        register(&lua, queue, cache, base).expect("Should register");

        let exists: bool = lua
            .load(r#"return worktree.exists("feature-a")"#)
            .eval()
            .unwrap();
        assert!(!exists);
    }

    #[test]
    fn test_exists_returns_true_when_found() {
        let lua = Lua::new();
        let (queue, cache, base) = create_test_queue_and_cache();

        cache.set_worktrees(vec![(
            "/path/to/wt1".to_string(),
            "feature-a".to_string(),
        )]);

        register(&lua, queue, cache, base).expect("Should register");

        let exists: bool = lua
            .load(r#"return worktree.exists("feature-a")"#)
            .eval()
            .unwrap();
        assert!(exists);
    }

    #[test]
    fn test_exists_returns_false_for_wrong_branch() {
        let lua = Lua::new();
        let (queue, cache, base) = create_test_queue_and_cache();

        cache.set_worktrees(vec![(
            "/path/to/wt1".to_string(),
            "feature-a".to_string(),
        )]);

        register(&lua, queue, cache, base).expect("Should register");

        let exists: bool = lua
            .load(r#"return worktree.exists("feature-b")"#)
            .eval()
            .unwrap();
        assert!(!exists);
    }

    #[test]
    fn test_find_returns_nil_when_not_found() {
        let lua = Lua::new();
        let (queue, cache, base) = create_test_queue_and_cache();

        register(&lua, queue, cache, base).expect("Should register");

        let result: LuaValue = lua
            .load(r#"return worktree.find("feature-a")"#)
            .eval()
            .unwrap();
        assert!(matches!(result, LuaValue::Nil));
    }

    #[test]
    fn test_find_returns_path_when_found() {
        let lua = Lua::new();
        let (queue, cache, base) = create_test_queue_and_cache();

        cache.set_worktrees(vec![(
            "/path/to/wt1".to_string(),
            "feature-a".to_string(),
        )]);

        register(&lua, queue, cache, base).expect("Should register");

        let path: String = lua
            .load(r#"return worktree.find("feature-a")"#)
            .eval()
            .unwrap();
        assert_eq!(path, "/path/to/wt1");
    }

    #[test]
    fn test_delete_queues_request() {
        let lua = Lua::new();
        let (queue, cache, base) = create_test_queue_and_cache();

        register(&lua, Arc::clone(&queue), cache, base).expect("Should register");

        lua.load(r#"worktree.delete("/path/to/wt", "feature-branch")"#)
            .exec()
            .expect("Should delete worktree");

        let requests = queue
            .lock()
            .expect("Worktree request queue mutex poisoned");
        assert_eq!(requests.len(), 1);

        match &requests[0] {
            WorktreeRequest::Delete { path, branch } => {
                assert_eq!(path, "/path/to/wt");
                assert_eq!(branch, "feature-branch");
            }
            _ => panic!("Expected Delete request"),
        }
    }

    #[test]
    fn test_multiple_delete_requests_queue_in_order() {
        let lua = Lua::new();
        let (queue, cache, base) = create_test_queue_and_cache();

        register(&lua, Arc::clone(&queue), cache, base).expect("Should register");

        lua.load(
            r#"
            worktree.delete("/path/to/wt1", "branch-1")
            worktree.delete("/path/to/wt2", "branch-2")
        "#,
        )
        .exec()
        .expect("Should queue multiple requests");

        let requests = queue
            .lock()
            .expect("Worktree request queue mutex poisoned");
        assert_eq!(requests.len(), 2);

        assert!(matches!(requests[0], WorktreeRequest::Delete { .. }));
        assert!(matches!(requests[1], WorktreeRequest::Delete { .. }));
    }

    #[test]
    fn test_repo_root_returns_value_or_nil() {
        let lua = Lua::new();
        let (queue, cache, base) = create_test_queue_and_cache();

        register(&lua, queue, cache, base).expect("Should register");

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
    fn test_create_returns_error_for_invalid_branch() {
        let lua = Lua::new();
        let (queue, cache, base) = create_test_queue_and_cache();

        register(&lua, queue, cache, base).expect("Should register");

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
}
