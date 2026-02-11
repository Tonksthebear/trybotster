//! PTY spawning utilities.
//!
//! This module provides common functionality for spawning PTY processes,
//! extracting shared patterns from CLI and Server PTY creation.
//!
//! # Event-Driven Architecture
//!
//! Reader threads broadcast [`PtyEvent::Output`] via a broadcast channel.
//! This enables decoupled pub/sub where:
//! - TUI client feeds output to its local vt100 parser
//! - Browser client encrypts and sends via ActionCable channel
//!
//! Each client subscribes to events independently and handles them
//! according to their transport requirements.

// Rust guideline compliant 2026-02

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtyPair, PtySize};
use tokio::sync::broadcast;

use super::notification::detect_notifications;
use super::pty::{PtyEvent, MAX_SCROLLBACK_BYTES};

/// Configuration for spawning a process in a PtySession.
///
/// This struct captures all the parameters needed to spawn a process,
/// unifying the CLI and server spawn paths into a single configurable
/// entry point via [`PtySession::spawn()`](super::pty::PtySession::spawn).
///
/// # Example
///
/// ```ignore
/// let config = PtySpawnConfig {
///     worktree_path: PathBuf::from("/path/to/worktree"),
///     command: "bash".to_string(),
///     env: HashMap::new(),
///     init_commands: vec!["source .botster/shared/sessions/agent/initialization".to_string()],
///     detect_notifications: true,
///     port: None,
///     context: String::new(),
/// };
/// pty_session.spawn(config)?;
/// ```
#[derive(Debug)]
pub struct PtySpawnConfig {
    /// Working directory for the process.
    pub worktree_path: PathBuf,
    /// Command to run (e.g., "bash").
    pub command: String,
    /// Environment variables to set.
    pub env: HashMap<String, String>,
    /// Commands to run after spawn (e.g., sourcing a session initialization script).
    pub init_commands: Vec<String>,
    /// Enable OSC notification detection on this session.
    ///
    /// When true, the reader thread will parse PTY output for OSC 9 and
    /// OSC 777 notification sequences and broadcast them as
    /// [`PtyEvent::Notification`](super::pty::PtyEvent::Notification) events.
    pub detect_notifications: bool,
    /// HTTP forwarding port (if this session runs a server).
    ///
    /// When set, stored on the PtySession via `set_port()` for browser
    /// clients to query when proxying preview requests.
    pub port: Option<u16>,
    /// Context string written to PTY before init commands.
    ///
    /// Typically used to send initial context to the agent process
    /// before running init scripts.
    pub context: String,
}

/// Open a new PTY pair with the given dimensions.
pub fn open_pty(rows: u16, cols: u16) -> Result<PtyPair> {
    let pty_system = native_pty_system();
    let size = PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    };
    pty_system.openpty(size).context("Failed to open PTY")
}

/// Build a command from a command string.
#[allow(
    clippy::implicit_hasher,
    reason = "internal API doesn't need hasher generalization"
)]
pub fn build_command(
    command_str: &str,
    cwd: &std::path::Path,
    env_vars: &std::collections::HashMap<String, String>,
) -> CommandBuilder {
    let parts: Vec<&str> = command_str.split_whitespace().collect();
    let mut cmd = CommandBuilder::new(parts[0]);
    for arg in &parts[1..] {
        cmd.arg(arg);
    }
    cmd.cwd(cwd);
    for (key, value) in env_vars {
        cmd.env(key, value);
    }
    cmd
}

/// Spawn a unified PTY reader thread with optional notification detection.
///
/// This is the reader thread implementation used by
/// [`PtySession::spawn()`](super::pty::PtySession::spawn), parameterized
/// by `detect_notifications`:
///
/// - When `true`, OSC notification sequences are detected and broadcast
///   as [`PtyEvent::Notification`] events (CLI session behavior).
/// - When `false`, notification detection is skipped (server session behavior).
///
/// # Arguments
///
/// * `reader` - PTY output reader
/// * `scrollback_buffer` - Raw byte buffer for session replay
/// * `event_tx` - Broadcast channel for PtyEvent notifications
/// * `detect_notifs` - Enable OSC notification detection
pub fn spawn_reader_thread(
    reader: Box<dyn Read + Send>,
    scrollback_buffer: Arc<Mutex<VecDeque<u8>>>,
    event_tx: broadcast::Sender<PtyEvent>,
    detect_notifs: bool,
) -> thread::JoinHandle<()> {
    let label = if detect_notifs { "CLI" } else { "Server" };

    thread::spawn(move || {
        let mut reader = reader;
        log::info!("{label} PTY reader thread started");
        let mut buf = [0u8; 4096];
        let mut total_bytes_read: usize = 0;

        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    total_bytes_read += n;
                    if total_bytes_read % 10240 < n {
                        log::info!("{label} PTY reader: {total_bytes_read} total bytes read");
                    }

                    // Detect notifications and broadcast as PtyEvent::Notification
                    if detect_notifs {
                        let notifications = detect_notifications(&buf[..n]);
                        for notif in notifications {
                            log::info!("Broadcasting PTY notification: {:?}", notif);
                            let _ = event_tx.send(PtyEvent::notification(notif));
                        }
                    }

                    // Add raw bytes to scrollback buffer
                    {
                        let mut buffer = scrollback_buffer
                            .lock()
                            .expect("scrollback_buffer lock poisoned");
                        buffer.extend(buf[..n].iter().copied());
                        // Trim from front if over limit
                        while buffer.len() > MAX_SCROLLBACK_BYTES {
                            buffer.pop_front();
                        }
                    }

                    // Broadcast output event to all subscribers
                    // Clients parse bytes in their own parsers when they receive this event
                    let _ = event_tx.send(PtyEvent::output(buf[..n].to_vec()));
                }
                Err(e) => {
                    log::error!("{label} PTY read error: {e}");
                    break;
                }
            }
        }
        log::info!("{label} PTY reader thread exiting");
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_open_pty() {
        // This test may fail in CI environments without PTY support
        let result = open_pty(24, 80);
        // Just check it doesn't panic - actual success depends on environment
        let _ = result;
    }

    #[test]
    fn test_build_command() {
        use std::collections::HashMap;
        use std::path::PathBuf;

        let env = HashMap::new();
        let cwd = PathBuf::from("/tmp");
        let cmd = build_command("echo hello world", &cwd, &env);

        // CommandBuilder doesn't expose its internals, so we just verify it was created
        let _ = cmd;
    }

    // =========================================================================
    // Reader Thread Tests
    // =========================================================================

    /// Mock reader that returns predefined data.
    struct MockReader {
        data: Cursor<Vec<u8>>,
    }

    impl MockReader {
        fn new(data: &[u8]) -> Box<dyn Read + Send> {
            Box::new(Self {
                data: Cursor::new(data.to_vec()),
            })
        }
    }

    impl Read for MockReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.data.read(buf)
        }
    }

    #[test]
    fn test_unified_reader_broadcasts_output_without_notifications() {
        // Tests spawn_reader_thread with detect_notifs=false (server mode)
        let test_data = b"Hello from unified reader (server mode)";
        let reader = MockReader::new(test_data);
        let scrollback = Arc::new(Mutex::new(VecDeque::new()));
        let (tx, mut rx) = broadcast::channel::<PtyEvent>(16);

        let handle = spawn_reader_thread(reader, scrollback.clone(), tx, false);
        handle.join().expect("Reader thread panicked");

        // Verify event was broadcast
        let event = rx.try_recv().expect("Should receive Output event");
        match event {
            PtyEvent::Output(data) => {
                assert_eq!(data, test_data, "Broadcast data should match input");
            }
            _ => panic!("Expected Output event"),
        }

        // Verify scrollback was populated
        let buffer = scrollback.lock().unwrap();
        let snapshot: Vec<u8> = buffer.iter().copied().collect();
        assert_eq!(snapshot, test_data, "Scrollback should contain input data");
    }

    #[test]
    fn test_unified_reader_broadcasts_output_with_notifications() {
        // Tests spawn_reader_thread with detect_notifs=true (CLI mode)
        let test_data = b"Hello from unified reader (CLI mode)";
        let reader = MockReader::new(test_data);
        let scrollback = Arc::new(Mutex::new(VecDeque::new()));
        let (event_tx, mut event_rx) = broadcast::channel::<PtyEvent>(16);

        let handle = spawn_reader_thread(reader, scrollback.clone(), event_tx, true);
        handle.join().expect("Reader thread panicked");

        // Verify output event was broadcast
        let event = event_rx.try_recv().expect("Should receive Output event");
        match event {
            PtyEvent::Output(data) => {
                assert_eq!(data, test_data, "Broadcast data should match input");
            }
            _ => panic!("Expected Output event"),
        }
    }

    #[test]
    fn test_unified_reader_detects_notifications_when_enabled() {
        // Tests that spawn_reader_thread broadcasts PtyEvent::Notification when detect_notifs=true
        // OSC 9 notification: ESC ] 9 ; message BEL
        let test_data = b"\x1b]9;Build complete\x07";
        let reader = MockReader::new(test_data);
        let scrollback = Arc::new(Mutex::new(VecDeque::new()));
        let (event_tx, mut event_rx) = broadcast::channel::<PtyEvent>(16);

        let handle = spawn_reader_thread(reader, scrollback, event_tx, true);
        handle.join().expect("Reader thread panicked");

        // Should receive the Notification event first (broadcast before Output)
        let event = event_rx.try_recv().expect("Should receive Notification event");
        match event {
            PtyEvent::Notification(notif) => {
                match notif {
                    super::super::notification::AgentNotification::Osc9(Some(msg)) => {
                        assert_eq!(msg, "Build complete");
                    }
                    _ => panic!("Expected Osc9 notification"),
                }
            }
            _ => panic!("Expected Notification event, got {:?}", event),
        }

        // Should also receive the Output event
        let output = event_rx.try_recv().expect("Should receive Output event");
        assert!(matches!(output, PtyEvent::Output(_)));
    }

    #[test]
    fn test_unified_reader_skips_notifications_when_disabled() {
        // Tests that spawn_reader_thread does NOT broadcast notifications when detect_notifs=false
        let test_data = b"\x1b]9;Build complete\x07";
        let reader = MockReader::new(test_data);
        let scrollback = Arc::new(Mutex::new(VecDeque::new()));
        let (event_tx, mut event_rx) = broadcast::channel::<PtyEvent>(16);

        let handle = spawn_reader_thread(reader, scrollback, event_tx, false);
        handle.join().expect("Reader thread panicked");

        // Output should still be broadcast (the raw bytes including the OSC sequence)
        let event = event_rx.try_recv().expect("Should receive Output event");
        assert!(matches!(event, PtyEvent::Output(_)));

        // No Notification event should be queued (channel may be empty or closed)
        match event_rx.try_recv() {
            Err(broadcast::error::TryRecvError::Empty)
            | Err(broadcast::error::TryRecvError::Closed) => {}
            other => panic!("Expected no more events, got {:?}", other),
        }
    }
}
