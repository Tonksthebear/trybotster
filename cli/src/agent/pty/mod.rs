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
//! └── scrollback_buffer: VecDeque<u8> (raw byte history)
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

pub mod cli;
pub mod server;

pub use cli::{spawn_cli_pty, CliSpawnResult};
pub use server::spawn_server_pty;

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

/// Resize a PTY session with screen clearing if dimensions changed.
///
/// Clears the VT100 screen before resizing to prevent content from being stuck
/// at old dimensions. This is necessary because terminal emulators don't
/// automatically reflow content.
///
/// # Arguments
///
/// * `pty` - The PTY session to resize
/// * `rows` - New terminal height
/// * `cols` - New terminal width
/// * `label` - Label for logging (e.g., "CLI" or "Server")
pub fn resize_with_clear(pty: &PtySession, rows: u16, cols: u16, label: &str) {
    let needs_clear = {
        let parser = pty.vt100_parser.lock().expect("parser lock poisoned");
        let (current_rows, current_cols) = parser.screen().size();
        current_rows != rows || current_cols != cols
    };

    if needs_clear {
        log::info!("{label} PTY resize: clearing screen and setting {cols}x{rows}");
        let mut parser = pty.vt100_parser.lock().expect("parser lock poisoned");
        // Reset attributes, clear screen, clear scrollback, move cursor home
        parser.process(b"\x1b[0m\x1b[2J\x1b[3J\x1b[H");
        parser.screen_mut().set_scrollback(0);
        parser.screen_mut().set_size(rows, cols);
    }

    pty.resize(rows, cols);
}

/// Maximum bytes to keep in scrollback buffer.
///
/// 4MB balances memory usage with sufficient history for debugging.
/// Based on typical agent session output rates, this provides
/// several hours of scrollback.
pub const MAX_SCROLLBACK_BYTES: usize = 4 * 1024 * 1024;

/// Encapsulates all state for a single PTY session.
///
/// Each PTY session manages:
/// - A pseudo-terminal for process I/O
/// - A VT100 parser for terminal emulation
/// - A raw byte scrollback buffer for history replay
/// - Notification channel for OSC sequences
/// - Raw output queue for browser streaming
///
/// # Thread Safety
///
/// The VT100 parser, scrollback buffer, and raw output queue are wrapped in `Arc<Mutex<>>`
/// to allow concurrent reads from the PTY reader thread and writes from the main thread.
pub struct PtySession {
    /// Master PTY for resizing.
    pub master_pty: Option<Box<dyn MasterPty + Send>>,
    /// Writer for sending input to the PTY.
    pub writer: Option<Box<dyn Write + Send>>,
    /// Reader thread handle.
    pub reader_thread: Option<thread::JoinHandle<()>>,
    /// VT100 terminal emulator with scrollback.
    pub vt100_parser: Arc<Mutex<Parser>>,
    /// Raw byte scrollback buffer for history replay.
    /// Stores raw PTY output so xterm.js can interpret escape sequences correctly.
    pub scrollback_buffer: Arc<Mutex<VecDeque<u8>>>,
    /// Raw output queue for streaming to browser (GUI mode).
    /// Reader thread pushes raw PTY bytes here; browser output drains it.
    pub raw_output_queue: Arc<Mutex<VecDeque<Vec<u8>>>>,
    /// Channel for sending detected notifications.
    pub notification_tx: Option<Sender<AgentNotification>>,
    /// Child process handle - stored so we can kill it on drop.
    child: Option<Box<dyn Child + Send>>,
}

impl std::fmt::Debug for PtySession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtySession")
            .field("has_master_pty", &self.master_pty.is_some())
            .field("has_writer", &self.writer.is_some())
            .field("has_reader_thread", &self.reader_thread.is_some())
            .field("has_child", &self.child.is_some())
            .finish_non_exhaustive()
    }
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
        // Use a reasonable scrollback for the vt100 parser (current screen state)
        let parser = Parser::new(rows, cols, 1000);
        Self {
            master_pty: None,
            writer: None,
            reader_thread: None,
            vt100_parser: Arc::new(Mutex::new(parser)),
            scrollback_buffer: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_SCROLLBACK_BYTES))),
            raw_output_queue: Arc::new(Mutex::new(VecDeque::new())),
            notification_tx: None,
            child: None,
        }
    }

    /// Drain all pending raw output from the queue.
    ///
    /// Returns the raw PTY bytes that have accumulated since last drain.
    /// Used by browser streaming to send raw output instead of rendered screen.
    #[must_use]
    pub fn drain_raw_output(&self) -> Vec<u8> {
        let mut queue = self.raw_output_queue.lock().expect("raw_output_queue lock poisoned");
        let mut result = Vec::new();
        while let Some(chunk) = queue.pop_front() {
            result.extend(chunk);
        }
        result
    }

    /// Check if a process has been spawned in this PTY session.
    #[must_use]
    pub fn is_spawned(&self) -> bool {
        self.master_pty.is_some()
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

    /// Add raw bytes to the scrollback buffer.
    ///
    /// Bytes exceeding `MAX_SCROLLBACK_BYTES` are dropped from the front.
    pub fn add_to_scrollback(&self, data: &[u8]) {
        let mut buffer = self.scrollback_buffer.lock().expect("scrollback_buffer lock poisoned");

        // Add new bytes
        buffer.extend(data.iter().copied());

        // Trim from front if over limit
        while buffer.len() > MAX_SCROLLBACK_BYTES {
            buffer.pop_front();
        }
    }

    /// Get a snapshot of the scrollback buffer as raw bytes.
    #[must_use]
    pub fn get_scrollback_snapshot(&self) -> Vec<u8> {
        self.scrollback_buffer
            .lock()
            .expect("scrollback_buffer lock poisoned")
            .iter()
            .copied()
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
    fn test_pty_session_scrollback() {
        let session = PtySession::new(24, 80);

        session.add_to_scrollback(b"test line 1\n");
        session.add_to_scrollback(b"test line 2\n");

        let snapshot = session.get_scrollback_snapshot();
        assert_eq!(snapshot, b"test line 1\ntest line 2\n");
    }

    #[test]
    fn test_pty_session_scrollback_limit() {
        let session = PtySession::new(24, 80);

        // Add more bytes than MAX_SCROLLBACK_BYTES
        let chunk = vec![b'x'; 1024]; // 1KB chunks
        let num_chunks = MAX_SCROLLBACK_BYTES / 1024 + 100;
        for _ in 0..num_chunks {
            session.add_to_scrollback(&chunk);
        }

        let snapshot = session.get_scrollback_snapshot();
        assert!(snapshot.len() <= MAX_SCROLLBACK_BYTES);
    }
}
