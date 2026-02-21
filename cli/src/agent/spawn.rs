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
                Ok(0) => {
                    log::info!("{label} PTY reader got EOF, broadcasting ProcessExited");
                    let _ = event_tx.send(PtyEvent::process_exited(None));
                    break;
                }
                Ok(n) => {
                    let chunk = &buf[..n];

                    // Detect notifications and broadcast as PtyEvent::Notification
                    if detect_notifs {
                        let notifications = detect_notifications(chunk);
                        for notif in notifications {
                            log::info!("Broadcasting PTY notification: {:?}", notif);
                            let _ = event_tx.send(PtyEvent::notification(notif));
                        }
                    }

                    // Detect OSC metadata events (title, CWD, prompt marks).
                    // These run for all sessions (not just detect_notifs) because
                    // title/CWD/prompt info is useful for both CLI and server PTYs.
                    if let Some(title) = scan_window_title(chunk) {
                        let _ = event_tx.send(PtyEvent::title_changed(title));
                    }
                    if let Some(cwd) = scan_cwd(chunk) {
                        let _ = event_tx.send(PtyEvent::cwd_changed(cwd));
                    }
                    for mark in scan_prompt_marks(chunk) {
                        let _ = event_tx.send(PtyEvent::prompt_mark(mark));
                    }

                    // Track kitty keyboard protocol state from raw PTY output.
                    // The vt100 crate doesn't parse kitty sequences, so we scan
                    // the raw bytes for CSI > flags u (push) / CSI < u (pop).
                    if let Some(state) = scan_kitty_keyboard_state(chunk) {
                        kitty_enabled.store(state, Ordering::Relaxed);
                        let _ = event_tx.send(PtyEvent::kitty_changed(state));
                    }

                    // Detect focus reporting enable (CSI ? 1004 h) so the TUI
                    // can respond with the current terminal focus state.
                    if scan_focus_reporting_enabled(chunk) {
                        let _ = event_tx.send(PtyEvent::focus_requested());
                    }

                    // Feed PTY bytes to shadow screen for parsed state tracking.
                    // vt100 handles scrollback limits internally by line count.
                    //
                    // Safety net: vt100 0.16.2 has arithmetic overflow bugs
                    // (e.g. `grid.rs:683` col_wrap on 1-row grids). The primary
                    // defense is the MIN_PARSER_ROWS clamp in PtySession and
                    // resize_shadow_screen(), but we keep catch_unwind as a
                    // generic safety net for any unforeseen vt100 panics.
                    // On panic the parser is replaced with a fresh instance
                    // to avoid cascading failures.
                    {
                        let mut parser = shadow_screen
                            .lock()
                            .expect("shadow_screen lock poisoned");

                        // catch_unwind requires the closure to be UnwindSafe.
                        // MutexGuard is !UnwindSafe because the guarded state
                        // may be inconsistent after a panic. We use
                        // AssertUnwindSafe because on panic we immediately
                        // replace the parser with a fresh instance, so
                        // any inconsistent grid state is discarded.
                        let result = std::panic::catch_unwind(
                            std::panic::AssertUnwindSafe(|| {
                                parser.process(chunk);
                            }),
                        );

                        if let Err(panic_info) = result {
                            let msg = panic_info
                                .downcast_ref::<String>()
                                .map(String::as_str)
                                .or_else(|| {
                                    panic_info.downcast_ref::<&str>().copied()
                                })
                                .unwrap_or("unknown panic");

                            // The parser's internal grid state is now
                            // inconsistent (e.g. invalid scroll region),
                            // so every future process() would also panic.
                            // Replace with a fresh parser at the same
                            // dimensions. Reading size() is safe — it just
                            // returns stored u16 fields, no grid arithmetic.
                            let (rows, cols) = parser.screen().size();
                            let rows = rows.max(super::pty::MIN_PARSER_ROWS);
                            *parser = vt100::Parser::new(
                                rows,
                                cols,
                                super::pty::SHADOW_SCROLLBACK_LINES,
                            );
                            log::error!(
                                "{label} vt100 parser panicked (reset {rows}x{cols}): {msg}"
                            );
                        } else {
                            // CSI 3 J (\x1b[3J) = "Erase Saved Lines" (clear
                            // scrollback). The vt100 crate ignores this
                            // sequence, so we handle it manually by replacing
                            // the parser with a fresh one seeded with the
                            // current visible screen state.
                            if contains_clear_scrollback(chunk) {
                                let (rows, cols) = parser.screen().size();
                                let visible =
                                    parser.screen().contents_formatted();
                                *parser = vt100::Parser::new(
                                    rows,
                                    cols,
                                    super::pty::SHADOW_SCROLLBACK_LINES,
                                );
                                parser.process(&visible);
                                log::info!(
                                    "{label} shadow screen scrollback cleared (CSI 3 J)"
                                );
                            }
                        }
                    }

                    // Broadcast raw output to all live subscribers.
                    // Clients parse bytes in their own parsers (xterm.js, TUI vt100).
                    let _ = event_tx.send(PtyEvent::output(chunk.to_vec()));
                }
                Err(e) => {
                    log::error!("{label} PTY read error: {e}");
                    let _ = event_tx.send(PtyEvent::process_exited(None));
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
pub(crate) fn contains_clear_scrollback(data: &[u8]) -> bool {
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

/// Scan PTY output for focus reporting enable sequence (`CSI ? 1004 h`).
///
/// Returns `true` if the byte stream contains `\x1b[?1004h`, indicating
/// the application wants terminal focus events. The TUI should respond
/// with the current focus state (`CSI I` or `CSI O`).
pub fn scan_focus_reporting_enabled(data: &[u8]) -> bool {
    // Match the byte sequence: ESC [ ? 1 0 0 4 h
    let needle = b"\x1b[?1004h";
    data.windows(needle.len()).any(|w| w == needle)
}

/// Scan PTY output for OSC 0/2 window title sequences.
///
/// Returns the last title found in the buffer, or `None` if no title sequences
/// were detected. We return only the last because rapid title updates (e.g.,
/// shell prompt) mean only the final value matters.
///
/// Supports both BEL (0x07) and ST (ESC \) terminators.
/// - OSC 0: `ESC ] 0 ; title BEL` — sets window title and icon name
/// - OSC 2: `ESC ] 2 ; title BEL` — sets window title only
pub fn scan_window_title(data: &[u8]) -> Option<String> {
    let mut last_title = None;
    let mut i = 0;

    while i + 1 < data.len() {
        if data[i] == 0x1b && data[i + 1] == b']' {
            let osc_start = i + 2;
            // Check for OSC 0; or OSC 2; prefix
            let is_title = osc_start + 2 <= data.len()
                && (data[osc_start] == b'0' || data[osc_start] == b'2')
                && osc_start + 1 < data.len()
                && data[osc_start + 1] == b';';

            if is_title {
                let title_start = osc_start + 2;
                // Find terminator (BEL or ST)
                let mut end = None;
                for j in title_start..data.len() {
                    if data[j] == 0x07 {
                        end = Some((j, j + 1));
                        break;
                    } else if j + 1 < data.len() && data[j] == 0x1b && data[j + 1] == b'\\' {
                        end = Some((j, j + 2));
                        break;
                    }
                }
                if let Some((content_end, skip_to)) = end {
                    let title = String::from_utf8_lossy(&data[title_start..content_end]).to_string();
                    last_title = Some(title);
                    i = skip_to;
                    continue;
                }
            }
        }
        i += 1;
    }

    last_title
}

/// Scan PTY output for OSC 7 current working directory sequences.
///
/// Returns the last CWD path found in the buffer, or `None`.
/// Format: `ESC ] 7 ; file://hostname/path BEL`
///
/// Extracts just the path component, percent-decoded.
pub fn scan_cwd(data: &[u8]) -> Option<String> {
    let mut last_cwd = None;
    let mut i = 0;

    while i + 1 < data.len() {
        if data[i] == 0x1b && data[i + 1] == b']' {
            let osc_start = i + 2;
            // Check for OSC 7; prefix
            let is_cwd = osc_start + 2 <= data.len()
                && data[osc_start] == b'7'
                && osc_start + 1 < data.len()
                && data[osc_start + 1] == b';';

            if is_cwd {
                let uri_start = osc_start + 2;
                let mut end = None;
                for j in uri_start..data.len() {
                    if data[j] == 0x07 {
                        end = Some((j, j + 1));
                        break;
                    } else if j + 1 < data.len() && data[j] == 0x1b && data[j + 1] == b'\\' {
                        end = Some((j, j + 2));
                        break;
                    }
                }
                if let Some((content_end, skip_to)) = end {
                    let uri = String::from_utf8_lossy(&data[uri_start..content_end]);
                    // Extract path from file://hostname/path URI
                    let path = if let Some(rest) = uri.strip_prefix("file://") {
                        // Skip hostname (everything up to the next /)
                        if let Some(slash_pos) = rest.find('/') {
                            percent_decode(&rest[slash_pos..])
                        } else {
                            percent_decode(rest)
                        }
                    } else {
                        // Not a proper URI, use as-is
                        uri.to_string()
                    };
                    if !path.is_empty() {
                        last_cwd = Some(path);
                    }
                    i = skip_to;
                    continue;
                }
            }
        }
        i += 1;
    }

    last_cwd
}

/// Simple percent-decoding for file URIs.
///
/// Decodes `%XX` sequences to their byte values. Used for OSC 7 paths
/// which may contain encoded spaces and special characters.
fn percent_decode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(
                &input[i + 1..i + 3],
                16,
            ) {
                result.push(byte as char);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Scan PTY output for OSC 133/633 shell integration prompt marks.
///
/// Returns all prompt marks found in the buffer. These sequences mark
/// command boundaries:
/// - `A` — Prompt start (shell is about to display prompt)
/// - `B` — Command start (user finished typing, prompt ended)
/// - `C` — Command executed (output begins)
/// - `D` [; exitcode] — Command finished
/// - `E` ; commandline — (OSC 633 only) Command text
///
/// Supports both OSC 133 (FinalTerm/iTerm2) and OSC 633 (VS Code).
pub fn scan_prompt_marks(data: &[u8]) -> Vec<super::pty::PromptMark> {
    use super::pty::PromptMark;
    let mut marks = Vec::new();
    let mut i = 0;

    while i + 1 < data.len() {
        if data[i] == 0x1b && data[i + 1] == b']' {
            let osc_start = i + 2;

            // Check for OSC 133; or OSC 633; prefix
            let (prefix_len, is_vscode) = if osc_start + 4 <= data.len()
                && &data[osc_start..osc_start + 4] == b"133;"
            {
                (4, false)
            } else if osc_start + 4 <= data.len()
                && &data[osc_start..osc_start + 4] == b"633;"
            {
                (4, true)
            } else {
                i += 1;
                continue;
            };

            let mark_start = osc_start + prefix_len;

            // Find terminator
            let mut end = None;
            for j in mark_start..data.len() {
                if data[j] == 0x07 {
                    end = Some((j, j + 1));
                    break;
                } else if j + 1 < data.len() && data[j] == 0x1b && data[j + 1] == b'\\' {
                    end = Some((j, j + 2));
                    break;
                }
            }

            if let Some((content_end, skip_to)) = end {
                let content = &data[mark_start..content_end];

                let mark = if !content.is_empty() {
                    match content[0] {
                        b'A' => Some(PromptMark::PromptStart),
                        b'B' => Some(PromptMark::CommandStart),
                        b'C' => Some(PromptMark::CommandExecuted(None)),
                        b'D' => {
                            // Optional exit code after ;
                            let exit_code = if content.len() > 2 && content[1] == b';' {
                                String::from_utf8_lossy(&content[2..])
                                    .trim()
                                    .parse::<i32>()
                                    .ok()
                            } else {
                                None
                            };
                            Some(PromptMark::CommandFinished(exit_code))
                        }
                        b'E' if is_vscode && content.len() > 2 && content[1] == b';' => {
                            // VS Code command text: OSC 633;E;command_text BEL
                            let cmd = String::from_utf8_lossy(&content[2..]).to_string();
                            Some(PromptMark::CommandExecuted(Some(cmd)))
                        }
                        _ => None,
                    }
                } else {
                    None
                };

                if let Some(m) = mark {
                    marks.push(m);
                }
                i = skip_to;
                continue;
            }
        }
        i += 1;
    }

    marks
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

        // Reader emits ProcessExited on EOF
        let exit_event = event_rx.try_recv().expect("Should receive ProcessExited on EOF");
        assert!(matches!(exit_event, PtyEvent::ProcessExited { exit_code: None }));
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

    // =========================================================================
    // Reader Thread OSC Event Integration Tests
    // =========================================================================

    #[test]
    fn test_reader_emits_title_changed_event() {
        let mut data = Vec::new();
        data.extend(b"some output\r\n");
        data.extend(b"\x1b]0;My Agent Title\x07");
        data.extend(b"more output");

        let reader = MockReader::new(&data);
        let shadow = test_shadow_screen();
        let (event_tx, mut rx) = broadcast::channel::<PtyEvent>(32);

        let handle = spawn_reader_thread(reader, shadow, event_tx, false, test_kitty_flag());
        handle.join().expect("Reader thread panicked");

        // Collect all events
        let mut found_title = false;
        while let Ok(event) = rx.try_recv() {
            if let PtyEvent::TitleChanged(title) = event {
                assert_eq!(title, "My Agent Title");
                found_title = true;
            }
        }
        assert!(found_title, "Should have emitted TitleChanged event");
    }

    #[test]
    fn test_reader_emits_cwd_changed_event() {
        let data = b"\x1b]7;file://localhost/home/user/projects\x07";
        let reader = MockReader::new(data);
        let shadow = test_shadow_screen();
        let (event_tx, mut rx) = broadcast::channel::<PtyEvent>(32);

        let handle = spawn_reader_thread(reader, shadow, event_tx, false, test_kitty_flag());
        handle.join().expect("Reader thread panicked");

        let mut found_cwd = false;
        while let Ok(event) = rx.try_recv() {
            if let PtyEvent::CwdChanged(cwd) = event {
                assert_eq!(cwd, "/home/user/projects");
                found_cwd = true;
            }
        }
        assert!(found_cwd, "Should have emitted CwdChanged event");
    }

    #[test]
    fn test_reader_emits_prompt_mark_events() {
        let mut data = Vec::new();
        data.extend(b"\x1b]133;A\x07"); // prompt start
        data.extend(b"$ ls\r\n");
        data.extend(b"\x1b]133;D;0\x07"); // command finished

        let reader = MockReader::new(&data);
        let shadow = test_shadow_screen();
        let (event_tx, mut rx) = broadcast::channel::<PtyEvent>(32);

        let handle = spawn_reader_thread(reader, shadow, event_tx, false, test_kitty_flag());
        handle.join().expect("Reader thread panicked");

        let mut marks = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let PtyEvent::PromptMark(mark) = event {
                marks.push(mark);
            }
        }
        assert_eq!(marks.len(), 2);
        assert_eq!(marks[0], super::super::pty::PromptMark::PromptStart);
        assert_eq!(marks[1], super::super::pty::PromptMark::CommandFinished(Some(0)));
    }

    #[test]
    fn test_reader_emits_osc_events_without_detect_notifs() {
        // OSC metadata events should fire even with detect_notifs=false (server sessions)
        let mut data = Vec::new();
        data.extend(b"\x1b]0;Server Title\x07");
        data.extend(b"\x1b]7;file:///var/www\x07");
        data.extend(b"\x1b]9;notification\x07"); // this should NOT emit (detect_notifs=false)

        let reader = MockReader::new(&data);
        let shadow = test_shadow_screen();
        let (event_tx, mut rx) = broadcast::channel::<PtyEvent>(32);

        let handle = spawn_reader_thread(reader, shadow, event_tx, false, test_kitty_flag());
        handle.join().expect("Reader thread panicked");

        let mut has_title = false;
        let mut has_cwd = false;
        let mut has_notification = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                PtyEvent::TitleChanged(_) => has_title = true,
                PtyEvent::CwdChanged(_) => has_cwd = true,
                PtyEvent::Notification(_) => has_notification = true,
                _ => {}
            }
        }
        assert!(has_title, "Title should emit regardless of detect_notifs");
        assert!(has_cwd, "CWD should emit regardless of detect_notifs");
        assert!(!has_notification, "Notification should NOT emit when detect_notifs=false");
    }

    // =========================================================================
    // Window Title Scanner Tests (OSC 0/2)
    // =========================================================================

    #[test]
    fn test_scan_window_title_osc0_bel() {
        let data = b"\x1b]0;My Terminal Title\x07";
        assert_eq!(scan_window_title(data), Some("My Terminal Title".to_string()));
    }

    #[test]
    fn test_scan_window_title_osc2_bel() {
        let data = b"\x1b]2;Window Title Only\x07";
        assert_eq!(scan_window_title(data), Some("Window Title Only".to_string()));
    }

    #[test]
    fn test_scan_window_title_st_terminator() {
        let data = b"\x1b]0;Title with ST\x1b\\";
        assert_eq!(scan_window_title(data), Some("Title with ST".to_string()));
    }

    #[test]
    fn test_scan_window_title_last_wins() {
        let data = b"\x1b]0;First\x07\x1b]0;Second\x07";
        assert_eq!(scan_window_title(data), Some("Second".to_string()));
    }

    #[test]
    fn test_scan_window_title_empty() {
        // Empty title (program clearing the title)
        let data = b"\x1b]0;\x07";
        assert_eq!(scan_window_title(data), Some(String::new()));
    }

    #[test]
    fn test_scan_window_title_none() {
        assert_eq!(scan_window_title(b"plain text"), None);
        assert_eq!(scan_window_title(b"\x1b]9;notification\x07"), None);
    }

    #[test]
    fn test_scan_window_title_embedded_in_output() {
        let mut data = Vec::new();
        data.extend(b"\x1b[32mgreen\x1b[0m");
        data.extend(b"\x1b]0;~/projects/botster\x07");
        data.extend(b"more output");
        assert_eq!(scan_window_title(&data), Some("~/projects/botster".to_string()));
    }

    // =========================================================================
    // CWD Scanner Tests (OSC 7)
    // =========================================================================

    #[test]
    fn test_scan_cwd_basic() {
        let data = b"\x1b]7;file://localhost/home/user/projects\x07";
        assert_eq!(scan_cwd(data), Some("/home/user/projects".to_string()));
    }

    #[test]
    fn test_scan_cwd_st_terminator() {
        let data = b"\x1b]7;file://host/tmp/dir\x1b\\";
        assert_eq!(scan_cwd(data), Some("/tmp/dir".to_string()));
    }

    #[test]
    fn test_scan_cwd_percent_encoded() {
        let data = b"\x1b]7;file://localhost/home/user/my%20project\x07";
        assert_eq!(scan_cwd(data), Some("/home/user/my project".to_string()));
    }

    #[test]
    fn test_scan_cwd_empty_hostname() {
        // Some shells emit file:///path (empty hostname)
        let data = b"\x1b]7;file:///Users/jason/code\x07";
        assert_eq!(scan_cwd(data), Some("/Users/jason/code".to_string()));
    }

    #[test]
    fn test_scan_cwd_none() {
        assert_eq!(scan_cwd(b"plain text"), None);
        assert_eq!(scan_cwd(b"\x1b]0;title\x07"), None);
    }

    // =========================================================================
    // Prompt Mark Scanner Tests (OSC 133/633)
    // =========================================================================

    #[test]
    fn test_scan_prompt_marks_133_a() {
        use super::super::pty::PromptMark;
        let data = b"\x1b]133;A\x07";
        let marks = scan_prompt_marks(data);
        assert_eq!(marks, vec![PromptMark::PromptStart]);
    }

    #[test]
    fn test_scan_prompt_marks_633_a() {
        use super::super::pty::PromptMark;
        let data = b"\x1b]633;A\x07";
        let marks = scan_prompt_marks(data);
        assert_eq!(marks, vec![PromptMark::PromptStart]);
    }

    #[test]
    fn test_scan_prompt_marks_all_types() {
        use super::super::pty::PromptMark;
        let mut data = Vec::new();
        data.extend(b"\x1b]133;A\x07");  // prompt start
        data.extend(b"\x1b]133;B\x07");  // command start
        data.extend(b"\x1b]133;C\x07");  // command executed
        data.extend(b"\x1b]133;D;0\x07"); // command finished (exit 0)
        let marks = scan_prompt_marks(&data);
        assert_eq!(marks, vec![
            PromptMark::PromptStart,
            PromptMark::CommandStart,
            PromptMark::CommandExecuted(None),
            PromptMark::CommandFinished(Some(0)),
        ]);
    }

    #[test]
    fn test_scan_prompt_marks_633_e_command_text() {
        use super::super::pty::PromptMark;
        let data = b"\x1b]633;E;ls -la\x07";
        let marks = scan_prompt_marks(data);
        assert_eq!(marks, vec![PromptMark::CommandExecuted(Some("ls -la".to_string()))]);
    }

    #[test]
    fn test_scan_prompt_marks_exit_code() {
        use super::super::pty::PromptMark;
        let data = b"\x1b]133;D;1\x07";
        let marks = scan_prompt_marks(data);
        assert_eq!(marks, vec![PromptMark::CommandFinished(Some(1))]);
    }

    #[test]
    fn test_scan_prompt_marks_no_exit_code() {
        use super::super::pty::PromptMark;
        let data = b"\x1b]133;D\x07";
        let marks = scan_prompt_marks(data);
        assert_eq!(marks, vec![PromptMark::CommandFinished(None)]);
    }

    #[test]
    fn test_scan_prompt_marks_none() {
        assert!(scan_prompt_marks(b"plain text").is_empty());
        assert!(scan_prompt_marks(b"\x1b]0;title\x07").is_empty());
    }

    #[test]
    fn test_percent_decode() {
        assert_eq!(percent_decode("/path/to/file"), "/path/to/file");
        assert_eq!(percent_decode("/path%20with%20spaces"), "/path with spaces");
        assert_eq!(percent_decode("%2Froot"), "/root");
        assert_eq!(percent_decode("no%encoding"), "no%encoding"); // invalid hex
    }

    // =========================================================================
    // vt100 Panic Recovery Tests
    // =========================================================================

    /// Confirm the vt100 bug exists: resize can create a single-row scroll
    /// region (scroll_top == scroll_bottom) that `col_wrap` cannot handle.
    ///
    /// vt100 0.16.2 `grid.rs:683` does `prev_pos.row -= scrolled` without
    /// overflow protection. When cursor is at row 0 inside a single-row
    /// scroll region and wraps, `scrolled=1` and `0 - 1` panics.
    #[test]
    #[should_panic(expected = "attempt to subtract with overflow")]
    fn test_vt100_col_wrap_underflow_reproducer() {
        // Step 1: 3-row terminal, set scroll region rows 1..2 (0-indexed: 0..1).
        let mut parser = vt100::Parser::new(3, 10, 0);
        parser.process(b"\x1b[1;2r"); // scroll_top=0, scroll_bottom=1

        // Step 2: Resize to 1 row. set_size clamps scroll_bottom to 0,
        //         but the `< scroll_top` guard doesn't fire (0 < 0 = false),
        //         leaving scroll_top=0, scroll_bottom=0 — an invalid state
        //         that set_scroll_region would reject.
        parser.screen_mut().set_size(1, 10);

        // Step 3: Fill the row (10 chars) then one more to trigger col_wrap.
        //         col_wrap: prev_pos.row=0, row_inc_scroll returns 1,
        //         0_u16 - 1_u16 → overflow panic.
        parser.process(b"AAAAAAAAAAB");
    }

    /// Verify the reader thread survives a vt100 parser panic (safety net).
    ///
    /// This test bypasses the production DECSTBM reset fix by calling
    /// `set_size()` directly, simulating a hypothetical vt100 panic from
    /// any cause. Without `catch_unwind` + parser reset, the panic would
    /// kill the thread and poison the `shadow_screen` mutex.
    #[test]
    fn test_reader_survives_vt100_parser_panic() {
        // Phase 1: establish a scroll region (delivered as PTY output).
        let phase1 = b"\x1b[1;2r";

        // Phase 2: post-resize overflow trigger + post-panic output.
        let phase2 = b"AAAAAAAAAAB\x1b[r after panic";

        // We use two separate MockReaders stitched by a wrapper that
        // injects a resize between phases, simulating a real terminal
        // resize arriving between two PTY read() calls.
        struct ResizeThenRead {
            phase: u8,
            phase1: Cursor<Vec<u8>>,
            phase2: Cursor<Vec<u8>>,
            shadow: Arc<Mutex<vt100::Parser>>,
        }
        impl Read for ResizeThenRead {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                match self.phase {
                    0 => {
                        let n = self.phase1.read(buf)?;
                        if n == 0 {
                            // Between reads, resize shrinks terminal to 1 row,
                            // creating the invalid single-row scroll region.
                            self.shadow
                                .lock()
                                .unwrap()
                                .screen_mut()
                                .set_size(1, 10);
                            self.phase = 1;
                            self.phase2.read(buf)
                        } else {
                            Ok(n)
                        }
                    }
                    _ => self.phase2.read(buf),
                }
            }
        }

        // Start with 3 rows, 10 cols.
        let shadow: Arc<Mutex<vt100::Parser>> =
            Arc::new(Mutex::new(vt100::Parser::new(3, 10, 0)));
        let reader: Box<dyn Read + Send> = Box::new(ResizeThenRead {
            phase: 0,
            phase1: Cursor::new(phase1.to_vec()),
            phase2: Cursor::new(phase2.to_vec()),
            shadow: Arc::clone(&shadow),
        });
        let (event_tx, mut event_rx) = broadcast::channel::<PtyEvent>(64);

        let handle = spawn_reader_thread(
            reader,
            shadow.clone(),
            event_tx,
            false,
            test_kitty_flag(),
        );

        // The reader thread must not panic.
        handle.join().expect("Reader thread panicked — catch_unwind missing");

        // The shadow_screen mutex must not be poisoned.
        assert!(
            shadow.lock().is_ok(),
            "shadow_screen mutex poisoned — catch_unwind missing"
        );

        // Output events should still have been broadcast.
        let mut got_output = false;
        while let Ok(event) = event_rx.try_recv() {
            if matches!(event, PtyEvent::Output(_)) {
                got_output = true;
            }
        }
        assert!(got_output, "Reader should still broadcast output events");
    }

    /// Verify that clamping rows to `MIN_PARSER_ROWS` prevents the vt100
    /// col_wrap panic.
    ///
    /// This is the production fix: all `set_size()` and `Parser::new()` calls
    /// clamp rows to at least 2. A 1-row grid triggers an underflow in
    /// `Grid::col_wrap` (grid.rs:683) on any line wrap.
    #[test]
    fn test_min_rows_clamp_prevents_panic() {
        use crate::agent::pty::MIN_PARSER_ROWS;

        // Without clamp: 1-row grid panics on line wrap.
        let result = std::panic::catch_unwind(|| {
            let mut parser = vt100::Parser::new(1, 10, 0);
            parser.process(b"AAAAAAAAAAB"); // wraps → col_wrap → boom
        });
        assert!(result.is_err(), "1-row grid should panic on col_wrap");

        // With clamp: MIN_PARSER_ROWS avoids the panic.
        let rows: u16 = 1;
        let mut parser = vt100::Parser::new(rows.max(MIN_PARSER_ROWS), 10, 0);
        parser.process(b"AAAAAAAAAAB"); // wraps safely with 2+ rows

        // Also safe after set_size with clamp.
        parser.screen_mut().set_size(rows.max(MIN_PARSER_ROWS), 10);
        parser.process(b"AAAAAAAAAAB");
    }
}
