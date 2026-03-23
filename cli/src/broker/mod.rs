//! PTY broker process — holds PTY file descriptors across Hub restarts.
//!
//! # Wire Format Compatibility
//!
//! Broker and Hub perform a control-plane handshake on connect:
//! - protocol `version`
//! - `capabilities` bitflags
//!
//! `FdTransferPayload` encoding is capability-gated. When both sides negotiate
//! `FD_TRANSFER_V1`, the payload carries an explicit format marker/version.
//! Otherwise the broker accepts legacy v0 payload bytes.
//!
//! # Purpose
//!
//! The broker is a lightweight process that outlives the Hub daemon. By
//! holding the master PTY FDs and raw output ring buffers, it allows agent
//! sessions to survive a Hub restart without the user's processes (Claude,
//! shells, etc.) being killed.
//!
//! # Architecture
//!
//! ```text
//! Hub  ──sendmsg(FdTransfer + SCM_RIGHTS FD)──► Broker
//!      ◄── BrokerMessage::Registered(session_id) ──
//!
//! Hub  ──PtyInput(session_id, bytes)──► Broker ──write──► PTY master
//! Hub  ◄──PtyOutput(session_id, bytes)──  Broker ◄──read── PTY master
//!
//! Hub disconnects → broker starts reconnect_timeout countdown
//! Hub reconnects  → Hub sends GetSnapshot(session_id) per session
//!                   Broker calls generate_ansi_snapshot() on its AlacrittyParser
//!                   Hub feeds the ANSI snapshot into a fresh shadow screen
//!
//! Timeout expires → broker kills children and exits
//! KillAll command → broker kills children and exits immediately
//! ```
//!
//! # Spawning
//!
//! The Hub spawns the broker with:
//! ```sh
//! botster broker --hub-id <id> [--timeout <secs>]
//! ```
//! The broker exits automatically when its timeout elapses without a Hub
//! reconnect, ensuring no orphan processes linger.
//!
//! # FD transfer (SCM_RIGHTS)
//!
//! `O_CLOEXEC` is process-scoped; it does **not** block `SCM_RIGHTS`
//! transfers across Unix domain sockets. No special handling is required
//! when sending a cloexec-flagged FD via `sendmsg`.
//!
//! Writing to a PTY master FD bypasses `portable_pty`'s private types by
//! using `ManuallyDrop<File>` for borrow-only access and
//! `ioctl(TIOCSWINSZ)` directly for resizes.

// Rust guideline compliant 2026-02

pub mod connection;
pub mod protocol;

#[cfg(test)]
mod integration_test_full;

pub(crate) use connection::{BrokerConnection, SharedBrokerConnection};

use crate::terminal::{generate_ansi_snapshot, AlacrittyParser, DEFAULT_SCROLLBACK_LINES};

use alacritty_terminal::event::{Event as AlacrittyEvent, EventListener};

use std::collections::HashMap;
use std::io::{Read, Write};
use std::mem::ManuallyDrop;
use std::os::unix::io::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use protocol::{
    control_handshake_server, encode_broker_control, encode_data, encode_pty_output, frame_type,
    sideband, BrokerFrameDecoder, BrokerMessage, BrokerSessionInventory, BrokerTermEvent,
    FdTransferPayload, HubMessage,
};

// ─── BrokerEventListener ──────────────────────────────────────────────────

/// Collected terminal events from the broker's alacritty parser.
type CollectedEvents = Arc<Mutex<Vec<BrokerTermEvent>>>;

/// Event listener that captures alacritty terminal events for forwarding to the Hub.
///
/// Installed on the broker's parser so `Event::Title`, `Event::ResetTitle`, and
/// `Event::Bell` are captured during `process()`. The reader thread drains the
/// queue after each chunk and sends them as `BrokerMessage::TermEvent` frames.
///
/// `Event::PtyWrite` responses (DSR, DA) are written directly to the PTY master fd.
#[derive(Clone)]
struct BrokerEventListener {
    collected: CollectedEvents,
    /// Raw PTY master fd for writing PtyWrite responses.
    pty_fd: RawFd,
}

impl EventListener for BrokerEventListener {
    fn send_event(&self, event: AlacrittyEvent) {
        match event {
            AlacrittyEvent::Title(title) => {
                if let Ok(mut events) = self.collected.lock() {
                    events.push(BrokerTermEvent::TitleChanged { title });
                }
            }
            AlacrittyEvent::ResetTitle => {
                if let Ok(mut events) = self.collected.lock() {
                    events.push(BrokerTermEvent::ResetTitle);
                }
            }
            AlacrittyEvent::Bell => {
                if let Ok(mut events) = self.collected.lock() {
                    events.push(BrokerTermEvent::Bell);
                }
            }
            AlacrittyEvent::PtyWrite(response) => {
                // Write DSR/DA responses directly to the PTY master fd.
                // Small writes to a PTY master are non-blocking.
                let bytes = response.as_bytes();
                unsafe {
                    libc::write(self.pty_fd, bytes.as_ptr().cast(), bytes.len());
                }
            }
            // Clipboard, color requests, cursor blink, wakeup, etc. — not relevant.
            _ => {}
        }
    }
}

/// Maximum path length for a Unix domain socket (macOS kernel limit).
const MAX_SOCK_PATH: usize = 104;

/// Default rotation cap: 10 MiB.
///
/// Matches the default passed from Lua (`10 * 1024 * 1024`).  Callers can
/// override via `HubMessage::ArmTee { cap_bytes }`.
const DEFAULT_TEE_CAP_BYTES: u64 = 10 * 1024 * 1024;

// ─── Tee ───────────────────────────────────────────────────────────────────

/// File tee attached to a PTY reader thread.
///
/// Appends a copy of every PTY output byte to `log_path`.  A single rotation
/// is applied when `bytes_written >= cap_bytes`:
///
/// - `pty-0.log` is renamed to `pty-0.log.1` (overwriting any prior `.1`).
/// - A fresh `pty-0.log` is opened.
///
/// Write failures set `degraded = true` and are logged; the read loop is
/// never crashed by tee I/O errors.
struct TeeState {
    /// Absolute path of the active log file (e.g., `.../pty-0.log`).
    log_path: PathBuf,
    /// Open file handle — append-only.
    file: std::fs::File,
    /// Bytes written to the current `log_path` (reset after rotation).
    bytes_written: u64,
    /// Maximum bytes before rotation.
    cap_bytes: u64,
    /// Set on the first write failure; subsequent writes are skipped.
    degraded: bool,
}

impl TeeState {
    /// Open (or create) the tee log file and return a ready [`TeeState`].
    ///
    /// Creates any missing parent directories.  Initialises `bytes_written`
    /// from the existing file length so rotation accounting is correct when
    /// the Hub re-arms an existing session.
    ///
    /// # Errors
    ///
    /// Returns an error if directory creation or file open fails.
    fn open(log_path: PathBuf, cap_bytes: u64) -> anyhow::Result<Self> {
        if let Some(dir) = log_path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("create tee log dir: {}", dir.display()))?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("open tee log: {}", log_path.display()))?;
        // Track pre-existing bytes so rotation fires at the right threshold.
        let bytes_written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            log_path,
            file,
            bytes_written,
            cap_bytes,
            degraded: false,
        })
    }

    /// Write `data` to the tee, rotating at `cap_bytes` first if needed.
    ///
    /// Sets `degraded = true` on any I/O failure; subsequent calls are
    /// no-ops so the reader loop can continue without special casing.
    fn write_data(&mut self, data: &[u8]) {
        if self.degraded {
            return;
        }

        // Rotate before writing if the cap would be reached.
        if self.bytes_written + data.len() as u64 >= self.cap_bytes {
            let rotated = PathBuf::from(format!("{}.1", self.log_path.display()));
            // Rename current → .1 (overwrite any prior rotation).  Ignore
            // rename errors — we still try to open a fresh file below.
            let _ = std::fs::rename(&self.log_path, &rotated);
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.log_path)
            {
                Ok(fresh) => {
                    self.file = fresh;
                    self.bytes_written = 0;
                    log::info!("[broker] tee rotated: {}", self.log_path.display());
                }
                Err(e) => {
                    log::error!(
                        "[broker] tee rotation failed for {}: {e}",
                        self.log_path.display()
                    );
                    self.degraded = true;
                    return;
                }
            }
        }

        if let Err(e) = self.file.write_all(data) {
            log::error!(
                "[broker] tee write failed for {}: {e}",
                self.log_path.display()
            );
            self.degraded = true;
            return;
        }
        self.bytes_written += data.len() as u64;
    }
}

/// Broker-global tee state for a session, shared between the main thread
/// (which arms it on `ArmTee`) and the reader thread (which writes it).
///
/// `None` until `ArmTee` is received; may be replaced by a later `ArmTee`.
type SharedTee = Arc<Mutex<Option<TeeState>>>;

/// Per-session bounded queue depth for PTY write/resize commands.
///
/// Keeps broker memory bounded if the Hub sends input faster than the PTY can
/// consume it.
const PTY_WRITER_QUEUE_CAPACITY: usize = 1024;
/// Per-connection bounded queue depth for broker -> Hub PTY output.
const BROKER_OUTPUT_QUEUE_CAPACITY: usize = 256;

/// Commands processed by the per-session PTY writer thread.
enum PtyWriteCommand {
    Input(Vec<u8>),
    Resize { rows: u16, cols: u16 },
    Shutdown,
}

// ─── Session ───────────────────────────────────────────────────────────────

/// Broker-side state for a single PTY session.
struct Session {
    #[allow(dead_code)] // stored for diagnostics / future use
    session_id: u32,
    session_uuid: String,
    /// The master PTY FD.  `OwnedFd` closes on drop.
    master_fd: OwnedFd,
    child_pid: u32,
    /// Terminal parser, shared with the reader thread.
    ///
    /// The reader feeds raw PTY bytes in; on `GetSnapshot` the broker calls
    /// `generate_ansi_snapshot()` directly from parsed cell state instead of
    /// storing raw bytes in a separate ring buffer.
    parser: Arc<Mutex<AlacrittyParser<BrokerEventListener>>>,
    /// File tee shared with the reader thread.
    ///
    /// Set to `Some` when `HubMessage::ArmTee` is received.  The reader
    /// thread writes to it on every output chunk; the main thread replaces it
    /// on re-arm without stopping the reader.
    tee: SharedTee,
    /// Reader thread handle — joined on shutdown.
    reader: Option<thread::JoinHandle<()>>,
    /// Writer thread command channel — sole path for PTY stdin writes/resizes.
    writer_tx: std::sync::mpsc::SyncSender<PtyWriteCommand>,
    /// Writer thread handle — joined on shutdown.
    writer: Option<thread::JoinHandle<()>>,
    /// True after a resize is applied but before the PTY produces output at the
    /// new dimensions.  Mirrors `PtySession::resize_pending` — checked by
    /// `GetSnapshot` to avoid capturing stale visible-screen content.
    resize_pending: Arc<AtomicBool>,
}

impl Session {
    /// Queue raw bytes for PTY stdin write on the dedicated writer thread.
    fn write_input(&self, data: &[u8]) -> Result<()> {
        self.writer_tx
            .try_send(PtyWriteCommand::Input(data.to_vec()))
            .map_err(|e| anyhow::anyhow!("enqueue PTY input: {e}"))
    }

    /// Queue a PTY resize for the dedicated writer thread.
    ///
    /// The writer coalesces adjacent resize commands and applies only the last
    /// one, which avoids jank during reconnect/layout resize bursts.
    fn resize(&self, rows: u16, cols: u16) {
        if let Err(e) = self.writer_tx.send(PtyWriteCommand::Resize { rows, cols }) {
            log::warn!(
                "[broker] queue resize failed for session {}: {e}",
                self.session_id
            );
        }
    }

    /// Kill the child process (SIGHUP → SIGKILL after 200 ms).
    fn kill_child(&self) {
        let pid = self.child_pid as libc::pid_t;
        unsafe { libc::kill(pid, libc::SIGHUP) };
        thread::sleep(Duration::from_millis(200));
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

/// Apply a PTY resize ioctl and keep the shadow parser dimensions in sync.
///
/// Sets `resize_pending` so `GetSnapshot` knows the visible screen may be
/// stale until the application redraws at the new dimensions.
fn apply_pty_resize(
    fd: RawFd,
    rows: u16,
    cols: u16,
    parser: &Arc<Mutex<AlacrittyParser<BrokerEventListener>>>,
    resize_pending: &AtomicBool,
) {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) };
    if let Ok(mut p) = parser.lock() {
        p.resize(rows, cols);
    }
    resize_pending.store(true, Ordering::Release);
}

/// Drain queued resize commands and keep only the final dimensions.
///
/// Returns `(final_rows, final_cols, deferred_cmd)` where `deferred_cmd` is the
/// first non-resize command encountered while draining.
fn coalesce_resize_commands(
    mut rows: u16,
    mut cols: u16,
    rx: &std::sync::mpsc::Receiver<PtyWriteCommand>,
) -> (u16, u16, Option<PtyWriteCommand>) {
    let mut deferred = None;
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            PtyWriteCommand::Resize { rows: r, cols: c } => {
                rows = r;
                cols = c;
            }
            other => {
                deferred = Some(other);
                break;
            }
        }
    }
    (rows, cols, deferred)
}

/// Per-session PTY writer loop.
///
/// This is the sole writer for PTY input and resize operations to avoid
/// read/write interleaving hazards in the broker main loop.
fn pty_writer_loop(
    fd: RawFd,
    parser: Arc<Mutex<AlacrittyParser<BrokerEventListener>>>,
    resize_pending: Arc<AtomicBool>,
    rx: std::sync::mpsc::Receiver<PtyWriteCommand>,
) {
    // Borrow-only File wrapper — do not close the FD here.
    let mut file = ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
    let mut deferred: Option<PtyWriteCommand> = None;

    loop {
        let cmd = if let Some(cmd) = deferred.take() {
            cmd
        } else {
            match rx.recv() {
                Ok(cmd) => cmd,
                Err(_) => break,
            }
        };

        match cmd {
            PtyWriteCommand::Input(data) => {
                if let Err(e) = file.write_all(&data) {
                    log::warn!("[broker] PTY input write failed: {e}");
                    break;
                }
            }
            PtyWriteCommand::Resize { rows, cols } => {
                let (rows, cols, next) = coalesce_resize_commands(rows, cols, &rx);
                apply_pty_resize(fd, rows, cols, &parser, &resize_pending);
                deferred = next;
            }
            PtyWriteCommand::Shutdown => break,
        }
    }
}

// ─── Broker ────────────────────────────────────────────────────────────────

/// Shared output sink — updated on every Hub connect and reconnect.
///
/// All PTY reader threads hold an `Arc` clone of this mutex so that a
/// single update in `handle_connection` re-wires every surviving reader
/// thread to the new Hub connection without restarting the threads.
///
/// During the reconnect window the inner `Option` is `None`; reader threads
/// attempt to lock and find `None`, so PTY output is silently dropped until
/// the Hub reconnects.  This is intentional — output produced between
/// disconnect and reconnect is already captured in each session's
/// `AlacrittyParser` ring buffer and replayed via `GetSnapshot`.
struct OutputSink {
    tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    overflow_tx: std::sync::mpsc::Sender<()>,
    overflow_notified: Arc<std::sync::atomic::AtomicBool>,
}
type SharedWriter = Arc<Mutex<Option<OutputSink>>>;

/// The broker state: all registered PTY sessions plus configuration.
struct Broker {
    /// All active sessions, keyed by session_id.
    sessions: HashMap<u32, Session>,
    /// Maps session_uuid → session_id for lookup by key.
    key_map: HashMap<String, u32>,
    next_session_id: u32,
    reconnect_timeout: Duration,
    /// Shared channel sender — updated at the start of every `handle_connection`
    /// call so all reader threads automatically route output to the current
    /// Hub connection.  Cleared to `None` on Hub disconnect.
    shared_writer: SharedWriter,
}

impl Broker {
    fn new(timeout_secs: u64) -> Self {
        Self {
            sessions: HashMap::new(),
            key_map: HashMap::new(),
            next_session_id: 1,
            reconnect_timeout: Duration::from_secs(timeout_secs),
            shared_writer: Arc::new(Mutex::new(None)),
        }
    }

    fn alloc_session_id(&mut self) -> u32 {
        let id = self.next_session_id.max(1); // ensure 0 is never returned
        self.next_session_id = id.wrapping_add(1).max(1);
        id
    }

    /// Register a new session, spawning a reader thread for the PTY.
    ///
    /// The reader thread uses `self.shared_writer` — the same `Arc` shared by
    /// all sessions — so a single update in `handle_connection` re-wires all
    /// reader threads to the current Hub connection on reconnect.
    fn register(&mut self, fd: OwnedFd, reg: FdTransferPayload) -> u32 {
        let session_id = self.alloc_session_id();
        let raw: RawFd = std::os::unix::io::AsRawFd::as_raw_fd(&fd);
        let collected: CollectedEvents = Arc::new(Mutex::new(Vec::new()));
        let listener = BrokerEventListener {
            collected: Arc::clone(&collected),
            pty_fd: raw,
        };
        let parser = Arc::new(Mutex::new(AlacrittyParser::new_with_listener(
            reg.rows,
            reg.cols,
            DEFAULT_SCROLLBACK_LINES,
            listener,
        )));
        let parser_clone = Arc::clone(&parser);
        let parser_for_writer = Arc::clone(&parser);

        // Reader thread: blocking read loop on the master PTY FD.
        // Uses Arc::clone of shared_writer so a reconnect updates ALL reader
        // threads with a single mutex write rather than stopping and restarting them.
        let reader_sid = session_id;
        let shared = Arc::clone(&self.shared_writer);
        // Shared tee — None until ArmTee is received.  The reader thread
        // acquires this on each output chunk; the main thread replaces it on
        // re-arm without stopping the reader.
        let shared_tee: SharedTee = Arc::new(Mutex::new(None));
        let tee_clone = Arc::clone(&shared_tee);
        let collected_clone = Arc::clone(&collected);
        let resize_pending = Arc::new(AtomicBool::new(false));
        let resize_pending_reader = Arc::clone(&resize_pending);
        let resize_pending_writer = Arc::clone(&resize_pending);
        let reader = thread::spawn(move || {
            reader_loop(
                raw,
                reader_sid,
                parser_clone,
                shared,
                tee_clone,
                collected_clone,
                resize_pending_reader,
            );
        });
        let (writer_tx, writer_rx) =
            std::sync::mpsc::sync_channel::<PtyWriteCommand>(PTY_WRITER_QUEUE_CAPACITY);
        let writer = thread::spawn(move || {
            pty_writer_loop(raw, parser_for_writer, resize_pending_writer, writer_rx)
        });

        self.key_map.insert(reg.session_uuid.clone(), session_id);
        self.sessions.insert(
            session_id,
            Session {
                session_id,
                session_uuid: reg.session_uuid,
                master_fd: fd,
                child_pid: reg.child_pid,
                parser,
                tee: shared_tee,
                reader: Some(reader),
                writer_tx,
                writer: Some(writer),
                resize_pending,
            },
        );

        session_id
    }

    /// Unregister a session (process already exited, Hub is cleaning up).
    fn unregister(&mut self, session_id: u32) {
        if let Some(mut sess) = self.sessions.remove(&session_id) {
            self.key_map.remove(&sess.session_uuid);
            let _ = sess.writer_tx.send(PtyWriteCommand::Shutdown);
            if let Some(handle) = sess.writer.take() {
                let _ = handle.join();
            }
            // Join the reader — it will exit when the PTY FD is closed on drop.
            if let Some(handle) = sess.reader.take() {
                drop(sess.master_fd); // close FD first so reader unblocks
                let _ = handle.join();
            }
        }
    }

    /// Kill all PTY children and drop all sessions.
    fn kill_all(&mut self) {
        for (_, mut sess) in self.sessions.drain() {
            sess.kill_child();
            let _ = sess.writer_tx.send(PtyWriteCommand::Shutdown);
            if let Some(handle) = sess.writer.take() {
                let _ = handle.join();
            }
            if let Some(handle) = sess.reader.take() {
                drop(sess.master_fd);
                let _ = handle.join();
            }
        }
        self.key_map.clear();
    }
}

/// PTY reader loop — runs in a dedicated thread per session.
///
/// Reads from the master FD (borrowing, not owning), feeds bytes into the
/// session's `AlacrittyParser`, optionally tees them to a log file, and
/// forwards encoded `PtyOutput` frames to the Hub via `shared_writer`.
///
/// `shared_writer` is the broker-global `Arc<Mutex<Option<Sender>>>` updated by
/// `handle_connection` on every Hub connect and reconnect.  Locking it before
/// each send means a single mutex write re-wires all reader threads to the new
/// Hub connection without stopping or restarting them.
///
/// During the reconnect window (`Option` is `None`) output is silently dropped
/// **but still written to the tee log** so the log file captures output even
/// while the Hub is down.  The `AlacrittyParser` also continues to receive
/// bytes so `GetSnapshot` returns accurate state when the Hub reconnects.
///
/// `shared_tee` starts as `None` and is set to `Some` when the Hub sends
/// `HubMessage::ArmTee`.  The main thread replaces it on re-arm without
/// stopping this loop.
fn reader_loop(
    fd: RawFd,
    session_id: u32,
    parser: Arc<Mutex<AlacrittyParser<BrokerEventListener>>>,
    shared_writer: SharedWriter,
    shared_tee: SharedTee,
    collected_events: CollectedEvents,
    resize_pending: Arc<AtomicBool>,
) {
    let mut buf = [0u8; 4096];
    // Borrow-only File — ManuallyDrop prevents close on drop.
    // SAFETY: fd is a valid master PTY FD owned by this session for the
    // lifetime of the reader thread.  ManuallyDrop ensures we do not close
    // it (ownership remains with the Session struct).
    let mut file = ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });

    loop {
        match file.read(&mut buf) {
            Ok(0) | Err(_) => {
                // PTY FD closed or read error — child has exited (or FD was
                // explicitly closed by unregister).
                //
                // NOTE: `BrokerMessage::PtyExited` is defined in the protocol
                // but is NOT sent here in v1.  The Hub integration agent must
                // not rely on receiving that notification yet.  Detecting child
                // exit is left as a future improvement (e.g. waitpid thread or
                // signalfd).  The Hub will discover the exit via its own PTY
                // read path once the session has been handed back.
                break;
            }
            Ok(n) => {
                let data = &buf[..n];

                // Feed into the parser so GetSnapshot can generate from cell state.
                // Extract sideband flags after processing.
                let flags = if let Ok(mut p) = parser.lock() {
                    p.process(data);
                    let mut f = 0u8;
                    if !p.cursor_hidden() {
                        f |= sideband::CURSOR_VISIBLE;
                    }
                    if p.kitty_enabled() {
                        f |= sideband::KITTY_ENABLED;
                    }
                    f
                } else {
                    sideband::CURSOR_VISIBLE // safe default: cursor visible, kitty off
                };

                // App produced output — parser state is no longer stale from
                // a prior resize.  Mirrors PtySession / spawn.rs pattern.
                resize_pending.store(false, Ordering::Release);

                // Write to tee log (if armed).  Runs even during the Hub
                // reconnect window so the log captures output while Hub is down.
                if let Ok(mut tee_guard) = shared_tee.lock() {
                    if let Some(ref mut tee) = *tee_guard {
                        tee.write_data(data);
                    }
                }

                // Drain terminal events collected during parser.process().
                let events: Vec<BrokerTermEvent> = if let Ok(mut q) = collected_events.lock() {
                    q.drain(..).collect()
                } else {
                    Vec::new()
                };

                // Forward to Hub via the shared writer.  `None` during reconnect
                // window — drop the frame (already captured in parser and tee above).
                let frame = encode_pty_output(session_id, flags, data);
                if let Ok(guard) = shared_writer.lock() {
                    if let Some(ref sink) = *guard {
                        match sink.tx.try_send(frame) {
                            Ok(()) => {}
                            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                                if !sink.overflow_notified.swap(true, Ordering::AcqRel) {
                                    let _ = sink.overflow_tx.send(());
                                }
                            }
                            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {}
                        }

                        // Send terminal events as BrokerControl frames.
                        for event in events {
                            let msg = BrokerMessage::TermEvent { session_id, event };
                            let event_frame = encode_broker_control(&msg);
                            let _ = sink.tx.try_send(event_frame);
                        }
                    }
                }
            }
        }
    }
}

// ─── SCM_RIGHTS receive ────────────────────────────────────────────────────

/// Receive up to `max_bytes` from a Unix stream socket using `recvmsg`,
/// capturing any file descriptors passed via SCM_RIGHTS ancillary data.
///
/// Returns `(bytes_read, received_bytes, fds)`.
fn recvmsg_fds(sock_fd: RawFd, max_bytes: usize) -> std::io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut data_buf = vec![0u8; max_bytes];
    // Ancillary buffer large enough for one FD.
    let cmsg_space = unsafe { libc::CMSG_SPACE(std::mem::size_of::<libc::c_int>() as _) } as usize;
    let mut cmsg_buf = vec![0u8; cmsg_space * 4]; // room for a few FDs

    let mut iov = libc::iovec {
        iov_base: data_buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: data_buf.len(),
    };
    let mut msg = libc::msghdr {
        msg_name: std::ptr::null_mut(),
        msg_namelen: 0,
        msg_iov: &mut iov,
        msg_iovlen: 1,
        msg_control: cmsg_buf.as_mut_ptr() as *mut libc::c_void,
        msg_controllen: cmsg_buf.len() as _,
        msg_flags: 0,
    };

    let n = unsafe { libc::recvmsg(sock_fd, &mut msg, 0) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }

    data_buf.truncate(n as usize);

    // Extract FDs from ancillary data.
    let mut fds = Vec::new();
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let data = libc::CMSG_DATA(cmsg);
                let fd_count = ((*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize)
                    / std::mem::size_of::<libc::c_int>();
                for i in 0..fd_count {
                    let fd: libc::c_int = std::ptr::read_unaligned(
                        data.add(i * std::mem::size_of::<libc::c_int>()) as *const libc::c_int,
                    );
                    fds.push(OwnedFd::from_raw_fd(fd));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }

    Ok((data_buf, fds))
}

// ─── Hub connection handler ────────────────────────────────────────────────

/// Spawn a writer thread that prioritizes control frames over output frames.
fn spawn_connection_writer_thread(
    stream: UnixStream,
    control_rx: std::sync::mpsc::Receiver<Vec<u8>>,
    output_rx: std::sync::mpsc::Receiver<Vec<u8>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut stream = stream;
        let mut control_open = true;
        let mut output_open = true;
        const POLL: Duration = Duration::from_millis(100);

        loop {
            let mut progressed = false;

            // Control responses are latency-sensitive; always service them first.
            if control_open {
                match control_rx.try_recv() {
                    Ok(frame) => {
                        progressed = true;
                        if frame.is_empty() || stream.write_all(&frame).is_err() {
                            break;
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        control_open = false;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                }
            }

            if progressed {
                continue;
            }

            if output_open {
                match output_rx.recv_timeout(POLL) {
                    Ok(frame) => {
                        if frame.is_empty() || stream.write_all(&frame).is_err() {
                            break;
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        output_open = false;
                    }
                }
            } else if control_open {
                match control_rx.recv_timeout(POLL) {
                    Ok(frame) => {
                        if frame.is_empty() || stream.write_all(&frame).is_err() {
                            break;
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        control_open = false;
                    }
                }
            }

            if !control_open && !output_open {
                break;
            }
        }
    })
}

/// Handle one Hub connection until it disconnects.
///
/// Returns the broker state so the caller can wait for a reconnect.
fn handle_connection(mut stream: UnixStream, broker: &mut Broker) -> Result<()> {
    use protocol::BrokerFrame;

    let negotiated = control_handshake_server(&mut stream).context("control handshake from hub")?;
    log::debug!(
        "[broker] control handshake negotiated: version={} caps=0x{:x}",
        negotiated.version,
        negotiated.capabilities
    );

    let sock_fd: RawFd = std::os::unix::io::AsRawFd::as_raw_fd(&stream);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));

    // Per-consumer queues:
    // - control_tx: unbounded, latency-sensitive control replies
    // - output_tx: bounded, high-volume PTY output (disconnect on overflow)
    let (control_tx, control_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let (output_tx, output_rx) =
        std::sync::mpsc::sync_channel::<Vec<u8>>(BROKER_OUTPUT_QUEUE_CAPACITY);
    let (overflow_tx, overflow_rx) = std::sync::mpsc::channel::<()>();
    let overflow_notified = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Re-wire all existing reader threads to this Hub connection.
    //
    // On initial connect there are no sessions yet, so this is a no-op.
    // On reconnect, surviving sessions' reader threads held the previous
    // (dead) sender; updating the shared Arc here makes them route PTY
    // output to the new Hub connection without stopping the threads.
    {
        let mut guard = broker
            .shared_writer
            .lock()
            .expect("shared_writer mutex poisoned");
        *guard = Some(OutputSink {
            tx: output_tx.clone(),
            overflow_tx: overflow_tx.clone(),
            overflow_notified: Arc::clone(&overflow_notified),
        });
    }

    let write_stream = stream.try_clone().context("clone socket for writer")?;
    let writer = spawn_connection_writer_thread(write_stream, control_rx, output_rx);

    let mut decoder = BrokerFrameDecoder::new();
    let mut pending_fd: Option<OwnedFd> = None;

    loop {
        if overflow_rx.try_recv().is_ok() {
            log::warn!(
                "[broker] output queue overflow (cap={BROKER_OUTPUT_QUEUE_CAPACITY}); disconnecting Hub"
            );
            break;
        }
        // Use recvmsg so we capture SCM_RIGHTS ancillary data on FdTransfer.
        let (data, fds) = match recvmsg_fds(sock_fd, 65536) {
            Ok((d, f)) if d.is_empty() && f.is_empty() => break, // Hub disconnected
            Ok(r) => r,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue
            }
            Err(_) => break,
        };

        // Stash any received FD for the upcoming FdTransfer frame.
        if let Some(fd) = fds.into_iter().next() {
            pending_fd = Some(fd);
        }

        let frames = match decoder.feed(&data) {
            Ok(f) => f,
            Err(e) => {
                log::warn!("[broker] frame decode error: {e}");
                break;
            }
        };

        for frame in frames {
            match frame {
                BrokerFrame::FdTransfer(reg) => {
                    let fd = match pending_fd.take() {
                        Some(f) => f,
                        None => {
                            log::error!("[broker] FdTransfer received but no FD in ancillary data");
                            continue;
                        }
                    };
                    let session_uuid = reg.session_uuid.clone();
                    // register() spawns the reader thread using Arc::clone of
                    // broker.shared_writer (already wired to this connection above).
                    let session_id = broker.register(fd, reg);
                    let resp = encode_broker_control(&BrokerMessage::Registered {
                        session_uuid,
                        session_id,
                    });
                    let _ = control_tx.send(resp);
                }

                BrokerFrame::PtyInput(session_id, data) => {
                    if let Some(sess) = broker.sessions.get(&session_id) {
                        if let Err(e) = sess.write_input(&data) {
                            log::warn!("[broker] write to session {session_id}: {e}");
                        }
                    }
                }

                BrokerFrame::HubControl(HubMessage::ResizePty {
                    session_id,
                    rows,
                    cols,
                }) => {
                    if let Some(sess) = broker.sessions.get(&session_id) {
                        sess.resize(rows, cols);
                    }
                }

                BrokerFrame::HubControl(HubMessage::GetSnapshot { session_id }) => {
                    let frame = if let Some(sess) = broker.sessions.get(&session_id) {
                        let skip_visible =
                            sess.resize_pending.swap(false, Ordering::AcqRel);
                        let snapshot = sess
                            .parser
                            .lock()
                            .map(|p| generate_ansi_snapshot(&p, skip_visible))
                            .unwrap_or_default();
                        encode_data(frame_type::SNAPSHOT, session_id, &snapshot)
                    } else {
                        log::warn!("[broker] GetSnapshot for unknown session {session_id}");
                        encode_broker_control(&BrokerMessage::Error {
                            message: format!("no session {session_id}"),
                        })
                    };
                    let _ = control_tx.send(frame);
                }

                BrokerFrame::HubControl(HubMessage::GetScreen { session_id }) => {
                    let frame = if let Some(sess) = broker.sessions.get(&session_id) {
                        let text = sess.parser.lock().map(|p| p.contents()).unwrap_or_default();
                        encode_data(frame_type::SCREEN, session_id, text.as_bytes())
                    } else {
                        log::warn!("[broker] GetScreen for unknown session {session_id}");
                        encode_broker_control(&BrokerMessage::Error {
                            message: format!("no session {session_id}"),
                        })
                    };
                    let _ = control_tx.send(frame);
                }

                BrokerFrame::HubControl(HubMessage::ListSessions) => {
                    let mut sessions: Vec<BrokerSessionInventory> = broker
                        .sessions
                        .values()
                        .map(|sess| BrokerSessionInventory {
                            session_id: sess.session_id,
                            session_uuid: sess.session_uuid.clone(),
                        })
                        .collect();
                    sessions.sort_by_key(|s| s.session_id);
                    let _ =
                        control_tx.send(encode_broker_control(&BrokerMessage::SessionInventory {
                            sessions,
                        }));
                }

                BrokerFrame::HubControl(HubMessage::UnregisterPty { session_id }) => {
                    broker.unregister(session_id);
                    let _ = control_tx.send(encode_broker_control(&BrokerMessage::Ack));
                }

                BrokerFrame::HubControl(HubMessage::SetTimeout { seconds }) => {
                    broker.reconnect_timeout = Duration::from_secs(seconds);
                    let _ = control_tx.send(encode_broker_control(&BrokerMessage::Ack));
                }

                BrokerFrame::HubControl(HubMessage::KillAll) => {
                    broker.kill_all();
                    {
                        let mut guard = broker
                            .shared_writer
                            .lock()
                            .expect("shared_writer mutex poisoned");
                        *guard = None;
                    }
                    let _ = control_tx.send(vec![]);
                    let _ = output_tx.try_send(vec![]);
                    drop(control_tx);
                    drop(output_tx);
                    let _ = writer.join();
                    return Ok(());
                }

                BrokerFrame::HubControl(HubMessage::ArmTee {
                    session_id,
                    log_path,
                    cap_bytes,
                }) => {
                    // Arm (or re-arm) the file tee for this session.
                    //
                    // The SharedTee Arc is shared with the reader thread; replacing
                    // its contents here re-wires the tee without stopping the reader.
                    // cap_bytes defaults to DEFAULT_TEE_CAP_BYTES when the caller
                    // passes 0 (Lua may omit the argument).
                    let effective_cap = if cap_bytes == 0 {
                        DEFAULT_TEE_CAP_BYTES
                    } else {
                        cap_bytes
                    };
                    let resp = if let Some(sess) = broker.sessions.get(&session_id) {
                        match TeeState::open(PathBuf::from(&log_path), effective_cap) {
                            Ok(tee) => match sess.tee.lock() {
                                Ok(mut guard) => {
                                    *guard = Some(tee);
                                    log::info!(
                                            "[broker] tee armed: session={session_id} path={log_path} cap={effective_cap}"
                                        );
                                    encode_broker_control(&BrokerMessage::Ack)
                                }
                                Err(_) => {
                                    log::error!(
                                        "[broker] tee mutex poisoned for session {session_id}"
                                    );
                                    encode_broker_control(&BrokerMessage::Error {
                                        message: format!(
                                            "tee mutex poisoned for session {session_id}"
                                        ),
                                    })
                                }
                            },
                            Err(e) => {
                                log::error!("[broker] ArmTee failed for session {session_id}: {e}");
                                encode_broker_control(&BrokerMessage::Error {
                                    message: format!("ArmTee failed: {e}"),
                                })
                            }
                        }
                    } else {
                        log::warn!("[broker] ArmTee for unknown session {session_id}");
                        encode_broker_control(&BrokerMessage::Error {
                            message: format!("no session {session_id}"),
                        })
                    };
                    let _ = control_tx.send(resp);
                }

                BrokerFrame::HubControl(HubMessage::Ping) => {
                    let _ = control_tx.send(encode_broker_control(&BrokerMessage::Pong));
                }

                _ => {
                    log::debug!("[broker] ignoring unexpected frame direction");
                }
            }
        }
    }

    // Hub disconnected — signal the writer thread to exit.
    //
    // Clear shared_writer first so reader threads stop queuing into the dead
    // channel during the reconnect window.  Output is still captured by each
    // session's AlacrittyParser and will be replayed via GetSnapshot when the
    // Hub reconnects.
    {
        let mut guard = broker
            .shared_writer
            .lock()
            .expect("shared_writer mutex poisoned");
        *guard = None;
    }

    // Send sentinel to stop writer promptly; then drop both senders.
    let _ = control_tx.send(vec![]);
    let _ = output_tx.try_send(vec![]);
    drop(control_tx);
    drop(output_tx);
    let _ = writer.join();

    Ok(())
}

// ─── Main entry point ──────────────────────────────────────────────────────

/// Build the broker socket path for a given hub_id.
///
/// Format: `/tmp/botster-{uid}/broker-{hub_id}.sock`
/// Length is validated against the macOS 104-byte kernel limit.
pub fn broker_socket_path(hub_id: &str) -> Result<PathBuf> {
    let uid = unsafe { libc::getuid() };
    let path = PathBuf::from(format!("/tmp/botster-{uid}/broker-{hub_id}.sock"));
    let path_str = path.to_string_lossy();
    if path_str.len() > MAX_SOCK_PATH {
        anyhow::bail!(
            "broker socket path too long ({} > {MAX_SOCK_PATH}): {path_str}",
            path_str.len()
        );
    }
    Ok(path)
}

/// Wait for a Hub connection within a timeout window.
///
/// Sets the listener non-blocking and polls until a connection arrives or
/// the deadline passes.  Returns `None` on timeout.
///
/// The listener is left in non-blocking mode; callers that need blocking
/// accepts should call `set_nonblocking(false)` themselves.
fn wait_for_reconnect(listener: &UnixListener, timeout: Duration) -> Result<Option<UnixStream>> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + timeout;

    loop {
        match listener.accept() {
            Ok((stream, _)) => return Ok(Some(stream)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                thread::sleep(Duration::from_millis(250));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Run the broker process.
///
/// Listens for Hub connections on the broker socket. When the Hub
/// disconnects, waits up to `timeout_secs` for a reconnect before
/// killing all PTY children and exiting.  The timeout window applies
/// consistently after **every** Hub disconnect, not just the first.
pub fn run(hub_id: &str, timeout_secs: u64) -> Result<()> {
    let socket_path = broker_socket_path(hub_id)?;

    // Create parent directory if needed.
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create broker socket dir: {}", parent.display()))?;
    }

    // Remove stale socket file from a previous run.
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("bind broker socket: {}", socket_path.display()))?;

    // Owner-only permissions (0o600).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600));
    }

    log::info!("[broker] listening on {}", socket_path.display());

    let mut broker = Broker::new(timeout_secs);

    // Wait indefinitely for the first Hub connection.
    listener.set_nonblocking(false)?;
    let (stream, _) = listener
        .accept()
        .context("waiting for initial Hub connection")?;
    log::info!("[broker] Hub connected");
    let _ = handle_connection(stream, &mut broker);
    log::info!("[broker] Hub disconnected");

    // After every subsequent disconnect, apply the same reconnect timeout.
    // This loop is entered after the *first* disconnect and continues as long
    // as there are live sessions to preserve.
    loop {
        if broker.sessions.is_empty() {
            log::info!("[broker] no sessions remaining, exiting");
            break;
        }

        log::info!(
            "[broker] waiting {}s for Hub reconnect ({} session(s))",
            broker.reconnect_timeout.as_secs(),
            broker.sessions.len(),
        );

        match wait_for_reconnect(&listener, broker.reconnect_timeout)? {
            Some(stream) => {
                log::info!("[broker] Hub reconnected");
                let _ = handle_connection(stream, &mut broker);
                log::info!("[broker] Hub disconnected");
            }
            None => {
                log::warn!(
                    "[broker] reconnect timeout expired — killing {} session(s)",
                    broker.sessions.len()
                );
                broker.kill_all();
                break;
            }
        }
    }

    let _ = std::fs::remove_file(&socket_path);
    log::info!("[broker] exiting");
    Ok(())
}

// ─── TeeState unit tests ────────────────────────────────────────────────────

#[cfg(test)]
mod writer_tests {
    use super::*;

    #[test]
    fn coalesce_resize_uses_last_and_defers_first_non_resize() {
        let (tx, rx) = std::sync::mpsc::channel::<PtyWriteCommand>();
        tx.send(PtyWriteCommand::Resize { rows: 30, cols: 90 })
            .unwrap();
        tx.send(PtyWriteCommand::Resize {
            rows: 40,
            cols: 120,
        })
        .unwrap();
        tx.send(PtyWriteCommand::Input(b"abc".to_vec())).unwrap();
        tx.send(PtyWriteCommand::Resize {
            rows: 50,
            cols: 140,
        })
        .unwrap();

        let (rows, cols, deferred) = coalesce_resize_commands(24, 80, &rx);
        assert_eq!((rows, cols), (40, 120));
        assert!(matches!(
            deferred,
            Some(PtyWriteCommand::Input(ref data)) if data == b"abc"
        ));
        assert!(matches!(
            rx.recv().unwrap(),
            PtyWriteCommand::Resize {
                rows: 50,
                cols: 140
            }
        ));
    }

    #[test]
    fn coalesce_resize_empty_queue_returns_initial() {
        let (_tx, rx) = std::sync::mpsc::channel::<PtyWriteCommand>();
        let (rows, cols, deferred) = coalesce_resize_commands(24, 80, &rx);
        assert_eq!((rows, cols), (24, 80));
        assert!(deferred.is_none());
    }
}

#[cfg(test)]
mod tee_tests {
    use super::*;
    use tempfile::TempDir;

    fn make_tee(dir: &TempDir, cap_bytes: u64) -> TeeState {
        let path = dir.path().join("sessions").join("0").join("pty-0.log");
        TeeState::open(path, cap_bytes).expect("TeeState::open should succeed")
    }

    /// Data written to the tee appears verbatim in the log file.
    #[test]
    fn tee_writes_data_to_log() {
        let dir = TempDir::new().unwrap();
        let mut tee = make_tee(&dir, DEFAULT_TEE_CAP_BYTES);

        tee.write_data(b"hello");
        tee.write_data(b" world");

        // Flush via drop — the File buffers nothing, but let's re-read.
        let log_path = dir.path().join("sessions").join("0").join("pty-0.log");
        let content = std::fs::read(&log_path).unwrap();
        assert_eq!(content, b"hello world");
    }

    /// Binary PTY escape sequences survive the tee without corruption.
    #[test]
    fn tee_preserves_binary_data() {
        let dir = TempDir::new().unwrap();
        let mut tee = make_tee(&dir, DEFAULT_TEE_CAP_BYTES);

        let esc: Vec<u8> = vec![0x1b, 0x5b, b'2', b'J', 0x1b, 0x5b, b'H', 0x00, 0xff];
        tee.write_data(&esc);

        let log_path = dir.path().join("sessions").join("0").join("pty-0.log");
        assert_eq!(std::fs::read(&log_path).unwrap(), esc);
    }

    /// Rotation fires when `bytes_written >= cap_bytes`:
    /// - `pty-0.log` is renamed to `pty-0.log.1`
    /// - a fresh `pty-0.log` is created with just the new data.
    #[test]
    fn tee_rotates_at_cap() {
        let dir = TempDir::new().unwrap();
        let log_dir = dir.path().join("sessions").join("0");
        std::fs::create_dir_all(&log_dir).unwrap();
        let log_path = log_dir.join("pty-0.log");
        let rotated_path = log_dir.join("pty-0.log.1");

        // Cap of 5 bytes so we can trigger rotation cheaply.
        let mut tee = TeeState::open(log_path.clone(), 5).unwrap();

        tee.write_data(b"abcde"); // 5 bytes — exactly at cap, rotation fires on next write
        tee.write_data(b"X"); // triggers rotation before writing "X"

        // After rotation: .log.1 contains "abcde", .log contains "X".
        assert!(
            rotated_path.exists(),
            "pty-0.log.1 should exist after rotation"
        );
        assert_eq!(std::fs::read(&rotated_path).unwrap(), b"abcde");
        assert_eq!(std::fs::read(&log_path).unwrap(), b"X");
        assert_eq!(tee.bytes_written, 1);
    }

    /// A second rotation overwrites the previous `.log.1`.
    ///
    /// Rotation fires when `bytes_written + incoming >= cap`.  With cap=3 and
    /// starting from an empty file (bytes_written=0), the first `write_data(b"aaa")`
    /// triggers rotation immediately (0+3 >= 3) — the empty file is renamed to
    /// `.log.1` and "aaa" is written to the fresh `.log`.
    ///
    /// Sequence (cap=3):
    /// 1. write "aaa": rotate (empty → .log.1), write "aaa" → .log="aaa", bw=3
    /// 2. write "B":   rotate ("aaa" → .log.1), write "B" → .log="B",   bw=1
    /// 3. write "cc":  rotate ("B" → .log.1),   write "cc" → .log="cc",  bw=2
    /// 4. write "D":   rotate ("cc" → .log.1),  write "D" → .log="D",   bw=1
    #[test]
    fn tee_second_rotation_overwrites_dot_one() {
        let dir = TempDir::new().unwrap();
        let log_dir = dir.path().join("sessions").join("0");
        std::fs::create_dir_all(&log_dir).unwrap();
        let log_path = log_dir.join("pty-0.log");
        let rotated_path = log_dir.join("pty-0.log.1");

        let mut tee = TeeState::open(log_path.clone(), 3).unwrap();

        tee.write_data(b"aaa");
        tee.write_data(b"B");
        tee.write_data(b"cc");
        tee.write_data(b"D");

        // After the 4th rotation: .log.1 holds "cc" (from step 4), .log holds "D".
        assert_eq!(std::fs::read(&rotated_path).unwrap(), b"cc");
        assert_eq!(std::fs::read(&log_path).unwrap(), b"D");
    }

    /// `bytes_written` is initialised from the existing file length so re-arming
    /// an in-progress session does not reset the rotation counter to zero.
    #[test]
    fn tee_open_accounts_for_existing_file_length() {
        let dir = TempDir::new().unwrap();
        let log_dir = dir.path().join("sessions").join("0");
        std::fs::create_dir_all(&log_dir).unwrap();
        let log_path = log_dir.join("pty-0.log");

        // Pre-populate the log with 8 bytes — simulates a session that ran before
        // the Hub restarted and re-armed the tee.
        std::fs::write(&log_path, b"12345678").unwrap();

        let tee = TeeState::open(log_path, DEFAULT_TEE_CAP_BYTES).unwrap();
        assert_eq!(
            tee.bytes_written, 8,
            "bytes_written should reflect pre-existing content"
        );
    }

    /// Once `degraded`, subsequent `write_data` calls are no-ops (no panic, no crash).
    #[test]
    fn tee_degraded_write_is_noop() {
        let dir = TempDir::new().unwrap();
        let mut tee = make_tee(&dir, DEFAULT_TEE_CAP_BYTES);

        tee.degraded = true;
        tee.write_data(b"should be ignored");

        let log_path = dir.path().join("sessions").join("0").join("pty-0.log");
        assert_eq!(
            std::fs::read(&log_path).unwrap(),
            b"",
            "Degraded tee must not write anything"
        );
    }

    /// `ArmTee` with an unknown session_id must leave the tee map untouched
    /// and return an error response.  Test via the SharedTee directly rather
    /// than a full broker socket.
    #[test]
    fn shared_tee_none_before_arm() {
        let shared: SharedTee = Arc::new(Mutex::new(None));
        assert!(
            shared.lock().unwrap().is_none(),
            "SharedTee must start as None before ArmTee"
        );
    }

    /// The SharedTee Arc is clone-safe: arming from the main thread is
    /// visible to the reader thread clone immediately.
    #[test]
    fn shared_tee_arm_visible_across_clones() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("sessions").join("0").join("pty-0.log");

        let shared: SharedTee = Arc::new(Mutex::new(None));
        let reader_clone = Arc::clone(&shared);

        // Arm from the "main thread" side.
        let tee = TeeState::open(log_path.clone(), DEFAULT_TEE_CAP_BYTES).unwrap();
        *shared.lock().unwrap() = Some(tee);

        // Verify the "reader thread" clone sees the armed tee and can write through it.
        {
            let mut guard = reader_clone.lock().unwrap();
            if let Some(ref mut t) = *guard {
                t.write_data(b"visible");
            } else {
                panic!("Reader clone did not see the armed tee");
            }
        }

        assert_eq!(std::fs::read(&log_path).unwrap(), b"visible");
    }
}
