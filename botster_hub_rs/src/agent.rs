use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};
use vt100::Parser;

const MAX_BUFFER_LINES: usize = 20000;

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
    pub issue_number: u32,
    pub worktree_path: PathBuf,
    pub start_time: chrono::DateTime<chrono::Utc>,
    pub status: AgentStatus,
    pub buffer: Arc<Mutex<VecDeque<String>>>, // Deprecated - keeping for compatibility
    pub vt100_parser: Arc<Mutex<Parser>>,     // VT100 terminal emulator
    pub scrollback_history: Arc<Mutex<Vec<String>>>, // Proper scrollback: snapshots of VT100 screen
    pub terminal_window_id: Option<String>,   // macOS Terminal window ID for focusing
    master_pty: Option<Box<dyn MasterPty + Send>>, // PTY master for resizing
    writer: Option<Box<dyn Write + Send>>,
    reader_thread: Option<thread::JoinHandle<()>>,
}

impl Agent {
    pub fn new(id: uuid::Uuid, repo: String, issue_number: u32, worktree_path: PathBuf) -> Self {
        // Create VT100 parser with 0 scrollback (vt100 0.15 has scrollback bugs)
        let parser = Parser::new(24, 80, 0);

        Self {
            id,
            repo,
            issue_number,
            worktree_path,
            start_time: chrono::Utc::now(),
            status: AgentStatus::Initializing,
            buffer: Arc::new(Mutex::new(VecDeque::new())),
            vt100_parser: Arc::new(Mutex::new(parser)),
            scrollback_history: Arc::new(Mutex::new(Vec::new())),
            terminal_window_id: None,
            master_pty: None,
            writer: None,
            reader_thread: None,
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
        self.add_to_buffer(&format!(
            "==> Spawning agent: {}#{}",
            self.repo, self.issue_number
        ));
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

        // Spawn reader thread with VT100 parser
        let buffer = Arc::clone(&self.buffer);
        let vt100_parser = Arc::clone(&self.vt100_parser);
        let scrollback_history = Arc::clone(&self.scrollback_history);

        self.reader_thread = Some(thread::spawn(move || {
            let mut buf = [0u8; 4096];

            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
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
        format!("{}-{}", self.repo.replace('/', "-"), self.issue_number)
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
            42,
            temp_dir.path().to_path_buf(),
        );

        assert_eq!(agent.repo, "test/repo");
        assert_eq!(agent.issue_number, 42);
        assert_eq!(agent.status, AgentStatus::Initializing);
    }

    #[test]
    fn test_session_key() {
        let temp_dir = TempDir::new().unwrap();
        let id = uuid::Uuid::new_v4();
        let agent = Agent::new(
            id,
            "owner/repo".to_string(),
            123,
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
            1,
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
            1,
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
            1,
            temp_dir.path().to_path_buf(),
        );

        let age = agent.age();
        assert!(age.as_secs() < 1); // Should be very recent
    }
}
