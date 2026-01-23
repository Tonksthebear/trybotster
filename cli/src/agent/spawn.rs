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

// Rust guideline compliant 2026-01

use std::collections::VecDeque;
use std::io::Read;
use std::sync::{mpsc::Sender, Arc, Mutex};
use std::thread;

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtyPair, PtySize};
use tokio::sync::broadcast;

use super::notification::{detect_notifications, AgentNotification};
use super::pty::{PtyEvent, MAX_SCROLLBACK_BYTES};

/// Configuration for spawning a PTY process.
#[derive(Debug)]
pub struct PtySpawnConfig<'a> {
    /// Terminal rows.
    pub rows: u16,
    /// Terminal columns.
    pub cols: u16,
    /// Working directory for the command.
    pub cwd: &'a std::path::Path,
    /// Environment variables to set.
    pub env_vars: &'a std::collections::HashMap<String, String>,
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

/// Spawn a CLI PTY reader thread with notification detection.
///
/// Reads PTY output, broadcasts via event channel, adds to scrollback buffer,
/// and detects OSC notification sequences.
///
/// Note: This does NOT parse bytes through a vt100 parser. Clients (TuiRunner,
/// TuiClient) own their own parsers and feed bytes in their `on_output()` handlers.
///
/// # Arguments
///
/// * `reader` - PTY output reader
/// * `scrollback_buffer` - Raw byte buffer for session replay
/// * `event_tx` - Broadcast channel for PtyEvent notifications
/// * `notification_tx` - Channel for OSC notification events
pub fn spawn_cli_reader_thread(
    reader: Box<dyn Read + Send>,
    scrollback_buffer: Arc<Mutex<VecDeque<u8>>>,
    event_tx: broadcast::Sender<PtyEvent>,
    notification_tx: Sender<AgentNotification>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = reader;
        log::info!("CLI PTY reader thread started");
        let mut buf = [0u8; 4096];
        let mut total_bytes_read: usize = 0;

        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    total_bytes_read += n;
                    if total_bytes_read % 10240 < n {
                        log::info!("CLI PTY reader: {total_bytes_read} total bytes read");
                    }

                    // Detect notifications in output
                    let notifications = detect_notifications(&buf[..n]);
                    for notif in notifications {
                        log::info!("Sending notification to channel: {:?}", notif);
                        let _ = notification_tx.send(notif);
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
                    log::error!("CLI PTY read error: {e}");
                    break;
                }
            }
        }
        log::info!("CLI PTY reader thread exiting");
    })
}

/// Spawn a Server PTY reader thread (no notification detection).
///
/// Similar to CLI reader but without OSC notification detection.
/// Broadcasts output via [`PtyEvent::Output`].
///
/// Note: This does NOT parse bytes through a vt100 parser. Clients (TuiRunner,
/// TuiClient) own their own parsers and feed bytes in their `on_output()` handlers.
///
/// # Arguments
///
/// * `reader` - PTY output reader
/// * `scrollback_buffer` - Raw byte buffer for session replay
/// * `event_tx` - Broadcast channel for PtyEvent notifications
pub fn spawn_server_reader_thread(
    reader: Box<dyn Read + Send>,
    scrollback_buffer: Arc<Mutex<VecDeque<u8>>>,
    event_tx: broadcast::Sender<PtyEvent>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = reader;
        log::info!("Server PTY reader thread started");
        let mut buf = [0u8; 4096];
        let mut total_bytes_read: usize = 0;

        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    total_bytes_read += n;
                    if total_bytes_read % 10240 < n {
                        log::info!("Server PTY reader: {total_bytes_read} total bytes read");
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
                    log::error!("Server PTY read error: {e}");
                    break;
                }
            }
        }
        log::info!("Server PTY reader thread exiting");
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::mpsc;

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
    // Hot Path Tests - Reader Thread Broadcasting
    // =========================================================================
    // These tests verify the critical hot path: reader thread -> broadcast::send()
    //
    // Note: spawn_cli_reader_thread and spawn_server_reader_thread are nearly
    // identical in their broadcast behavior. The CLI version additionally does
    // notification detection.

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
    fn test_hot_path_server_reader_broadcasts_output() {
        // Tests that spawn_server_reader_thread broadcasts PtyEvent::Output
        let test_data = b"Hello from server PTY";
        let reader = MockReader::new(test_data);
        let scrollback = Arc::new(Mutex::new(VecDeque::new()));
        let (tx, mut rx) = broadcast::channel::<PtyEvent>(16);

        // Spawn the reader thread
        let handle = spawn_server_reader_thread(reader, scrollback.clone(), tx);

        // Wait for thread to process and complete (short data = quick)
        handle.join().expect("Reader thread panicked");

        // Verify event was broadcast
        let event = rx.try_recv().expect("Should receive Output event");
        match event {
            PtyEvent::Output(data) => {
                assert_eq!(data, test_data, "Broadcast data should match input");
            }
            _ => panic!("Expected Output event"),
        }
    }

    #[test]
    fn test_hot_path_server_reader_populates_scrollback() {
        // Tests that reader thread adds data to scrollback buffer
        let test_data = b"Scrollback test data";
        let reader = MockReader::new(test_data);
        let scrollback = Arc::new(Mutex::new(VecDeque::new()));
        let (tx, _rx) = broadcast::channel::<PtyEvent>(16);

        let handle = spawn_server_reader_thread(reader, scrollback.clone(), tx);
        handle.join().expect("Reader thread panicked");

        // Verify scrollback was populated
        let buffer = scrollback.lock().unwrap();
        let snapshot: Vec<u8> = buffer.iter().copied().collect();
        assert_eq!(snapshot, test_data, "Scrollback should contain input data");
    }

    #[test]
    fn test_hot_path_reader_broadcasts_to_multiple_subscribers() {
        // Tests that multiple subscribers all receive the output
        let test_data = b"Multi-subscriber test";
        let reader = MockReader::new(test_data);
        let scrollback = Arc::new(Mutex::new(VecDeque::new()));
        let (tx, mut rx1) = broadcast::channel::<PtyEvent>(16);

        // Create additional subscribers
        let mut rx2 = tx.subscribe();
        let mut rx3 = tx.subscribe();

        let handle = spawn_server_reader_thread(reader, scrollback, tx);
        handle.join().expect("Reader thread panicked");

        // All three receivers should get the event
        for (i, rx) in [&mut rx1, &mut rx2, &mut rx3].iter_mut().enumerate() {
            let event = rx
                .try_recv()
                .unwrap_or_else(|_| panic!("Receiver {} should have event", i));
            match event {
                PtyEvent::Output(data) => {
                    assert_eq!(data, test_data, "Receiver {} got wrong data", i);
                }
                _ => panic!("Receiver {} expected Output event", i),
            }
        }
    }

    #[test]
    fn test_hot_path_cli_reader_broadcasts_output() {
        // Tests that spawn_cli_reader_thread broadcasts PtyEvent::Output
        let test_data = b"Hello from CLI PTY";
        let reader = MockReader::new(test_data);
        let scrollback = Arc::new(Mutex::new(VecDeque::new()));
        let (event_tx, mut event_rx) = broadcast::channel::<PtyEvent>(16);
        let (notif_tx, _notif_rx) = mpsc::channel::<AgentNotification>();

        let handle = spawn_cli_reader_thread(reader, scrollback.clone(), event_tx, notif_tx);
        handle.join().expect("Reader thread panicked");

        // Verify event was broadcast
        let event = event_rx.try_recv().expect("Should receive Output event");
        match event {
            PtyEvent::Output(data) => {
                assert_eq!(data, test_data, "Broadcast data should match input");
            }
            _ => panic!("Expected Output event"),
        }
    }

    #[test]
    fn test_hot_path_cli_reader_populates_scrollback() {
        // Tests that CLI reader thread adds data to scrollback buffer
        let test_data = b"CLI scrollback test";
        let reader = MockReader::new(test_data);
        let scrollback = Arc::new(Mutex::new(VecDeque::new()));
        let (event_tx, _rx) = broadcast::channel::<PtyEvent>(16);
        let (notif_tx, _notif_rx) = mpsc::channel::<AgentNotification>();

        let handle = spawn_cli_reader_thread(reader, scrollback.clone(), event_tx, notif_tx);
        handle.join().expect("Reader thread panicked");

        // Verify scrollback was populated
        let buffer = scrollback.lock().unwrap();
        let snapshot: Vec<u8> = buffer.iter().copied().collect();
        assert_eq!(snapshot, test_data, "Scrollback should contain input data");
    }

    #[test]
    fn test_hot_path_reader_handles_chunked_data() {
        // Tests that larger data is correctly chunked and broadcast
        // The reader uses a 4096-byte buffer, so test with data requiring multiple reads
        let large_data: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();
        let reader = MockReader::new(&large_data);
        let scrollback = Arc::new(Mutex::new(VecDeque::new()));
        let (tx, mut rx) = broadcast::channel::<PtyEvent>(16);

        let handle = spawn_server_reader_thread(reader, scrollback.clone(), tx);
        handle.join().expect("Reader thread panicked");

        // Collect all output events
        let mut received_data = Vec::new();
        while let Ok(PtyEvent::Output(chunk)) = rx.try_recv() {
            received_data.extend(chunk);
        }

        // Total received should match input
        assert_eq!(received_data, large_data, "All data should be received");

        // Scrollback should also have all data
        let buffer = scrollback.lock().unwrap();
        let scrollback_data: Vec<u8> = buffer.iter().copied().collect();
        assert_eq!(
            scrollback_data, large_data,
            "Scrollback should have all data"
        );
    }

    #[test]
    fn test_hot_path_reader_respects_scrollback_limit() {
        // Tests that scrollback buffer is trimmed to MAX_SCROLLBACK_BYTES
        // Create data larger than MAX_SCROLLBACK_BYTES
        let oversized_data: Vec<u8> = vec![b'x'; MAX_SCROLLBACK_BYTES + 1000];
        let reader = MockReader::new(&oversized_data);
        let scrollback = Arc::new(Mutex::new(VecDeque::new()));
        let (tx, _rx) = broadcast::channel::<PtyEvent>(64); // Large capacity for all chunks

        let handle = spawn_server_reader_thread(reader, scrollback.clone(), tx);
        handle.join().expect("Reader thread panicked");

        // Scrollback should be trimmed
        let buffer = scrollback.lock().unwrap();
        assert!(
            buffer.len() <= MAX_SCROLLBACK_BYTES,
            "Scrollback should be <= MAX_SCROLLBACK_BYTES, got {}",
            buffer.len()
        );
    }
}
