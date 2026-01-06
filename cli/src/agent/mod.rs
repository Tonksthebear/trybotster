//! Agent management for the botster-hub TUI.
//!
//! This module provides the core agent types for managing PTY sessions
//! and Claude Code instances. Each agent runs in its own git worktree
//! with dedicated PTY sessions for CLI and optionally a dev server.
//!
//! # Architecture
//!
//! ```text
//! Agent
//! ├── cli_pty: PtySession (runs Claude Code)
//! └── server_pty: Option<PtySession> (runs dev server)
//! ```
//!
//! # Submodules
//!
//! - [`notification`]: Terminal notification detection (OSC 9, OSC 777)

// Rust guideline compliant 2025-01

pub mod notification;

pub use notification::{detect_notifications, AgentNotification, AgentStatus};

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    path::PathBuf,
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
use vt100::Parser;

/// Maximum lines to keep in scrollback buffer.
///
/// 20K lines balances memory usage (~2-4MB per agent) with sufficient
/// history for debugging. Based on typical Claude Code session output
/// rates of ~100 lines/minute, this provides ~3 hours of scrollback.
const MAX_BUFFER_LINES: usize = 20000;

/// Which PTY view is currently active in the TUI.
///
/// Agents can have both a CLI PTY (running Claude) and a server PTY
/// (running the dev server). This enum tracks which one is displayed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PtyView {
    /// CLI view - shows Claude Code output.
    #[default]
    Cli,
    /// Server view - shows dev server output.
    Server,
}

/// Encapsulates all state for a single PTY session.
///
/// Each PTY session manages:
/// - A pseudo-terminal for process I/O
/// - A VT100 parser for terminal emulation
/// - A line buffer for pattern detection
/// - Notification channel for OSC sequences
pub struct PtySession {
    /// Master PTY for resizing.
    pub master_pty: Option<Box<dyn MasterPty + Send>>,
    /// Writer for sending input to the PTY.
    pub writer: Option<Box<dyn Write + Send>>,
    /// Reader thread handle.
    pub reader_thread: Option<thread::JoinHandle<()>>,
    /// VT100 terminal emulator with scrollback.
    pub vt100_parser: Arc<Mutex<Parser>>,
    /// Line-based buffer for pattern detection.
    pub buffer: Arc<Mutex<VecDeque<String>>>,
    /// Channel for sending detected notifications.
    notification_tx: Option<Sender<AgentNotification>>,
    /// Child process handle - stored so we can kill it on drop.
    child: Option<Box<dyn Child + Send>>,
}

impl PtySession {
    /// Creates a new PTY session with the specified dimensions.
    ///
    /// The VT100 parser is initialized with scrollback enabled.
    pub fn new(rows: u16, cols: u16) -> Self {
        // Enable scrollback buffer (MAX_BUFFER_LINES worth of scrollback)
        let parser = Parser::new(rows, cols, MAX_BUFFER_LINES);
        Self {
            master_pty: None,
            writer: None,
            reader_thread: None,
            vt100_parser: Arc::new(Mutex::new(parser)),
            buffer: Arc::new(Mutex::new(VecDeque::new())),
            notification_tx: None,
            child: None,
        }
    }

    /// Store the child process handle (called after spawn).
    pub fn set_child(&mut self, child: Box<dyn Child + Send>) {
        self.child = Some(child);
    }

    /// Kill the child process if running.
    pub fn kill_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            log::info!("Killing PTY child process");
            if let Err(e) = child.kill() {
                log::warn!("Failed to kill PTY child: {}", e);
            }
            // Wait for process to exit to prevent zombies
            let _ = child.wait();
        }
    }

    /// Resize the PTY and VT100 parser to new dimensions.
    pub fn resize(&self, rows: u16, cols: u16) {
        // Resize the VT100 parser (in 0.16, set_size is on Screen)
        {
            let mut parser = self.vt100_parser.lock().unwrap();
            parser.screen_mut().set_size(rows, cols);
        }

        // Resize the PTY to match
        if let Some(master_pty) = &self.master_pty {
            let _ = master_pty.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    /// Write input bytes to the PTY.
    pub fn write_input(&mut self, input: &[u8]) -> Result<()> {
        if let Some(writer) = &mut self.writer {
            writer.write_all(input)?;
            writer.flush()?;
        }
        Ok(())
    }

    /// Write a string to the PTY.
    pub fn write_input_str(&mut self, input: &str) -> Result<()> {
        self.write_input(input.as_bytes())
    }

    /// Add a line to the buffer for pattern detection.
    pub fn add_to_buffer(&self, line: &str) {
        let mut buffer = self.buffer.lock().unwrap();
        buffer.push_back(line.to_string());
        if buffer.len() > MAX_BUFFER_LINES {
            buffer.pop_front();
        }
    }

    /// Get a snapshot of the buffer contents.
    pub fn get_buffer_snapshot(&self) -> Vec<String> {
        self.buffer.lock().unwrap().iter().cloned().collect()
    }

    /// Get the rendered VT100 screen as lines.
    pub fn get_vt100_screen(&self) -> Vec<String> {
        let parser = self.vt100_parser.lock().unwrap();
        let screen = parser.screen();
        screen.rows(0, screen.size().1).collect()
    }

    /// Get the screen as ANSI escape sequences for WebRTC streaming.
    pub fn get_screen_as_ansi(&self) -> String {
        use std::fmt::Write;

        let parser = self.vt100_parser.lock().unwrap();
        let screen = parser.screen();
        let (rows, cols) = screen.size();

        let mut output = String::new();

        // Hide cursor during update to prevent flicker
        output.push_str("\x1b[?25l");

        // Reset attributes, clear screen and scrollback, move to home
        output.push_str("\x1b[0m\x1b[2J\x1b[3J\x1b[H");

        for row in 0..rows {
            let _ = write!(output, "\x1b[{};1H", row + 1);

            let mut last_fg = vt100::Color::Default;
            let mut last_bg = vt100::Color::Default;
            let mut last_bold = false;
            let mut last_italic = false;
            let mut last_underline = false;
            let mut last_inverse = false;

            let mut col = 0u16;
            while col < cols {
                let cell = screen.cell(row, col);
                if let Some(cell) = cell {
                    let contents = cell.contents();

                    if contents.is_empty() {
                        col += 1;
                        continue;
                    }

                    let _ = write!(output, "\x1b[{};{}H", row + 1, col + 1);

                    let fg = cell.fgcolor();
                    let bg = cell.bgcolor();
                    let bold = cell.bold();
                    let italic = cell.italic();
                    let underline = cell.underline();
                    let inverse = cell.inverse();

                    let attrs_changed = fg != last_fg
                        || bg != last_bg
                        || bold != last_bold
                        || italic != last_italic
                        || underline != last_underline
                        || inverse != last_inverse;

                    if attrs_changed {
                        output.push_str("\x1b[0m");

                        match fg {
                            vt100::Color::Default => {}
                            vt100::Color::Idx(i) => {
                                let _ = write!(output, "\x1b[38;5;{}m", i);
                            }
                            vt100::Color::Rgb(r, g, b) => {
                                let _ = write!(output, "\x1b[38;2;{};{};{}m", r, g, b);
                            }
                        }

                        match bg {
                            vt100::Color::Default => {}
                            vt100::Color::Idx(i) => {
                                let _ = write!(output, "\x1b[48;5;{}m", i);
                            }
                            vt100::Color::Rgb(r, g, b) => {
                                let _ = write!(output, "\x1b[48;2;{};{};{}m", r, g, b);
                            }
                        }

                        if bold {
                            output.push_str("\x1b[1m");
                        }
                        if italic {
                            output.push_str("\x1b[3m");
                        }
                        if underline {
                            output.push_str("\x1b[4m");
                        }
                        if inverse {
                            output.push_str("\x1b[7m");
                        }

                        last_fg = fg;
                        last_bg = bg;
                        last_bold = bold;
                        last_italic = italic;
                        last_underline = underline;
                        last_inverse = inverse;
                    }

                    output.push_str(&contents);
                }
                col += 1;
            }
        }

        output.push_str("\x1b[0m");

        let cursor = screen.cursor_position();
        let _ = write!(output, "\x1b[{};{}H", cursor.0 + 1, cursor.1 + 1);

        output.push_str("\x1b[?25h");

        output
    }

    /// Get a hash of the current screen content for change detection.
    pub fn get_screen_hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let parser = self.vt100_parser.lock().unwrap();
        let screen = parser.screen();

        let mut hasher = DefaultHasher::new();
        screen.contents().hash(&mut hasher);
        screen.cursor_position().hash(&mut hasher);
        hasher.finish()
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.kill_child();
    }
}

/// A Claude Code agent running in a git worktree.
///
/// Each agent has:
/// - A unique ID and session key
/// - A CLI PTY running Claude Code
/// - An optional server PTY for the dev server
/// - Notification channel for terminal events
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

    /// Primary PTY (CLI - runs Claude Code).
    pub cli_pty: Option<PtySession>,

    /// Secondary PTY (Server - runs dev server).
    pub server_pty: Option<PtySession>,

    /// Which PTY is currently displayed.
    pub active_pty: PtyView,

    /// Backward compatibility fields (delegate to cli_pty).
    pub buffer: Arc<Mutex<VecDeque<String>>>,
    pub vt100_parser: Arc<Mutex<Parser>>,
    pub scrollback_history: Arc<Mutex<Vec<String>>>,

    notification_rx: Option<Receiver<AgentNotification>>,
}

impl Agent {
    /// Creates a new agent for the specified repository and worktree.
    pub fn new(
        id: uuid::Uuid,
        repo: String,
        issue_number: Option<u32>,
        branch_name: String,
        worktree_path: PathBuf,
    ) -> Self {
        // Create VT100 parser with scrollback enabled
        let parser = Parser::new(24, 80, MAX_BUFFER_LINES);

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
            cli_pty: None,
            server_pty: None,
            active_pty: PtyView::Cli,
            buffer: Arc::new(Mutex::new(VecDeque::new())),
            vt100_parser: Arc::new(Mutex::new(parser)),
            scrollback_history: Arc::new(Mutex::new(Vec::new())),
            notification_rx: None,
        }
    }

    /// Check if we're in scrollback mode (scrolled up from live view).
    pub fn is_scrolled(&self) -> bool {
        let parser = self.get_active_parser();
        let p = parser.lock().unwrap();
        p.screen().scrollback() > 0
    }

    /// Get current scroll offset from vt100.
    pub fn get_scroll_offset(&self) -> usize {
        let parser = self.get_active_parser();
        let p = parser.lock().unwrap();
        p.screen().scrollback()
    }

    /// Scroll up by the specified number of lines.
    pub fn scroll_up(&mut self, lines: usize) {
        let parser = self.get_active_parser();
        let mut p = parser.lock().unwrap();
        let current = p.screen().scrollback();
        p.screen_mut().set_scrollback(current.saturating_add(lines));
    }

    /// Scroll down by the specified number of lines.
    pub fn scroll_down(&mut self, lines: usize) {
        let parser = self.get_active_parser();
        let mut p = parser.lock().unwrap();
        let current = p.screen().scrollback();
        p.screen_mut().set_scrollback(current.saturating_sub(lines));
    }

    /// Scroll to the bottom (return to live view).
    pub fn scroll_to_bottom(&mut self) {
        let parser = self.get_active_parser();
        let mut p = parser.lock().unwrap();
        p.screen_mut().set_scrollback(0);
    }

    /// Scroll to the top of the scrollback buffer.
    pub fn scroll_to_top(&mut self) {
        let parser = self.get_active_parser();
        let mut p = parser.lock().unwrap();
        p.screen_mut().set_scrollback(usize::MAX);
    }

    /// Get the buffer for the currently active PTY.
    pub fn get_active_buffer(&self) -> Arc<Mutex<VecDeque<String>>> {
        match self.active_pty {
            PtyView::Cli => {
                if let Some(cli_pty) = &self.cli_pty {
                    Arc::clone(&cli_pty.buffer)
                } else {
                    Arc::clone(&self.buffer)
                }
            }
            PtyView::Server => {
                if let Some(server_pty) = &self.server_pty {
                    Arc::clone(&server_pty.buffer)
                } else if let Some(cli_pty) = &self.cli_pty {
                    Arc::clone(&cli_pty.buffer)
                } else {
                    Arc::clone(&self.buffer)
                }
            }
        }
    }

    /// Resize all PTY sessions to new dimensions.
    ///
    /// This clears the vt100 parser screens to ensure content renders at the new
    /// dimensions. Old content would otherwise be stuck at the old column width.
    pub fn resize(&self, rows: u16, cols: u16) {
        // Resize the fallback parser
        {
            let mut parser = self.vt100_parser.lock().unwrap();
            parser.screen_mut().set_size(rows, cols);
        }

        // Resize CLI PTY and clear its parser
        if let Some(cli_pty) = &self.cli_pty {
            let needs_clear = {
                let parser = cli_pty.vt100_parser.lock().unwrap();
                let (current_rows, current_cols) = parser.screen().size();
                current_rows != rows || current_cols != cols
            };

            if needs_clear {
                log::info!(
                    "CLI PTY resize: clearing screen and setting {}x{}",
                    cols, rows
                );
                let mut parser = cli_pty.vt100_parser.lock().unwrap();
                // Clear screen and scrollback, then set new size
                // Also reset scroll offset to 0 since scrollback is cleared
                parser.process(b"\x1b[0m\x1b[2J\x1b[3J\x1b[H");
                parser.screen_mut().set_scrollback(0);
                parser.screen_mut().set_size(rows, cols);
            }
            cli_pty.resize(rows, cols);
        }

        // Resize Server PTY and clear its parser
        if let Some(server_pty) = &self.server_pty {
            let needs_clear = {
                let parser = server_pty.vt100_parser.lock().unwrap();
                let (current_rows, current_cols) = parser.screen().size();
                current_rows != rows || current_cols != cols
            };

            if needs_clear {
                log::info!(
                    "Server PTY resize: clearing screen and setting {}x{}",
                    cols, rows
                );
                let mut parser = server_pty.vt100_parser.lock().unwrap();
                // Clear screen and scrollback, then set new size
                // Also reset scroll offset to 0 since scrollback is cleared
                parser.process(b"\x1b[0m\x1b[2J\x1b[3J\x1b[H");
                parser.screen_mut().set_scrollback(0);
                parser.screen_mut().set_size(rows, cols);
            }
            server_pty.resize(rows, cols);
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
    pub fn get_active_parser(&self) -> Arc<Mutex<Parser>> {
        match self.active_pty {
            PtyView::Cli => {
                if let Some(cli_pty) = &self.cli_pty {
                    Arc::clone(&cli_pty.vt100_parser)
                } else {
                    Arc::clone(&self.vt100_parser)
                }
            }
            PtyView::Server => {
                if let Some(server_pty) = &self.server_pty {
                    Arc::clone(&server_pty.vt100_parser)
                } else if let Some(cli_pty) = &self.cli_pty {
                    Arc::clone(&cli_pty.vt100_parser)
                } else {
                    Arc::clone(&self.vt100_parser)
                }
            }
        }
    }

    /// Write input to the currently active PTY.
    pub fn write_to_active_pty(&mut self, input: &[u8]) -> Result<()> {
        match self.active_pty {
            PtyView::Cli => self.write_input(input),
            PtyView::Server => {
                if let Some(server_pty) = &mut self.server_pty {
                    server_pty.write_input(input)
                } else {
                    self.write_input(input)
                }
            }
        }
    }

    /// Check if server PTY is available.
    pub fn has_server_pty(&self) -> bool {
        self.server_pty.is_some()
    }

    /// Check if the dev server is running.
    pub fn is_server_running(&self) -> bool {
        if let Some(port) = self.tunnel_port {
            use std::net::TcpStream;
            use std::time::Duration;

            TcpStream::connect_timeout(
                &format!("127.0.0.1:{}", port).parse().unwrap(),
                Duration::from_millis(50),
            )
            .is_ok()
        } else {
            false
        }
    }

    /// Spawn the CLI PTY with the given command and environment.
    pub fn spawn(
        &mut self,
        command_str: &str,
        context: &str,
        init_commands: Vec<String>,
        env_vars: HashMap<String, String>,
    ) -> Result<()> {
        let agent_label = if let Some(issue_num) = self.issue_number {
            format!("{}#{}", self.repo, issue_num)
        } else {
            format!("{}/{}", self.repo, self.branch_name)
        };
        self.add_to_buffer(&format!("==> Spawning agent: {}", agent_label));
        self.add_to_buffer(&format!("==> Command: {}", command_str));
        self.add_to_buffer(&format!("==> Worktree: {}", self.worktree_path.display()));
        self.add_to_buffer("");

        let (rows, cols) = {
            let parser = self.vt100_parser.lock().unwrap();
            let screen = parser.screen();
            screen.size()
        };

        let mut cli_pty = PtySession::new(rows, cols);

        let pty_system = native_pty_system();
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        let pair = pty_system.openpty(size).context("Failed to open PTY")?;

        let parts: Vec<&str> = command_str.split_whitespace().collect();
        let mut cmd = CommandBuilder::new(parts[0]);
        for arg in &parts[1..] {
            cmd.arg(arg);
        }
        cmd.cwd(&self.worktree_path);

        for (key, value) in env_vars {
            cmd.env(key, value);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("Failed to spawn command")?;

        cli_pty.set_child(child);

        let mut reader = pair.master.try_clone_reader()?;

        cli_pty.writer = Some(pair.master.take_writer()?);

        let (notification_tx, notification_rx) = mpsc::channel::<AgentNotification>();
        self.notification_rx = Some(notification_rx);
        cli_pty.notification_tx = Some(notification_tx.clone());

        let buffer = Arc::clone(&self.buffer);
        let pty_buffer = Arc::clone(&cli_pty.buffer);
        let vt100_parser = Arc::clone(&self.vt100_parser);
        let pty_parser = Arc::clone(&cli_pty.vt100_parser);

        cli_pty.reader_thread = Some(thread::spawn(move || {
            log::info!("CLI PTY reader thread started");
            let mut buf = [0u8; 4096];
            let mut total_bytes_read: usize = 0;

            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        total_bytes_read += n;
                        if total_bytes_read % 10240 < n {
                            log::info!("CLI PTY reader: {} total bytes read", total_bytes_read);
                        }

                        let has_esc_bracket = buf[..n].windows(2).any(|w| w == [0x1b, b']']);
                        let has_bel = buf[..n].contains(&0x07);
                        if has_esc_bracket || has_bel {
                            log::info!(
                                "PTY output contains potential notification markers: ESC]={}, BEL={}. First 100 bytes: {:?}",
                                has_esc_bracket,
                                has_bel,
                                &buf[..n.min(100)]
                            );
                        }

                        let notifications = detect_notifications(&buf[..n]);
                        if !notifications.is_empty() {
                            log::info!(
                                "Detected {} notification(s) in PTY output",
                                notifications.len()
                            );
                        }
                        for notification in notifications {
                            log::info!("Sending notification to channel: {:?}", notification);
                            let _ = notification_tx.send(notification);
                        }

                        {
                            let mut parser = vt100_parser.lock().unwrap();
                            parser.process(&buf[..n]);
                        }
                        {
                            let mut parser = pty_parser.lock().unwrap();
                            parser.process(&buf[..n]);
                        }

                        let output = String::from_utf8_lossy(&buf[..n]);
                        {
                            let mut buffer_lock = buffer.lock().unwrap();
                            for line in output.lines() {
                                buffer_lock.push_back(line.to_string());
                                if buffer_lock.len() > MAX_BUFFER_LINES {
                                    buffer_lock.pop_front();
                                }
                            }
                        }
                        {
                            let mut pty_buffer_lock = pty_buffer.lock().unwrap();
                            for line in output.lines() {
                                pty_buffer_lock.push_back(line.to_string());
                                if pty_buffer_lock.len() > MAX_BUFFER_LINES {
                                    pty_buffer_lock.pop_front();
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("CLI PTY read error: {}", e);
                        break;
                    }
                }
            }
            log::info!("CLI PTY reader thread exiting");
        }));

        cli_pty.master_pty = Some(pair.master);

        self.cli_pty = Some(cli_pty);

        self.status = AgentStatus::Running;

        if !context.is_empty() {
            self.add_to_buffer("==> Sending context to agent...");
            self.write_input_str(&format!("{}\n", context))?;
        }

        if !init_commands.is_empty() {
            log::info!("Sending {} init command(s) to agent", init_commands.len());
            thread::sleep(Duration::from_millis(100));

            for cmd in init_commands {
                log::debug!("Running init command: {}", cmd);
                self.write_input_str(&format!("{}\n", cmd))?;
                thread::sleep(Duration::from_millis(50));
            }
        }

        Ok(())
    }

    /// Spawn a server PTY to run the dev server.
    pub fn spawn_server_pty(
        &mut self,
        init_script: &str,
        env_vars: HashMap<String, String>,
    ) -> Result<()> {
        log::info!("Spawning server PTY with init script: {}", init_script);

        let (rows, cols) = {
            let parser = self.vt100_parser.lock().unwrap();
            let screen = parser.screen();
            screen.size()
        };

        let mut server_pty = PtySession::new(rows, cols);

        let pty_system = native_pty_system();
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        let pair = pty_system
            .openpty(size)
            .context("Failed to open server PTY")?;

        let mut cmd = CommandBuilder::new("bash");
        cmd.cwd(&self.worktree_path);

        for (key, value) in env_vars {
            cmd.env(key, value);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("Failed to spawn server shell")?;

        server_pty.set_child(child);

        let mut reader = pair.master.try_clone_reader()?;

        server_pty.writer = Some(pair.master.take_writer()?);

        let pty_buffer = Arc::clone(&server_pty.buffer);
        let pty_parser = Arc::clone(&server_pty.vt100_parser);

        server_pty.reader_thread = Some(thread::spawn(move || {
            log::info!("Server PTY reader thread started");
            let mut buf = [0u8; 4096];
            let mut total_bytes_read: usize = 0;

            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        total_bytes_read += n;
                        if total_bytes_read % 10240 < n {
                            log::info!("Server PTY reader: {} total bytes read", total_bytes_read);
                        }

                        {
                            let mut parser = pty_parser.lock().unwrap();
                            parser.process(&buf[..n]);
                        }

                        let output = String::from_utf8_lossy(&buf[..n]);
                        {
                            let mut buffer_lock = pty_buffer.lock().unwrap();
                            for line in output.lines() {
                                buffer_lock.push_back(line.to_string());
                                if buffer_lock.len() > MAX_BUFFER_LINES {
                                    buffer_lock.pop_front();
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Server PTY read error: {}", e);
                        break;
                    }
                }
            }
            log::info!("Server PTY reader thread exiting");
        }));

        server_pty.master_pty = Some(pair.master);

        self.server_pty = Some(server_pty);

        thread::sleep(Duration::from_millis(100));
        if let Some(ref mut server_pty) = self.server_pty {
            if let Some(ref mut writer) = server_pty.writer {
                log::info!(
                    "Sending init command to server PTY: source {}",
                    init_script
                );
                writer.write_all(format!("source {}\n", init_script).as_bytes())?;
                writer.flush()?;
            }
        }

        log::info!("Server PTY spawned successfully");
        Ok(())
    }

    /// Write input to the currently active PTY (CLI or Server based on active_pty).
    pub fn write_input(&mut self, input: &[u8]) -> Result<()> {
        match self.active_pty {
            PtyView::Cli => {
                if let Some(cli_pty) = &mut self.cli_pty {
                    cli_pty.write_input(input)?;
                }
            }
            PtyView::Server => {
                if let Some(server_pty) = &mut self.server_pty {
                    server_pty.write_input(input)?;
                } else {
                    // Fall back to CLI if no server PTY
                    if let Some(cli_pty) = &mut self.cli_pty {
                        cli_pty.write_input(input)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Write a string to the currently active PTY.
    pub fn write_input_str(&mut self, input: &str) -> Result<()> {
        self.write_input(input.as_bytes())
    }

    /// Write input specifically to the CLI PTY (for notifications, etc.).
    pub fn write_input_to_cli(&mut self, input: &[u8]) -> Result<()> {
        if let Some(cli_pty) = &mut self.cli_pty {
            cli_pty.write_input(input)?;
        }
        Ok(())
    }

    /// Add a line to the buffer.
    pub fn add_to_buffer(&self, line: &str) {
        let mut buffer = self.buffer.lock().unwrap();
        buffer.push_back(line.to_string());
        if buffer.len() > MAX_BUFFER_LINES {
            buffer.pop_front();
        }
    }

    /// Get how long this agent has been running.
    pub fn age(&self) -> Duration {
        chrono::Utc::now()
            .signed_duration_since(self.start_time)
            .to_std()
            .unwrap_or_default()
    }

    /// Generate a unique session key for this agent.
    pub fn session_key(&self) -> String {
        let repo_safe = self.repo.replace('/', "-");
        if let Some(issue_num) = self.issue_number {
            format!("{}-{}", repo_safe, issue_num)
        } else {
            format!("{}-{}", repo_safe, self.branch_name.replace('/', "-"))
        }
    }

    /// Poll for any pending notifications from the PTY (non-blocking).
    pub fn poll_notifications(&self) -> Vec<AgentNotification> {
        let mut notifications = Vec::new();
        if let Some(ref rx) = self.notification_rx {
            while let Ok(notification) = rx.try_recv() {
                notifications.push(notification);
            }
        }
        notifications
    }

    /// Get a snapshot of the buffer contents.
    pub fn get_buffer_snapshot(&self) -> Vec<String> {
        self.buffer.lock().unwrap().iter().cloned().collect()
    }

    /// Get the rendered VT100 screen as lines.
    pub fn get_vt100_screen(&self) -> Vec<String> {
        let parser = self.vt100_parser.lock().unwrap();
        let screen = parser.screen();
        screen.rows(0, screen.size().1).collect()
    }

    /// Get screen with cursor position.
    pub fn get_vt100_screen_with_cursor(&self) -> (Vec<String>, (u16, u16)) {
        let parser = self.vt100_parser.lock().unwrap();
        let screen = parser.screen();

        let lines: Vec<String> = screen.rows(0, screen.size().1).collect();
        let cursor = screen.cursor_position();

        (lines, cursor)
    }

    /// Get scrollback from the line buffer.
    pub fn get_vt100_with_scrollback(&self) -> Vec<String> {
        self.get_buffer_snapshot()
    }

    /// Get the screen as ANSI escape sequences for WebRTC streaming.
    pub fn get_screen_as_ansi(&self) -> String {
        use std::fmt::Write;

        let active_parser = self.get_active_parser();
        let parser = active_parser.lock().unwrap();
        let screen = parser.screen();
        let (rows, cols) = screen.size();

        let mut output = String::new();

        output.push_str("\x1b[?25l");
        output.push_str("\x1b[0m\x1b[2J\x1b[3J\x1b[H");

        for row in 0..rows {
            let _ = write!(output, "\x1b[{};1H", row + 1);

            let mut last_fg = vt100::Color::Default;
            let mut last_bg = vt100::Color::Default;
            let mut last_bold = false;
            let mut last_italic = false;
            let mut last_underline = false;
            let mut last_inverse = false;

            let mut col = 0u16;
            while col < cols {
                let cell = screen.cell(row, col);
                if let Some(cell) = cell {
                    let contents = cell.contents();

                    if contents.is_empty() {
                        col += 1;
                        continue;
                    }

                    let _ = write!(output, "\x1b[{};{}H", row + 1, col + 1);

                    let fg = cell.fgcolor();
                    let bg = cell.bgcolor();
                    let bold = cell.bold();
                    let italic = cell.italic();
                    let underline = cell.underline();
                    let inverse = cell.inverse();

                    let attrs_changed = fg != last_fg
                        || bg != last_bg
                        || bold != last_bold
                        || italic != last_italic
                        || underline != last_underline
                        || inverse != last_inverse;

                    if attrs_changed {
                        output.push_str("\x1b[0m");

                        match fg {
                            vt100::Color::Default => {}
                            vt100::Color::Idx(i) => {
                                let _ = write!(output, "\x1b[38;5;{}m", i);
                            }
                            vt100::Color::Rgb(r, g, b) => {
                                let _ = write!(output, "\x1b[38;2;{};{};{}m", r, g, b);
                            }
                        }

                        match bg {
                            vt100::Color::Default => {}
                            vt100::Color::Idx(i) => {
                                let _ = write!(output, "\x1b[48;5;{}m", i);
                            }
                            vt100::Color::Rgb(r, g, b) => {
                                let _ = write!(output, "\x1b[48;2;{};{};{}m", r, g, b);
                            }
                        }

                        if bold {
                            output.push_str("\x1b[1m");
                        }
                        if italic {
                            output.push_str("\x1b[3m");
                        }
                        if underline {
                            output.push_str("\x1b[4m");
                        }
                        if inverse {
                            output.push_str("\x1b[7m");
                        }

                        last_fg = fg;
                        last_bg = bg;
                        last_bold = bold;
                        last_italic = italic;
                        last_underline = underline;
                        last_inverse = inverse;
                    }

                    output.push_str(&contents);
                }
                col += 1;
            }
        }

        output.push_str("\x1b[0m");

        let cursor = screen.cursor_position();
        let _ = write!(output, "\x1b[{};{}H", cursor.0 + 1, cursor.1 + 1);

        output.push_str("\x1b[?25h");

        output
    }

    /// Get a hash of the current screen content for change detection.
    pub fn get_screen_hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let active_parser = self.get_active_parser();
        let parser = active_parser.lock().unwrap();
        let screen = parser.screen();

        let mut hasher = DefaultHasher::new();
        screen.contents().hash(&mut hasher);
        screen.cursor_position().hash(&mut hasher);
        screen.scrollback().hash(&mut hasher);
        hasher.finish()
    }

    /// Get the current screen dimensions for debugging.
    pub fn get_screen_info(&self) -> ScreenInfo {
        let active_parser = self.get_active_parser();
        let parser = active_parser.lock().unwrap();
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        ScreenInfo { rows, cols }
    }
}

/// Screen dimension info for debugging.
pub struct ScreenInfo {
    pub rows: u16,
    pub cols: u16,
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

        if let Some(ref mut cli_pty) = self.cli_pty {
            log::info!("Killing CLI PTY child process");
            cli_pty.kill_child();
        }
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

        // Add more lines than MAX_BUFFER_LINES
        for i in 0..MAX_BUFFER_LINES + 100 {
            agent.add_to_buffer(&format!("line {}", i));
        }

        let snapshot = agent.get_buffer_snapshot();
        assert_eq!(snapshot.len(), MAX_BUFFER_LINES);
        // First line should be offset by 100
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

        // Simulate some output to create scrollback
        {
            let mut parser = agent.vt100_parser.lock().unwrap();
            for i in 0..50 {
                parser.process(format!("Line {}\r\n", i).as_bytes());
            }
        }

        // Now scroll up
        agent.scroll_up(10);
        assert!(agent.is_scrolled());
        assert_eq!(agent.get_scroll_offset(), 10);

        // Scroll down
        agent.scroll_down(5);
        assert_eq!(agent.get_scroll_offset(), 5);

        // Scroll to bottom
        agent.scroll_to_bottom();
        assert!(!agent.is_scrolled());
        assert_eq!(agent.get_scroll_offset(), 0);
    }

    #[test]
    fn test_scroll_down_does_not_go_negative() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        // Start at 0
        assert_eq!(agent.get_scroll_offset(), 0);

        // Scroll down when already at 0 should stay at 0
        agent.scroll_down(10);
        assert_eq!(agent.get_scroll_offset(), 0);
        assert!(!agent.is_scrolled());
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

        // Default should be CLI
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
    fn test_pty_session_creation_with_scrollback() {
        let session = PtySession::new(24, 80);

        let parser = session.vt100_parser.lock().unwrap();
        let screen = parser.screen();
        let (rows, cols) = screen.size();

        assert_eq!(rows, 24);
        assert_eq!(cols, 80);
    }

    #[test]
    fn test_pty_session_resize() {
        let session = PtySession::new(24, 80);
        session.resize(40, 120);

        let parser = session.vt100_parser.lock().unwrap();
        let screen = parser.screen();
        let (rows, cols) = screen.size();

        assert_eq!(rows, 40);
        assert_eq!(cols, 120);
    }

    #[test]
    fn test_pty_session_buffer() {
        let session = PtySession::new(24, 80);

        session.add_to_buffer("test line 1");
        session.add_to_buffer("test line 2");

        let snapshot = session.get_buffer_snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0], "test line 1");
    }

    #[test]
    fn test_get_active_parser_cli() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        agent.cli_pty = Some(PtySession::new(30, 100));

        {
            let cli_parser = agent.cli_pty.as_ref().unwrap().vt100_parser.lock().unwrap();
            cli_parser.screen().size();
        }

        let active = agent.get_active_parser();
        let parser = active.lock().unwrap();
        let (rows, cols) = parser.screen().size();

        assert_eq!(rows, 30);
        assert_eq!(cols, 100);
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

        agent.cli_pty = Some(PtySession::new(24, 80));
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
            let mut parser = agent.vt100_parser.lock().unwrap();
            for i in 0..100 {
                parser.process(format!("Line {}\r\n", i).as_bytes());
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
            let mut parser = agent.vt100_parser.lock().unwrap();
            for i in 0..100 {
                parser.process(format!("Line {}\r\n", i).as_bytes());
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
            let mut parser = agent.vt100_parser.lock().unwrap();
            for i in 0..50 {
                parser.process(format!("Line {}\r\n", i).as_bytes());
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

        agent.cli_pty = Some(PtySession::new(24, 80));
        agent.server_pty = Some(PtySession::new(24, 80));

        {
            let cli_pty = agent.cli_pty.as_ref().unwrap();
            let mut parser = cli_pty.vt100_parser.lock().unwrap();
            for i in 0..50 {
                parser.process(format!("CLI Line {}\r\n", i).as_bytes());
            }
        }

        {
            let server_pty = agent.server_pty.as_ref().unwrap();
            let mut parser = server_pty.vt100_parser.lock().unwrap();
            for i in 0..30 {
                parser.process(format!("Server Line {}\r\n", i).as_bytes());
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
            let mut parser = agent.vt100_parser.lock().unwrap();
            // Need more than screen height (24) to create scrollback
            for i in 0..50 {
                parser.process(format!("Line {}\r\n", i).as_bytes());
            }
        }

        for _ in 0..100 {
            agent.scroll_up(5);
        }

        // Should be scrolled up (clamped to max scrollback)
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

        agent.cli_pty = Some(PtySession::new(24, 80));
        agent.server_pty = Some(PtySession::new(24, 80));

        {
            let cli_pty = agent.cli_pty.as_ref().unwrap();
            let mut parser = cli_pty.vt100_parser.lock().unwrap();
            for i in 0..50 {
                parser.process(format!("Line {}\r\n", i).as_bytes());
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

        agent.cli_pty = Some(PtySession::new(24, 80));
        agent.server_pty = Some(PtySession::new(24, 80));

        {
            let cli_pty = agent.cli_pty.as_ref().unwrap();
            let mut parser = cli_pty.vt100_parser.lock().unwrap();
            parser.process(b"CLI CONTENT HERE\r\n");
        }

        {
            let server_pty = agent.server_pty.as_ref().unwrap();
            let mut parser = server_pty.vt100_parser.lock().unwrap();
            parser.process(b"SERVER CONTENT HERE\r\n");
        }

        // Simple ANSI stripping: remove escape sequences
        fn extract_text(ansi: &str) -> String {
            let mut result = String::new();
            let mut chars = ansi.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\x1b' {
                    // Skip until we hit a letter (end of escape sequence)
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
            let mut parser = agent.vt100_parser.lock().unwrap();
            for i in 0..50 {
                parser.process(format!("Line {}\r\n", i).as_bytes());
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

        agent.cli_pty = Some(PtySession::new(24, 80));
        agent.server_pty = Some(PtySession::new(24, 80));

        {
            let cli_pty = agent.cli_pty.as_ref().unwrap();
            let mut parser = cli_pty.vt100_parser.lock().unwrap();
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

    #[test]
    fn test_webrtc_scroll_command_flow() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        {
            let mut parser = agent.vt100_parser.lock().unwrap();
            for i in 0..100 {
                parser.process(format!("Line {}\r\n", i).as_bytes());
            }
        }

        let initial_hash = agent.get_screen_hash();

        agent.scroll_up(20);

        let scrolled_hash = agent.get_screen_hash();
        assert_ne!(initial_hash, scrolled_hash);

        agent.scroll_to_bottom();

        let bottom_hash = agent.get_screen_hash();
        assert_eq!(initial_hash, bottom_hash);
    }

    #[test]
    fn test_webrtc_toggle_command_flow() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "issue-1".to_string(),
            temp_dir.path().to_path_buf(),
        );

        agent.cli_pty = Some(PtySession::new(24, 80));
        agent.server_pty = Some(PtySession::new(24, 80));

        // Simple ANSI stripping: remove escape sequences
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

        {
            let cli_pty = agent.cli_pty.as_ref().unwrap();
            let mut parser = cli_pty.vt100_parser.lock().unwrap();
            parser.process(b"CLI OUTPUT\r\n");
        }

        {
            let server_pty = agent.server_pty.as_ref().unwrap();
            let mut parser = server_pty.vt100_parser.lock().unwrap();
            parser.process(b"SERVER OUTPUT\r\n");
        }

        assert_eq!(agent.active_pty, PtyView::Cli);
        let cli_ansi = agent.get_screen_as_ansi();
        assert!(extract_text(&cli_ansi).contains("CLI OUTPUT"));

        agent.toggle_pty_view();

        assert_eq!(agent.active_pty, PtyView::Server);
        let server_ansi = agent.get_screen_as_ansi();
        assert!(extract_text(&server_ansi).contains("SERVER OUTPUT"));
    }
}
