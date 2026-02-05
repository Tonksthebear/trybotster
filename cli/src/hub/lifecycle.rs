//! Agent lifecycle management.
//!
//! This module provides the `close_agent()` function for server-initiated
//! agent cleanup. Agent creation is fully owned by Lua
//! (`handlers/agents.lua` + `lib/agent.lua`).

// Rust guideline compliant 2026-02

use anyhow::Result;

use super::HubState;

/// Close an agent and optionally delete its worktree.
///
/// # Arguments
///
/// * `state` - Mutable reference to the Hub state
/// * `agent_id` - The agent ID of the agent to close
/// * `delete_worktree` - Whether to delete the agent's worktree
///
/// # Returns
///
/// `Ok(true)` if the agent was found and closed, `Ok(false)` if not found.
///
/// # Errors
///
/// Returns an error if worktree deletion fails (but agent is still removed).
pub fn close_agent(state: &mut HubState, agent_id: &str, delete_worktree: bool) -> Result<bool> {
    let Some(agent) = state.remove_agent(agent_id) else {
        log::info!("No agent found with agent ID: {agent_id}");
        return Ok(false);
    };

    let label = format_agent_label(agent.issue_number, &agent.branch_name);

    if delete_worktree {
        if let Err(e) = state
            .git_manager
            .delete_worktree_by_path(&agent.worktree_path, &agent.branch_name)
        {
            log::error!("Failed to delete worktree for {label}: {e}");
            // Still return Ok since agent was removed
        } else {
            log::info!("Closed agent and deleted worktree for {label}");
        }
    } else {
        log::info!("Closed agent for {label} (worktree preserved)");
    }

    Ok(true)
}

/// Format a human-readable label for an agent.
fn format_agent_label(issue_number: Option<u32>, branch_name: &str) -> String {
    if let Some(num) = issue_number {
        format!("issue #{num}")
    } else {
        format!("branch {branch_name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_close_agent_not_found() {
        let mut state = HubState::new(PathBuf::from("/tmp/worktrees"));
        let result = close_agent(&mut state, "nonexistent-key", false).unwrap();
        assert!(!result);
    }
}
