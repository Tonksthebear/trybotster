//! PTY session management for agents.
//!
//! This module provides pseudo-terminal (PTY) session handling for agent processes.
//! Each agent can have multiple PTY sessions (CLI and server) running concurrently.
//!
//! # Architecture
//!
//! ```text
//! PtySession
//! ├── master_pty: MasterPty (for resizing)
//! ├── writer: Write (for input)
//! ├── vt100_parser: Parser (terminal emulation)
//! └── buffer: VecDeque<String> (line-based history)
//! ```
//!
//! # Usage
//!
//! PTY sessions are typically created and managed by the [`Agent`](super::Agent) struct.
//! Direct usage is for advanced scenarios like custom PTY spawning.
//!
//! ```ignore
//! let mut session = PtySession::new(24, 80);
//! // Spawn a process...
//! session.write_input(b"ls -la\n")?;
//! let screen = session.get_vt100_screen();
//! ```

// Rust guideline compliant 2025-01

use anyhow::Result;
use portable_pty::{Child, MasterPty, PtySize};
use std::{
    collections::VecDeque,
    io::Write,
    sync::{mpsc::Sender, Arc, Mutex},
    thread,
};
use vt100::Parser;

use super::notification::AgentNotification;
use super::screen;

/// Maximum lines to keep in scrollback buffer.
///
/// 20K lines balances memory usage (~2-4MB per agent) with sufficient
/// history for debugging. Based on typical agent session output
/// rates of ~100 lines/minute, this provides ~3 hours of scrollback.
pub const MAX_BUFFER_LINES: usize = 20000;

/// Encapsulates all state for a single PTY session.
///
/// Each PTY session manages:
/// - A pseudo-terminal for process I/O
/// - A VT100 parser for terminal emulation
/// - A line buffer for pattern detection
/// - Notification channel for OSC sequences
///
/// # Thread Safety
///
/// The VT100 parser and buffer are wrapped in `Arc<Mutex<>>` to allow
/// concurrent reads from the PTY reader thread and writes from the main thread.
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
    pub notification_tx: Option<Sender<AgentNotification>>,
    /// Child process handle - stored so we can kill it on drop.
    child: Option<Box<dyn Child + Send>>,
}

impl PtySession {
    /// Creates a new PTY session with the specified dimensions.
    ///
    /// The VT100 parser is initialized with scrollback enabled.
    ///
    /// # Arguments
    ///
    /// * `rows` - Terminal height in rows
    /// * `cols` - Terminal width in columns
    #[must_use]
    pub fn new(rows: u16, cols: u16) -> Self {
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
    ///
    /// This is automatically called on drop, but can be called manually
    /// for explicit cleanup.
    pub fn kill_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            log::info!("Killing PTY child process");
            if let Err(e) = child.kill() {
                log::warn!("Failed to kill PTY child: {e}");
            }
            // Wait for process to exit to prevent zombies
            let _ = child.wait();
        }
    }

    /// Resize the PTY and VT100 parser to new dimensions.
    ///
    /// # Arguments
    ///
    /// * `rows` - New terminal height
    /// * `cols` - New terminal width
    pub fn resize(&self, rows: u16, cols: u16) {
        // Resize the VT100 parser
        {
            let mut parser = self.vt100_parser.lock().expect("parser lock poisoned");
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
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails or no writer is available.
    pub fn write_input(&mut self, input: &[u8]) -> Result<()> {
        if let Some(writer) = &mut self.writer {
            writer.write_all(input)?;
            writer.flush()?;
        }
        Ok(())
    }

    /// Write a string to the PTY.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn write_input_str(&mut self, input: &str) -> Result<()> {
        self.write_input(input.as_bytes())
    }

    /// Add a line to the buffer for pattern detection.
    ///
    /// Lines exceeding `MAX_BUFFER_LINES` are dropped from the front.
    pub fn add_to_buffer(&self, line: &str) {
        let mut buffer = self.buffer.lock().expect("buffer lock poisoned");
        buffer.push_back(line.to_string());
        if buffer.len() > MAX_BUFFER_LINES {
            buffer.pop_front();
        }
    }

    /// Get a snapshot of the buffer contents.
    #[must_use]
    pub fn get_buffer_snapshot(&self) -> Vec<String> {
        self.buffer
            .lock()
            .expect("buffer lock poisoned")
            .iter()
            .cloned()
            .collect()
    }

    /// Get the rendered VT100 screen as lines.
    #[must_use]
    pub fn get_vt100_screen(&self) -> Vec<String> {
        let parser = self.vt100_parser.lock().expect("parser lock poisoned");
        let s = parser.screen();
        s.rows(0, s.size().1).collect()
    }

    /// Get the screen as ANSI escape sequences for streaming.
    ///
    /// The output includes cursor positioning and attribute sequences
    /// suitable for replaying on a remote terminal.
    #[must_use]
    pub fn get_screen_as_ansi(&self) -> String {
        let parser = self.vt100_parser.lock().expect("parser lock poisoned");
        screen::render_screen_as_ansi(parser.screen())
    }

    /// Get a hash of the current screen content for change detection.
    ///
    /// The hash includes screen contents, cursor position, and scrollback offset.
    #[must_use]
    pub fn get_screen_hash(&self) -> u64 {
        let parser = self.vt100_parser.lock().expect("parser lock poisoned");
        screen::compute_screen_hash(parser.screen())
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.kill_child();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pty_session_creation_with_scrollback() {
        let session = PtySession::new(24, 80);

        let parser = session.vt100_parser.lock().unwrap();
        let s = parser.screen();
        let (rows, cols) = s.size();

        assert_eq!(rows, 24);
        assert_eq!(cols, 80);
    }

    #[test]
    fn test_pty_session_resize() {
        let session = PtySession::new(24, 80);
        session.resize(40, 120);

        let parser = session.vt100_parser.lock().unwrap();
        let s = parser.screen();
        let (rows, cols) = s.size();

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
    fn test_pty_session_buffer_limit() {
        let session = PtySession::new(24, 80);

        // Add more lines than MAX_BUFFER_LINES
        for i in 0..MAX_BUFFER_LINES + 100 {
            session.add_to_buffer(&format!("line {i}"));
        }

        let snapshot = session.get_buffer_snapshot();
        assert_eq!(snapshot.len(), MAX_BUFFER_LINES);
        assert_eq!(snapshot[0], "line 100");
    }
}
