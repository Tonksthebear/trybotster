use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    path::PathBuf,
    sync::{
        mpsc::{self, Receiver},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
use vt100::Parser;

const MAX_BUFFER_LINES: usize = 20000;

/// Notification types detected from PTY output
#[derive(Clone, Debug)]
pub enum AgentNotification {
    /// OSC 9 notification with optional message
    Osc9(Option<String>),
    /// OSC 777 notification (rxvt-unicode style) with title and body
    Osc777 { title: String, body: String },
}

/// Detect terminal notifications in raw PTY output (OSC 9, OSC 777)
fn detect_notifications(data: &[u8]) -> Vec<AgentNotification> {
    let mut notifications = Vec::new();

    // Parse OSC sequences (ESC ] ... BEL or ESC ] ... ESC \)
    let mut i = 0;
    while i < data.len() {
        // Check for OSC sequence start: ESC ]
        if i + 1 < data.len() && data[i] == 0x1b && data[i + 1] == b']' {
            // Find the end of the OSC sequence (BEL or ST)
            let osc_start = i + 2;
            let mut osc_end = None;

            for j in osc_start..data.len() {
                if data[j] == 0x07 {
                    // Ends with BEL
                    osc_end = Some(j);
                    break;
                } else if j + 1 < data.len() && data[j] == 0x1b && data[j + 1] == b'\\' {
                    // Ends with ST (ESC \)
                    osc_end = Some(j);
                    break;
                }
            }

            if let Some(end) = osc_end {
                let osc_content = &data[osc_start..end];

                // Parse OSC 9: notification
                // Filter out messages that look like escape sequences (only digits/semicolons)
                if osc_content.starts_with(b"9;") {
                    let message = String::from_utf8_lossy(&osc_content[2..]).to_string();
                    // Only add if message is meaningful (not just numbers/semicolons)
                    let is_escape_sequence = message.chars().all(|c| c.is_ascii_digit() || c == ';');
                    if !message.is_empty() && !is_escape_sequence {
                        notifications.push(AgentNotification::Osc9(Some(message)));
                    }
                }
                // Parse OSC 777: notify;title;body
                else if osc_content.starts_with(b"777;notify;") {
                    let content = String::from_utf8_lossy(&osc_content[11..]).to_string();
                    let parts: Vec<&str> = content.splitn(2, ';').collect();
                    let title = parts.first().unwrap_or(&"").to_string();
                    let body = parts.get(1).unwrap_or(&"").to_string();
                    // Only add if there's meaningful content
                    if !title.is_empty() || !body.is_empty() {
                        notifications.push(AgentNotification::Osc777 { title, body });
                    }
                }

                // Skip past the OSC sequence
                i = end + 1;
                continue;
            }
        }

        i += 1;
    }

    notifications
}

#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)]
pub enum AgentStatus {
    Initializing,
    Running,
    Finished,
    Failed(String),
    Killed,
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentStatus::Initializing => write!(f, "initializing"),
            AgentStatus::Running => write!(f, "running"),
            AgentStatus::Finished => write!(f, "finished"),
            AgentStatus::Failed(e) => write!(f, "failed: {}", e),
            AgentStatus::Killed => write!(f, "killed"),
        }
    }
}

pub struct Agent {
    pub id: uuid::Uuid,
    pub repo: String,
    pub issue_number: Option<u32>,
    pub branch_name: String,
    pub worktree_path: PathBuf,
    pub start_time: chrono::DateTime<chrono::Utc>,
    pub status: AgentStatus,
    pub last_invocation_url: Option<String>, // GitHub URL where this agent was last invoked from
    pub buffer: Arc<Mutex<VecDeque<String>>>, // Deprecated - keeping for compatibility
    pub vt100_parser: Arc<Mutex<Parser>>,     // VT100 terminal emulator
    pub scrollback_history: Arc<Mutex<Vec<String>>>, // Proper scrollback: snapshots of VT100 screen
    pub terminal_window_id: Option<String>,   // macOS Terminal window ID for focusing
    master_pty: Option<Box<dyn MasterPty + Send>>, // PTY master for resizing
    writer: Option<Box<dyn Write + Send>>,
    reader_thread: Option<thread::JoinHandle<()>>,
    notification_rx: Option<Receiver<AgentNotification>>, // Receives notifications from PTY reader
}

impl Agent {
    pub fn new(
        id: uuid::Uuid,
        repo: String,
        issue_number: Option<u32>,
        branch_name: String,
        worktree_path: PathBuf,
    ) -> Self {
        // Create VT100 parser with 0 scrollback (vt100 0.15 has scrollback bugs)
        let parser = Parser::new(24, 80, 0);

        Self {
            id,
            repo,
            issue_number,
            branch_name,
            worktree_path,
            start_time: chrono::Utc::now(),
            status: AgentStatus::Initializing,
            last_invocation_url: None,
            buffer: Arc::new(Mutex::new(VecDeque::new())),
            vt100_parser: Arc::new(Mutex::new(parser)),
            scrollback_history: Arc::new(Mutex::new(Vec::new())),
            terminal_window_id: None,
            master_pty: None,
            writer: None,
            reader_thread: None,
            notification_rx: None,
        }
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        // Resize the VT100 parser
        {
            let mut parser = self.vt100_parser.lock().unwrap();
            parser.set_size(rows, cols);
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

        let pty_system = native_pty_system();

        // Get current parser size to match PTY
        let (rows, cols) = {
            let parser = self.vt100_parser.lock().unwrap();
            let screen = parser.screen();
            screen.size()
        };

        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        let pair = pty_system.openpty(size).context("Failed to open PTY")?;

        // Parse command
        let parts: Vec<&str> = command_str.split_whitespace().collect();
        let mut cmd = CommandBuilder::new(parts[0]);
        for arg in &parts[1..] {
            cmd.arg(arg);
        }
        cmd.cwd(&self.worktree_path);

        // Set environment variables
        for (key, value) in env_vars {
            cmd.env(key, value);
        }

        let _child = pair
            .slave
            .spawn_command(cmd)
            .context("Failed to spawn command")?;

        // Clone reader before moving master
        let mut reader = pair.master.try_clone_reader()?;

        // Get writer for sending input
        self.writer = Some(pair.master.take_writer()?);

        // Create notification channel for detecting terminal bells/notifications
        let (notification_tx, notification_rx) = mpsc::channel::<AgentNotification>();
        self.notification_rx = Some(notification_rx);

        // Spawn reader thread with VT100 parser
        let buffer = Arc::clone(&self.buffer);
        let vt100_parser = Arc::clone(&self.vt100_parser);
        let _scrollback_history = Arc::clone(&self.scrollback_history);

        self.reader_thread = Some(thread::spawn(move || {
            log::info!("PTY reader thread started");
            let mut buf = [0u8; 4096];
            let mut total_bytes_read: usize = 0;

            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        total_bytes_read += n;
                        // Log every 10KB of data read
                        if total_bytes_read % 10240 < n {
                            log::info!("PTY reader: {} total bytes read", total_bytes_read);
                        }

                        // Detect terminal notifications (BEL, OSC 9, OSC 777)
                        // Check for ESC ] which starts OSC sequences
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
                            log::info!("Detected {} notification(s) in PTY output", notifications.len());
                        }
                        for notification in notifications {
                            log::info!("Sending notification to channel: {:?}", notification);
                            // Send notification, ignore if receiver is gone
                            let _ = notification_tx.send(notification);
                        }

                        // Feed raw bytes to VT100 parser
                        {
                            let mut parser = vt100_parser.lock().unwrap();
                            parser.process(&buf[..n]);
                        }

                        // Keep old buffer for backwards compatibility
                        let output = String::from_utf8_lossy(&buf[..n]);
                        let mut buffer_lock = buffer.lock().unwrap();
                        for line in output.lines() {
                            buffer_lock.push_back(line.to_string());
                            if buffer_lock.len() > MAX_BUFFER_LINES {
                                buffer_lock.pop_front();
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Read error: {}", e);
                        break;
                    }
                }
            }
        }));

        // Store the master PTY for resizing
        self.master_pty = Some(pair.master);

        self.status = AgentStatus::Running;

        // Send context
        if !context.is_empty() {
            self.add_to_buffer("==> Sending context to agent...");
            self.write_input_str(&format!("{}\n", context))?;
        }

        // Send init commands from .botster_init
        if !init_commands.is_empty() {
            log::info!("Sending {} init command(s) to agent", init_commands.len());
            // Small delay to let shell start
            thread::sleep(Duration::from_millis(100));

            for cmd in init_commands {
                log::debug!("Running init command: {}", cmd);
                self.write_input_str(&format!("{}\n", cmd))?;
                // Small delay between commands
                thread::sleep(Duration::from_millis(50));
            }
        }

        Ok(())
    }

    pub fn write_input(&mut self, input: &[u8]) -> Result<()> {
        if let Some(writer) = &mut self.writer {
            writer.write_all(input)?;
            writer.flush()?;
        }
        Ok(())
    }

    pub fn write_input_str(&mut self, input: &str) -> Result<()> {
        self.write_input(input.as_bytes())
    }

    pub fn add_to_buffer(&self, line: &str) {
        let mut buffer = self.buffer.lock().unwrap();
        buffer.push_back(line.to_string());
        if buffer.len() > MAX_BUFFER_LINES {
            buffer.pop_front();
        }
    }

    pub fn age(&self) -> Duration {
        chrono::Utc::now()
            .signed_duration_since(self.start_time)
            .to_std()
            .unwrap_or_default()
    }

    pub fn session_key(&self) -> String {
        let repo_safe = self.repo.replace('/', "-");
        if let Some(issue_num) = self.issue_number {
            format!("{}-{}", repo_safe, issue_num)
        } else {
            format!("{}-{}", repo_safe, self.branch_name.replace('/', "-"))
        }
    }

    /// Poll for any pending notifications from the PTY (non-blocking)
    /// Returns all notifications that have been received since the last poll
    pub fn poll_notifications(&self) -> Vec<AgentNotification> {
        let mut notifications = Vec::new();
        if let Some(ref rx) = self.notification_rx {
            // Non-blocking receive of all pending notifications
            while let Ok(notification) = rx.try_recv() {
                notifications.push(notification);
            }
        }
        notifications
    }

    pub fn get_buffer_snapshot(&self) -> Vec<String> {
        self.buffer.lock().unwrap().iter().cloned().collect()
    }

    /// Get the rendered VT100 screen as lines (proper terminal emulation)
    pub fn get_vt100_screen(&self) -> Vec<String> {
        let parser = self.vt100_parser.lock().unwrap();
        let screen = parser.screen();

        // rows() takes (start_col, width) and returns iterator
        screen.rows(0, screen.size().1).collect()
    }

    /// Get screen with cursor position
    pub fn get_vt100_screen_with_cursor(&self) -> (Vec<String>, (u16, u16)) {
        let parser = self.vt100_parser.lock().unwrap();
        let screen = parser.screen();

        let lines: Vec<String> = screen.rows(0, screen.size().1).collect();
        let cursor = screen.cursor_position();

        (lines, cursor)
    }

    /// Get scrollback from our custom buffer
    /// The VT100 parser's built-in scrollback doesn't work for our use case,
    /// so we use the old line-based buffer for scrollback history
    pub fn get_vt100_with_scrollback(&self) -> Vec<String> {
        // Use the old buffer-based scrollback
        self.get_buffer_snapshot()
    }

    /// Get the screen as ANSI escape sequences for WebRTC streaming
    /// This produces output that can be written directly to xterm.js
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

        let mut last_fg = vt100::Color::Default;
        let mut last_bg = vt100::Color::Default;
        let mut last_bold = false;
        let mut last_italic = false;
        let mut last_underline = false;
        let mut last_inverse = false;

        for row in 0..rows {
            // Explicitly position at start of each row to avoid drift
            let _ = write!(output, "\x1b[{};1H", row + 1);

            // Reset tracking for each row
            last_fg = vt100::Color::Default;
            last_bg = vt100::Color::Default;
            last_bold = false;
            last_italic = false;
            last_underline = false;
            last_inverse = false;

            let mut col = 0u16;
            while col < cols {
                let cell = screen.cell(row, col);
                if let Some(cell) = cell {
                    let contents = cell.contents();

                    // Skip empty cells (continuation of wide chars)
                    if contents.is_empty() {
                        col += 1;
                        continue;
                    }

                    // Explicitly position for this cell to prevent drift from wide chars
                    let _ = write!(output, "\x1b[{};{}H", row + 1, col + 1);

                    let fg = cell.fgcolor();
                    let bg = cell.bgcolor();
                    let bold = cell.bold();
                    let italic = cell.italic();
                    let underline = cell.underline();
                    let inverse = cell.inverse();

                    // Only emit attribute changes when they differ
                    let attrs_changed = fg != last_fg
                        || bg != last_bg
                        || bold != last_bold
                        || italic != last_italic
                        || underline != last_underline
                        || inverse != last_inverse;

                    if attrs_changed {
                        // Reset and apply new attributes
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

                    // Write the character
                    output.push_str(&contents);
                }
                col += 1;
            }
        }

        // Reset attributes
        output.push_str("\x1b[0m");

        // Position cursor at correct location from the agent's terminal
        let cursor = screen.cursor_position();
        let _ = write!(output, "\x1b[{};{}H", cursor.0 + 1, cursor.1 + 1);

        // Show cursor again
        output.push_str("\x1b[?25h");

        output
    }

    /// Get a hash of the current screen content for change detection
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_agent_creation() {
        let temp_dir = TempDir::new().unwrap();
        let id = uuid::Uuid::new_v4();
        let agent = Agent::new(
            id,
            "test/repo".to_string(),
            Some(42),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );

        assert_eq!(agent.repo, "test/repo");
        assert_eq!(agent.issue_number, Some(42));
        assert_eq!(agent.status, AgentStatus::Initializing);
    }

    #[test]
    fn test_session_key() {
        let temp_dir = TempDir::new().unwrap();
        let id = uuid::Uuid::new_v4();
        let agent = Agent::new(
            id,
            "owner/repo".to_string(),
            Some(123),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );

        assert_eq!(agent.session_key(), "owner-repo-123");
    }

    #[test]
    fn test_add_to_buffer() {
        let temp_dir = TempDir::new().unwrap();
        let id = uuid::Uuid::new_v4();
        let agent = Agent::new(
            id,
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );

        agent.add_to_buffer("Test line 1");
        agent.add_to_buffer("Test line 2");

        let snapshot = agent.get_buffer_snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0], "Test line 1");
        assert_eq!(snapshot[1], "Test line 2");
    }

    #[test]
    fn test_buffer_limit() {
        let temp_dir = TempDir::new().unwrap();
        let id = uuid::Uuid::new_v4();
        let agent = Agent::new(
            id,
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );

        // Add more than MAX_BUFFER_LINES
        for i in 0..MAX_BUFFER_LINES + 100 {
            agent.add_to_buffer(&format!("Line {}", i));
        }

        let snapshot = agent.get_buffer_snapshot();
        assert_eq!(snapshot.len(), MAX_BUFFER_LINES);
        // First line should be line 100 (0-99 were dropped)
        assert_eq!(snapshot[0], "Line 100");
    }

    #[test]
    fn test_agent_age() {
        let temp_dir = TempDir::new().unwrap();
        let id = uuid::Uuid::new_v4();
        let agent = Agent::new(
            id,
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );

        let age = agent.age();
        assert!(age.as_secs() < 1); // Should be very recent
    }

    #[test]
    fn test_standalone_bell_ignored() {
        // Standalone BEL character is ignored (legacy - not useful for Claude Code)
        let data = b"some output\x07more output";
        let notifications = detect_notifications(data);
        assert_eq!(notifications.len(), 0, "Standalone BEL should be ignored");
    }

    #[test]
    fn test_detect_osc9_with_bel_terminator() {
        // OSC 9 with BEL terminator: ESC ] 9 ; message BEL
        let data = b"\x1b]9;Test notification\x07";
        let notifications = detect_notifications(data);
        assert_eq!(notifications.len(), 1);
        match &notifications[0] {
            AgentNotification::Osc9(Some(msg)) => assert_eq!(msg, "Test notification"),
            _ => panic!("Expected Osc9 notification"),
        }
    }

    #[test]
    fn test_detect_osc9_with_st_terminator() {
        // OSC 9 with ST terminator: ESC ] 9 ; message ESC \
        // This is what Claude Code uses: \033]9;message\033\\
        let data = b"\x1b]9;Claude notification\x1b\\";
        let notifications = detect_notifications(data);
        assert_eq!(notifications.len(), 1);
        match &notifications[0] {
            AgentNotification::Osc9(Some(msg)) => assert_eq!(msg, "Claude notification"),
            _ => panic!("Expected Osc9 notification with ST terminator"),
        }
    }

    #[test]
    fn test_detect_osc777_notification() {
        // OSC 777: ESC ] 777 ; notify ; title ; body BEL
        let data = b"\x1b]777;notify;Build Complete;All tests passed\x07";
        let notifications = detect_notifications(data);
        assert_eq!(notifications.len(), 1);
        match &notifications[0] {
            AgentNotification::Osc777 { title, body } => {
                assert_eq!(title, "Build Complete");
                assert_eq!(body, "All tests passed");
            }
            _ => panic!("Expected Osc777 notification"),
        }
    }

    #[test]
    fn test_no_false_positive_bel_in_osc() {
        // BEL inside OSC should not trigger standalone Bell notification
        let data = b"\x1b]9;message\x07";
        let notifications = detect_notifications(data);
        assert_eq!(notifications.len(), 1);
        // Should be Osc9, not Bell
        assert!(matches!(notifications[0], AgentNotification::Osc9(_)));
    }

    #[test]
    fn test_osc9_filters_escape_sequence_messages() {
        // OSC 9 with escape-sequence-like content (just numbers/semicolons) should be filtered
        let data = b"\x1b]9;4;0;\x07";
        let notifications = detect_notifications(data);
        assert_eq!(notifications.len(), 0, "Should filter escape-sequence-like messages");

        // But real messages should still work
        let data = b"\x1b]9;Real notification message\x07";
        let notifications = detect_notifications(data);
        assert_eq!(notifications.len(), 1);
        match &notifications[0] {
            AgentNotification::Osc9(Some(msg)) => assert_eq!(msg, "Real notification message"),
            _ => panic!("Expected Osc9 notification"),
        }
    }

    #[test]
    fn test_multiple_notifications() {
        // Multiple notifications in one buffer (without Bell since it's disabled)
        let data = b"\x07\x1b]9;first\x07\x07\x1b]9;second\x1b\\";
        let notifications = detect_notifications(data);
        // Should detect: Osc9("first"), Osc9("second") - no standalone Bell
        assert_eq!(notifications.len(), 2);
    }

    #[test]
    fn test_no_notifications_in_regular_output() {
        // Regular output without OSC sequences
        let data = b"Building project...\nCompilation complete.";
        let notifications = detect_notifications(data);
        assert_eq!(notifications.len(), 0);
    }
}
