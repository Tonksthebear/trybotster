//! Hub state management.
//!
//! This module contains the core state types for the Hub, including
//! agent lifecycle operations and worktree/port tracking.
//!
//! # Lua Migration
//!
//! Agent metadata (repo, issue, status, etc.) is managed by Lua.
//! HubState retains ownership of Rust Agent structs for PTY lifecycle
//! (spawn, close, PTY handle extraction). The agent registry (which
//! agents exist, their metadata for display) has moved to Lua.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use crate::agent::Agent;
use crate::git::WorktreeManager;
use crate::hub::agent_handle::{AgentPtys, PtyHandle};

/// Shared reference to HubState for thread-safe read access.
///
/// Hub owns this via `hub.state`. The RwLock allows multiple readers without
/// blocking Hub's write operations (when no write is in progress).
pub type SharedHubState = Arc<RwLock<HubState>>;

/// Core hub state - manages agent PTY lifecycle and infrastructure.
///
/// Agent metadata (repo, issue, status) is managed by Lua. HubState retains
/// the Rust Agent structs for PTY lifecycle operations and exposes PTY handles
/// via `get_agent_handle()` for the HandleCache.
///
/// # Worktree and Port Management
///
/// HubState also manages worktree tracking and port allocation, which are
/// infrastructure concerns that remain in Rust.
pub struct HubState {
    /// Active agents indexed by session key.
    ///
    /// These are the Rust Agent structs that own PtySession instances.
    /// Lua manages agent metadata; Rust manages PTY lifecycle.
    ///
    /// Session keys are formatted as `{repo-safe}-{issue_number}` or
    /// `{repo-safe}-{branch-name}` for branch-based sessions.
    pub agents: HashMap<String, Agent>,

    /// Ordered list of agent keys for handle indexing.
    ///
    /// Maintains insertion order so HandleCache indices are stable.
    pub agent_keys_ordered: Vec<String>,

    /// Available worktrees for spawning new agents.
    ///
    /// Each tuple contains (path, branch_name). Excludes worktrees
    /// that already have active agents.
    pub available_worktrees: Vec<(String, String)>,

    /// Git worktree manager for creating/deleting worktrees.
    pub git_manager: WorktreeManager,
}

impl std::fmt::Debug for HubState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubState")
            .field("agents", &self.agents.len())
            .field("available_worktrees", &self.available_worktrees.len())
            .finish_non_exhaustive()
    }
}

impl HubState {
    /// Creates a new HubState with the given worktree base directory.
    pub fn new(worktree_base: std::path::PathBuf) -> Self {
        Self {
            agents: HashMap::new(),
            agent_keys_ordered: Vec::new(),
            available_worktrees: Vec::new(),
            git_manager: WorktreeManager::new(worktree_base),
        }
    }

    // =========================================================================
    // Agent Lifecycle (Rust-owned PTY management)
    // =========================================================================

    /// Returns the number of active agents.
    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    /// Adds an agent to the hub state.
    ///
    /// The agent will be added to both the HashMap and the ordered list.
    /// After adding, call `Hub::sync_handle_cache()` to update the HandleCache.
    pub fn add_agent(&mut self, session_key: String, agent: Agent) {
        self.agent_keys_ordered.push(session_key.clone());
        self.agents.insert(session_key, agent);
    }

    /// Removes an agent from the hub state.
    ///
    /// Returns the removed agent if it existed.
    /// After removing, call `Hub::sync_handle_cache()` to update the HandleCache.
    pub fn remove_agent(&mut self, session_key: &str) -> Option<Agent> {
        self.agent_keys_ordered.retain(|k| k != session_key);
        self.agents.remove(session_key)
    }

    // =========================================================================
    // PTY Handle Extraction (for HandleCache)
    // =========================================================================

    /// Get an `AgentPtys` for the agent at the given index.
    ///
    /// Returns `None` if the index is out of bounds.
    ///
    /// The handle provides PTY access via `get_pty(0)` (CLI) and `get_pty(1)` (Server).
    /// Agent metadata is managed by Lua, not included in the handle.
    ///
    /// # Arguments
    ///
    /// * `index` - The index of the agent in display order (0-based)
    #[must_use]
    pub fn get_agent_handle(&self, index: usize) -> Option<AgentPtys> {
        let agent_key = self.agent_keys_ordered.get(index)?;
        let agent = self.agents.get(agent_key)?;

        // Build PTY handles vector: ptys[0] = CLI, ptys[1] = Server (if exists)
        let mut ptys = Vec::with_capacity(2);

        // CLI PTY is always present (index 0)
        let (cli_shared_state, cli_scrollback, cli_event_tx) = agent.cli_pty.get_direct_access();
        ptys.push(PtyHandle::new(
            cli_event_tx,
            cli_shared_state,
            cli_scrollback,
            agent.cli_pty.port(),
        ));

        // Server PTY if available (index 1)
        if let Some(ref server_pty) = agent.server_pty {
            let (server_shared_state, server_scrollback, server_event_tx) = server_pty.get_direct_access();
            ptys.push(PtyHandle::new(
                server_event_tx,
                server_shared_state,
                server_scrollback,
                server_pty.port(),
            ));
        }

        Some(AgentPtys::new(agent_key, ptys, index))
    }

    // =========================================================================
    // Worktree Management
    // =========================================================================

    /// Load available worktrees for the selection UI.
    ///
    /// Queries git for all worktrees and filters out:
    /// - Worktrees that already have active agents
    /// - The main repository (not a worktree)
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

        // Filter out worktrees already in use and the main repository
        let open_paths: HashSet<_> = self
            .agents
            .values()
            .map(|a| a.worktree_path.display().to_string())
            .collect();

        self.available_worktrees = worktrees
            .into_iter()
            .filter(|(path, _)| {
                if open_paths.contains(path) {
                    return false;
                }
                // Worktrees have a .git *file*, main repos have a .git *directory*
                let git_path = std::path::Path::new(path).join(".git");
                if !git_path.is_file() {
                    return false;
                }
                true
            })
            .collect();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn create_test_agent(repo: &str, issue: Option<u32>, branch: &str) -> Agent {
        Agent::new(
            Uuid::new_v4(),
            repo.to_string(),
            issue,
            branch.to_string(),
            PathBuf::from("/tmp/test"),
        )
    }

    #[test]
    fn test_hub_state_new() {
        let state = HubState::new(PathBuf::from("/tmp/worktrees"));
        assert_eq!(state.agent_count(), 0);
    }

    #[test]
    fn test_add_and_remove_agent() {
        let mut state = HubState::new(PathBuf::from("/tmp/worktrees"));

        let agent = create_test_agent("owner/repo", Some(42), "botster-issue-42");
        state.add_agent("owner-repo-42".to_string(), agent);

        assert_eq!(state.agent_count(), 1);
        assert!(state.agents.contains_key("owner-repo-42"));

        let removed = state.remove_agent("owner-repo-42");
        assert!(removed.is_some());
        assert_eq!(state.agent_count(), 0);
    }

    #[test]
    fn test_agent_keys_ordered() {
        let mut state = HubState::new(PathBuf::from("/tmp/worktrees"));

        // Add agents in order
        for i in 1..=3 {
            let agent = create_test_agent("owner/repo", Some(i), &format!("botster-issue-{i}"));
            state.add_agent(format!("owner-repo-{i}"), agent);
        }

        // Verify order matches insertion order
        assert_eq!(
            state.agent_keys_ordered,
            vec!["owner-repo-1", "owner-repo-2", "owner-repo-3"]
        );
    }

    #[test]
    fn test_multiple_agents_same_worktree() {
        let mut state = HubState::new(PathBuf::from("/tmp/worktrees"));

        // Simulate Lua-side instance suffixes: first agent has no suffix,
        // subsequent agents get "-2", "-3", etc.
        let agent1 = create_test_agent("owner/repo", Some(42), "botster-issue-42");
        let agent2 = create_test_agent("owner/repo", Some(42), "botster-issue-42");
        let agent3 = create_test_agent("owner/repo", Some(42), "botster-issue-42");

        state.add_agent("owner-repo-42".to_string(), agent1);
        state.add_agent("owner-repo-42-2".to_string(), agent2);
        state.add_agent("owner-repo-42-3".to_string(), agent3);

        assert_eq!(state.agent_count(), 3);
        assert!(state.agents.contains_key("owner-repo-42"));
        assert!(state.agents.contains_key("owner-repo-42-2"));
        assert!(state.agents.contains_key("owner-repo-42-3"));

        // Remove middle agent — others remain
        let removed = state.remove_agent("owner-repo-42-2");
        assert!(removed.is_some());
        assert_eq!(state.agent_count(), 2);
        assert!(state.agents.contains_key("owner-repo-42"));
        assert!(state.agents.contains_key("owner-repo-42-3"));
    }

    #[test]
    fn test_multiple_agents_main_branch() {
        let mut state = HubState::new(PathBuf::from("/tmp/worktrees"));

        let agent1 = create_test_agent("owner/repo", None, "main");
        let agent2 = create_test_agent("owner/repo", None, "main");

        state.add_agent("owner-repo-main".to_string(), agent1);
        state.add_agent("owner-repo-main-2".to_string(), agent2);

        assert_eq!(state.agent_count(), 2);
        assert!(state.agents.contains_key("owner-repo-main"));
        assert!(state.agents.contains_key("owner-repo-main-2"));
    }
}
