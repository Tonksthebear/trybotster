//! Thread-safe cache of session PTY handles for read-only access.
//!
//! Lua manages session lifecycle and updates the cache via
//! `hub.register_session()` / `hub.unregister_session()`.
//! Clients call `HandleCache::get_session()` to read directly without
//! sending commands.
//!
//! # Why This Exists
//!
//! PTY I/O operations need non-blocking access to session handles. Blocking
//! commands from Hub's thread would deadlock. HandleCache provides direct,
//! non-blocking access. Lua updates the cache on session lifecycle events.
//!
//! # Lua Migration
//!
//! Session metadata (repo, issue, status) is managed by Lua. HandleCache
//! only stores PTY handles (session_uuid + PtyHandle). Lua enriches
//! the PTY-level data with its own session registry.
//!
//! # Usage
//!
//! - **Hub tick loop**: `HandleCache::get_session()` reads directly
//! - **TuiRunner PTY I/O**: Hub reads from cache to forward input/resize
//! - **Lua primitives**: `hub.register_session()` / `hub.unregister_session()` manage cache
//!
//! # Design Principle
//!
//! "Don't share state, share handles to state"
//! - HubState contains mutable, non-Sync data (owned by Hub thread)
//! - HandleCache contains cloneable handles (shared across threads)

use std::collections::HashMap;
use std::sync::RwLock;

use super::agent_handle::SessionHandle;

/// Thread-safe cache of session PTY handles and shared read-only data.
///
/// Stores `SessionHandle` instances (session_uuid + PtyHandle) and other data
/// that clients need to read without sending blocking commands through Hub.
/// Session metadata (repo, issue, status) is managed by Lua, not cached here.
///
/// # Thread Safety
///
/// - Uses `RwLock` for interior mutability
/// - All stored types are `Clone + Send + Sync`
/// - Multiple readers can access simultaneously
/// - Single writer (Lua via Hub primitives) updates on lifecycle events
///
/// # Cached Data
///
/// - **Session PTY handles**: Updated by Lua via `hub.register_session()` / `hub.unregister_session()`
/// - **Worktrees**: Updated when Hub loads worktrees (menu open, session lifecycle)
/// - **Connection URL**: Updated when Hub generates/refreshes the device key bundle
#[derive(Debug, Default)]
pub struct HandleCache {
    /// Session handles keyed by session UUID.
    sessions: RwLock<HashMap<String, SessionHandle>>,

    /// Display ordering of session UUIDs.
    ///
    /// Maintains the order in which sessions should appear in the UI.
    /// New sessions are appended; removals shift remaining items.
    order: RwLock<Vec<String>>,

    /// Available worktrees for session creation.
    ///
    /// Each tuple contains (path, branch_name). Hub updates this when
    /// worktrees are loaded (e.g., opening New Agent modal, session lifecycle).
    worktrees: RwLock<Vec<(String, String)>>,

    /// Cached connection URL for browser pairing.
    ///
    /// Hub updates this whenever the device key bundle changes (initialization,
    /// refresh, or show_connection_code action).
    connection_url: RwLock<Option<Result<String, String>>>,
}

impl HandleCache {
    /// Create a new empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            order: RwLock::new(Vec::new()),
            worktrees: RwLock::new(Vec::new()),
            connection_url: RwLock::new(None),
        }
    }

    /// Get session handle by UUID.
    ///
    /// Returns `None` if UUID is not found or lock is poisoned.
    /// This is a direct read - no command channel involved.
    #[must_use]
    pub fn get_session(&self, uuid: &str) -> Option<SessionHandle> {
        self.sessions.read().ok()?.get(uuid).cloned()
    }

    /// Get session handle by display index (for TUI navigation).
    ///
    /// Looks up the UUID in the order vec, then fetches from the map.
    /// Returns `None` if index is out of bounds or lock is poisoned.
    #[must_use]
    pub fn get_session_by_index(&self, index: usize) -> Option<SessionHandle> {
        let order = self.order.read().ok()?;
        let uuid = order.get(index)?;
        self.sessions.read().ok()?.get(uuid).cloned()
    }

    /// Get all session handles in display order.
    ///
    /// Returns empty vec if lock is poisoned.
    #[must_use]
    pub fn get_all_sessions(&self) -> Vec<SessionHandle> {
        let Ok(order) = self.order.read() else {
            return Vec::new();
        };
        let Ok(sessions) = self.sessions.read() else {
            return Vec::new();
        };
        order
            .iter()
            .filter_map(|uuid| sessions.get(uuid).cloned())
            .collect()
    }

    /// Get the number of cached sessions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions.read().map(|s| s.len()).unwrap_or(0)
    }

    /// Check if cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get the display index for a session UUID.
    #[must_use]
    pub fn index_of(&self, uuid: &str) -> Option<usize> {
        self.order.read().ok()?.iter().position(|u| u == uuid)
    }

    /// Add or update a session handle.
    ///
    /// If a session with the same UUID exists, it's replaced (order preserved).
    /// New sessions are appended to the end of the order list.
    pub fn add_session(&self, handle: SessionHandle) {
        let uuid = handle.session_uuid().to_string();

        if let Ok(mut sessions) = self.sessions.write() {
            sessions.insert(uuid.clone(), handle);
        }

        // Add to order if not already present
        if let Ok(mut order) = self.order.write() {
            if !order.contains(&uuid) {
                order.push(uuid);
            }
        }
    }

    /// Update metadata (label, workspace_id) on an existing session handle.
    ///
    /// Returns `true` if the session was found and updated. The PtyHandle
    /// is left untouched — no new reader thread, no new connection.
    pub fn update_session_metadata(
        &self,
        uuid: &str,
        label: Option<&str>,
        workspace_id: Option<Option<&str>>,
    ) -> bool {
        if let Ok(mut sessions) = self.sessions.write() {
            if let Some(handle) = sessions.get_mut(uuid) {
                if let Some(l) = label {
                    handle.label = l.to_string();
                }
                if let Some(ws) = workspace_id {
                    handle.workspace_id = ws.map(|s| s.to_string());
                }
                return true;
            }
        }
        false
    }

    /// Remove a session by UUID.
    ///
    /// Returns true if the session was found and removed.
    pub fn remove_session(&self, uuid: &str) -> bool {
        let removed = if let Ok(mut sessions) = self.sessions.write() {
            sessions.remove(uuid).is_some()
        } else {
            false
        };

        if removed {
            if let Ok(mut order) = self.order.write() {
                order.retain(|u| u != uuid);
            }
        }

        removed
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
    /// after session lifecycle events).
    pub fn set_worktrees(&self, worktrees: Vec<(String, String)>) {
        if let Ok(mut wt) = self.worktrees.write() {
            *wt = worktrees;
        }
    }

    /// Remove a worktree entry by branch name.
    ///
    /// Matches on branch rather than path to avoid string-equality fragility
    /// (trailing slashes, symlink resolution, non-canonical forms).
    /// Branch name is the stable, git-enforced unique key for a worktree.
    pub fn remove_worktree_by_branch(&self, branch: &str) {
        if let Ok(mut wt) = self.worktrees.write() {
            wt.retain(|(_, b)| b != branch);
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
    /// Called by Hub when the device key bundle changes (initialization, refresh,
    /// or show_connection_code action).
    pub fn set_connection_url(&self, result: Result<String, String>) {
        if let Ok(mut url) = self.connection_url.write() {
            *url = Some(result);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_cache_is_empty() {
        let cache = HandleCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_get_session_not_found() {
        let cache = HandleCache::new();
        assert!(cache.get_session("nonexistent").is_none());
    }

    #[test]
    fn test_get_session_by_index_out_of_bounds() {
        let cache = HandleCache::new();
        assert!(cache.get_session_by_index(0).is_none());
        assert!(cache.get_session_by_index(100).is_none());
    }

    #[test]
    fn test_get_all_sessions_empty() {
        let cache = HandleCache::new();
        assert!(cache.get_all_sessions().is_empty());
    }

    #[test]
    fn test_remove_worktree_by_branch_removes_matching_entry() {
        let cache = HandleCache::new();
        cache.set_worktrees(vec![
            ("/worktrees/repo-feat".to_string(), "feature".to_string()),
            ("/worktrees/repo-main".to_string(), "main".to_string()),
        ]);

        cache.remove_worktree_by_branch("feature");

        let wt = cache.get_worktrees();
        assert_eq!(wt.len(), 1);
        assert_eq!(wt[0].1, "main");
    }

    #[test]
    fn test_remove_worktree_by_branch_ignores_path_format() {
        // Branch match is immune to path variations (trailing slash, etc.)
        let cache = HandleCache::new();
        cache.set_worktrees(vec![(
            "/worktrees/repo-feat/".to_string(),
            "feature".to_string(),
        )]);

        // Passing a different path format — branch still matches
        cache.remove_worktree_by_branch("feature");

        assert!(cache.get_worktrees().is_empty());
    }

    #[test]
    fn test_remove_worktree_by_branch_noop_when_not_found() {
        let cache = HandleCache::new();
        cache.set_worktrees(vec![(
            "/worktrees/repo-main".to_string(),
            "main".to_string(),
        )]);

        cache.remove_worktree_by_branch("nonexistent");

        assert_eq!(cache.get_worktrees().len(), 1);
    }

    // ── update_session_metadata tests ────────────────────────────────────

    /// Helper: create a test SessionHandle with a real PtyHandle.
    fn create_test_session(uuid: &str, label: &str, workspace_id: Option<&str>) -> SessionHandle {
        use crate::agent::pty::PtySession;
        let pty_session = PtySession::new(24, 80);
        let (shared_state, event_tx, kitty_enabled, cursor_visible, resize_pending) =
            pty_session.get_direct_access();
        std::mem::forget(pty_session);
        let pty = super::super::agent_handle::PtyHandle::new(
            event_tx,
            shared_state,
            kitty_enabled,
            cursor_visible,
            resize_pending,
            None,
        );
        SessionHandle::new(
            uuid,
            label,
            Default::default(),
            workspace_id.map(String::from),
            pty,
        )
    }

    #[test]
    fn test_update_session_metadata_updates_label() {
        let cache = HandleCache::new();
        cache.add_session(create_test_session("sess-1", "old-label", None));

        let updated = cache.update_session_metadata("sess-1", Some("new-label"), None);

        assert!(updated);
        let handle = cache.get_session("sess-1").unwrap();
        assert_eq!(handle.label(), "new-label");
    }

    #[test]
    fn test_update_session_metadata_updates_workspace_id() {
        let cache = HandleCache::new();
        cache.add_session(create_test_session("sess-1", "label", None));

        let updated = cache.update_session_metadata("sess-1", None, Some(Some("ws-new")));

        assert!(updated);
        let handle = cache.get_session("sess-1").unwrap();
        assert_eq!(handle.workspace_id(), Some("ws-new"));
    }

    #[test]
    fn test_update_session_metadata_clears_workspace_id() {
        let cache = HandleCache::new();
        cache.add_session(create_test_session("sess-1", "label", Some("ws-old")));

        let updated = cache.update_session_metadata("sess-1", None, Some(None));

        assert!(updated);
        let handle = cache.get_session("sess-1").unwrap();
        assert!(handle.workspace_id().is_none());
    }

    #[test]
    fn test_update_session_metadata_returns_false_when_not_found() {
        let cache = HandleCache::new();

        let updated = cache.update_session_metadata("nonexistent", Some("label"), None);

        assert!(!updated);
    }

    #[test]
    fn test_update_session_metadata_preserves_pty_handle() {
        let cache = HandleCache::new();
        cache.add_session(create_test_session("sess-1", "label", Some("ws-1")));

        // Subscribe before update to verify event_tx is the same Arc
        let handle_before = cache.get_session("sess-1").unwrap();
        let _rx = handle_before.pty().subscribe();

        cache.update_session_metadata("sess-1", Some("new-label"), Some(Some("ws-2")));

        // PtyHandle should still work — same broadcast channel
        let handle_after = cache.get_session("sess-1").unwrap();
        handle_after.pty().notify_process_exited(Some(0));
    }
}
