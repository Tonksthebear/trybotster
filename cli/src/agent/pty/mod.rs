//! PTY session management with integrated channel ownership.
//!
//! This module provides pseudo-terminal (PTY) session handling where each PTY
//! owns its communication channel. This architecture ensures channel lifecycle
//! is tied to PTY lifecycle - when a PTY dies, its channel dies with it.
//!
//! # Architecture
//!
//! ```text
//! Agent (container)
//!  └── cli_pty: PtySession
//!  └── server_pty: Option<PtySession>
//!
//! PtySession (owns I/O and channel)
//!  ├── master_pty: MasterPty (for resizing)
//!  ├── writer: Write (for input)
//!  ├── vt100_parser: Parser (terminal emulation)
//!  ├── scrollback_buffer: VecDeque<u8> (raw byte history)
//!  ├── raw_output_queue: VecDeque<Vec<u8>> (pending output)
//!  └── channel: Option<ActionCableChannel> (encrypted relay)
//! ```
//!
//! # Channel Ownership
//!
//! Each PTY session owns its terminal relay channel:
//! - CLI PTY (pty_index=0) owns the CLI terminal channel
//! - Server PTY (pty_index=1) owns the Server terminal channel
//!
//! The channel handles:
//! - **Output broadcast**: PTY output is encrypted and sent to all connected browsers
//! - **Input routing**: Browser input is decrypted and written to the PTY
//!
//! # Usage
//!
//! ```ignore
//! // Create PTY session
//! let mut session = PtySession::new(24, 80);
//!
//! // Spawn a process
//! session.spawn("bash", &env)?;
//!
//! // Connect channel (typically done by Hub after spawn)
//! session.connect_channel(channel_config, crypto_service).await?;
//!
//! // Write input (from browser or keyboard)
//! session.write_input(b"ls -la\n")?;
//!
//! // Drain and broadcast output
//! let output = session.drain_raw_output();
//! session.broadcast_output(&output).await?;
//! ```
//!
//! # Thread Safety
//!
//! The VT100 parser, scrollback buffer, and raw output queue are wrapped in
//! `Arc<Mutex<>>` to allow concurrent reads from the PTY reader thread and
//! writes from the main thread.

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

use crate::channel::{ActionCableChannel, Channel, ChannelConfig, ChannelError};
use crate::relay::crypto_service::CryptoServiceHandle;

use super::notification::AgentNotification;
use super::screen;

/// Resize a PTY session with screen clearing if dimensions changed.
///
/// Clears the VT100 screen before resizing to prevent content from being stuck
/// at old dimensions. This is necessary because terminal emulators don't
/// automatically reflow content.
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

/// PTY index constants for channel routing.
pub mod pty_index {
    /// CLI PTY index (main agent process).
    pub const CLI: usize = 0;
    /// Server PTY index (dev server process).
    pub const SERVER: usize = 1;
}

/// Encapsulates all state for a single PTY session.
///
/// Each PTY session manages:
/// - A pseudo-terminal for process I/O
/// - A VT100 parser for terminal emulation
/// - A raw byte scrollback buffer for history replay
/// - An optional encrypted ActionCable channel for browser communication
///
/// # Channel Ownership
///
/// The PTY owns its channel, ensuring lifecycle alignment. When the PTY is
/// dropped, its channel is automatically disconnected and cleaned up.
///
/// # Thread Safety
///
/// The VT100 parser, scrollback buffer, and raw output queue are wrapped in
/// `Arc<Mutex<>>` to allow concurrent reads from the PTY reader thread and
/// writes from the main thread.
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

    // === Channel ownership ===
    /// Encrypted terminal relay channel.
    ///
    /// The PTY owns its channel for output broadcast and input routing.
    /// Connected after spawn via `connect_channel()`.
    pub channel: Option<ActionCableChannel>,
}

impl std::fmt::Debug for PtySession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtySession")
            .field("has_master_pty", &self.master_pty.is_some())
            .field("has_writer", &self.writer.is_some())
            .field("has_reader_thread", &self.reader_thread.is_some())
            .field("has_channel", &self.channel.is_some())
            .finish_non_exhaustive()
    }
}

impl PtySession {
    /// Creates a new PTY session with the specified dimensions.
    ///
    /// The VT100 parser is initialized with scrollback enabled.
    /// The channel is initially None - call `connect_channel()` after spawn.
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
            channel: None,
        }
    }

    // =========================================================================
    // Channel Management
    // =========================================================================

    /// Connect the PTY's terminal relay channel.
    ///
    /// Creates an encrypted ActionCable channel for browser communication.
    /// The channel broadcasts output and receives input from connected browsers.
    ///
    /// # Arguments
    ///
    /// * `config` - Channel configuration (hub_id, agent_index, pty_index, etc.)
    /// * `crypto_service` - Crypto service handle for Signal Protocol encryption
    /// * `server_url` - Rails server URL
    /// * `api_key` - API key for authentication
    ///
    /// # Errors
    ///
    /// Returns an error if channel connection fails.
    pub async fn connect_channel(
        &mut self,
        config: ChannelConfig,
        crypto_service: CryptoServiceHandle,
        server_url: &str,
        api_key: &str,
    ) -> Result<(), ChannelError> {
        use crate::channel::ActionCableChannelBuilder;

        let mut channel = ActionCableChannelBuilder::default()
            .server_url(server_url)
            .api_key(api_key)
            .crypto_service(crypto_service)
            .reliable(true)
            .build();

        channel.connect(config).await?;
        self.channel = Some(channel);
        Ok(())
    }

    /// Check if the channel is connected.
    #[must_use]
    pub fn has_channel(&self) -> bool {
        self.channel.is_some()
    }

    /// Get the channel's sender handle for async output broadcasting.
    ///
    /// Returns None if no channel is connected.
    #[must_use]
    pub fn get_channel_sender(&self) -> Option<crate::channel::ChannelSenderHandle> {
        self.channel.as_ref().and_then(|c| c.get_sender_handle())
    }

    // =========================================================================
    // PTY I/O
    // =========================================================================

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

    // =========================================================================
    // Scrollback & Screen
    // =========================================================================

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
        // Channel is dropped automatically, which disconnects it
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

    #[test]
    fn test_pty_session_has_no_channel_initially() {
        let session = PtySession::new(24, 80);
        assert!(!session.has_channel());
        assert!(session.get_channel_sender().is_none());
    }

    #[test]
    fn test_pty_session_drain_output_empty() {
        let session = PtySession::new(24, 80);
        let output = session.drain_raw_output();
        assert!(output.is_empty());
    }

    #[test]
    fn test_pty_session_drain_output_with_data() {
        let session = PtySession::new(24, 80);

        // Simulate PTY reader thread pushing output
        {
            let mut queue = session.raw_output_queue.lock().unwrap();
            queue.push_back(b"hello".to_vec());
            queue.push_back(b" world".to_vec());
        }

        let output = session.drain_raw_output();
        assert_eq!(output, b"hello world");

        // Second drain should be empty
        let output2 = session.drain_raw_output();
        assert!(output2.is_empty());
    }
}
