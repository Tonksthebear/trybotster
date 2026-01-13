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
//! ├── cli_pty: PtySession (runs main agent process)
//! └── server_pty: Option<PtySession> (runs dev server)
//! ```
//!
//! Agents are agnostic - they spawn whatever processes the user configures
//! via `.botster_init` and `.botster_server` scripts in the worktree.
//!
//! # Submodules
//!
//! - [`notification`]: Terminal notification detection (OSC 9, OSC 777)
//! - [`pty`]: PTY session management
//! - [`screen`]: Screen rendering utilities

// Rust guideline compliant 2025-01

pub mod notification;
pub mod pty;
pub mod screen;
pub mod scroll;
pub mod spawn;

pub use notification::{detect_notifications, AgentNotification, AgentStatus};
pub use pty::PtySession;
pub use screen::ScreenInfo;

use anyhow::Result;
use pty::MAX_BUFFER_LINES;
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        mpsc::Receiver,
        Arc, Mutex,
    },
    time::Duration,
};
use vt100::Parser;

/// Which PTY view is currently active in the TUI.
///
/// Agents can have both a CLI PTY (main process) and a server PTY
/// (dev server). This enum tracks which one is displayed.
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

    /// Which PTY is currently displayed.
    pub active_pty: PtyView,

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
            .field("active_pty", &self.active_pty)
            .finish_non_exhaustive()
    }
}

impl Agent {
    /// Creates a new agent for the specified repository and worktree.
    #[must_use]
    pub fn new(
        id: uuid::Uuid,
        repo: String,
        issue_number: Option<u32>,
        branch_name: String,
        worktree_path: PathBuf,
    ) -> Self {
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
            cli_pty: PtySession::new(24, 80),
            server_pty: None,
            active_pty: PtyView::Cli,
            notification_rx: None,
        }
    }

    /// Check if we're in scrollback mode (scrolled up from live view).
    /// Delegates to `scroll::is_scrolled()`.
    #[must_use]
    pub fn is_scrolled(&self) -> bool {
        scroll::is_scrolled(self)
    }

    /// Get current scroll offset from vt100.
    /// Delegates to `scroll::get_offset()`.
    #[must_use]
    pub fn get_scroll_offset(&self) -> usize {
        scroll::get_offset(self)
    }

    /// Scroll up by the specified number of lines.
    /// Delegates to `scroll::up()`.
    pub fn scroll_up(&mut self, lines: usize) {
        scroll::up(self, lines);
    }

    /// Scroll down by the specified number of lines.
    /// Delegates to `scroll::down()`.
    pub fn scroll_down(&mut self, lines: usize) {
        scroll::down(self, lines);
    }

    /// Scroll to the bottom (return to live view).
    /// Delegates to `scroll::to_bottom()`.
    pub fn scroll_to_bottom(&mut self) {
        scroll::to_bottom(self);
    }

    /// Scroll to the top of the scrollback buffer.
    /// Delegates to `scroll::to_top()`.
    pub fn scroll_to_top(&mut self) {
        scroll::to_top(self);
    }

    /// Get the buffer for the currently active PTY.
    #[must_use]
    pub fn get_active_buffer(&self) -> Arc<Mutex<VecDeque<String>>> {
        match self.active_pty {
            PtyView::Cli => Arc::clone(&self.cli_pty.buffer),
            PtyView::Server => {
                if let Some(server_pty) = &self.server_pty {
                    Arc::clone(&server_pty.buffer)
                } else {
                    Arc::clone(&self.cli_pty.buffer)
                }
            }
        }
    }

    /// Resize all PTY sessions to new dimensions.
    ///
    /// This clears the vt100 parser screens to ensure content renders at the new
    /// dimensions. Old content would otherwise be stuck at the old column width.
    pub fn resize(&self, rows: u16, cols: u16) {
        pty::resize_with_clear(&self.cli_pty, rows, cols, "CLI");
        if let Some(server_pty) = &self.server_pty {
            pty::resize_with_clear(server_pty, rows, cols, "Server");
        }
    }

    /// Toggle between CLI and Server PTY views.
    pub fn toggle_pty_view(&mut self) {
        self.active_pty = match self.active_pty {
            PtyView::Cli => {
                if self.server_pty.is_some() {
                    PtyView::Server
                } else {
                    PtyView::Cli
                }
            }
            PtyView::Server => PtyView::Cli,
        };
    }

    /// Get the VT100 parser for the currently active PTY view.
    #[must_use]
    pub fn get_active_parser(&self) -> Arc<Mutex<Parser>> {
        match self.active_pty {
            PtyView::Cli => Arc::clone(&self.cli_pty.vt100_parser),
            PtyView::Server => {
                if let Some(server_pty) = &self.server_pty {
                    Arc::clone(&server_pty.vt100_parser)
                } else {
                    Arc::clone(&self.cli_pty.vt100_parser)
                }
            }
        }
    }

    /// Check if server PTY is available.
    #[must_use]
    pub fn has_server_pty(&self) -> bool {
        self.server_pty.is_some()
    }

    /// Get the current PTY size (rows, cols).
    ///
    /// Returns the dimensions of the CLI PTY's vt100 parser screen.
    #[must_use]
    pub fn get_pty_size(&self) -> (u16, u16) {
        let parser = self.cli_pty.vt100_parser.lock().expect("parser lock poisoned");
        parser.screen().size()
    }

    /// Check if the dev server is running.
    #[must_use]
    pub fn is_server_running(&self) -> bool {
        if let Some(port) = self.tunnel_port {
            use std::net::TcpStream;

            TcpStream::connect_timeout(
                &format!("127.0.0.1:{port}").parse().expect("valid socket addr"),
                Duration::from_millis(50),
            )
            .is_ok()
        } else {
            false
        }
    }

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
        let agent_label = self.issue_number.map_or_else(
            || format!("{}/{}", self.repo, self.branch_name),
            |num| format!("{}#{num}", self.repo),
        );
        self.add_to_buffer(&format!("==> Spawning agent: {agent_label}"));
        self.add_to_buffer(&format!("==> Command: {command_str}"));
        self.add_to_buffer(&format!("==> Worktree: {}", self.worktree_path.display()));
        self.add_to_buffer("");

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
        let (rows, cols) = {
            let parser = self.cli_pty.vt100_parser.lock().expect("parser lock poisoned");
            parser.screen().size()
        };

        // Use the extracted spawn function
        let server_pty = pty::spawn_server_pty(
            &self.worktree_path,
            init_script,
            env_vars,
            (rows, cols),
        )?;

        self.server_pty = Some(server_pty);
        Ok(())
    }

    /// Write input to the currently active PTY (CLI or Server based on `active_pty`).
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn write_input(&mut self, input: &[u8]) -> Result<()> {
        match self.active_pty {
            PtyView::Cli => {
                self.cli_pty.write_input(input)?;
            }
            PtyView::Server => {
                if let Some(server_pty) = &mut self.server_pty {
                    server_pty.write_input(input)?;
                } else {
                    self.cli_pty.write_input(input)?;
                }
            }
        }
        Ok(())
    }

    /// Write a string to the currently active PTY.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn write_input_str(&mut self, input: &str) -> Result<()> {
        self.write_input(input.as_bytes())
    }

    /// Write input specifically to the CLI PTY (for notifications, etc.).
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn write_input_to_cli(&mut self, input: &[u8]) -> Result<()> {
        self.cli_pty.write_input(input)
    }

    /// Add a line to the buffer.
    pub fn add_to_buffer(&self, line: &str) {
        let mut buffer = self.cli_pty.buffer.lock().expect("buffer lock poisoned");
        buffer.push_back(line.to_string());
        if buffer.len() > MAX_BUFFER_LINES {
            buffer.pop_front();
        }
    }

    /// Get how long this agent has been running.
    #[must_use]
    pub fn age(&self) -> Duration {
        chrono::Utc::now()
            .signed_duration_since(self.start_time)
            .to_std()
            .unwrap_or_default()
    }

    /// Generate a unique session key for this agent.
    #[must_use]
    pub fn session_key(&self) -> String {
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

    /// Get a snapshot of the buffer contents.
    #[must_use]
    pub fn get_buffer_snapshot(&self) -> Vec<String> {
        self.cli_pty
            .buffer
            .lock()
            .expect("buffer lock poisoned")
            .iter()
            .cloned()
            .collect()
    }

    /// Get the rendered VT100 screen as lines.
    #[must_use]
    pub fn get_vt100_screen(&self) -> Vec<String> {
        let parser = self.cli_pty.vt100_parser.lock().expect("parser lock poisoned");
        let s = parser.screen();
        s.rows(0, s.size().1).collect()
    }

    /// Get screen with cursor position.
    #[must_use]
    pub fn get_vt100_screen_with_cursor(&self) -> (Vec<String>, (u16, u16)) {
        let parser = self.cli_pty.vt100_parser.lock().expect("parser lock poisoned");
        let s = parser.screen();

        let lines: Vec<String> = s.rows(0, s.size().1).collect();
        let cursor = s.cursor_position();

        (lines, cursor)
    }

    /// Get scrollback from the line buffer.
    #[must_use]
    pub fn get_vt100_with_scrollback(&self) -> Vec<String> {
        self.get_buffer_snapshot()
    }

    /// Get the screen as ANSI escape sequences for streaming.
    #[must_use]
    pub fn get_screen_as_ansi(&self) -> String {
        let active_parser = self.get_active_parser();
        let parser = active_parser.lock().expect("parser lock poisoned");
        screen::render_screen_as_ansi(parser.screen())
    }

    /// Drain raw PTY output for browser streaming.
    ///
    /// Returns all raw bytes that have accumulated since last drain.
    /// Used by browser GUI to stream raw output - xterm.js handles
    /// parsing and scrollback naturally.
    #[must_use]
    pub fn drain_raw_output(&self) -> Vec<u8> {
        self.cli_pty.drain_raw_output()
    }

    /// Get a hash of the current screen content for change detection.
    #[must_use]
    pub fn get_screen_hash(&self) -> u64 {
        let active_parser = self.get_active_parser();
        let parser = active_parser.lock().expect("parser lock poisoned");
        screen::compute_screen_hash(parser.screen())
    }

    /// Get the current screen dimensions for debugging.
    #[must_use]
    pub fn get_screen_info(&self) -> ScreenInfo {
        let active_parser = self.get_active_parser();
        let parser = active_parser.lock().expect("parser lock poisoned");
        let s = parser.screen();
        let (rows, cols) = s.size();
        ScreenInfo { rows, cols }
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        log::info!(
            "Agent {} dropping - cleaning up PTY sessions",
            self.session_key()
        );

        if let Some(ref mut server_pty) = self.server_pty {
            log::info!("Killing server PTY child process");
            server_pty.kill_child();
        }

        log::info!("Killing CLI PTY child process");
        self.cli_pty.kill_child();
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
    fn test_session_key() {
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            uuid::Uuid::new_v4(),
            "owner/repo".to_string(),
            Some(42),
            "issue-42".to_string(),
            temp_dir.path().to_path_buf(),
        );

        assert_eq!(agent.session_key(), "owner-repo-42");
    }

    #[test]
    fn test_add_to_buffer() {
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        agent.add_to_buffer("line 1");
        agent.add_to_buffer("line 2");

        let snapshot = agent.get_buffer_snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0], "line 1");
        assert_eq!(snapshot[1], "line 2");
    }

    #[test]
    fn test_buffer_limit() {
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        for i in 0..MAX_BUFFER_LINES + 100 {
            agent.add_to_buffer(&format!("line {i}"));
        }

        let snapshot = agent.get_buffer_snapshot();
        assert_eq!(snapshot.len(), MAX_BUFFER_LINES);
        assert_eq!(snapshot[0], "line 100");
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
    fn test_agent_scrollback_initial_state() {
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        assert!(!agent.is_scrolled());
        assert_eq!(agent.get_scroll_offset(), 0);
    }

    #[test]
    fn test_agent_scroll_up_and_down() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        {
            let mut parser = agent.cli_pty.vt100_parser.lock().unwrap();
            for i in 0..50 {
                parser.process(format!("Line {i}\r\n").as_bytes());
            }
        }

        agent.scroll_up(10);
        assert!(agent.is_scrolled());
        assert_eq!(agent.get_scroll_offset(), 10);

        agent.scroll_down(5);
        assert_eq!(agent.get_scroll_offset(), 5);

        agent.scroll_to_bottom();
        assert!(!agent.is_scrolled());
        assert_eq!(agent.get_scroll_offset(), 0);
    }

    #[test]
    fn test_scroll_down_does_not_go_negative() {
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        assert_eq!(agent.get_scroll_offset(), 0);
    }

    #[test]
    fn test_pty_view_toggle() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        assert_eq!(agent.active_pty, PtyView::Cli);

        // Toggle without server PTY should stay on CLI
        agent.toggle_pty_view();
        assert_eq!(agent.active_pty, PtyView::Cli);

        // Add a mock server PTY
        agent.server_pty = Some(PtySession::new(24, 80));

        // Now toggle should work
        agent.toggle_pty_view();
        assert_eq!(agent.active_pty, PtyView::Server);

        // Toggle back
        agent.toggle_pty_view();
        assert_eq!(agent.active_pty, PtyView::Cli);
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
    fn test_get_active_parser_cli() {
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        // cli_pty is initialized with default 24x80
        let active = agent.get_active_parser();
        let parser = active.lock().unwrap();
        let (rows, cols) = parser.screen().size();

        assert_eq!(rows, 24);
        assert_eq!(cols, 80);
    }

    #[test]
    fn test_get_active_parser_server() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        agent.server_pty = Some(PtySession::new(40, 120));
        agent.active_pty = PtyView::Server;

        let active = agent.get_active_parser();
        let parser = active.lock().unwrap();
        let (rows, cols) = parser.screen().size();

        assert_eq!(rows, 40);
        assert_eq!(cols, 120);
    }

    #[test]
    fn test_pty_view_default() {
        assert_eq!(PtyView::default(), PtyView::Cli);
    }

    #[test]
    fn test_scroll_up_extreme_value_does_not_crash() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        {
            let mut parser = agent.cli_pty.vt100_parser.lock().unwrap();
            for i in 0..100 {
                parser.process(format!("Line {i}\r\n").as_bytes());
            }
        }

        agent.scroll_up(usize::MAX);
        assert!(agent.is_scrolled());
    }

    #[test]
    fn test_scroll_to_top() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        {
            let mut parser = agent.cli_pty.vt100_parser.lock().unwrap();
            for i in 0..100 {
                parser.process(format!("Line {i}\r\n").as_bytes());
            }
        }

        agent.scroll_to_top();
        assert!(agent.is_scrolled());
        let offset = agent.get_scroll_offset();
        assert!(offset > 0);
    }

    #[test]
    fn test_scroll_up_overflow_with_existing_offset() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        {
            let mut parser = agent.cli_pty.vt100_parser.lock().unwrap();
            for i in 0..50 {
                parser.process(format!("Line {i}\r\n").as_bytes());
            }
        }

        agent.scroll_up(10);
        let offset1 = agent.get_scroll_offset();
        assert_eq!(offset1, 10);

        agent.scroll_up(usize::MAX - 5);
        assert!(agent.is_scrolled());
    }

    #[test]
    fn test_server_pty_scroll_independence() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        agent.server_pty = Some(PtySession::new(24, 80));

        {
            let mut parser = agent.cli_pty.vt100_parser.lock().unwrap();
            for i in 0..50 {
                parser.process(format!("CLI Line {i}\r\n").as_bytes());
            }
        }

        {
            let server_pty = agent.server_pty.as_ref().unwrap();
            let mut parser = server_pty.vt100_parser.lock().unwrap();
            for i in 0..30 {
                parser.process(format!("Server Line {i}\r\n").as_bytes());
            }
        }

        agent.active_pty = PtyView::Cli;
        agent.scroll_up(15);
        let cli_offset = agent.get_scroll_offset();
        assert_eq!(cli_offset, 15);

        agent.active_pty = PtyView::Server;
        let server_offset = agent.get_scroll_offset();
        assert_eq!(server_offset, 0);

        agent.scroll_up(5);
        let server_offset_after = agent.get_scroll_offset();
        assert_eq!(server_offset_after, 5);

        agent.active_pty = PtyView::Cli;
        let cli_offset_unchanged = agent.get_scroll_offset();
        assert_eq!(cli_offset_unchanged, 15);
    }

    #[test]
    fn test_repeated_scroll_up_past_buffer() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        {
            let mut parser = agent.cli_pty.vt100_parser.lock().unwrap();
            for i in 0..50 {
                parser.process(format!("Line {i}\r\n").as_bytes());
            }
        }

        for _ in 0..100 {
            agent.scroll_up(5);
        }

        assert!(agent.is_scrolled());
    }

    #[test]
    fn test_scroll_preserves_across_view_switch() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        agent.server_pty = Some(PtySession::new(24, 80));

        {
            let mut parser = agent.cli_pty.vt100_parser.lock().unwrap();
            for i in 0..50 {
                parser.process(format!("Line {i}\r\n").as_bytes());
            }
        }

        agent.scroll_up(20);
        assert_eq!(agent.get_scroll_offset(), 20);

        agent.toggle_pty_view();
        assert_eq!(agent.active_pty, PtyView::Server);

        agent.toggle_pty_view();
        assert_eq!(agent.active_pty, PtyView::Cli);
        assert_eq!(agent.get_scroll_offset(), 20);
    }

    #[test]
    fn test_get_screen_as_ansi_uses_active_pty() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        agent.server_pty = Some(PtySession::new(24, 80));

        {
            let mut parser = agent.cli_pty.vt100_parser.lock().unwrap();
            parser.process(b"CLI CONTENT HERE\r\n");
        }

        {
            let server_pty = agent.server_pty.as_ref().unwrap();
            let mut parser = server_pty.vt100_parser.lock().unwrap();
            parser.process(b"SERVER CONTENT HERE\r\n");
        }

        fn extract_text(ansi: &str) -> String {
            let mut result = String::new();
            let mut chars = ansi.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\x1b' {
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if next.is_ascii_alphabetic() || next == 'h' || next == 'l' {
                            break;
                        }
                    }
                } else {
                    result.push(c);
                }
            }
            result
        }

        agent.active_pty = PtyView::Cli;
        let cli_ansi = agent.get_screen_as_ansi();
        let cli_text = extract_text(&cli_ansi);
        assert!(cli_text.contains("CLI CONTENT HERE"));
        assert!(!cli_text.contains("SERVER CONTENT HERE"));

        agent.active_pty = PtyView::Server;
        let server_ansi = agent.get_screen_as_ansi();
        let server_text = extract_text(&server_ansi);
        assert!(server_text.contains("SERVER CONTENT HERE"));
        assert!(!server_text.contains("CLI CONTENT HERE"));
    }

    #[test]
    fn test_get_screen_hash_changes_on_scroll() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        {
            let mut parser = agent.cli_pty.vt100_parser.lock().unwrap();
            for i in 0..50 {
                parser.process(format!("Line {i}\r\n").as_bytes());
            }
        }

        let hash1 = agent.get_screen_hash();

        agent.scroll_up(10);

        let hash2 = agent.get_screen_hash();

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_get_screen_hash_changes_on_pty_toggle() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        agent.server_pty = Some(PtySession::new(24, 80));

        {
            let mut parser = agent.cli_pty.vt100_parser.lock().unwrap();
            parser.process(b"CLI unique content\r\n");
        }

        {
            let server_pty = agent.server_pty.as_ref().unwrap();
            let mut parser = server_pty.vt100_parser.lock().unwrap();
            parser.process(b"SERVER unique content\r\n");
        }

        agent.active_pty = PtyView::Cli;
        let cli_hash = agent.get_screen_hash();

        agent.active_pty = PtyView::Server;
        let server_hash = agent.get_screen_hash();

        assert_ne!(cli_hash, server_hash);
    }
}
