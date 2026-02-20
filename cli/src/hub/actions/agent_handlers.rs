//! Agent lifecycle handlers - close operations.
//!
//! Agent creation is fully owned by Lua (`handlers/agents.lua` + `lib/agent.lua`).
//! Rust retains only the close path for server-initiated cleanup messages.

use crate::hub::{lifecycle, Hub};

/// Handle closing an agent.
///
/// Closes the agent identified by session_key, optionally deleting its worktree.
pub fn handle_close_agent(hub: &mut Hub, session_key: &str, delete_worktree: bool) {
    log::debug!("CloseAgent: session_key={}", session_key);
    let result = lifecycle::close_agent(
        &mut hub.state.write().expect("HubState RwLock poisoned"),
        session_key,
        delete_worktree,
    );
    match result {
        Ok(true) => {
            // Clean up any paste files for this agent
            hub.cleanup_paste_files(session_key);
            // Sync handle cache for thread-safe agent access
            hub.sync_handle_cache();
        }
        Ok(false) => {}
        Err(e) => {
            log::error!("Failed to close agent {}: {}", session_key, e);
        }
    }
}
