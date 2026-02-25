//! Hub state management.
//!
//! This module contains the core state types for the Hub, including
//! worktree tracking and infrastructure management.
//!
//! # Lua Migration
//!
//! Agent metadata and lifecycle are fully managed by Lua
//! (`handlers/agents.lua` + `lib/agent.lua`). HubState retains
//! infrastructure concerns: worktree discovery and port tracking.

use std::sync::{Arc, RwLock};

use crate::git::WorktreeManager;

/// Shared reference to HubState for thread-safe read access.
///
/// Hub owns this via `hub.state`. The RwLock allows multiple readers without
/// blocking Hub's write operations (when no write is in progress).
pub type SharedHubState = Arc<RwLock<HubState>>;

/// Core hub state - manages infrastructure concerns.
///
/// Agent metadata and lifecycle are managed by Lua. Agent PTY handles
/// are managed by HandleCache. HubState retains worktree discovery
/// and port allocation.
pub struct HubState {
    /// Available worktrees for spawning new agents.
    ///
    /// Each tuple contains (path, branch_name).
    pub available_worktrees: Vec<(String, String)>,

    /// Git worktree manager for creating/deleting worktrees.
    pub git_manager: WorktreeManager,
}

impl std::fmt::Debug for HubState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubState")
            .field("available_worktrees", &self.available_worktrees.len())
            .finish_non_exhaustive()
    }
}

impl HubState {
    /// Creates a new HubState with the given worktree base directory.
    pub fn new(worktree_base: std::path::PathBuf) -> Self {
        Self {
            available_worktrees: Vec::new(),
            git_manager: WorktreeManager::new(worktree_base),
        }
    }

    // =========================================================================
    // Worktree Management
    // =========================================================================

    /// Load available worktrees for the selection UI.
    ///
    /// Queries git for all worktrees and filters out the main repository
    /// (not a worktree). Agent-level deduplication is handled by Lua.
    ///
    /// # Errors
    ///
    /// Returns an error if git commands fail.
    pub fn load_available_worktrees(&mut self) -> anyhow::Result<()> {
        use std::process::Command;

        let (repo_path, _) = WorktreeManager::detect_current_repo()?;

        let output = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&repo_path)
            .output()?;

        if !output.status.success() {
            anyhow::bail!(
                "Failed to list worktrees: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let worktree_output = String::from_utf8_lossy(&output.stdout);
        let mut current_path = String::new();
        let mut current_branch = String::new();
        let mut worktrees = Vec::new();

        for line in worktree_output.lines() {
            if let Some(path) = line.strip_prefix("worktree ") {
                current_path = path.to_string();
            } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
                current_branch = branch.to_string();
            } else if line.is_empty() && !current_path.is_empty() {
                worktrees.push((current_path.clone(), current_branch.clone()));
                current_path.clear();
                current_branch.clear();
            }
        }

        if !current_path.is_empty() {
            worktrees.push((current_path, current_branch));
        }

        // Filter to actual worktrees (have .git file, not directory)
        self.available_worktrees = worktrees
            .into_iter()
            .filter(|(path, _)| {
                // Worktrees have a .git *file*, main repos have a .git *directory*
                let git_path = std::path::Path::new(path).join(".git");
                git_path.is_file()
            })
            .collect();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_hub_state_new() {
        let state = HubState::new(PathBuf::from("/tmp/worktrees"));
        assert!(state.available_worktrees.is_empty());
    }
}
