//! Agent management for the botster.
//!
//! This module provides the core agent types for managing PTY sessions.
//! Each agent runs in its own git worktree with a single PTY session.
//!
//! # Architecture
//!
//! ```text
//! Agent
//! +-- pty: PtySession (runs the agent process)
//! ```
//!
//! Agents are agnostic — they spawn whatever processes the user configures
//! via `.botster/` session initialization scripts in the worktree.
//!
//! # Submodules
//!
//! - [`notification`]: Terminal notification detection (OSC 9, OSC 777)
//! - [`pty`]: PTY session management

// Rust guideline compliant 2026-03

pub mod message_delivery;
pub mod notification;
pub mod pty;
pub mod spawn;

pub use crate::tui::screen::ScreenInfo;
pub use notification::{detect_notifications, AgentNotification, AgentStatus};
pub use pty::PtySession;

use anyhow::Result;
use std::{path::PathBuf, time::Duration};

/// An agent running in a git worktree.
///
/// Each agent has:
/// - A unique ID and session key
/// - A single PTY running the agent process
///
/// The agent is process-agnostic - it runs whatever the user configures.
///
/// Agent metadata (repo, issue, status, etc.) is managed by Lua.
/// This struct provides PTY infrastructure for tests. In production,
/// PTY sessions are created directly and registered via HandleCache.
pub struct Agent {
    /// Unique identifier for this agent instance.
    pub id: uuid::Uuid,
    /// Repository name in "owner/repo" format.
    pub repo: String,
    /// Git branch name.
    pub branch_name: String,
    /// Path to the git worktree directory.
    pub worktree_path: PathBuf,
    /// When this agent was created.
    pub start_time: chrono::DateTime<chrono::Utc>,
    /// Current execution status.
    pub status: AgentStatus,
    /// macOS Terminal window ID for focusing.
    pub terminal_window_id: Option<String>,

    /// Single PTY session (runs the agent process).
    ///
    /// Always exists. Check `pty.is_spawned()` to see if a process is running.
    pub pty: PtySession,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("id", &self.id)
            .field("repo", &self.repo)
            .field("branch_name", &self.branch_name)
            .field("worktree_path", &self.worktree_path)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// Default PTY dimensions used when no specific dimensions are provided.
///
/// These are placeholder values; the PTY should be resized to actual terminal
/// dimensions before spawning a process. See `new_with_dims()` for creating
/// agents with specific dimensions.
const DEFAULT_PTY_ROWS: u16 = 24;
const DEFAULT_PTY_COLS: u16 = 80;

impl Agent {
    /// Creates a new agent for the specified repository and worktree.
    ///
    /// Uses default PTY dimensions (24x80). For production use, prefer
    /// `new_with_dims()` which accepts actual terminal dimensions.
    #[must_use]
    pub fn new(id: uuid::Uuid, repo: String, branch_name: String, worktree_path: PathBuf) -> Self {
        Self::new_with_dims(
            id,
            repo,
            branch_name,
            worktree_path,
            (DEFAULT_PTY_ROWS, DEFAULT_PTY_COLS),
        )
    }

    /// Creates a new agent with specific PTY dimensions.
    ///
    /// # Arguments
    ///
    /// * `id` - Unique agent identifier
    /// * `repo` - Repository name in "owner/repo" format
    /// * `branch_name` - Git branch name
    /// * `worktree_path` - Path to the git worktree
    /// * `terminal_dims` - PTY dimensions as (rows, cols)
    #[must_use]
    pub fn new_with_dims(
        id: uuid::Uuid,
        repo: String,
        branch_name: String,
        worktree_path: PathBuf,
        terminal_dims: (u16, u16),
    ) -> Self {
        let (rows, cols) = terminal_dims;
        Self {
            id,
            repo,
            branch_name,
            worktree_path,
            start_time: chrono::Utc::now(),
            status: AgentStatus::Initializing,
            terminal_window_id: None,
            pty: PtySession::new(rows, cols),
        }
    }

    // =========================================================================
    // PTY Access
    // =========================================================================

    /// Get a PtyHandle for this agent's PTY.
    #[must_use]
    #[cfg(test)]
    pub fn get_pty_handle(&self) -> crate::hub::agent_handle::PtyHandle {
        let (shared_state, shadow_screen, event_tx, kitty, cursor_vis, resize) =
            self.pty.get_direct_access();
        crate::hub::agent_handle::PtyHandle::new(
            event_tx,
            shared_state,
            shadow_screen,
            kitty,
            cursor_vis,
            resize,
            self.pty.port(),
        )
    }

    /// Get the current PTY size (rows, cols).
    #[must_use]
    pub fn get_pty_size(&self) -> (u16, u16) {
        self.pty.dimensions()
    }

    // =========================================================================
    // Input/Output
    // =========================================================================

    /// Write input to the PTY.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn write_input(&mut self, input: &[u8]) -> Result<()> {
        self.pty.write_input(input)
    }

    // =========================================================================
    // Metadata & Info
    // =========================================================================

    /// Get how long this agent has been running.
    #[must_use]
    pub fn age(&self) -> Duration {
        chrono::Utc::now()
            .signed_duration_since(self.start_time)
            .to_std()
            .unwrap_or_default()
    }

    /// Generate a unique agent ID for this agent.
    ///
    /// Format: `{repo-safe}-{branch-name}` (slashes replaced with dashes).
    #[must_use]
    pub fn agent_id(&self) -> String {
        let repo_safe = self.repo.replace('/', "-");
        format!("{}-{}", repo_safe, self.branch_name.replace('/', "-"))
    }

    // =========================================================================
    // Screen & Scrollback
    // =========================================================================

    /// Get a clean ANSI snapshot of the terminal state.
    ///
    /// Returns parsed screen contents as clean ANSI escape sequences with
    /// correct cursor positioning. Used for browser connect/reconnect.
    #[must_use]
    pub fn get_snapshot(&self) -> Vec<u8> {
        self.pty.get_snapshot()
    }

    /// Get the current screen dimensions.
    #[must_use]
    pub fn get_screen_info(&self) -> ScreenInfo {
        let (rows, cols) = self.pty.dimensions();
        ScreenInfo { rows, cols }
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        log::info!(
            "Agent {} dropping - cleaning up PTY sessions",
            self.agent_id()
        );
        // PTY child processes are killed by PtySession's Drop
        // which is called when Agent is dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_agent_creation() {
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        assert_eq!(agent.repo, "test/repo");
        assert_eq!(agent.branch_name, "issue-1");
        assert!(matches!(agent.status, AgentStatus::Initializing));
    }

    #[test]
    fn test_agent_id() {
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            uuid::Uuid::new_v4(),
            "owner/repo".to_string(),
            "botster-issue-42".to_string(),
            temp_dir.path().to_path_buf(),
        );

        assert_eq!(agent.agent_id(), "owner-repo-botster-issue-42");
    }

    #[test]
    fn test_scrollback_snapshot() {
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        // Snapshot contains reset sequences even when empty
        let snapshot = agent.get_snapshot();
        // Should contain at least the ANSI reset/clear prefix
        assert!(!snapshot.is_empty());
    }

    #[test]
    fn test_agent_age() {
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        let age = agent.age();
        assert!(age.as_millis() < 1000);
    }

    #[test]
    fn test_pty_access() {
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        let (rows, cols) = agent.pty.dimensions();
        assert_eq!(rows, 24);
        assert_eq!(cols, 80);
    }

    #[test]
    fn test_get_screen_info() {
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        let info = agent.get_screen_info();
        assert_eq!(info.rows, 24);
        assert_eq!(info.cols, 80);
    }
}
