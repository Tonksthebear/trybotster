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

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtyPair, PtySize};
use tokio::sync::broadcast;

use super::notification::detect_notifications;
use super::pty::PtyEvent;

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
/// The reader thread feeds every PTY byte to both the shadow screen (for
/// reconnect snapshots) and the broadcast channel (for live subscribers).
///
/// # Arguments
///
/// * `reader` - PTY output reader
/// * `shadow_screen` - Shadow terminal for parsed state snapshots
/// * `event_tx` - Broadcast channel for PtyEvent notifications
/// * `detect_notifs` - Enable OSC notification detection
/// * `kitty_enabled` - Shared flag updated when kitty keyboard protocol is pushed/popped
pub fn spawn_reader_thread(
    reader: Box<dyn Read + Send>,
    shadow_screen: Arc<Mutex<vt100::Parser>>,
    event_tx: broadcast::Sender<PtyEvent>,
    detect_notifs: bool,
    kitty_enabled: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    let label = if detect_notifs { "CLI" } else { "Server" };

    thread::spawn(move || {
        let mut reader = reader;
        log::info!("{label} PTY reader thread started");
        let mut buf = [0u8; 4096];

        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    // Detect notifications and broadcast as PtyEvent::Notification
                    if detect_notifs {
                        let notifications = detect_notifications(&buf[..n]);
                        for notif in notifications {
                            log::info!("Broadcasting PTY notification: {:?}", notif);
                            let _ = event_tx.send(PtyEvent::notification(notif));
                        }
                    }

                    // Track kitty keyboard protocol state from raw PTY output.
                    // The vt100 crate doesn't parse kitty sequences, so we scan
                    // the raw bytes for CSI > flags u (push) / CSI < u (pop).
                    if let Some(state) = scan_kitty_keyboard_state(&buf[..n]) {
                        kitty_enabled.store(state, Ordering::Relaxed);
                    }

                    // Feed PTY bytes to shadow screen for parsed state tracking.
                    // vt100 handles scrollback limits internally by line count.
                    {
                        let mut parser = shadow_screen
                            .lock()
                            .expect("shadow_screen lock poisoned");
                        parser.process(&buf[..n]);

                        // CSI 3 J (\x1b[3J) = "Erase Saved Lines" (clear scrollback).
                        // The vt100 crate ignores this sequence, so we handle it manually
                        // by replacing the parser with a fresh one seeded with the current
                        // visible screen state. This ensures `clear` drops stale history
                        // from reconnect snapshots.
                        if contains_clear_scrollback(&buf[..n]) {
                            let (rows, cols) = parser.screen().size();
                            let visible = parser.screen().contents_formatted();
                            *parser = vt100::Parser::new(
                                rows,
                                cols,
                                super::pty::SHADOW_SCROLLBACK_LINES,
                            );
                            parser.process(&visible);
                            log::info!("{label} shadow screen scrollback cleared (CSI 3 J)");
                        }
                    }

                    // Broadcast raw output to all live subscribers.
                    // Clients parse bytes in their own parsers (xterm.js, TUI vt100).
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

/// Check if a byte buffer contains the CSI 3 J (clear scrollback) sequence.
///
/// Scans for `\x1b[3J` which terminals emit when the user runs `clear` or
/// equivalent commands. The vt100 crate does not handle this sequence, so
/// callers must clear scrollback manually when this returns true.
fn contains_clear_scrollback(data: &[u8]) -> bool {
    // CSI 3 J = ESC [ 3 J = [0x1b, 0x5b, 0x33, 0x4a]
    data.windows(4)
        .any(|w| w == b"\x1b[3J")
}

/// Scan PTY output for kitty keyboard protocol push/pop sequences.
///
/// Returns `Some(true)` if the last relevant sequence is a push (`CSI > flags u`),
/// `Some(false)` if it's a pop (`CSI < u`), or `None` if no kitty sequences found.
///
/// The vt100 crate does not track kitty keyboard protocol state, so we scan
/// the raw byte stream directly. We check the *last* occurrence because a
/// single output chunk may contain multiple push/pop sequences (e.g. during
/// shell startup).
pub fn scan_kitty_keyboard_state(data: &[u8]) -> Option<bool> {
    let mut result = None;

    // Scan for ESC [ > ... u (push) and ESC [ < ... u (pop)
    let mut i = 0;
    while i + 2 < data.len() {
        if data[i] == 0x1b && data[i + 1] == b'[' {
            let start = i + 2;
            if start < data.len() && data[start] == b'>' {
                // Potential push: ESC [ > <digits> u
                let mut j = start + 1;
                while j < data.len() && data[j].is_ascii_digit() {
                    j += 1;
                }
                if j < data.len() && data[j] == b'u' {
                    result = Some(true);
                    i = j + 1;
                    continue;
                }
            } else if start < data.len() && data[start] == b'<' {
                // Potential pop: ESC [ < u  (or ESC [ < <digits> u)
                let mut j = start + 1;
                while j < data.len() && data[j].is_ascii_digit() {
                    j += 1;
                }
                if j < data.len() && data[j] == b'u' {
                    result = Some(false);
                    i = j + 1;
                    continue;
                }
            }
        }
        i += 1;
    }

    result
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

    /// Helper: create a shadow screen for testing.
    fn test_shadow_screen() -> Arc<Mutex<vt100::Parser>> {
        Arc::new(Mutex::new(vt100::Parser::new(24, 80, 100)))
    }

    /// Helper: create a kitty flag for testing.
    fn test_kitty_flag() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    #[test]
    fn test_unified_reader_broadcasts_output_without_notifications() {
        let test_data = b"Hello from unified reader (server mode)";
        let reader = MockReader::new(test_data);
        let shadow = test_shadow_screen();
        let (tx, mut rx) = broadcast::channel::<PtyEvent>(16);

        let handle = spawn_reader_thread(reader, shadow.clone(), tx, false, test_kitty_flag());
        handle.join().expect("Reader thread panicked");

        // Verify event was broadcast
        let event = rx.try_recv().expect("Should receive Output event");
        match event {
            PtyEvent::Output(data) => {
                assert_eq!(data, test_data, "Broadcast data should match input");
            }
            _ => panic!("Expected Output event"),
        }

        // Verify shadow screen was fed
        let screen_text = shadow.lock().unwrap().screen().contents();
        assert!(
            screen_text.contains("Hello from unified reader"),
            "Shadow screen should contain the output"
        );
    }

    #[test]
    fn test_unified_reader_broadcasts_output_with_notifications() {
        let test_data = b"Hello from unified reader (CLI mode)";
        let reader = MockReader::new(test_data);
        let shadow = test_shadow_screen();
        let (event_tx, mut event_rx) = broadcast::channel::<PtyEvent>(16);

        let handle = spawn_reader_thread(reader, shadow, event_tx, true, test_kitty_flag());
        handle.join().expect("Reader thread panicked");

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
        // OSC 9 notification: ESC ] 9 ; message BEL
        let test_data = b"\x1b]9;Build complete\x07";
        let reader = MockReader::new(test_data);
        let shadow = test_shadow_screen();
        let (event_tx, mut event_rx) = broadcast::channel::<PtyEvent>(16);

        let handle = spawn_reader_thread(reader, shadow, event_tx, true, test_kitty_flag());
        handle.join().expect("Reader thread panicked");

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

        let output = event_rx.try_recv().expect("Should receive Output event");
        assert!(matches!(output, PtyEvent::Output(_)));
    }

    #[test]
    fn test_unified_reader_skips_notifications_when_disabled() {
        let test_data = b"\x1b]9;Build complete\x07";
        let reader = MockReader::new(test_data);
        let shadow = test_shadow_screen();
        let (event_tx, mut event_rx) = broadcast::channel::<PtyEvent>(16);

        let handle = spawn_reader_thread(reader, shadow, event_tx, false, test_kitty_flag());
        handle.join().expect("Reader thread panicked");

        let event = event_rx.try_recv().expect("Should receive Output event");
        assert!(matches!(event, PtyEvent::Output(_)));

        match event_rx.try_recv() {
            Err(broadcast::error::TryRecvError::Empty)
            | Err(broadcast::error::TryRecvError::Closed) => {}
            other => panic!("Expected no more events, got {:?}", other),
        }
    }

    // =========================================================================
    // Clear Scrollback Detection Tests
    // =========================================================================

    #[test]
    fn test_contains_clear_scrollback_detects_csi_3j() {
        assert!(contains_clear_scrollback(b"\x1b[3J"));
        // Embedded in larger output (typical `clear` command output)
        assert!(contains_clear_scrollback(b"\x1b[H\x1b[2J\x1b[3J"));
        assert!(contains_clear_scrollback(b"some text\x1b[3Jmore text"));
    }

    #[test]
    fn test_contains_clear_scrollback_ignores_other_sequences() {
        assert!(!contains_clear_scrollback(b"\x1b[2J"));
        assert!(!contains_clear_scrollback(b"\x1b[H"));
        assert!(!contains_clear_scrollback(b"plain text"));
        assert!(!contains_clear_scrollback(b""));
        // Partial sequence should not match
        assert!(!contains_clear_scrollback(b"\x1b[3"));
    }

    #[test]
    fn test_reader_clears_scrollback_on_csi_3j() {
        // Phase 1: Write some lines that generate scrollback
        let mut data = Vec::new();
        for i in 0..30 {
            data.extend(format!("line {i}\r\n").as_bytes());
        }
        // Phase 2: Send clear command (CSI H + CSI 2J + CSI 3J)
        data.extend(b"\x1b[H\x1b[2J\x1b[3J");
        // Phase 3: Write new content after clear
        data.extend(b"fresh start");

        let reader = MockReader::new(&data);
        let shadow = test_shadow_screen();
        let (event_tx, _event_rx) = broadcast::channel::<PtyEvent>(64);

        let handle = spawn_reader_thread(reader, shadow.clone(), event_tx, false, test_kitty_flag());
        handle.join().expect("Reader thread panicked");

        // Scrollback should have been cleared — only visible screen remains
        let parser = shadow.lock().unwrap();
        let screen = parser.screen();

        // Verify fresh content is visible
        let contents = screen.contents();
        assert!(
            contents.contains("fresh start"),
            "Screen should contain post-clear content"
        );

        // Verify scrollback is empty (the pre-clear lines were dropped).
        // Probe total scrollback lines the same way snapshot_with_scrollback does.
        drop(parser);
        let mut parser = shadow.lock().unwrap();
        let screen = parser.screen_mut();
        let saved = screen.scrollback();
        screen.set_scrollback(usize::MAX);
        let total_scrollback = screen.scrollback();
        screen.set_scrollback(saved);
        assert_eq!(
            total_scrollback, 0,
            "Scrollback should be empty after CSI 3 J"
        );
    }

    // =========================================================================
    // Kitty Keyboard Protocol Scanner Tests
    // =========================================================================

    #[test]
    fn test_scan_kitty_detects_push() {
        // CSI > 1 u = push with DISAMBIGUATE_ESCAPE_CODES
        assert_eq!(scan_kitty_keyboard_state(b"\x1b[>1u"), Some(true));
    }

    #[test]
    fn test_scan_kitty_detects_pop() {
        // CSI < u = pop
        assert_eq!(scan_kitty_keyboard_state(b"\x1b[<u"), Some(false));
        // CSI < 1 u = pop with count
        assert_eq!(scan_kitty_keyboard_state(b"\x1b[<1u"), Some(false));
    }

    #[test]
    fn test_scan_kitty_embedded_in_output() {
        // Kitty push buried in normal terminal output
        let mut data = Vec::new();
        data.extend(b"\x1b[32msome green text\x1b[0m");
        data.extend(b"\x1b[>1u"); // kitty push
        data.extend(b"more text");
        assert_eq!(scan_kitty_keyboard_state(&data), Some(true));
    }

    #[test]
    fn test_scan_kitty_last_wins() {
        // Push then pop → pop wins
        let mut data = Vec::new();
        data.extend(b"\x1b[>1u"); // push
        data.extend(b"\x1b[<u");  // pop
        assert_eq!(scan_kitty_keyboard_state(&data), Some(false));

        // Pop then push → push wins
        let mut data2 = Vec::new();
        data2.extend(b"\x1b[<u");  // pop
        data2.extend(b"\x1b[>1u"); // push
        assert_eq!(scan_kitty_keyboard_state(&data2), Some(true));
    }

    #[test]
    fn test_scan_kitty_no_sequences() {
        assert_eq!(scan_kitty_keyboard_state(b"plain text"), None);
        assert_eq!(scan_kitty_keyboard_state(b"\x1b[32m"), None);
        assert_eq!(scan_kitty_keyboard_state(b""), None);
    }

    #[test]
    fn test_scan_kitty_partial_sequences() {
        // Incomplete sequences should not match
        assert_eq!(scan_kitty_keyboard_state(b"\x1b[>1"), None);
        assert_eq!(scan_kitty_keyboard_state(b"\x1b[>"), None);
        assert_eq!(scan_kitty_keyboard_state(b"\x1b[<"), None);
    }

    #[test]
    fn test_scan_kitty_various_flags() {
        // Different flag values
        assert_eq!(scan_kitty_keyboard_state(b"\x1b[>0u"), Some(true));
        assert_eq!(scan_kitty_keyboard_state(b"\x1b[>3u"), Some(true));
        assert_eq!(scan_kitty_keyboard_state(b"\x1b[>31u"), Some(true));
    }

    // =========================================================================
    // Reader Thread Kitty State Tracking Tests
    // =========================================================================

    #[test]
    fn test_reader_sets_kitty_flag_on_push() {
        // PTY output containing a kitty push sequence
        let mut data = Vec::new();
        data.extend(b"some output\r\n");
        data.extend(b"\x1b[>1u"); // kitty push
        data.extend(b"more output");

        let reader = MockReader::new(&data);
        let shadow = test_shadow_screen();
        let (event_tx, _rx) = broadcast::channel::<PtyEvent>(16);
        let kitty = test_kitty_flag();

        assert!(!kitty.load(Ordering::Relaxed), "kitty should start false");

        let handle = spawn_reader_thread(reader, shadow, event_tx, false, Arc::clone(&kitty));
        handle.join().expect("Reader thread panicked");

        assert!(kitty.load(Ordering::Relaxed), "kitty should be true after push");
    }

    #[test]
    fn test_reader_clears_kitty_flag_on_pop() {
        let mut data = Vec::new();
        data.extend(b"\x1b[>1u"); // push
        data.extend(b"\x1b[<u");  // pop

        let reader = MockReader::new(&data);
        let shadow = test_shadow_screen();
        let (event_tx, _rx) = broadcast::channel::<PtyEvent>(16);
        let kitty = test_kitty_flag();

        let handle = spawn_reader_thread(reader, shadow, event_tx, false, Arc::clone(&kitty));
        handle.join().expect("Reader thread panicked");

        assert!(!kitty.load(Ordering::Relaxed), "kitty should be false after pop");
    }

    #[test]
    fn test_reader_kitty_flag_unset_for_normal_output() {
        let reader = MockReader::new(b"hello world\r\n");
        let shadow = test_shadow_screen();
        let (event_tx, _rx) = broadcast::channel::<PtyEvent>(16);
        let kitty = test_kitty_flag();

        let handle = spawn_reader_thread(reader, shadow, event_tx, false, Arc::clone(&kitty));
        handle.join().expect("Reader thread panicked");

        assert!(!kitty.load(Ordering::Relaxed), "kitty should remain false");
    }
}
