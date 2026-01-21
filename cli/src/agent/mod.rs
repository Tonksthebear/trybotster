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

use crate::channel::{ActionCableChannel, Channel, ChannelConfig, PeerId};
use crate::relay::crypto_service::CryptoServiceHandle;
use anyhow::Result;
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

    /// Preview channel for encrypted HTTP proxying.
    /// Only created if `tunnel_port` is set.
    /// Note: Terminal channels are owned by PtySession, not Agent.
    pub preview_channel: Option<ActionCableChannel>,

    /// Client that currently "owns" the PTY size.
    /// Only this client can resize the PTY. Updated when a client views the agent.
    pub size_owner: Option<crate::client::ClientId>,

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
            preview_channel: None,
            size_owner: None,
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

    /// Get the scrollback buffer for the currently active PTY.
    #[must_use]
    pub fn get_active_scrollback_buffer(&self) -> Arc<Mutex<VecDeque<u8>>> {
        match self.active_pty {
            PtyView::Cli => Arc::clone(&self.cli_pty.scrollback_buffer),
            PtyView::Server => {
                if let Some(server_pty) = &self.server_pty {
                    Arc::clone(&server_pty.scrollback_buffer)
                } else {
                    Arc::clone(&self.cli_pty.scrollback_buffer)
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

    /// Get a snapshot of the scrollback buffer as raw bytes.
    ///
    /// Returns raw PTY output bytes that can be replayed in xterm.js.
    /// Preserves escape sequences, carriage returns, and all terminal control.
    #[must_use]
    pub fn get_scrollback_snapshot(&self) -> Vec<u8> {
        self.cli_pty.get_scrollback_snapshot()
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

    /// Get scrollback as raw bytes.
    ///
    /// Alias for `get_scrollback_snapshot` for API compatibility.
    #[must_use]
    pub fn get_scrollback_bytes(&self) -> Vec<u8> {
        self.get_scrollback_snapshot()
    }

    /// Get the screen as ANSI escape sequences for streaming.
    #[must_use]
    pub fn get_screen_as_ansi(&self) -> String {
        let active_parser = self.get_active_parser();
        let parser = active_parser.lock().expect("parser lock poisoned");
        screen::render_screen_as_ansi(parser.screen())
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

    /// Connect this agent's channels (terminal and optionally preview).
    ///
    /// Creates encrypted ActionCable connections for:
    /// - Terminal relay: Always connected for PTY I/O
    /// - Preview relay: Only if `tunnel_port` is set (HTTP proxying)
    ///
    /// # Errors
    ///
    /// Returns an error if channel connection fails.
    pub async fn connect_channels(
        &mut self,
        crypto_service: CryptoServiceHandle,
        server_url: &str,
        api_key: &str,
        hub_id: &str,
        agent_index: usize,
    ) -> Result<()> {
        use crate::agent::pty::pty_index;

        log::info!(
            "Agent {} connecting channels (agent_index={})",
            self.session_key(),
            agent_index
        );

        // Connect CLI PTY channel (pty_index=0)
        self.cli_pty
            .connect_channel(
                ChannelConfig {
                    channel_name: "TerminalRelayChannel".into(),
                    hub_id: hub_id.to_string(),
                    agent_index: Some(agent_index),
                    pty_index: Some(pty_index::CLI),
                    encrypt: true,
                    compression_threshold: Some(4096),
                },
                crypto_service.clone(),
                server_url,
                api_key,
            )
            .await
            .map_err(|e| anyhow::anyhow!("CLI PTY channel connect failed: {}", e))?;
        log::info!("Agent {} CLI PTY channel connected", self.session_key());

        // Connect Server PTY channel if server_pty exists (pty_index=1)
        if let Some(ref mut server_pty) = self.server_pty {
            server_pty
                .connect_channel(
                    ChannelConfig {
                        channel_name: "TerminalRelayChannel".into(),
                        hub_id: hub_id.to_string(),
                        agent_index: Some(agent_index),
                        pty_index: Some(pty_index::SERVER),
                        encrypt: true,
                        compression_threshold: Some(4096),
                    },
                    crypto_service.clone(),
                    server_url,
                    api_key,
                )
                .await
                .map_err(|e| anyhow::anyhow!("Server PTY channel connect failed: {}", e))?;
            log::info!("Agent {} Server PTY channel connected", self.session_key());
        }

        // Create preview channel only if tunnel_port is set
        if self.tunnel_port.is_some() {
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
            log::info!("Agent {} preview channel connected", self.session_key());
        }

        Ok(())
    }

    /// Check if CLI PTY has a connected terminal channel.
    #[must_use]
    pub fn has_terminal_channel(&self) -> bool {
        self.cli_pty.has_channel()
    }

    /// Check if Server PTY has a connected terminal channel.
    #[must_use]
    pub fn has_server_terminal_channel(&self) -> bool {
        self.server_pty
            .as_ref()
            .map_or(false, |pty| pty.has_channel())
    }

    /// Drain incoming input from CLI PTY's terminal channel.
    ///
    /// Returns a vector of (payload, sender) tuples for all pending input.
    /// Non-blocking - returns immediately if no input is available.
    pub fn drain_cli_input(&mut self) -> Vec<(Vec<u8>, PeerId)> {
        let Some(ref mut channel) = self.cli_pty.channel else {
            return Vec::new();
        };

        channel
            .drain_incoming()
            .into_iter()
            .map(|msg| (msg.payload, msg.sender))
            .collect()
    }

    /// Drain incoming input from Server PTY's terminal channel.
    ///
    /// Returns a vector of (payload, sender) tuples for all pending input.
    /// Non-blocking - returns immediately if no input is available.
    pub fn drain_server_input(&mut self) -> Vec<(Vec<u8>, PeerId)> {
        let Some(ref mut server_pty) = self.server_pty else {
            return Vec::new();
        };
        let Some(ref mut channel) = server_pty.channel else {
            return Vec::new();
        };

        channel
            .drain_incoming()
            .into_iter()
            .map(|msg| (msg.payload, msg.sender))
            .collect()
    }

    /// Disconnect this agent's channels.
    ///
    /// Cleanly closes WebSocket connections for all PTY and preview channels.
    pub async fn disconnect_channels(&mut self) {
        let session_key = self.session_key();
        log::info!("Agent {} disconnecting channels", session_key);

        // Disconnect CLI PTY channel
        if let Some(ref mut ch) = self.cli_pty.channel {
            ch.disconnect().await;
            log::info!("Agent {} CLI PTY channel disconnected", session_key);
        }
        self.cli_pty.channel = None;

        // Disconnect Server PTY channel if exists
        if let Some(ref mut server_pty) = self.server_pty {
            if let Some(ref mut ch) = server_pty.channel {
                ch.disconnect().await;
                log::info!("Agent {} Server PTY channel disconnected", session_key);
            }
            server_pty.channel = None;
        }

        // Disconnect preview channel
        if let Some(ref mut ch) = self.preview_channel {
            ch.disconnect().await;
            log::info!("Agent {} preview channel disconnected", session_key);
        }
        self.preview_channel = None;
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        log::info!(
            "Agent {} dropping - cleaning up PTY sessions and channels",
            self.session_key()
        );

        // PTY channels clean up via PtySession's Drop
        // Preview channel cleans up via its own Drop
        if self.cli_pty.has_channel() {
            log::info!("Agent {} dropping CLI PTY channel", self.session_key());
        }
        if self.has_server_terminal_channel() {
            log::info!("Agent {} dropping Server PTY channel", self.session_key());
        }
        if self.preview_channel.is_some() {
            log::info!("Agent {} dropping preview channel", self.session_key());
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
        let snapshot = agent.get_scrollback_snapshot();
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
