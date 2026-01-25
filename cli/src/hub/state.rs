//! Hub state management.
//!
//! This module contains the core state types for the Hub, including
//! agent management and worktree tracking.
//!
//! # Selection Model
//!
//! Agent selection is now per-client, managed by the client abstraction layer.
//! See `crate::client` for the `TuiClient` and `BrowserClient` implementations.
//! This module only manages the agent registry itself.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::agent::Agent;
use crate::git::WorktreeManager;
use crate::hub::agent_handle::AgentHandle;
use crate::relay::types::AgentInfo;

/// Shared reference to HubState for thread-safe read access.
///
/// Clients store a clone of this to access agent state without going through
/// Hub commands. The RwLock allows multiple readers without blocking Hub's
/// write operations (when no write is in progress).
///
/// # Usage
///
/// ```ignore
/// let shared_state = hub.shared_state();
///
/// // In client code (possibly different thread):
/// let state = shared_state.read().unwrap();
/// let agents = state.get_agents_info();
/// for info in &agents {
///     println!("{}: {:?}", info.id, info.status);
/// }
///
/// // Get a handle for specific agent
/// if let Some(handle) = state.get_agent_handle(0) {
///     let pty = handle.get_pty(0).unwrap(); // CLI PTY
///     // Use pty handle...
/// }
/// ```
pub type SharedHubState = Arc<RwLock<HubState>>;

/// Core hub state - manages active agents.
///
/// This struct holds the minimal state needed for agent management,
/// delegating selection to the client abstraction layer.
///
/// # Example
///
/// ```ignore
/// let mut state = HubState::new(worktree_base);
///
/// // Add an agent
/// state.add_agent(session_key, agent);
///
/// // Query agents
/// for (key, agent) in state.agents_ordered() {
///     println!("Agent: {}", key);
/// }
/// ```
pub struct HubState {
    /// Active agents indexed by session key.
    ///
    /// Session keys are formatted as `{repo-safe}-{issue_number}` or
    /// `{repo-safe}-{branch-name}` for branch-based sessions.
    pub agents: HashMap<String, Agent>,

    /// Ordered list of agent keys for UI navigation.
    ///
    /// This maintains insertion order for consistent UI display.
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

    /// Returns the number of active agents.
    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    /// Returns true if there are no active agents.
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    /// Adds an agent to the hub state.
    ///
    /// The agent will be added to both the HashMap and the ordered list.
    pub fn add_agent(&mut self, session_key: String, agent: Agent) {
        self.agent_keys_ordered.push(session_key.clone());
        self.agents.insert(session_key, agent);
    }

    /// Removes an agent from the hub state.
    ///
    /// Returns the removed agent if it existed.
    /// Note: Client selection updates are handled by the client abstraction layer.
    pub fn remove_agent(&mut self, session_key: &str) -> Option<Agent> {
        self.agent_keys_ordered.retain(|k| k != session_key);
        self.agents.remove(session_key)
    }

    /// Returns an iterator over all agents in display order.
    pub fn agents_ordered(&self) -> impl Iterator<Item = (&str, &Agent)> {
        self.agent_keys_ordered
            .iter()
            .filter_map(|key| self.agents.get(key).map(|agent| (key.as_str(), agent)))
    }

    // =========================================================================
    // Client Data Access Methods
    // =========================================================================

    /// Get a snapshot of all agents as `AgentInfo` in display order.
    ///
    /// Returns a vector of `AgentInfo` structs that clients can use to
    /// display agent lists and implement `Client::get_agents()`.
    /// This is a snapshot - changes won't be reflected until the next call.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let state = hub.shared_state().read().unwrap();
    /// let agents = state.get_agents_info();
    /// for info in &agents {
    ///     println!("{}: {}", info.id, info.status.as_deref().unwrap_or("Unknown"));
    /// }
    /// ```
    #[must_use]
    pub fn get_agents_info(&self) -> Vec<AgentInfo> {
        self.agents_ordered()
            .map(|(agent_id, agent)| self.agent_to_info(agent_id, agent))
            .collect()
    }

    /// Get `AgentInfo` for a specific agent by ID.
    ///
    /// Returns `None` if the agent does not exist.
    #[must_use]
    pub fn get_agent_info(&self, agent_id: &str) -> Option<AgentInfo> {
        self.agents
            .get(agent_id)
            .map(|agent| self.agent_to_info(agent_id, agent))
    }

    /// Get an `AgentHandle` for the agent at the given index.
    ///
    /// Returns `None` if the index is out of bounds.
    ///
    /// The handle provides:
    /// - Agent metadata via `info()`
    /// - PTY access via `get_pty(0)` (CLI) and `get_pty(1)` (Server)
    ///
    /// # Arguments
    ///
    /// * `index` - The index of the agent in display order (0-based)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let state = hub.shared_state().read().unwrap();
    /// if let Some(handle) = state.get_agent_handle(0) {
    ///     println!("Agent: {}", handle.info().id);
    ///     // Connect to CLI PTY
    ///     let pty = handle.get_pty(0).unwrap();
    ///     // ...
    /// }
    /// ```
    #[must_use]
    pub fn get_agent_handle(&self, index: usize) -> Option<AgentHandle> {
        use crate::hub::agent_handle::PtyHandle;

        let agent_id = self.agent_keys_ordered.get(index)?;
        let agent = self.agents.get(agent_id)?;

        let info = self.agent_to_info(agent_id, agent);

        // Build PTY handles vector: ptys[0] = CLI, ptys[1] = Server (if exists)
        let mut ptys = Vec::with_capacity(2);

        // CLI PTY is always present (index 0)
        let (cli_event_tx, cli_cmd_tx) = agent.cli_pty.get_channels();
        ptys.push(PtyHandle::new(cli_event_tx, cli_cmd_tx));

        // Server PTY if available (index 1)
        if let Some(ref server_pty) = agent.server_pty {
            let (server_event_tx, server_cmd_tx) = server_pty.get_channels();
            ptys.push(PtyHandle::new(server_event_tx, server_cmd_tx));
        }

        Some(AgentHandle::new(agent_id, info, ptys, index))
    }

    /// Get the index of an agent by its ID.
    ///
    /// Returns `None` if no agent with that ID exists.
    #[must_use]
    pub fn get_agent_index(&self, agent_id: &str) -> Option<usize> {
        self.agent_keys_ordered.iter().position(|id| id == agent_id)
    }

    /// Convert an Agent to AgentInfo.
    ///
    /// Internal helper for creating snapshots.
    fn agent_to_info(&self, agent_id: &str, agent: &Agent) -> AgentInfo {
        AgentInfo {
            id: agent_id.to_string(),
            repo: Some(agent.repo.clone()),
            issue_number: agent.issue_number.map(u64::from),
            branch_name: Some(agent.branch_name.clone()),
            name: None,
            status: Some(format!("{:?}", agent.status)),
            tunnel_port: agent.tunnel_port,
            server_running: Some(agent.is_server_running()),
            has_server_pty: Some(agent.has_server_pty()),
            active_pty_view: None, // Client-owned state, not agent state
            scroll_offset: None,   // Client-owned state, not agent state
            hub_identifier: None,  // Set by Hub when sending to browser
        }
    }

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
        use std::collections::HashSet;
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
                if let Ok(repo) = git2::Repository::open(path) {
                    if !repo.is_worktree() {
                        return false;
                    }
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
        assert!(state.is_empty());
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
        assert!(state.is_empty());
    }

    #[test]
    fn test_agents_ordered() {
        let mut state = HubState::new(PathBuf::from("/tmp/worktrees"));

        // Add agents in order
        for i in 1..=3 {
            let agent = create_test_agent("owner/repo", Some(i), &format!("botster-issue-{i}"));
            state.add_agent(format!("owner-repo-{i}"), agent);
        }

        // Verify iteration order matches insertion order
        let keys: Vec<_> = state.agents_ordered().map(|(k, _)| k).collect();
        assert_eq!(keys, vec!["owner-repo-1", "owner-repo-2", "owner-repo-3"]);
    }
}
