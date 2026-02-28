//! PTY broker process — holds PTY file descriptors across Hub restarts.
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

use crate::terminal::{AlacrittyParser, DEFAULT_SCROLLBACK_LINES, NoopListener, generate_ansi_snapshot};

use std::collections::HashMap;
use std::io::{Read, Write};
use std::mem::ManuallyDrop;
use std::os::unix::io::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use protocol::{
    BrokerFrameDecoder, BrokerMessage, FdTransferPayload, HubMessage,
    encode_broker_control, encode_data, frame_type,
};

/// Maximum path length for a Unix domain socket (macOS kernel limit).
const MAX_SOCK_PATH: usize = 104;

// ─── Session ───────────────────────────────────────────────────────────────

/// Broker-side state for a single PTY session.
struct Session {
    #[allow(dead_code)] // stored for diagnostics / future use
    session_id: u32,
    agent_key: String,
    pty_index: usize,
    /// The master PTY FD.  `OwnedFd` closes on drop.
    master_fd: OwnedFd,
    child_pid: u32,
    /// Terminal parser, shared with the reader thread.
    ///
    /// The reader feeds raw PTY bytes in; on `GetSnapshot` the broker calls
    /// `generate_ansi_snapshot()` directly from parsed cell state instead of
    /// storing raw bytes in a separate ring buffer.
    parser: Arc<Mutex<AlacrittyParser<NoopListener>>>,
    /// Reader thread handle — joined on shutdown.
    reader: Option<thread::JoinHandle<()>>,
}

impl Session {
    /// Write raw bytes to the PTY master FD.
    ///
    /// Uses `ManuallyDrop<File>` so we borrow the FD without transferring
    /// ownership (and thus without an accidental close on drop).
    fn write_input(&self, data: &[u8]) -> Result<()> {
        let raw: RawFd = std::os::unix::io::AsRawFd::as_raw_fd(&self.master_fd);
        let mut file = ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(raw) });
        file.write_all(data).context("write to PTY master")?;
        Ok(())
    }

    /// Resize the PTY via `ioctl(TIOCSWINSZ)` and keep the parser in sync.
    fn resize(&self, rows: u16, cols: u16) {
        let raw: RawFd = std::os::unix::io::AsRawFd::as_raw_fd(&self.master_fd);
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe { libc::ioctl(raw, libc::TIOCSWINSZ, &ws) };
        if let Ok(mut p) = self.parser.lock() {
            p.resize(rows, cols);
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

// ─── Broker ────────────────────────────────────────────────────────────────

/// The broker state: all registered PTY sessions plus configuration.
struct Broker {
    /// All active sessions, keyed by session_id.
    sessions: HashMap<u32, Session>,
    /// Maps (agent_key, pty_index) → session_id for lookup by key.
    key_map: HashMap<(String, usize), u32>,
    next_session_id: u32,
    reconnect_timeout: Duration,
}

impl Broker {
    fn new(timeout_secs: u64) -> Self {
        Self {
            sessions: HashMap::new(),
            key_map: HashMap::new(),
            next_session_id: 1,
            reconnect_timeout: Duration::from_secs(timeout_secs),
        }
    }

    fn alloc_session_id(&mut self) -> u32 {
        let id = self.next_session_id;
        self.next_session_id = self.next_session_id.wrapping_add(1).max(1);
        id
    }

    /// Register a new session, spawning a reader thread for the PTY.
    ///
    /// `output_tx` is used by the reader thread to send `PtyOutput` frames
    /// back to the Hub connection handler.
    fn register(
        &mut self,
        fd: OwnedFd,
        reg: FdTransferPayload,
        output_tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    ) -> u32 {
        let session_id = self.alloc_session_id();
        let parser = Arc::new(Mutex::new(
            AlacrittyParser::new_noop(reg.rows, reg.cols, DEFAULT_SCROLLBACK_LINES),
        ));
        let parser_clone = Arc::clone(&parser);

        // Reader thread: blocking read loop on the master PTY FD.
        let raw: RawFd = std::os::unix::io::AsRawFd::as_raw_fd(&fd);
        let reader_sid = session_id;
        let reader = thread::spawn(move || {
            reader_loop(raw, reader_sid, parser_clone, output_tx);
        });

        self.key_map.insert((reg.agent_key.clone(), reg.pty_index), session_id);
        self.sessions.insert(session_id, Session {
            session_id,
            agent_key: reg.agent_key,
            pty_index: reg.pty_index,
            master_fd: fd,
            child_pid: reg.child_pid,
            parser,
            reader: Some(reader),
        });

        session_id
    }

    /// Unregister a session (process already exited, Hub is cleaning up).
    fn unregister(&mut self, session_id: u32) {
        if let Some(mut sess) = self.sessions.remove(&session_id) {
            self.key_map.remove(&(sess.agent_key.clone(), sess.pty_index));
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
/// session's `AlacrittyParser`, and forwards encoded `PtyOutput` frames to the
/// Hub connection handler via `output_tx`.
fn reader_loop(
    fd: RawFd,
    session_id: u32,
    parser: Arc<Mutex<AlacrittyParser<NoopListener>>>,
    output_tx: std::sync::mpsc::SyncSender<Vec<u8>>,
) {
    let mut buf = [0u8; 4096];
    // Borrow-only File — ManuallyDrop prevents close on drop.
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
                if let Ok(mut p) = parser.lock() {
                    p.process(data);
                }
                // Forward to Hub (best-effort; drop if Hub is disconnected).
                let frame = encode_data(frame_type::PTY_OUTPUT, session_id, data);
                let _ = output_tx.try_send(frame);
            }
        }
    }
}

// ─── SCM_RIGHTS receive ────────────────────────────────────────────────────

/// Receive up to `max_bytes` from a Unix stream socket using `recvmsg`,
/// capturing any file descriptors passed via SCM_RIGHTS ancillary data.
///
/// Returns `(bytes_read, received_bytes, fds)`.
fn recvmsg_fds(
    sock_fd: RawFd,
    max_bytes: usize,
) -> std::io::Result<(Vec<u8>, Vec<OwnedFd>)> {
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
            if (*cmsg).cmsg_level == libc::SOL_SOCKET
                && (*cmsg).cmsg_type == libc::SCM_RIGHTS
            {
                let data = libc::CMSG_DATA(cmsg);
                let fd_count = ((*cmsg).cmsg_len as usize
                    - libc::CMSG_LEN(0) as usize)
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

/// Handle one Hub connection until it disconnects.
///
/// Returns the broker state so the caller can wait for a reconnect.
fn handle_connection(
    stream: UnixStream,
    broker: &mut Broker,
) -> Result<()> {
    use protocol::BrokerFrame;

    let sock_fd: RawFd = std::os::unix::io::AsRawFd::as_raw_fd(&stream);

    // Bounded channel: broker reader → writer.  A backpressure bound of 256
    // frames keeps memory bounded even if the Hub is slow.
    let (output_tx, output_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(256);

    // Writer thread — sends encoded frames from broker sessions to the Hub.
    let write_stream = stream.try_clone().context("clone socket for writer")?;
    let writer = thread::spawn(move || {
        let mut ws = write_stream;
        for frame in output_rx {
            // An empty sentinel frame signals that the Hub has disconnected
            // and the writer should exit.  Real PtyOutput / control frames
            // are always non-empty (they have at minimum a 5-byte header).
            if frame.is_empty() {
                break;
            }
            if ws.write_all(&frame).is_err() {
                break;
            }
        }
    });

    let mut decoder = BrokerFrameDecoder::new();
    let mut pending_fd: Option<OwnedFd> = None;

    loop {
        // Use recvmsg so we capture SCM_RIGHTS ancillary data on FdTransfer.
        let (data, fds) = match recvmsg_fds(sock_fd, 65536) {
            Ok((d, f)) if d.is_empty() && f.is_empty() => break, // Hub disconnected
            Ok(r) => r,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) => continue,
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
                    let agent_key = reg.agent_key.clone();
                    let pty_index = reg.pty_index;
                    let session_id = broker.register(fd, reg, output_tx.clone());
                    let resp = encode_broker_control(&BrokerMessage::Registered {
                        agent_key,
                        pty_index,
                        session_id,
                    });
                    let _ = output_tx.try_send(resp);
                }

                BrokerFrame::PtyInput(session_id, data) => {
                    if let Some(sess) = broker.sessions.get(&session_id) {
                        if let Err(e) = sess.write_input(&data) {
                            log::warn!("[broker] write to session {session_id}: {e}");
                        }
                    }
                }

                BrokerFrame::HubControl(HubMessage::ResizePty { session_id, rows, cols }) => {
                    if let Some(sess) = broker.sessions.get(&session_id) {
                        sess.resize(rows, cols);
                    }
                }

                BrokerFrame::HubControl(HubMessage::GetSnapshot { session_id }) => {
                    if let Some(sess) = broker.sessions.get(&session_id) {
                        let snapshot = sess
                            .parser
                            .lock()
                            .map(|p| generate_ansi_snapshot(&p, false))
                            .unwrap_or_default();
                        let frame = encode_data(frame_type::SNAPSHOT, session_id, &snapshot);
                        let _ = output_tx.try_send(frame);
                    }
                }

                BrokerFrame::HubControl(HubMessage::UnregisterPty { session_id }) => {
                    broker.unregister(session_id);
                    let _ = output_tx.try_send(encode_broker_control(&BrokerMessage::Ack));
                }

                BrokerFrame::HubControl(HubMessage::SetTimeout { seconds }) => {
                    broker.reconnect_timeout = Duration::from_secs(seconds);
                    let _ = output_tx.try_send(encode_broker_control(&BrokerMessage::Ack));
                }

                BrokerFrame::HubControl(HubMessage::KillAll) => {
                    broker.kill_all();
                    drop(output_tx); // signal writer to exit
                    let _ = writer.join();
                    return Ok(());
                }

                BrokerFrame::HubControl(HubMessage::Ping) => {
                    let _ = output_tx.try_send(encode_broker_control(&BrokerMessage::Pong));
                }

                _ => {
                    log::debug!("[broker] ignoring unexpected frame direction");
                }
            }
        }
    }

    // Hub disconnected — wake the writer thread so it exits.
    //
    // After the main loop exits, `output_tx` is dropped here.  Normally the
    // writer's `for frame in output_rx` loop ends when ALL senders are gone.
    // But reader_loop threads for live sessions still hold their own clones of
    // `output_tx`; those threads may be blocked on `file.read()` and will not
    // drop their clone until the session ends.  If we simply drop and join, the
    // writer blocks forever — holding up `handle_connection` and preventing the
    // broker from entering `wait_for_reconnect`.
    //
    // Fix: send an empty sentinel frame before dropping.  The writer thread
    // breaks on the first empty frame (a zero-length `Vec` is never a valid
    // encoded frame, so this cannot be confused with real PTY output).
    let _ = output_tx.try_send(vec![]); // sentinel: signals writer to exit
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
fn wait_for_reconnect(
    listener: &UnixListener,
    timeout: Duration,
) -> Result<Option<UnixStream>> {
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
    let (stream, _) = listener.accept().context("waiting for initial Hub connection")?;
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
