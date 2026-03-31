//! Per-session process: holds a single PTY fd, binds a Unix socket, relays bytes.
//!
//! Uses one process per PTY instead of the old multiplexed architecture.
//! Each session process is spawned by the Hub and communicates over its own
//! Unix socket. The Hub connects, performs a handshake, and streams I/O.
//!
//! # Responsibilities
//!
//! - **PTY ownership**: creates the PTY via `portable_pty`, owns the master fd
//! - **Socket server**: binds a Unix socket, accepts one connection (the Hub)
//! - **Reader thread**: reads PTY output → feeds ghostty parser → forwards to Hub
//! - **Writer thread**: receives input from Hub → writes to PTY master fd
//! - **Terminal state**: ghostty parser tracks mode flags (kitty, cursor, mouse, etc.)
//! - **Event emission**: detects state changes and emits event frames to the Hub
//! - **Snapshot generation**: generates opaque terminal snapshots and plain text screen dumps on request
//! - **Tee/logging**: optional file tee with rotation
//! - **Lifecycle**: exits when socket file is deleted or child process dies
//!
//! # Event detection
//!
//! The reader thread emits event frames for terminal state changes:
//! - **Ghostty callbacks**: all event detection via patched libghostty-vt callbacks
//!   (title, bell, pwd/OSC 7, notifications/OSC 9/777, prompt marks/OSC 133, mode changes)
//! - **Zero byte scanning**: ghostty handles all VT parsing, Rust only wires callbacks to frames
//!
//! # What it does NOT do
//!
//! - No client routing (Hub manages TUI, browser, socket clients)
//! - No multiplexing (one socket, one session)

pub mod connection;
pub mod protocol;
#[cfg(test)]
mod tests;

use std::io::{self, Read, Write};
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{mem::ManuallyDrop, thread};

use anyhow::{bail, Context, Result};

use crate::terminal::{CallbackConfig, TerminalParser, DEFAULT_SCROLLBACK_LINES};

use protocol::*;

// ─── Tee (log file) ─────────────────────────────────────────────────────────

/// File tee for logging PTY output to disk.
struct Tee {
    path: PathBuf,
    file: std::fs::File,
    written: u64,
    cap: u64,
}

impl Tee {
    fn new(path: &Path, cap: u64) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create tee dir: {}", parent.display()))?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("open tee: {}", path.display()))?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            path: path.to_owned(),
            file,
            written,
            cap,
        })
    }

    fn write_data(&mut self, data: &[u8]) {
        if let Err(e) = self.file.write_all(data) {
            log::warn!("[session] tee write error: {e}");
            return;
        }
        self.written += data.len() as u64;
        if self.cap > 0 && self.written >= self.cap {
            self.rotate();
        }
    }

    fn rotate(&mut self) {
        let rotated = self.path.with_extension("log.1");
        if let Err(e) = std::fs::rename(&self.path, &rotated) {
            log::warn!("[session] tee rotate error: {e}");
            return;
        }
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            Ok(f) => {
                self.file = f;
                self.written = 0;
                log::info!("[session] tee rotated: {}", self.path.display());
            }
            Err(e) => log::warn!("[session] tee reopen error: {e}"),
        }
    }
}

type SharedTee = Arc<Mutex<Option<Tee>>>;

// ─── PTY write commands ──────────────────────────────────────────────────────

enum PtyWriteCommand {
    Input(Vec<u8>),
    Resize { rows: u16, cols: u16 },
    Shutdown,
}

/// Messages from the reader/child threads to the main I/O relay loop.
enum SessionOutput {
    /// Raw PTY output bytes.
    PtyData(Vec<u8>),
    /// Child process exited with optional exit code.
    ChildExited(Option<i32>),
    /// Pre-encoded event frame (mode changed, title, bell, CWD, notification, prompt mark).
    EventFrame(Vec<u8>),
}

/// Events from ghostty callbacks (fired during process(), relayed to reader thread).
#[allow(dead_code)]
enum VtEvent {
    Notification { title: String, body: String },
    SemanticPrompt(crate::ghostty_vt::GhosttySemanticPromptAction),
    ModeChanged { mode: u16, enabled: bool },
    KittyKeyboardChanged,
}

#[derive(Debug, Clone, Copy, Default)]
struct SessionCallbackFlags {
    notification: bool,
    semantic_prompt: bool,
    mode_changed: bool,
    kitty_keyboard_changed: bool,
}

impl SessionCallbackFlags {
    fn all_enabled() -> Self {
        Self {
            notification: true,
            semantic_prompt: true,
            mode_changed: true,
            kitty_keyboard_changed: true,
        }
    }

    fn from_env() -> Self {
        let raw = std::env::var("BOTSTER_GHOSTTY_SESSION_CALLBACKS").unwrap_or_default();
        if raw.trim().is_empty() {
            return Self::all_enabled();
        }

        let mut flags = Self::default();

        for token in raw
            .split(',')
            .map(str::trim)
            .filter(|token| !token.is_empty())
        {
            match token {
                "all" => {
                    flags = Self::all_enabled();
                }
                "none" => flags = Self::default(),
                "notification" => flags.notification = true,
                "semantic_prompt" => flags.semantic_prompt = true,
                "mode_changed" => flags.mode_changed = true,
                "kitty_keyboard_changed" => flags.kitty_keyboard_changed = true,
                other => {
                    log::warn!(
                        "[session] ignoring unknown BOTSTER_GHOSTTY_SESSION_CALLBACKS token: {}",
                        other
                    );
                }
            }
        }

        flags
    }
}

// ─── Session process entry point ─────────────────────────────────────────────

/// Configuration for spawning a session process.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SpawnConfig {
    /// Shell command to execute (e.g., "bash", "zsh").
    pub command: String,
    /// Arguments passed to the command.
    pub args: Vec<String>,
    /// Environment variable overrides as (key, value) pairs.
    pub env: Vec<(String, String)>,
    /// Working directory for the spawned process.
    pub cwd: Option<String>,
    /// Initial PTY row count.
    pub rows: u16,
    /// Initial PTY column count.
    pub cols: u16,
    /// Commands to write to PTY stdin after child spawns (e.g., "source init.sh").
    #[serde(default)]
    pub init_commands: Vec<String>,
    /// Path for tee logging of PTY output.
    pub tee_path: Option<String>,
    /// Maximum tee log file size in bytes.
    pub tee_cap: u64,
    /// Boot-probed default foreground color for the session's libghostty parser.
    #[serde(default)]
    pub default_foreground: Option<crate::terminal::Rgb>,
    /// Boot-probed default background color for the session's libghostty parser.
    #[serde(default)]
    pub default_background: Option<crate::terminal::Rgb>,
    /// Boot-probed default cursor color for the session's libghostty parser.
    #[serde(default)]
    pub default_cursor: Option<crate::terminal::Rgb>,
    /// Boot-probed palette entries for OSC 4 queries and indexed color rendering.
    #[serde(default)]
    pub palette_colors: Vec<(u8, crate::terminal::Rgb)>,
}

/// Run the session process.
///
/// This is the entry point called by `botster session`. It:
/// 1. Binds the Unix socket
/// 2. Waits for the Hub to connect and send spawn config
/// 3. Creates the PTY and spawns the child process
/// 4. Runs the I/O relay loop until the child exits or socket is deleted
pub fn run(session_uuid: &str, socket_path: &str, timeout_secs: u64) -> Result<()> {
    let socket_path = Path::new(socket_path);

    // Clean up stale socket if present
    if socket_path.exists() {
        std::fs::remove_file(socket_path).ok();
    }

    // Bind socket
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("bind session socket: {}", socket_path.display()))?;
    listener
        .set_nonblocking(false)
        .context("set socket blocking")?;
    log::info!(
        "[session {}] listening on {}",
        &session_uuid[..session_uuid.len().min(16)],
        socket_path.display()
    );

    // Accept hub connection (with timeout for initial connect)
    listener
        .set_nonblocking(true)
        .context("set socket nonblocking for accept")?;
    let stream = wait_for_connection(&listener, Duration::from_secs(timeout_secs), socket_path)?;
    stream
        .set_nonblocking(false)
        .context("set stream blocking")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .context("set read timeout")?;

    // Run the main session loop (handles reconnect)
    run_session(session_uuid, socket_path, &listener, stream, timeout_secs)
}

/// Wait for a Hub connection, checking socket-as-lease periodically.
fn wait_for_connection(
    listener: &UnixListener,
    timeout: Duration,
    socket_path: &Path,
) -> Result<UnixStream> {
    let deadline = std::time::Instant::now() + timeout;

    loop {
        match listener.accept() {
            Ok((stream, _)) => return Ok(stream),
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                // Check lease
                if !socket_path.exists() {
                    bail!("socket file deleted — exiting");
                }
                // Check timeout
                if std::time::Instant::now() >= deadline {
                    bail!("no hub connection within timeout");
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => bail!("accept error: {e}"),
        }
    }
}

/// Main session loop: handshake, spawn PTY, relay I/O.
/// Handles hub disconnect + reconnect within the timeout window.
fn run_session(
    session_uuid: &str,
    socket_path: &Path,
    listener: &UnixListener,
    mut stream: UnixStream,
    timeout_secs: u64,
) -> Result<()> {
    // Handshake: receive hello, send welcome (with placeholder metadata pre-spawn)
    let hub_version = handshake_session(
        &mut stream,
        &SessionMetadata {
            session_uuid: session_uuid.to_string(),
            pid: 0, // will be updated after spawn
            rows: 24,
            cols: 80,
            last_output_at: 0,
        },
    )
    .context("initial handshake")?;
    log::info!(
        "[session {}] hub connected (protocol v{})",
        &session_uuid[..session_uuid.len().min(16)],
        hub_version
    );

    // Read spawn config from hub (first frame must be a JSON config)
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .context("set config read timeout")?;
    let mut decoder = FrameDecoder::new();
    let config = read_spawn_config(&mut stream, &mut decoder)?;

    // Create PTY
    let pty_system = portable_pty::native_pty_system();
    let pair = pty_system
        .openpty(portable_pty::PtySize {
            rows: config.rows,
            cols: config.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("open PTY")?;

    // Build command
    let mut cmd = portable_pty::CommandBuilder::new(&config.command);
    cmd.args(&config.args);
    if let Some(ref cwd) = config.cwd {
        cmd.cwd(cwd);
    }
    for (key, val) in &config.env {
        cmd.env(key, val);
    }

    // Spawn child
    let child = pair.slave.spawn_command(cmd).context("spawn child")?;
    let child_pid = child.process_id().unwrap_or(0);
    drop(pair.slave); // close slave side

    log::info!(
        "[session {}] spawned child pid={} cmd={}",
        &session_uuid[..session_uuid.len().min(16)],
        child_pid,
        config.command
    );

    // Get master fd for the reader thread
    let master_fd = pair.master.as_raw_fd().context("get PTY master fd")?;
    let writer = pair.master.take_writer().context("take PTY writer")?;

    // Set up shared state
    let current_dims = Arc::new(Mutex::new((config.rows, config.cols)));

    // Event channel for ghostty callbacks → reader thread → hub.
    // Callbacks fire inside process() (parser mutex held), so we use
    // a lock-free mpsc channel to avoid deadlock.
    let (_event_tx, event_rx) = std::sync::mpsc::sync_channel::<VtEvent>(64);

    let title_changed_flag = Arc::new(AtomicBool::new(false));
    let bell_flag = Arc::new(AtomicBool::new(false));
    let pwd_changed_flag = Arc::new(AtomicBool::new(false));
    let callback_flags = SessionCallbackFlags::from_env();
    log::info!(
        "[session] enabled ghostty callbacks: notification={}, semantic_prompt={}, mode_changed={}, kitty_keyboard_changed={}",
        callback_flags.notification,
        callback_flags.semantic_prompt,
        callback_flags.mode_changed,
        callback_flags.kitty_keyboard_changed
    );

    let parser = {
        let write_fd = master_fd;
        let title_flag = Arc::clone(&title_changed_flag);
        let bell_flag_cb = Arc::clone(&bell_flag);
        let pwd_flag = Arc::clone(&pwd_changed_flag);
        let notif_tx = _event_tx.clone();
        let prompt_tx = _event_tx.clone();
        let mode_tx = _event_tx.clone();
        let kitty_tx = _event_tx.clone();

        let callbacks = CallbackConfig {
            write_pty: Some(Box::new(move |data: &[u8]| unsafe {
                libc::write(write_fd, data.as_ptr().cast(), data.len());
            })),
            title_changed: Some(Box::new(move |_title: &str| {
                title_flag.store(true, Ordering::Release);
            })),
            bell: Some(Box::new(move || {
                bell_flag_cb.store(true, Ordering::Release);
            })),
            pwd_changed: Some(Box::new(move || {
                pwd_flag.store(true, Ordering::Release);
            })),
            notification: callback_flags.notification.then(|| {
                Box::new(move |title: &str, body: &str| {
                    let _ = notif_tx.try_send(VtEvent::Notification {
                        title: title.to_string(),
                        body: body.to_string(),
                    });
                }) as Box<dyn FnMut(&str, &str) + Send>
            }),
            semantic_prompt: callback_flags.semantic_prompt.then(|| {
                Box::new(move |action| {
                    let _ = prompt_tx.try_send(VtEvent::SemanticPrompt(action));
                })
                    as Box<dyn FnMut(crate::ghostty_vt::GhosttySemanticPromptAction) + Send>
            }),
            mode_changed: callback_flags.mode_changed.then(|| {
                Box::new(move |mode: u16, enabled: bool| {
                    let _ = mode_tx.try_send(VtEvent::ModeChanged { mode, enabled });
                }) as Box<dyn FnMut(u16, bool) + Send>
            }),
            kitty_keyboard_changed: callback_flags.kitty_keyboard_changed.then(|| {
                Box::new(move || {
                    let _ = kitty_tx.try_send(VtEvent::KittyKeyboardChanged);
                }) as Box<dyn FnMut() + Send>
            }),
        };
        let mut parser = TerminalParser::new_with_callbacks(
            config.rows,
            config.cols,
            DEFAULT_SCROLLBACK_LINES,
            callbacks,
        );
        let mut color_cache = std::collections::HashMap::new();
        if let Some(color) = config.default_foreground {
            color_cache.insert(256usize, color);
        }
        if let Some(color) = config.default_background {
            color_cache.insert(257usize, color);
        }
        if let Some(color) = config.default_cursor {
            color_cache.insert(258usize, color);
        }
        for (index, color) in config.palette_colors {
            color_cache.insert(index as usize, color);
        }
        parser.apply_color_cache_map(&color_cache);
        Arc::new(Mutex::new(parser))
    };
    let last_output_at = Arc::new(AtomicU64::new(0));
    let resize_pending = Arc::new(AtomicBool::new(false));
    let shutdown = Arc::new(AtomicBool::new(false));
    let tee: SharedTee = Arc::new(Mutex::new(
        config
            .tee_path
            .as_ref()
            .and_then(|p| Tee::new(Path::new(p), config.tee_cap).ok()),
    ));

    // Writer thread — owns the writer and master_pty (for resize ioctl)
    let (writer_tx, writer_rx) = std::sync::mpsc::sync_channel::<PtyWriteCommand>(64);
    let parser_for_writer = Arc::clone(&parser);
    let current_dims_for_writer = Arc::clone(&current_dims);
    let resize_pending_writer = Arc::clone(&resize_pending);
    let master_pty = pair.master;
    let init_commands = config.init_commands.clone();
    let _writer_thread = thread::Builder::new()
        .name("session-writer".to_string())
        .spawn(move || {
            pty_writer_loop(
                writer,
                master_pty,
                parser_for_writer,
                current_dims_for_writer,
                resize_pending_writer,
                init_commands,
                writer_rx,
            );
        })
        .context("spawn writer thread")?;

    // Reader thread: reads PTY output, feeds parser, forwards to hub
    let parser_for_reader = Arc::clone(&parser);
    let last_output_reader = Arc::clone(&last_output_at);
    let tee_for_reader = Arc::clone(&tee);
    let shutdown_for_reader = Arc::clone(&shutdown);
    let title_flag_reader = Arc::clone(&title_changed_flag);
    let bell_flag_reader = Arc::clone(&bell_flag);
    let pwd_flag_reader = Arc::clone(&pwd_changed_flag);
    let (output_tx, output_rx) = std::sync::mpsc::sync_channel::<SessionOutput>(256);
    let output_tx_child = output_tx.clone();
    let _reader_thread = thread::Builder::new()
        .name("session-reader".to_string())
        .spawn(move || {
            reader_loop(
                master_fd,
                parser_for_reader,
                last_output_reader,
                tee_for_reader,
                output_tx,
                shutdown_for_reader,
                title_flag_reader,
                bell_flag_reader,
                pwd_flag_reader,
                event_rx,
            );
        })
        .context("spawn reader thread")?;

    // Child-waiter thread: waits for child exit, sends FRAME_PROCESS_EXITED
    let shutdown_for_child = Arc::clone(&shutdown);
    let _child_thread = thread::Builder::new()
        .name("session-child-waiter".to_string())
        .spawn(move || {
            let mut child = child;
            let exit_code = match child.wait() {
                Ok(status) => Some(status.exit_code() as i32),
                Err(e) => {
                    log::warn!("[session] child wait error: {e}");
                    None
                }
            };
            log::info!("[session] child exited (code={:?})", exit_code);
            shutdown_for_child.store(true, Ordering::Release);
            let _ = output_tx_child.try_send(SessionOutput::ChildExited(exit_code));
        })
        .context("spawn child waiter thread")?;

    // Socket-as-lease watcher
    let socket_path_owned = socket_path.to_owned();
    let shutdown_for_lease = Arc::clone(&shutdown);
    let _lease_thread = thread::Builder::new()
        .name("session-lease".to_string())
        .spawn(move || loop {
            thread::sleep(Duration::from_secs(2));
            if shutdown_for_lease.load(Ordering::Relaxed) {
                break;
            }
            if !socket_path_owned.exists() {
                // Gather context: does the parent directory still exist?
                let parent_exists = socket_path_owned
                    .parent()
                    .map(|p| p.exists())
                    .unwrap_or(false);
                let parent_contents = socket_path_owned.parent().and_then(|p| {
                    std::fs::read_dir(p).ok().map(|entries| {
                        let names: Vec<_> = entries
                            .filter_map(|e| e.ok().and_then(|e| e.file_name().into_string().ok()))
                            .take(20)
                            .collect();
                        let count = names.len();
                        let joined = names.join(", ");
                        if count == 20 {
                            format!("{joined}, ... (truncated)")
                        } else {
                            joined
                        }
                    })
                });
                log::warn!(
                    "[session] socket file deleted — shutting down \
                     (parent_dir_exists={}, parent_contents=[{}], socket_path={})",
                    parent_exists,
                    parent_contents.unwrap_or_default(),
                    socket_path_owned.display()
                );
                shutdown_for_lease.store(true, Ordering::Release);
                break;
            }
        })
        .context("spawn lease watcher")?;

    // Main I/O relay loop
    stream.set_read_timeout(Some(Duration::from_millis(50)))?;
    let mut hub_decoder = FrameDecoder::new();
    let mut read_buf = [0u8; 8192];

    let mut exit_reason = "shutdown_flag";
    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        // Forward PTY output / child exit to hub
        while let Ok(msg) = output_rx.try_recv() {
            let frame = match msg {
                SessionOutput::PtyData(data) => encode_frame(FRAME_PTY_OUTPUT, &data),
                SessionOutput::ChildExited(code) => {
                    match encode_json(
                        FRAME_PROCESS_EXITED,
                        &serde_json::json!({"exit_code": code}),
                    ) {
                        Ok(f) => f,
                        Err(_) => encode_frame(FRAME_PROCESS_EXITED, b"{}"),
                    }
                }
                SessionOutput::EventFrame(frame) => frame,
            };
            if stream.write_all(&frame).is_err() {
                // Hub disconnected
                log::info!("[session] hub disconnected (write error)");
                break;
            }
        }

        // Read hub commands
        match stream.read(&mut read_buf) {
            Ok(0) => {
                log::info!("[session] hub disconnected (EOF)");
                // Wait for reconnect
                match wait_for_reconnect(
                    listener,
                    Duration::from_secs(timeout_secs),
                    socket_path,
                    &shutdown,
                ) {
                    Ok(new_stream) => {
                        stream = new_stream;
                        stream.set_nonblocking(false)?;
                        stream.set_read_timeout(Some(Duration::from_millis(50)))?;

                        // Re-handshake
                        let (rows, cols) =
                            current_dims.lock().map(|dims| *dims).unwrap_or((24, 80));
                        let _ = handshake_session(
                            &mut stream,
                            &SessionMetadata {
                                session_uuid: session_uuid.to_string(),
                                pid: child_pid,
                                rows,
                                cols,
                                last_output_at: last_output_at.load(Ordering::Relaxed),
                            },
                        );
                        hub_decoder = FrameDecoder::new();
                        log::info!("[session] hub reconnected");
                        continue;
                    }
                    Err(e) => {
                        log::info!("[session] no reconnect: {e}");
                        exit_reason = "reconnect_failed";
                        break;
                    }
                }
            }
            Ok(n) => {
                for frame in hub_decoder.feed(&read_buf[..n]) {
                    handle_hub_frame(
                        &frame,
                        &writer_tx,
                        &parser,
                        &resize_pending,
                        &tee,
                        &mut stream,
                        &shutdown,
                    );
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                // Timeout — check for output and loop
            }
            Err(e) => {
                log::warn!("[session] hub read error: {e}");
                exit_reason = "hub_read_error";
                break;
            }
        }
    }

    // Clean shutdown
    shutdown.store(true, Ordering::Release);
    let _ = writer_tx.send(PtyWriteCommand::Shutdown);

    // Kill child process group so descendants (e.g. codex, claude) don't orphan
    if child_pid > 0 {
        let pgid = child_pid as i32;
        log::info!("[session] sending SIGTERM to process group {pgid}");
        unsafe { libc::killpg(pgid, libc::SIGTERM); }
        thread::sleep(Duration::from_millis(500));
        unsafe { libc::killpg(pgid, libc::SIGKILL); }
        log::info!("[session] sent SIGKILL to process group {pgid}");
    }

    let socket_existed = socket_path.exists();
    if socket_existed {
        std::fs::remove_file(socket_path).ok();
    }
    log::info!(
        "[session {}] exiting (reason={}, socket_existed={})",
        &session_uuid[..session_uuid.len().min(16)],
        exit_reason,
        socket_existed
    );
    Ok(())
}

/// Wait for a hub reconnection during the timeout window.
fn wait_for_reconnect(
    listener: &UnixListener,
    timeout: Duration,
    socket_path: &Path,
    shutdown: &AtomicBool,
) -> Result<UnixStream> {
    listener.set_nonblocking(true)?;
    let deadline = std::time::Instant::now() + timeout;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            bail!("shutdown requested");
        }
        match listener.accept() {
            Ok((stream, _)) => return Ok(stream),
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                if !socket_path.exists() {
                    bail!("socket file deleted");
                }
                if std::time::Instant::now() >= deadline {
                    bail!("reconnect timeout expired");
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => bail!("accept error: {e}"),
        }
    }
}

/// Read the spawn config (first frame from hub after handshake).
fn read_spawn_config(stream: &mut UnixStream, decoder: &mut FrameDecoder) -> Result<SpawnConfig> {
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).context("read spawn config")?;
        if n == 0 {
            bail!("hub disconnected before sending spawn config");
        }
        for frame in decoder.feed(&buf[..n]) {
            if frame.frame_type == FRAME_PTY_INPUT {
                // First frame is repurposed as spawn config JSON
                return frame.json::<SpawnConfig>().context("parse spawn config");
            }
        }
    }
}

/// Handle a single frame from the Hub.
fn handle_hub_frame(
    frame: &Frame,
    writer_tx: &std::sync::mpsc::SyncSender<PtyWriteCommand>,
    parser: &Arc<Mutex<TerminalParser>>,
    resize_pending: &AtomicBool,
    tee: &SharedTee,
    stream: &mut UnixStream,
    shutdown: &AtomicBool,
) {
    match frame.frame_type {
        FRAME_PTY_INPUT => {
            let probe_descriptions =
                crate::hub::terminal_profile::describe_probe_sequences(&frame.payload);
            if !probe_descriptions.is_empty() {
                log::info!(
                    "[session][PTY-PROBE] hub injected input into PTY: {}",
                    probe_descriptions.join(", ")
                );
            }
            let _ = writer_tx.try_send(PtyWriteCommand::Input(frame.payload.clone()));
        }

        FRAME_RESIZE => {
            if let Ok(resize) = frame.json::<serde_json::Value>() {
                let rows = resize["rows"].as_u64().unwrap_or(24) as u16;
                let cols = resize["cols"].as_u64().unwrap_or(80) as u16;
                resize_pending.store(true, Ordering::Release);
                let _ = writer_tx.send(PtyWriteCommand::Resize { rows, cols });
            }
        }

        FRAME_GET_SNAPSHOT => {
            // Single-call terminal snapshot: one opaque blob with everything.
            let snapshot = parser
                .lock()
                .map(|p| {
                    p.terminal().snapshot_export().unwrap_or_else(|| {
                        log::error!("[session] snapshot_export failed");
                        Vec::new()
                    })
                })
                .unwrap_or_default();
            let response = encode_frame(FRAME_SNAPSHOT, &snapshot);
            let _ = stream.write_all(&response);
        }

        FRAME_GET_SCREEN => {
            let text = parser.lock().map(|p| p.contents()).unwrap_or_default();
            let response = encode_frame(FRAME_SCREEN, text.as_bytes());
            let _ = stream.write_all(&response);
        }

        FRAME_GET_MODE_FLAGS => {
            let flags = parser
                .lock()
                .map(|p| ModeFlags {
                    kitty_enabled: p.kitty_enabled(),
                    cursor_visible: !p.cursor_hidden(),
                    bracketed_paste: p.bracketed_paste(),
                    mouse_mode: p.mouse_mode(),
                    alt_screen: p.alt_screen_active(),
                    focus_reporting: p.focus_reporting(),
                    application_cursor: p.application_cursor(),
                })
                .unwrap_or_default();
            if let Ok(response) = encode_json(FRAME_MODE_FLAGS, &flags) {
                let _ = stream.write_all(&response);
            }
        }

        FRAME_SET_COLOR_PROFILE => {
            match frame.json::<TerminalColorProfile>() {
                Ok(profile) => {
                    if let Ok(mut parser) = parser.lock() {
                        let bg = profile.colors.get(&257usize).copied();
                        log::debug!(
                            "[PTY-PROFILE] session applying color profile colors={} bg={:?}",
                            profile.colors.len(),
                            bg
                        );
                        parser.apply_color_cache_map(&profile.colors);
                    }
                }
                Err(error) => {
                    log::warn!("[session] invalid color profile payload: {error:#}");
                }
            }
        }

        FRAME_ARM_TEE => {
            if let Ok(config) = frame.json::<serde_json::Value>() {
                let path = config["log_path"].as_str().unwrap_or("");
                let cap = config["cap_bytes"].as_u64().unwrap_or(10 * 1024 * 1024);
                if !path.is_empty() {
                    match Tee::new(Path::new(path), cap) {
                        Ok(new_tee) => {
                            if let Ok(mut guard) = tee.lock() {
                                *guard = Some(new_tee);
                            }
                            log::info!("[session] tee armed: {path}");
                        }
                        Err(e) => log::warn!("[session] tee arm failed: {e}"),
                    }
                }
            }
        }

        FRAME_PING => {
            let response = encode_empty(FRAME_PONG);
            let _ = stream.write_all(&response);
        }

        FRAME_SHUTDOWN => {
            log::info!("[session] shutdown requested by hub");
            shutdown.store(true, Ordering::Release);
        }

        FRAME_SET_TIMEOUT => {
            // Acknowledged but timeout is set at process start; runtime changes
            // would require more infrastructure. Log for now.
            log::debug!("[session] set_timeout frame received (not yet implemented at runtime)");
        }

        _ => {
            log::debug!("[session] unknown frame type 0x{:02x}", frame.frame_type);
        }
    }
}

// ─── Reader loop ─────────────────────────────────────────────────────────────

/// Convert a Ghostty semantic prompt action to its transport name.
fn semantic_prompt_name(action: crate::ghostty_vt::GhosttySemanticPromptAction) -> &'static str {
    use crate::ghostty_vt::GhosttySemanticPromptAction;

    match action {
        GhosttySemanticPromptAction::FreshLine => "fresh_line",
        GhosttySemanticPromptAction::FreshLineNewPrompt => "fresh_line_new_prompt",
        GhosttySemanticPromptAction::NewCommand => "new_command",
        GhosttySemanticPromptAction::PromptStart => "prompt_start",
        GhosttySemanticPromptAction::EndPromptStartInput => "end_prompt_start_input",
        GhosttySemanticPromptAction::EndPromptStartInputTerminateEol => {
            "end_prompt_start_input_terminate_eol"
        }
        GhosttySemanticPromptAction::EndInputStartOutput => "end_input_start_output",
        GhosttySemanticPromptAction::EndCommand => "end_command",
    }
}

/// Read PTY output, feed parser, forward to hub via channel.
/// Terminal state change events are driven entirely by ghostty callbacks —
/// no byte scanning or mode diffing in Rust.
fn reader_loop(
    fd: RawFd,
    parser: Arc<Mutex<TerminalParser>>,
    last_output_at: Arc<AtomicU64>,
    tee: SharedTee,
    output_tx: std::sync::mpsc::SyncSender<SessionOutput>,
    shutdown: Arc<AtomicBool>,
    title_changed_flag: Arc<AtomicBool>,
    bell_flag: Arc<AtomicBool>,
    pwd_changed_flag: Arc<AtomicBool>,
    event_rx: std::sync::mpsc::Receiver<VtEvent>,
) {
    let mut buf = [0u8; 4096];
    let mut file = ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        match file.read(&mut buf) {
            Ok(0) => {
                log::info!("[session] PTY reader got EOF");
                shutdown.store(true, Ordering::Release);
                break;
            }
            Err(e) => {
                log::warn!("[session] PTY read error: {e}");
                shutdown.store(true, Ordering::Release);
                break;
            }
            Ok(n) => {
                let data = &buf[..n];

                // Feed parser — ghostty callbacks fire during process()
                if let Ok(mut p) = parser.lock() {
                    p.process(data);

                    // Title changed (flag set by ghostty callback)
                    if title_changed_flag.swap(false, Ordering::Acquire) {
                        let title = p.terminal().title();
                        let _ = output_tx.try_send(SessionOutput::EventFrame(encode_string(
                            FRAME_TITLE_CHANGED,
                            &title,
                        )));
                    }

                    // PWD changed (flag set by ghostty OSC 7 callback)
                    // ghostty stores the raw OSC 7 URI; extract the path portion.
                    if pwd_changed_flag.swap(false, Ordering::Acquire) {
                        let raw = p.terminal().pwd();
                        let path = if let Some(rest) = raw.strip_prefix("file://") {
                            rest.find('/').map(|i| &rest[i..]).unwrap_or(&raw)
                        } else {
                            &raw
                        };
                        if !path.is_empty() {
                            let _ = output_tx.try_send(SessionOutput::EventFrame(encode_string(
                                FRAME_CWD_CHANGED,
                                path,
                            )));
                        }
                    }
                }

                // Bell (flag set by ghostty callback)
                if bell_flag.swap(false, Ordering::Acquire) {
                    let _ = output_tx.try_send(SessionOutput::EventFrame(encode_empty(FRAME_BELL)));
                }

                // Drain events from ghostty callbacks (notification, prompt, mode)
                while let Ok(event) = event_rx.try_recv() {
                    let frame = match event {
                        VtEvent::Notification { title, body } => {
                            encode_json(FRAME_NOTIFICATION, &NotificationPayload { title, body })
                        }
                        VtEvent::SemanticPrompt(action) => encode_json(
                            FRAME_PROMPT_MARK,
                            &PromptMarkPayload {
                                mark: semantic_prompt_name(action).to_string(),
                            },
                        ),
                        VtEvent::KittyKeyboardChanged => {
                            let kitty = parser.lock().map(|p| p.kitty_enabled()).unwrap_or(false);
                            let mut changed = ModeChanged::default();
                            changed.kitty_enabled = Some(kitty);
                            encode_json(FRAME_MODE_CHANGED, &changed)
                        }
                        VtEvent::ModeChanged { mode, enabled } => {
                            use crate::ghostty_vt::*;
                            let mut changed = ModeChanged::default();
                            match mode {
                                MODE_CURSOR_VISIBLE => changed.cursor_visible = Some(enabled),
                                MODE_ALT_SCREEN_SAVE => changed.alt_screen = Some(enabled),
                                MODE_NORMAL_MOUSE | MODE_BUTTON_MOUSE | MODE_ANY_MOUSE => {
                                    if let Ok(p) = parser.lock() {
                                        changed.mouse_mode = Some(p.mouse_mode());
                                    }
                                }
                                MODE_BRACKETED_PASTE => changed.bracketed_paste = Some(enabled),
                                MODE_FOCUS_EVENT => changed.focus_reporting = Some(enabled),
                                MODE_DECCKM => changed.application_cursor = Some(enabled),
                                _ => continue,
                            };
                            encode_json(FRAME_MODE_CHANGED, &changed)
                        }
                    };
                    if let Ok(frame) = frame {
                        let _ = output_tx.try_send(SessionOutput::EventFrame(frame));
                    }
                }

                // Update idle timestamp
                last_output_at.store(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                    Ordering::Relaxed,
                );

                // Write to tee
                if let Ok(mut guard) = tee.lock() {
                    if let Some(ref mut t) = *guard {
                        t.write_data(data);
                    }
                }

                // Forward to hub (drop if channel full — continuous stream)
                let _ = output_tx.try_send(SessionOutput::PtyData(data.to_vec()));
            }
        }
    }
}

// ─── Writer loop ─────────────────────────────────────────────────────────────

/// Receive commands from hub, write input / apply resize to PTY.
///
/// Owns the master PTY for resize ioctl and the writer for stdin.
/// Writes init_commands to the PTY immediately on start.
fn pty_writer_loop(
    mut writer: Box<dyn Write + Send>,
    master_pty: Box<dyn portable_pty::MasterPty + Send>,
    parser: Arc<Mutex<TerminalParser>>,
    current_dims: Arc<Mutex<(u16, u16)>>,
    resize_pending: Arc<AtomicBool>,
    init_commands: Vec<String>,
    rx: std::sync::mpsc::Receiver<PtyWriteCommand>,
) {
    // Write init commands (e.g., "source init.sh") to PTY stdin
    for cmd in &init_commands {
        let line = if cmd.ends_with('\n') {
            cmd.clone()
        } else {
            format!("{cmd}\n")
        };
        if let Err(e) = writer.write_all(line.as_bytes()) {
            log::warn!("[session] init command write error: {e}");
            break;
        }
    }
    if !init_commands.is_empty() {
        let _ = writer.flush();
        log::info!("[session] wrote {} init command(s)", init_commands.len());
    }

    while let Ok(cmd) = rx.recv() {
        match cmd {
            PtyWriteCommand::Input(data) => {
                if let Err(e) = writer.write_all(&data) {
                    log::warn!("[session] PTY write error: {e}");
                    break;
                }
                let _ = writer.flush();
            }
            PtyWriteCommand::Resize { rows, cols } => {
                // Coalesce: drain pending resizes, apply only the last
                let (mut final_rows, mut final_cols) = (rows, cols);
                while let Ok(PtyWriteCommand::Resize { rows: r, cols: c }) = rx.try_recv() {
                    final_rows = r;
                    final_cols = c;
                }
                resize_pending.store(true, Ordering::Release);
                // Resize the actual PTY (sends SIGWINCH to child)
                if let Err(e) = master_pty.resize(portable_pty::PtySize {
                    rows: final_rows,
                    cols: final_cols,
                    pixel_width: 0,
                    pixel_height: 0,
                }) {
                    log::warn!("[session] PTY resize ioctl failed: {e}");
                }
                // Resize the parser to match
                if let Ok(mut p) = parser.lock() {
                    p.resize(final_rows, final_cols);
                }
                if let Ok(mut dims) = current_dims.lock() {
                    *dims = (final_rows, final_cols);
                }
                log::debug!("[session] resize to {}x{}", final_cols, final_rows);
            }
            PtyWriteCommand::Shutdown => break,
        }
    }
}

// ─── Socket path helpers ─────────────────────────────────────────────────────

/// Directory for session sockets.
pub fn sessions_socket_dir() -> Result<PathBuf> {
    let uid = unsafe { libc::getuid() };
    let dir = PathBuf::from(format!("/tmp/botster-{uid}/sessions"));
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create sessions dir: {}", dir.display()))?;
    Ok(dir)
}

/// Socket path for a specific session.
pub fn session_socket_path(session_uuid: &str) -> Result<PathBuf> {
    let dir = sessions_socket_dir()?;
    Ok(dir.join(format!("{session_uuid}.sock")))
}

/// Discover all live session sockets by scanning the directory.
pub fn discover_sessions() -> Result<Vec<PathBuf>> {
    let dir = sessions_socket_dir()?;
    let mut sockets = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "sock").unwrap_or(false) {
                sockets.push(path);
            }
        }
    }
    Ok(sockets)
}
