//! PTY spawning utilities.
//!
//! This module provides common functionality for spawning PTY processes,
//! extracting shared patterns from CLI and Server PTY creation.
//!
//! # Event-Driven Architecture
//!
//! Reader threads broadcast [`PtyEvent::Output`] via a broadcast channel.
//! This enables decoupled pub/sub where:
//! - TUI client feeds output to its local terminal parser
//! - Browser client encrypts and sends via ActionCable channel
//!
//! Each client subscribes to events independently and handles them
//! according to their transport requirements.

// Rust guideline compliant 2026-02

use std::collections::HashMap;
#[cfg(test)]
use std::io::Read;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
#[cfg(test)]
use std::thread;

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtyPair, PtySize};
use tokio::sync::broadcast;

use super::notification::detect_notifications;
use super::pty::{HubEventListener, PtyEvent};
use crate::terminal::AlacrittyParser;

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

    // Prepend the running binary's directory to PATH so agent PTYs always
    // resolve `botster` to the same build that's running the hub — no need
    // to install globally or manage PATH expectations.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            let base_path = env_vars
                .get("PATH")
                .cloned()
                .or_else(|| std::env::var("PATH").ok())
                .unwrap_or_default();
            cmd.env("PATH", format!("{}:{}", bin_dir.display(), base_path));
        }
    }

    cmd
}

/// Process a single chunk of raw PTY output bytes.
///
/// Single source of truth for per-chunk PTY byte processing.
/// Both the reader thread ([`spawn_reader_thread`]) and the broker output
/// path ([`crate::hub::agent_handle::PtyHandle::feed_broker_output`]) call
/// this function, guaranteeing identical behavior regardless of which path
/// delivers bytes to the Hub.
///
/// # What this does per chunk
///
/// 1. **OSC notification detection** — OSC 9 / OSC 777 sequences, gated by
///    `detect_notifs` (true for CLI sessions, false for server PTYs).
/// 2. **Window title** — OSC 2 sequences update the TUI tab title.
/// 3. **CWD** — OSC 7 sequences update the working directory indicator.
/// 4. **Prompt marks** — OSC 133 A/B/C/D sequences mark shell prompt regions.
/// 5. **Kitty keyboard protocol** — CSI > u push / CSI < u pop updates
///    `kitty_enabled` and fires [`PtyEvent::KittyChanged`].
/// 6. **Cursor visibility** — CSI ? 25 h/l transitions fire
///    [`PtyEvent::CursorVisibilityChanged`]. Only state *changes* are emitted
///    to avoid flooding subscribers during TUI redraws. `last_cursor_visible`
///    carries this state across calls and must be persisted by the caller.
/// 7. **Shadow screen update** — bytes are fed to the `AlacrittyParser`.
///    No `catch_unwind` guard needed — alacritty_terminal is robust at all
///    sizes and sequences. CSI 3 J (erase scrollback) and all other sequences
///    are handled natively by alacritty.
/// 8. **`resize_pending` clear** — signals the app has redrawn after a resize.
/// 9. **Output broadcast** — raw bytes are sent to all live subscribers via
///    the broadcast channel. Lagged receivers skip missed frames.
///
/// # Note: Focus reporting
///
/// CSI ? 1004 h (focus reporting enable) is intentionally **not** handled here.
/// Responding to every occurrence fires [`PtyEvent::FocusRequested`] → TUI writes
/// `\x1b[I` → app redraws → re-emits `\x1b[?1004h` → infinite loop. Focus event
/// delivery requires "arm once" state tracking (fire on first enable, suppress until
/// `\x1b[?1004l` disables it) which is not yet implemented.
///
/// # Arguments
///
/// * `data` - Raw bytes from the PTY master FD.
/// * `shadow_screen` - Shadow terminal for parsed state snapshots.
/// * `event_tx` - Broadcast channel for [`PtyEvent`] notifications.
/// * `kitty_enabled` - Shared flag tracking kitty keyboard protocol state.
/// * `resize_pending` - Cleared when output arrives after a resize.
/// * `detect_notifs` - Enable OSC notification detection (true for CLI sessions).
/// * `last_cursor_visible` - Persistent cursor visibility state for deduplication.
///   Caller must preserve this across calls (local var in reader thread,
///   `Arc<Mutex<Option<bool>>>` field in [`crate::hub::agent_handle::PtyHandle`]).
/// * `label` - Log label for this session (e.g., `"CLI"`, `"Server"`, `"Broker"`).
pub(crate) fn process_pty_bytes(
    data: &[u8],
    shadow_screen: &Arc<Mutex<AlacrittyParser<HubEventListener>>>,
    event_tx: &broadcast::Sender<PtyEvent>,
    kitty_enabled: &AtomicBool,
    resize_pending: &AtomicBool,
    detect_notifs: bool,
    last_cursor_visible: &mut Option<bool>,
    _label: &str,
) {
    // ── 1. OSC notification detection ────────────────────────────────────
    if detect_notifs {
        for notif in detect_notifications(data) {
            log::info!("Broadcasting PTY notification: {:?}", notif);
            let _ = event_tx.send(PtyEvent::notification(notif));
        }
    }

    // ── 2-3. OSC metadata: CWD, prompt marks ───────────────────────────
    // Title scanning removed — alacritty fires Event::Title via HubEventListener.
    if let Some(cwd) = scan_cwd(data) {
        let _ = event_tx.send(PtyEvent::cwd_changed(cwd));
    }
    for mark in scan_prompt_marks(data) {
        let _ = event_tx.send(PtyEvent::prompt_mark(mark));
    }

    // ── 4. Shadow screen update ──────────────────────────────────────────
    // alacritty_terminal handles all escape sequences natively:
    // - Title changes fire Event::Title via HubEventListener
    // - Kitty keyboard protocol tracked via TermMode::KITTY_KEYBOARD_PROTOCOL
    // - DECTCEM cursor visibility tracked via TermMode::SHOW_CURSOR
    // - CSI 3 J (clear scrollback) handled by alacritty grid
    // No catch_unwind needed — alacritty is robust at all terminal sizes.
    let (new_kitty, new_cursor_hidden) = {
        let mut parser = shadow_screen.lock().expect("shadow_screen lock poisoned");
        parser.process(data);
        (parser.kitty_enabled(), parser.cursor_hidden())
    };

    // ── 5. Kitty state transition ────────────────────────────────────────
    let old_kitty = kitty_enabled.load(Ordering::Relaxed);
    if new_kitty != old_kitty {
        kitty_enabled.store(new_kitty, Ordering::Relaxed);
        let _ = event_tx.send(PtyEvent::kitty_changed(new_kitty));
    }

    // ── 6. Cursor visibility transition ──────────────────────────────────
    let visible = !new_cursor_hidden;
    if *last_cursor_visible != Some(visible) {
        *last_cursor_visible = Some(visible);
        let _ = event_tx.send(PtyEvent::cursor_visibility_changed(visible));
    }

    // ── 7. Clear resize-pending flag ─────────────────────────────────────
    // App produced output — shadow screen is no longer stale from a prior resize.
    resize_pending.store(false, Ordering::Release);

    // ── 8. Broadcast raw bytes ───────────────────────────────────────────
    // Clients parse bytes in their own parsers (xterm.js, TUI).
    // Lagged receivers skip missed frames — same behavior as a live PTY.
    let _ = event_tx.send(PtyEvent::output(data.to_vec()));
}

/// Spawn a unified PTY reader thread with optional notification detection.
///
/// Used exclusively in unit tests to exercise [`process_pty_bytes`] via a
/// live pipe reader. Production agents route output through the broker and
/// call [`process_pty_bytes`] directly via
/// [`PtyHandle::feed_broker_output`](crate::hub::agent_handle::PtyHandle::feed_broker_output).
///
/// # Arguments
///
/// * `reader` - PTY output reader
/// * `shadow_screen` - Shadow terminal for parsed state snapshots
/// * `event_tx` - Broadcast channel for PtyEvent notifications
/// * `detect_notifs` - Enable OSC notification detection (true for CLI sessions)
/// * `kitty_enabled` - Shared flag updated when kitty keyboard protocol is pushed/popped
/// * `resize_pending` - Cleared when PTY output arrives (app has redrawn after resize)
#[cfg(test)]
pub(crate) fn spawn_reader_thread(
    reader: Box<dyn Read + Send>,
    shadow_screen: Arc<Mutex<AlacrittyParser<HubEventListener>>>,
    event_tx: broadcast::Sender<PtyEvent>,
    detect_notifs: bool,
    kitty_enabled: Arc<AtomicBool>,
    resize_pending: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    let label = if detect_notifs { "CLI" } else { "Server" };

    thread::spawn(move || {
        log::info!("{label} PTY reader thread started");
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        // Start as Some(true): alacritty initializes with SHOW_CURSOR set, so the
        // cursor is already visible. Initializing to None would emit a spurious
        // CursorVisibilityChanged(true) on the very first read even when nothing changed.
        let mut last_cursor_visible: Option<bool> = Some(true);

        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    log::info!("{label} PTY reader got EOF, broadcasting ProcessExited");
                    let _ = event_tx.send(PtyEvent::process_exited(None));
                    break;
                }
                Ok(n) => {
                    process_pty_bytes(
                        &buf[..n],
                        &shadow_screen,
                        &event_tx,
                        &kitty_enabled,
                        &resize_pending,
                        detect_notifs,
                        &mut last_cursor_visible,
                        label,
                    );
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

    /// Helper: create a shadow screen wired to the given event channel.
    ///
    /// Uses the same `event_tx` as the reader thread so that events fired by
    /// the [`HubEventListener`] (e.g. `TitleChanged`) arrive on the same
    /// receiver as `Output` events — matching the production `PtySession::new()`
    /// behaviour where both share a single broadcast channel.
    fn test_shadow_screen(event_tx: broadcast::Sender<PtyEvent>) -> Arc<Mutex<AlacrittyParser<HubEventListener>>> {
        let listener = HubEventListener::new(event_tx);
        Arc::new(Mutex::new(AlacrittyParser::new_with_listener(24, 80, 100, listener)))
    }

    /// Helper: create a kitty flag for testing.
    fn test_kitty_flag() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    fn test_resize_flag() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    #[test]
    fn test_unified_reader_broadcasts_output_without_notifications() {
        let test_data = b"Hello from unified reader (server mode)";
        let reader = MockReader::new(test_data);
        let (tx, mut rx) = broadcast::channel::<PtyEvent>(16);
        let shadow = test_shadow_screen(tx.clone());

        let handle = spawn_reader_thread(reader, shadow.clone(), tx, false, test_kitty_flag(), test_resize_flag());
        handle.join().expect("Reader thread panicked");

        // Verify event was broadcast
        let event = rx.try_recv().expect("Should receive Output event");
        match event {
            PtyEvent::Output(data) => {
                assert_eq!(data, test_data, "Broadcast data should match input");
            }
            _ => panic!("Expected Output event"),
        }

        // Verify shadow screen was fed — generate snapshot and check for text.
        let snapshot = crate::terminal::generate_ansi_snapshot(&*shadow.lock().unwrap(), false);
        let snapshot_str = String::from_utf8_lossy(&snapshot);
        assert!(
            snapshot_str.contains("Hello from unified reader"),
            "Shadow screen snapshot should contain the output"
        );
    }

    #[test]
    fn test_unified_reader_broadcasts_output_with_notifications() {
        let test_data = b"Hello from unified reader (CLI mode)";
        let reader = MockReader::new(test_data);
        let (event_tx, mut event_rx) = broadcast::channel::<PtyEvent>(16);
        let shadow = test_shadow_screen(event_tx.clone());

        let handle = spawn_reader_thread(reader, shadow, event_tx, true, test_kitty_flag(), test_resize_flag());
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
        let (event_tx, mut event_rx) = broadcast::channel::<PtyEvent>(16);
        let shadow = test_shadow_screen(event_tx.clone());

        let handle = spawn_reader_thread(reader, shadow, event_tx, true, test_kitty_flag(), test_resize_flag());
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
        let (event_tx, mut event_rx) = broadcast::channel::<PtyEvent>(16);
        let shadow = test_shadow_screen(event_tx.clone());

        let handle = spawn_reader_thread(reader, shadow, event_tx, false, test_kitty_flag(), test_resize_flag());
        handle.join().expect("Reader thread panicked");

        let event = event_rx.try_recv().expect("Should receive Output event");
        assert!(matches!(event, PtyEvent::Output(_)));

        // Reader emits ProcessExited on EOF
        let exit_event = event_rx.try_recv().expect("Should receive ProcessExited on EOF");
        assert!(matches!(exit_event, PtyEvent::ProcessExited { exit_code: None }));
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
        let (event_tx, _event_rx) = broadcast::channel::<PtyEvent>(64);
        let shadow = test_shadow_screen(event_tx.clone());

        let handle = spawn_reader_thread(reader, shadow.clone(), event_tx, false, test_kitty_flag(), test_resize_flag());
        handle.join().expect("Reader thread panicked");

        // Verify fresh content is visible via snapshot
        let snapshot = crate::terminal::generate_ansi_snapshot(&*shadow.lock().unwrap(), false);
        let snapshot_str = String::from_utf8_lossy(&snapshot);
        assert!(
            snapshot_str.contains("fresh start"),
            "Screen should contain post-clear content"
        );

        // Verify scrollback is empty (alacritty handles CSI 3J natively).
        let parser = shadow.lock().unwrap();
        assert_eq!(
            parser.history_size(), 0,
            "Scrollback should be empty after CSI 3 J"
        );
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
        let (event_tx, _rx) = broadcast::channel::<PtyEvent>(16);
        let shadow = test_shadow_screen(event_tx.clone());
        let kitty = test_kitty_flag();

        assert!(!kitty.load(Ordering::Relaxed), "kitty should start false");

        let handle = spawn_reader_thread(reader, shadow, event_tx, false, Arc::clone(&kitty), test_resize_flag());
        handle.join().expect("Reader thread panicked");

        assert!(kitty.load(Ordering::Relaxed), "kitty should be true after push");
    }

    #[test]
    fn test_reader_clears_kitty_flag_on_pop() {
        let mut data = Vec::new();
        data.extend(b"\x1b[>1u"); // push
        data.extend(b"\x1b[<u");  // pop

        let reader = MockReader::new(&data);
        let (event_tx, _rx) = broadcast::channel::<PtyEvent>(16);
        let shadow = test_shadow_screen(event_tx.clone());
        let kitty = test_kitty_flag();

        let handle = spawn_reader_thread(reader, shadow, event_tx, false, Arc::clone(&kitty), test_resize_flag());
        handle.join().expect("Reader thread panicked");

        assert!(!kitty.load(Ordering::Relaxed), "kitty should be false after pop");
    }

    #[test]
    fn test_reader_kitty_flag_unset_for_normal_output() {
        let reader = MockReader::new(b"hello world\r\n");
        let (event_tx, _rx) = broadcast::channel::<PtyEvent>(16);
        let shadow = test_shadow_screen(event_tx.clone());
        let kitty = test_kitty_flag();

        let handle = spawn_reader_thread(reader, shadow, event_tx, false, Arc::clone(&kitty), test_resize_flag());
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
        let (event_tx, mut rx) = broadcast::channel::<PtyEvent>(32);
        let shadow = test_shadow_screen(event_tx.clone());

        let handle = spawn_reader_thread(reader, shadow, event_tx, false, test_kitty_flag(), test_resize_flag());
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
        let (event_tx, mut rx) = broadcast::channel::<PtyEvent>(32);
        let shadow = test_shadow_screen(event_tx.clone());

        let handle = spawn_reader_thread(reader, shadow, event_tx, false, test_kitty_flag(), test_resize_flag());
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
        let (event_tx, mut rx) = broadcast::channel::<PtyEvent>(32);
        let shadow = test_shadow_screen(event_tx.clone());

        let handle = spawn_reader_thread(reader, shadow, event_tx, false, test_kitty_flag(), test_resize_flag());
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
        let (event_tx, mut rx) = broadcast::channel::<PtyEvent>(32);
        let shadow = test_shadow_screen(event_tx.clone());

        let handle = spawn_reader_thread(reader, shadow, event_tx, false, test_kitty_flag(), test_resize_flag());
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


}
