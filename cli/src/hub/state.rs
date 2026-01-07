//! Hub state management.
//!
//! This module contains the core state types for the Hub, including
//! agent management and worktree tracking.

use std::collections::HashMap;

use crate::agent::Agent;
use crate::git::WorktreeManager;

/// Core hub state - manages active agents and selection.
///
/// This struct holds the minimal state needed for agent management,
/// delegating TUI concerns to separate modules. The Hub owns this
/// state and provides query methods for adapters (TUI, Relay) to access it.
///
/// # Example
///
/// ```ignore
/// let mut state = HubState::new(worktree_base);
///
/// // Add an agent
/// state.add_agent(session_key, agent);
///
/// // Navigate
/// state.select_next();
/// state.select_previous();
///
/// // Query
/// if let Some(agent) = state.selected_agent() {
///     println!("Selected: {}", agent.session_key());
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

    /// Currently selected agent index.
    ///
    /// Index into `agent_keys_ordered`. Will be clamped to valid range
    /// when agents are added or removed.
    pub selected: usize,

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
            .field("selected", &self.selected)
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
            selected: 0,
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
    pub fn remove_agent(&mut self, session_key: &str) -> Option<Agent> {
        self.agent_keys_ordered.retain(|k| k != session_key);
        let agent = self.agents.remove(session_key);

        // Clamp selection to valid range
        if self.agent_keys_ordered.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.agent_keys_ordered.len() - 1);
        }

        agent
    }

    /// Returns the currently selected agent, if any.
    pub fn selected_agent(&self) -> Option<&Agent> {
        self.agent_keys_ordered
            .get(self.selected)
            .and_then(|key| self.agents.get(key))
    }

    /// Returns a mutable reference to the currently selected agent, if any.
    pub fn selected_agent_mut(&mut self) -> Option<&mut Agent> {
        let key = self.agent_keys_ordered.get(self.selected)?.clone();
        self.agents.get_mut(&key)
    }

    /// Returns the session key of the currently selected agent, if any.
    pub fn selected_session_key(&self) -> Option<&str> {
        self.agent_keys_ordered.get(self.selected).map(String::as_str)
    }

    /// Selects the next agent (wraps around).
    pub fn select_next(&mut self) {
        if !self.agent_keys_ordered.is_empty() {
            self.selected = (self.selected + 1) % self.agent_keys_ordered.len();
        }
    }

    /// Selects the previous agent (wraps around).
    pub fn select_previous(&mut self) {
        if !self.agent_keys_ordered.is_empty() {
            self.selected = if self.selected == 0 {
                self.agent_keys_ordered.len() - 1
            } else {
                self.selected - 1
            };
        }
    }

    /// Selects an agent by index (1-based, for keyboard shortcuts).
    ///
    /// Returns true if the selection was valid.
    pub fn select_by_index(&mut self, index: usize) -> bool {
        if index > 0 && index <= self.agent_keys_ordered.len() {
            self.selected = index - 1;
            true
        } else {
            false
        }
    }

    /// Selects an agent by session key.
    ///
    /// Returns true if the agent was found and selected.
    pub fn select_by_key(&mut self, session_key: &str) -> bool {
        if let Some(idx) = self
            .agent_keys_ordered
            .iter()
            .position(|k| k == session_key)
        {
            self.selected = idx;
            true
        } else {
            false
        }
    }

    /// Returns an iterator over all agents in display order.
    pub fn agents_ordered(&self) -> impl Iterator<Item = (&str, &Agent)> {
        self.agent_keys_ordered
            .iter()
            .filter_map(|key| self.agents.get(key).map(|agent| (key.as_str(), agent)))
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
        assert_eq!(state.selected, 0);
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
    fn test_selection_navigation() {
        let mut state = HubState::new(PathBuf::from("/tmp/worktrees"));

        // Add three agents
        for i in 1..=3 {
            let agent = create_test_agent("owner/repo", Some(i), &format!("botster-issue-{i}"));
            state.add_agent(format!("owner-repo-{i}"), agent);
        }

        assert_eq!(state.selected, 0);

        state.select_next();
        assert_eq!(state.selected, 1);

        state.select_next();
        assert_eq!(state.selected, 2);

        // Wrap around
        state.select_next();
        assert_eq!(state.selected, 0);

        // Wrap backwards
        state.select_previous();
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn test_select_by_index() {
        let mut state = HubState::new(PathBuf::from("/tmp/worktrees"));

        for i in 1..=3 {
            let agent = create_test_agent("owner/repo", Some(i), &format!("botster-issue-{i}"));
            state.add_agent(format!("owner-repo-{i}"), agent);
        }

        // 1-based indexing
        assert!(state.select_by_index(2));
        assert_eq!(state.selected, 1);

        // Out of bounds
        assert!(!state.select_by_index(0));
        assert!(!state.select_by_index(5));
    }

    #[test]
    fn test_select_by_key() {
        let mut state = HubState::new(PathBuf::from("/tmp/worktrees"));

        for i in 1..=3 {
            let agent = create_test_agent("owner/repo", Some(i), &format!("botster-issue-{i}"));
            state.add_agent(format!("owner-repo-{i}"), agent);
        }

        assert!(state.select_by_key("owner-repo-2"));
        assert_eq!(state.selected, 1);

        assert!(!state.select_by_key("nonexistent"));
    }

    #[test]
    fn test_selection_clamps_on_remove() {
        let mut state = HubState::new(PathBuf::from("/tmp/worktrees"));

        for i in 1..=3 {
            let agent = create_test_agent("owner/repo", Some(i), &format!("botster-issue-{i}"));
            state.add_agent(format!("owner-repo-{i}"), agent);
        }

        // Select last agent
        state.selected = 2;

        // Remove it
        state.remove_agent("owner-repo-3");

        // Selection should clamp
        assert_eq!(state.selected, 1);
    }
}
