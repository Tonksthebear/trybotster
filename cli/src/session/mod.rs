//! Per-session process: holds a single PTY fd, binds a Unix socket, relays bytes.
//!
//! Replaces the broker's multiplexed architecture with one process per PTY.
//! Each session process is spawned by the Hub and communicates over its own
//! Unix socket. The Hub connects, performs a handshake, and streams I/O.
//!
//! # Responsibilities
//!
//! - **PTY ownership**: creates the PTY via `portable_pty`, owns the master fd
//! - **Socket server**: binds a Unix socket, accepts one connection (the Hub)
//! - **Reader thread**: reads PTY output → feeds alacritty parser → forwards to Hub
//! - **Writer thread**: receives input from Hub → writes to PTY master fd
//! - **Terminal state**: alacritty parser tracks mode flags (kitty, cursor, mouse, etc.)
//! - **Snapshot generation**: generates ANSI snapshots from parser state on request
//! - **Tee/logging**: optional file tee with rotation
//! - **Lifecycle**: exits when socket file is deleted or child process dies
//!
//! # What it does NOT do
//!
//! - No byte scanning (OSC 7, OSC 133, notifications — Hub handles these)
//! - No client routing (Hub manages TUI, browser, socket clients)
//! - No multiplexing (one socket, one session)

pub mod connection;
pub mod protocol;

use std::io::{self, Read, Write};
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{mem::ManuallyDrop, thread};

use anyhow::{bail, Context, Result};

use crate::terminal::{generate_ansi_snapshot, AlacrittyParser, DEFAULT_SCROLLBACK_LINES};
use alacritty_terminal::event::{Event as AlacrittyEvent, EventListener};

use protocol::*;

// ─── Session event listener (for alacritty parser) ───────────────────────────

/// Event listener for the session process's alacritty parser.
///
/// Handles `PtyWrite` responses (DSR/DA) by writing directly back to the PTY fd.
/// All other events (Title, Bell) are ignored — the Hub's own parser handles those.
#[derive(Clone)]
struct SessionEventListener {
    pty_fd: RawFd,
}

impl EventListener for SessionEventListener {
    fn send_event(&self, event: AlacrittyEvent) {
        if let AlacrittyEvent::PtyWrite(response) = event {
            let bytes = response.as_bytes();
            unsafe {
                libc::write(self.pty_fd, bytes.as_ptr().cast(), bytes.len());
            }
        }
    }
}

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

// ─── Session process entry point ─────────────────────────────────────────────

/// Configuration for spawning a session process.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SpawnConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<String>,
    pub rows: u16,
    pub cols: u16,
    /// Commands to write to PTY stdin after child spawns (e.g., "source init.sh").
    #[serde(default)]
    pub init_commands: Vec<String>,
    pub tee_path: Option<String>,
    pub tee_cap: u64,
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
    stream.set_nonblocking(false).context("set stream blocking")?;
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
    let master_fd = pair
        .master
        .as_raw_fd()
        .context("get PTY master fd")?;
    let writer = pair.master.take_writer().context("take PTY writer")?;

    // Set up shared state
    let parser = {
        let listener = SessionEventListener { pty_fd: master_fd };
        Arc::new(Mutex::new(AlacrittyParser::new_with_listener(
            config.rows,
            config.cols,
            DEFAULT_SCROLLBACK_LINES,
            listener,
        )))
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
    let resize_pending_writer = Arc::clone(&resize_pending);
    let master_pty = pair.master;
    let init_commands = config.init_commands.clone();
    let _writer_thread = thread::Builder::new()
        .name("session-writer".to_string())
        .spawn(move || {
            pty_writer_loop(writer, master_pty, parser_for_writer, resize_pending_writer, init_commands, writer_rx);
        })
        .context("spawn writer thread")?;

    // Reader thread: reads PTY output, feeds parser, forwards to hub
    let parser_for_reader = Arc::clone(&parser);
    let last_output_reader = Arc::clone(&last_output_at);
    let tee_for_reader = Arc::clone(&tee);
    let shutdown_for_reader = Arc::clone(&shutdown);
    let (output_tx, output_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(256);
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
            );
        })
        .context("spawn reader thread")?;

    // Socket-as-lease watcher
    let socket_path_owned = socket_path.to_owned();
    let shutdown_for_lease = Arc::clone(&shutdown);
    let _lease_thread = thread::Builder::new()
        .name("session-lease".to_string())
        .spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(2));
                if shutdown_for_lease.load(Ordering::Relaxed) {
                    break;
                }
                if !socket_path_owned.exists() {
                    log::info!("[session] socket file deleted — shutting down");
                    shutdown_for_lease.store(true, Ordering::Release);
                    break;
                }
            }
        })
        .context("spawn lease watcher")?;

    // Main I/O relay loop
    stream.set_read_timeout(Some(Duration::from_millis(50)))?;
    let mut hub_decoder = FrameDecoder::new();
    let mut read_buf = [0u8; 8192];

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        // Forward PTY output to hub
        while let Ok(data) = output_rx.try_recv() {
            let frame = encode_frame(FRAME_PTY_OUTPUT, &data);
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
                        let _ = handshake_session(
                            &mut stream,
                            &SessionMetadata {
                                session_uuid: session_uuid.to_string(),
                                pid: child_pid,
                                rows: config.rows,
                                cols: config.cols,
                                last_output_at: last_output_at.load(Ordering::Relaxed),
                            },
                        );
                        hub_decoder = FrameDecoder::new();
                        log::info!("[session] hub reconnected");
                        continue;
                    }
                    Err(e) => {
                        log::info!("[session] no reconnect: {e}");
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
                break;
            }
        }
    }

    // Clean shutdown
    shutdown.store(true, Ordering::Release);
    let _ = writer_tx.send(PtyWriteCommand::Shutdown);
    if socket_path.exists() {
        std::fs::remove_file(socket_path).ok();
    }
    log::info!("[session {}] exiting", &session_uuid[..session_uuid.len().min(16)]);
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
    parser: &Arc<Mutex<AlacrittyParser<SessionEventListener>>>,
    resize_pending: &AtomicBool,
    tee: &SharedTee,
    stream: &mut UnixStream,
    shutdown: &AtomicBool,
) {
    match frame.frame_type {
        FRAME_PTY_INPUT => {
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
            // Never skip visible content — the session process's parser is
            // always up-to-date (reader thread feeds it continuously).
            // resize_pending is only relevant for the hub's shadow screen.
            let snapshot = parser
                .lock()
                .map(|p| generate_ansi_snapshot(&p, false))
                .unwrap_or_default();
            let response = encode_frame(FRAME_SNAPSHOT, &snapshot);
            let _ = stream.write_all(&response);
        }

        FRAME_GET_SCREEN => {
            let text = parser
                .lock()
                .map(|p| p.contents())
                .unwrap_or_default();
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
                })
                .unwrap_or_default();
            if let Ok(response) = encode_json(FRAME_MODE_FLAGS, &flags) {
                let _ = stream.write_all(&response);
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

/// Read PTY output, feed parser, forward to hub via channel.
fn reader_loop(
    fd: RawFd,
    parser: Arc<Mutex<AlacrittyParser<SessionEventListener>>>,
    last_output_at: Arc<AtomicU64>,
    tee: SharedTee,
    output_tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    shutdown: Arc<AtomicBool>,
) {
    let mut buf = [0u8; 4096];
    let mut file = ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        match file.read(&mut buf) {
            Ok(0) | Err(_) => {
                // PTY closed — child exited
                shutdown.store(true, Ordering::Release);
                break;
            }
            Ok(n) => {
                let data = &buf[..n];

                // Feed parser for terminal state tracking
                if let Ok(mut p) = parser.lock() {
                    p.process(data);
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
                let _ = output_tx.try_send(data.to_vec());
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
    parser: Arc<Mutex<AlacrittyParser<SessionEventListener>>>,
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
