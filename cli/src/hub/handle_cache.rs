//! Thread-safe cache of agent handles for read-only access.
//!
//! Hub maintains this separately from HubState. When agents are
//! created/deleted, Hub updates the cache. `HubHandle::get_agent()` reads
//! from the cache directly without sending commands.
//!
//! # Why This Exists
//!
//! Clients (TuiClient) run on Hub's thread. If they used
//! blocking commands like `GetAgentByIndex` to get agent handles, they
//! would deadlock (Hub can't process commands while blocked).
//!
//! HandleCache solves this by providing direct, non-blocking access to
//! agent handles. The cache is updated by Hub on agent lifecycle events.
//!
//! # Who Uses What
//!
//! - **TuiClient**: Use `HubHandle::get_agent()` → reads from cache
//! - **TuiRunner**: Uses `GetAgentByIndex` command (safe - runs on different thread)
//!
//! # Design Principle
//!
//! "Don't share state, share handles to state"
//! - HubState contains mutable, non-Sync data (owned by Hub thread)
//! - HandleCache contains cloneable handles (shared across threads)

use std::sync::RwLock;

use super::agent_handle::AgentHandle;

/// Thread-safe cache of agent handles and shared read-only data.
///
/// This cache stores `AgentHandle` instances and other data that clients
/// need to read without sending blocking commands through Hub's channel.
/// Unlike `HubState`, this only contains cloneable, thread-safe types.
///
/// # Thread Safety
///
/// - Uses `RwLock` for interior mutability
/// - All stored types are `Clone + Send + Sync`
/// - Multiple readers can access simultaneously
/// - Single writer (Hub) updates on lifecycle events
///
/// # Cached Data
///
/// - **Agent handles**: Updated on agent create/delete via `sync_handle_cache()`
/// - **Worktrees**: Updated when Hub loads worktrees (menu open, agent lifecycle)
/// - **Connection URL**: Updated when Hub generates/refreshes the Signal bundle
#[derive(Debug, Default)]
pub struct HandleCache {
    /// Agent handles indexed by display order.
    agents: RwLock<Vec<AgentHandle>>,

    /// Available worktrees for agent creation.
    ///
    /// Each tuple contains (path, branch_name). Hub updates this when
    /// worktrees are loaded (e.g., opening New Agent modal, agent lifecycle).
    worktrees: RwLock<Vec<(String, String)>>,

    /// Cached connection URL for browser pairing.
    ///
    /// Hub updates this whenever the Signal bundle changes (initialization,
    /// refresh, or ShowConnectionCode action).
    connection_url: RwLock<Option<Result<String, String>>>,
}

impl HandleCache {
    /// Create a new empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            agents: RwLock::new(Vec::new()),
            worktrees: RwLock::new(Vec::new()),
            connection_url: RwLock::new(None),
        }
    }

    /// Get agent handle by display index.
    ///
    /// Returns `None` if index is out of bounds or lock is poisoned.
    /// This is a direct read - no command channel involved.
    #[must_use]
    pub fn get_agent(&self, index: usize) -> Option<AgentHandle> {
        self.agents
            .read()
            .ok()?
            .get(index)
            .cloned()
    }

    /// Get all agent handles.
    ///
    /// Returns empty vec if lock is poisoned.
    #[must_use]
    pub fn get_all_agents(&self) -> Vec<AgentHandle> {
        self.agents
            .read()
            .map(|agents| agents.clone())
            .unwrap_or_default()
    }

    /// Get the number of cached agents.
    #[must_use]
    pub fn len(&self) -> usize {
        self.agents
            .read()
            .map(|agents| agents.len())
            .unwrap_or(0)
    }

    /// Check if cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Insert or update an agent handle at the given index.
    ///
    /// Called by Hub when an agent is created. If index is beyond
    /// current length, the vector is extended.
    pub fn insert_agent(&self, index: usize, handle: AgentHandle) {
        if let Ok(mut agents) = self.agents.write() {
            if index >= agents.len() {
                agents.resize(index + 1, handle.clone());
            }
            agents[index] = handle;
        }
    }

    /// Remove an agent handle at the given index.
    ///
    /// Called by Hub when an agent is deleted. Removes the handle
    /// and shifts subsequent handles down.
    pub fn remove_agent(&self, index: usize) {
        if let Ok(mut agents) = self.agents.write() {
            if index < agents.len() {
                agents.remove(index);
            }
        }
    }

    /// Replace all agent handles.
    ///
    /// Called by Hub to sync the entire cache with current state.
    pub fn set_all(&self, handles: Vec<AgentHandle>) {
        if let Ok(mut agents) = self.agents.write() {
            *agents = handles;
        }
    }

    // ============================================================
    // Worktree Cache
    // ============================================================

    /// Get available worktrees.
    ///
    /// Returns the cached worktree list. Returns empty vec if lock is poisoned.
    #[must_use]
    pub fn get_worktrees(&self) -> Vec<(String, String)> {
        self.worktrees
            .read()
            .map(|wt| wt.clone())
            .unwrap_or_default()
    }

    /// Update the cached worktree list.
    ///
    /// Called by Hub when worktrees are loaded (e.g., opening New Agent modal,
    /// after agent lifecycle events).
    pub fn set_worktrees(&self, worktrees: Vec<(String, String)>) {
        if let Ok(mut wt) = self.worktrees.write() {
            *wt = worktrees;
        }
    }

    // ============================================================
    // Connection URL Cache
    // ============================================================

    /// Get the cached connection URL.
    ///
    /// Returns an error if no URL has been cached yet.
    pub fn get_connection_url(&self) -> Result<String, String> {
        match self.connection_url.read() {
            Ok(guard) => match &*guard {
                Some(result) => result.clone(),
                None => Err("Connection code not yet generated".to_string()),
            },
            Err(_) => Err("Connection URL lock poisoned".to_string()),
        }
    }

    /// Update the cached connection URL.
    ///
    /// Called by Hub when the Signal bundle changes (initialization, refresh,
    /// or ShowConnectionCode action).
    pub fn set_connection_url(&self, result: Result<String, String>) {
        if let Ok(mut url) = self.connection_url.write() {
            *url = Some(result);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests use a mock AgentHandle. Full integration tests
    // will be added after AgentHandle is available.

    #[test]
    fn test_new_cache_is_empty() {
        let cache = HandleCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_get_agent_out_of_bounds() {
        let cache = HandleCache::new();
        assert!(cache.get_agent(0).is_none());
        assert!(cache.get_agent(100).is_none());
    }

    #[test]
    fn test_get_all_agents_empty() {
        let cache = HandleCache::new();
        assert!(cache.get_all_agents().is_empty());
    }
}
