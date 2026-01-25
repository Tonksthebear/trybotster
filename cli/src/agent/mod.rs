//! Agent management for the botster-hub.
//!
//! This module provides the core agent types for managing PTY sessions.
//! Each agent runs in its own git worktree with dedicated PTY sessions
//! for the CLI process and optionally a dev server.
//!
//! # Architecture
//!
//! ```text
//! Agent
//! +-- cli_pty: PtySession (runs main agent process)
//! +-- server_pty: Option<PtySession> (runs dev server)
//! ```
//!
//! Agents are agnostic - they spawn whatever processes the user configures
//! via `.botster_init` and `.botster_server` scripts in the worktree.
//!
//! # Client State vs Agent State
//!
//! Agent owns process state (PTYs, channels, metadata).
//! Clients own view state (active_pty, scroll position).
//!
//! This separation allows multiple clients to view the same agent with
//! independent view states - one client can be scrolled up in CLI PTY
//! while another views the Server PTY live.
//!
//! # Submodules
//!
//! - [`notification`]: Terminal notification detection (OSC 9, OSC 777)
//! - [`pty`]: PTY session management

// Rust guideline compliant 2026-01

pub mod notification;
pub mod pty;
pub mod spawn;

pub use crate::tui::screen::ScreenInfo;
pub use notification::{detect_notifications, AgentNotification, AgentStatus};
pub use pty::PtySession;

use crate::channel::{ActionCableChannel, Channel, ChannelConfig};
use crate::relay::crypto_service::CryptoServiceHandle;
use anyhow::Result;
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{mpsc::Receiver, Arc, Mutex},
    time::Duration,
};

/// Which PTY view to target for operations.
///
/// Agents can have both a CLI PTY (main process) and a server PTY
/// (dev server). Operations that need to target a specific PTY
/// take this enum as a parameter.
///
/// Note: This is NOT agent state - it's a parameter for operations.
/// Each client tracks their own active view separately.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PtyView {
    /// CLI view - shows main agent process output.
    #[default]
    Cli,
    /// Server view - shows dev server output.
    Server,
}

/// An agent running in a git worktree.
///
/// Each agent has:
/// - A unique ID and session key
/// - A CLI PTY running the main agent process
/// - An optional server PTY for the dev server
/// - Notification channel for terminal events
///
/// The agent is process-agnostic - it runs whatever the user configures.
///
/// # Client State Separation
///
/// Agent does NOT track:
/// - `active_pty` - Each client tracks their own view
/// - `size_owner` - PTY sessions track connected clients
/// - Scroll position - Each client tracks their own scroll
///
/// Methods that previously used `active_pty` now take a `view: PtyView`
/// parameter, allowing each client to operate independently.
pub struct Agent {
    /// Unique identifier for this agent instance.
    pub id: uuid::Uuid,
    /// Repository name in "owner/repo" format.
    pub repo: String,
    /// Issue number if working on a specific issue.
    pub issue_number: Option<u32>,
    /// Git branch name.
    pub branch_name: String,
    /// Path to the git worktree directory.
    pub worktree_path: PathBuf,
    /// When this agent was created.
    pub start_time: chrono::DateTime<chrono::Utc>,
    /// Current execution status.
    pub status: AgentStatus,
    /// GitHub URL where this agent was last invoked from.
    pub last_invocation_url: Option<String>,
    /// Port for HTTP tunnel forwarding.
    pub tunnel_port: Option<u16>,
    /// macOS Terminal window ID for focusing.
    pub terminal_window_id: Option<String>,

    /// Primary PTY (CLI - runs main agent process).
    ///
    /// Always exists. Check `cli_pty.is_spawned()` to see if a process is running.
    pub cli_pty: PtySession,

    /// Secondary PTY (Server - runs dev server).
    pub server_pty: Option<PtySession>,

    /// Preview channel for encrypted HTTP proxying.
    /// Only created if `tunnel_port` is set.
    /// Note: Terminal channels are owned by PtySession, not Agent.
    pub preview_channel: Option<ActionCableChannel>,

    notification_rx: Option<Receiver<AgentNotification>>,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("id", &self.id)
            .field("repo", &self.repo)
            .field("issue_number", &self.issue_number)
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
    pub fn new(
        id: uuid::Uuid,
        repo: String,
        issue_number: Option<u32>,
        branch_name: String,
        worktree_path: PathBuf,
    ) -> Self {
        Self::new_with_dims(
            id,
            repo,
            issue_number,
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
    /// * `issue_number` - Optional issue number
    /// * `branch_name` - Git branch name
    /// * `worktree_path` - Path to the git worktree
    /// * `terminal_dims` - PTY dimensions as (rows, cols)
    #[must_use]
    pub fn new_with_dims(
        id: uuid::Uuid,
        repo: String,
        issue_number: Option<u32>,
        branch_name: String,
        worktree_path: PathBuf,
        terminal_dims: (u16, u16),
    ) -> Self {
        let (rows, cols) = terminal_dims;
        Self {
            id,
            repo,
            issue_number,
            branch_name,
            worktree_path,
            start_time: chrono::Utc::now(),
            status: AgentStatus::Initializing,
            last_invocation_url: None,
            tunnel_port: None,
            terminal_window_id: None,
            cli_pty: PtySession::new(rows, cols),
            server_pty: None,
            preview_channel: None,
            notification_rx: None,
        }
    }

    // =========================================================================
    // PTY Access
    // =========================================================================

    /// Get the PTY session for the specified view.
    ///
    /// Falls back to CLI PTY if Server view is requested but server_pty is None.
    #[must_use]
    pub fn get_pty(&self, view: PtyView) -> &PtySession {
        match view {
            PtyView::Cli => &self.cli_pty,
            PtyView::Server => self.server_pty.as_ref().unwrap_or(&self.cli_pty),
        }
    }

    /// Get mutable PTY session for the specified view.
    ///
    /// Falls back to CLI PTY if Server view is requested but server_pty is None.
    #[must_use]
    pub fn get_pty_mut(&mut self, view: PtyView) -> &mut PtySession {
        match view {
            PtyView::Cli => &mut self.cli_pty,
            PtyView::Server => {
                if self.server_pty.is_some() {
                    self.server_pty.as_mut().unwrap()
                } else {
                    &mut self.cli_pty
                }
            }
        }
    }

    /// Get the scrollback buffer for the specified PTY view.
    #[must_use]
    pub fn get_scrollback_buffer(&self, view: PtyView) -> Arc<Mutex<VecDeque<u8>>> {
        Arc::clone(&self.get_pty(view).scrollback_buffer)
    }

    /// Check if server PTY is available.
    #[must_use]
    pub fn has_server_pty(&self) -> bool {
        self.server_pty.is_some()
    }

    /// Get a PtyHandle for the specified PTY index.
    ///
    /// - Index 0: CLI PTY (always present)
    /// - Index 1: Server PTY (if server is running)
    ///
    /// Returns `None` if index is out of bounds or server PTY not available.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Get handle for CLI PTY
    /// let cli_handle = agent.get_pty_handle(0)?;
    ///
    /// // Subscribe to PTY events
    /// let rx = cli_handle.subscribe();
    /// ```
    #[must_use]
    pub fn get_pty_handle(&self, pty_index: usize) -> Option<crate::hub::agent_handle::PtyHandle> {
        let (event_tx, command_tx) = match pty_index {
            0 => self.cli_pty.get_channels(),
            1 => self.server_pty.as_ref()?.get_channels(),
            _ => return None,
        };
        Some(crate::hub::agent_handle::PtyHandle::new(event_tx, command_tx))
    }

    /// Get the current PTY size (rows, cols).
    ///
    /// Returns the dimensions tracked by the CLI PTY.
    #[must_use]
    pub fn get_pty_size(&self) -> (u16, u16) {
        self.cli_pty.dimensions()
    }

    // =========================================================================
    // Resize Operations
    // =========================================================================

    /// Resize all PTY sessions to new dimensions.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.cli_pty.resize(rows, cols);
        if let Some(server_pty) = &mut self.server_pty {
            server_pty.resize(rows, cols);
        }
    }

    /// Resize a specific PTY view.
    pub fn resize_pty(&mut self, view: PtyView, rows: u16, cols: u16) {
        self.get_pty_mut(view).resize(rows, cols);
    }

    // =========================================================================
    // Input/Output
    // =========================================================================

    /// Write input to the specified PTY view.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn write_input(&mut self, view: PtyView, input: &[u8]) -> Result<()> {
        self.get_pty_mut(view).write_input(input)
    }

    /// Write a string to the specified PTY view.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn write_input_str(&mut self, view: PtyView, input: &str) -> Result<()> {
        self.write_input(view, input.as_bytes())
    }

    /// Write input specifically to the CLI PTY (for notifications, etc.).
    ///
    /// Convenience method that always targets CLI regardless of client view.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn write_input_to_cli(&mut self, input: &[u8]) -> Result<()> {
        self.cli_pty.write_input(input)
    }

    // =========================================================================
    // Lifecycle & Spawn
    // =========================================================================

    /// Spawn the CLI PTY with the given command and environment.
    ///
    /// # Errors
    ///
    /// Returns an error if PTY creation or command spawn fails.
    pub fn spawn(
        &mut self,
        command_str: &str,
        context: &str,
        init_commands: Vec<String>,
        env_vars: &HashMap<String, String>,
    ) -> Result<()> {
        log::info!(
            "Spawning agent for {}: command={}, worktree={}",
            self.issue_number.map_or_else(
                || format!("{}/{}", self.repo, self.branch_name),
                |num| format!("{}#{num}", self.repo),
            ),
            command_str,
            self.worktree_path.display()
        );

        // Use the extracted spawn function
        let result = pty::spawn_cli_pty(
            &mut self.cli_pty,
            &self.worktree_path,
            command_str,
            env_vars,
            init_commands,
            context,
        )?;

        self.notification_rx = Some(result.notification_rx);
        self.status = AgentStatus::Running;

        Ok(())
    }

    /// Spawn a server PTY to run the dev server.
    ///
    /// # Errors
    ///
    /// Returns an error if PTY creation or shell spawn fails.
    pub fn spawn_server_pty(
        &mut self,
        init_script: &str,
        env_vars: &HashMap<String, String>,
    ) -> Result<()> {
        let (rows, cols) = self.cli_pty.dimensions();

        // Use the extracted spawn function
        let server_pty =
            pty::spawn_server_pty(&self.worktree_path, init_script, env_vars, (rows, cols))?;

        self.server_pty = Some(server_pty);
        Ok(())
    }

    /// Check if the dev server is running.
    #[must_use]
    pub fn is_server_running(&self) -> bool {
        if let Some(port) = self.tunnel_port {
            use std::net::TcpStream;

            TcpStream::connect_timeout(
                &format!("127.0.0.1:{port}")
                    .parse()
                    .expect("valid socket addr"),
                Duration::from_millis(50),
            )
            .is_ok()
        } else {
            false
        }
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
    /// Format: `{repo-safe}-{issue_number}` or `{repo-safe}-{branch-name}`.
    #[must_use]
    pub fn agent_id(&self) -> String {
        let repo_safe = self.repo.replace('/', "-");
        if let Some(issue_num) = self.issue_number {
            format!("{repo_safe}-{issue_num}")
        } else {
            format!("{}-{}", repo_safe, self.branch_name.replace('/', "-"))
        }
    }

    /// Poll for any pending notifications from the PTY (non-blocking).
    #[must_use]
    pub fn poll_notifications(&self) -> Vec<AgentNotification> {
        let mut notifications = Vec::new();
        if let Some(ref rx) = self.notification_rx {
            while let Ok(notif) = rx.try_recv() {
                notifications.push(notif);
            }
        }
        notifications
    }

    // =========================================================================
    // Screen & Scrollback (parameterized by view)
    // =========================================================================

    /// Get a snapshot of the scrollback buffer as raw bytes for the specified view.
    ///
    /// Returns raw PTY output bytes that can be replayed in xterm.js.
    /// Preserves escape sequences, carriage returns, and all terminal control.
    #[must_use]
    pub fn get_scrollback_snapshot(&self, view: PtyView) -> Vec<u8> {
        self.get_pty(view).get_scrollback_snapshot()
    }

    /// Get scrollback as raw bytes for CLI PTY.
    ///
    /// Convenience method for backward compatibility.
    #[must_use]
    pub fn get_scrollback_bytes(&self) -> Vec<u8> {
        self.get_scrollback_snapshot(PtyView::Cli)
    }

    /// Get the current screen dimensions for the specified view.
    #[must_use]
    pub fn get_screen_info(&self, view: PtyView) -> ScreenInfo {
        let (rows, cols) = self.get_pty(view).dimensions();
        ScreenInfo { rows, cols }
    }

    // =========================================================================
    // Channel Management
    // =========================================================================

    /// Connect this agent's preview channel (if tunnel_port is set).
    ///
    /// Note: Terminal channels are managed by BrowserClient, not Agent.
    /// Agent only manages the preview channel for HTTP proxying.
    ///
    /// # Errors
    ///
    /// Returns an error if channel connection fails.
    pub async fn connect_preview_channel(
        &mut self,
        crypto_service: CryptoServiceHandle,
        server_url: &str,
        api_key: &str,
        hub_id: &str,
        agent_index: usize,
    ) -> Result<()> {
        // Only create preview channel if tunnel_port is set
        if self.tunnel_port.is_some() {
            log::info!(
                "Agent {} connecting preview channel (agent_index={})",
                self.agent_id(),
                agent_index
            );

            let mut preview = ActionCableChannel::encrypted(
                crypto_service,
                server_url.to_string(),
                api_key.to_string(),
            );
            preview
                .connect(ChannelConfig {
                    channel_name: "PreviewChannel".into(),
                    hub_id: hub_id.to_string(),
                    agent_index: Some(agent_index),
                    pty_index: None, // Preview channel doesn't use PTY index
                    encrypt: true,
                    compression_threshold: Some(4096),
                })
                .await
                .map_err(|e| anyhow::anyhow!("Preview channel connect failed: {}", e))?;

            self.preview_channel = Some(preview);
            log::info!("Agent {} preview channel connected", self.agent_id());
        }

        Ok(())
    }

    /// Check if this agent has a connected preview channel.
    #[must_use]
    pub fn has_preview_channel(&self) -> bool {
        self.preview_channel.is_some()
    }

    /// Disconnect this agent's preview channel.
    ///
    /// Note: Terminal channels are managed by BrowserClient, not Agent.
    pub async fn disconnect_preview_channel(&mut self) {
        let agent_id = self.agent_id().to_string();
        if let Some(ref mut ch) = self.preview_channel {
            log::info!("Agent {} disconnecting preview channel", agent_id);
            ch.disconnect().await;
        }
        self.preview_channel = None;
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        log::info!(
            "Agent {} dropping - cleaning up PTY sessions",
            self.agent_id()
        );

        // Preview channel cleans up via its own Drop
        if self.preview_channel.is_some() {
            log::info!("Agent {} dropping preview channel", self.agent_id());
        }

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
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        assert_eq!(agent.repo, "test/repo");
        assert_eq!(agent.issue_number, Some(1));
        assert_eq!(agent.branch_name, "issue-1");
        assert!(matches!(agent.status, AgentStatus::Initializing));
    }

    #[test]
    fn test_agent_id() {
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            uuid::Uuid::new_v4(),
            "owner/repo".to_string(),
            Some(42),
            "issue-42".to_string(),
            temp_dir.path().to_path_buf(),
        );

        assert_eq!(agent.agent_id(), "owner-repo-42");
    }

    #[test]
    fn test_scrollback_snapshot() {
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        // Scrollback is initially empty (populated by PTY reader thread)
        let snapshot = agent.get_scrollback_snapshot(PtyView::Cli);
        assert!(snapshot.is_empty());
    }

    #[test]
    fn test_agent_age() {
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        let age = agent.age();
        assert!(age.as_millis() < 1000);
    }

    #[test]
    fn test_get_pty_cli() {
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        // CLI PTY is always available, check dimensions
        let pty = agent.get_pty(PtyView::Cli);
        let (rows, cols) = pty.dimensions();

        assert_eq!(rows, 24);
        assert_eq!(cols, 80);
    }

    #[test]
    fn test_get_pty_server_fallback() {
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        // Without server_pty, Server view falls back to CLI
        let pty = agent.get_pty(PtyView::Server);
        let (rows, cols) = pty.dimensions();

        assert_eq!(rows, 24);
        assert_eq!(cols, 80);
    }

    #[test]
    fn test_get_pty_server_when_available() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        // Add server PTY with different dimensions
        agent.server_pty = Some(PtySession::new(40, 120));

        let pty = agent.get_pty(PtyView::Server);
        let (rows, cols) = pty.dimensions();

        assert_eq!(rows, 40);
        assert_eq!(cols, 120);
    }

    #[test]
    fn test_has_server_pty() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        assert!(!agent.has_server_pty());

        agent.server_pty = Some(PtySession::new(24, 80));
        assert!(agent.has_server_pty());
    }

    #[test]
    fn test_pty_view_default() {
        assert_eq!(PtyView::default(), PtyView::Cli);
    }

    #[test]
    fn test_scrollback_for_view() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        agent.server_pty = Some(PtySession::new(24, 80));

        // Add different scrollback to each PTY
        agent.cli_pty.add_to_scrollback(b"CLI scrollback");
        agent
            .server_pty
            .as_ref()
            .unwrap()
            .add_to_scrollback(b"SERVER scrollback");

        let cli_snapshot = agent.get_scrollback_snapshot(PtyView::Cli);
        let server_snapshot = agent.get_scrollback_snapshot(PtyView::Server);

        assert_eq!(cli_snapshot, b"CLI scrollback");
        assert_eq!(server_snapshot, b"SERVER scrollback");
    }

    #[test]
    fn test_get_screen_info() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        agent.server_pty = Some(PtySession::new(40, 120));

        let cli_info = agent.get_screen_info(PtyView::Cli);
        assert_eq!(cli_info.rows, 24);
        assert_eq!(cli_info.cols, 80);

        let server_info = agent.get_screen_info(PtyView::Server);
        assert_eq!(server_info.rows, 40);
        assert_eq!(server_info.cols, 120);
    }
}
