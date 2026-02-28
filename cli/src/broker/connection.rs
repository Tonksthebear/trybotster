//! Hub-side PTY broker connection.
//!
//! Connects to the broker process via a Unix domain socket and provides
//! typed methods for the full Hub → Broker protocol, including SCM_RIGHTS
//! FD transfer for PTY registration.
//!
//! # Lifecycle
//!
//! ```text
//! Hub::setup() ──try_connect_broker()──► BrokerConnection::connect(path)
//!                                             │
//!                                     (broker already running?)
//!                                          yes │  no
//!                                             │   └── spawn broker subprocess
//!                                             ▼
//!                                     set_timeout(120)
//!                                     install_forwarder(event_tx)
//!
//! PTY spawn ──register_pty(key, idx, pid, rows, cols, fd)──► BrokerMessage::Registered
//!
//! Hub restart (graceful) ──disconnect_graceful()──► broker starts timeout window
//! Hub shutdown  (clean)  ──kill_all()──────────────► broker kills children + exits
//! ```
//!
//! # Single-reader architecture
//!
//! After `install_forwarder()` is called, all socket reads are owned by a
//! dedicated demux thread. The thread routes frames to two destinations:
//!
//! - `PtyOutput` / `PtyExited` → `HubEvent` channel (async event loop)
//! - Control responses (`Registered`, `Snapshot`, `Ack`, `Pong`) → internal
//!   `mpsc` channel consumed by `read_response()`
//!
//! This eliminates the race condition that existed when `try_clone_stream()`
//! was used: two readers sharing the same kernel socket receive buffer meant
//! that `Registered` frames could be consumed by the forwarder thread before
//! `read_response()` could see them, causing silent registration failure.
//!
//! # Relay mode after restart
//!
//! When the Hub reconnects after a restart it no longer holds the master PTY
//! FDs. The broker continues reading the PTYs and forwards output via
//! `PtyOutput` frames. The Hub feeds those bytes into an `AlacrittyParser`
//! shadow screen to reconstruct terminal state, and routes PTY input back via `PtyInput`
//! frames until the agent processes terminate.
//!
//! The initial reconnect always calls `get_snapshot()` per session to obtain
//! the ring-buffer contents for immediate shadow-screen reconstruction.

// Rust guideline compliant 2026-02

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::io::RawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use super::protocol::{
    encode_data, encode_fd_transfer, encode_hub_control, frame_type, BrokerFrame,
    BrokerFrameDecoder, BrokerMessage, FdTransferPayload, HubMessage,
};

/// Shared, thread-safe broker connection.
///
/// Wrapped in `Arc<Mutex<Option<...>>>` so both the Hub main loop and
/// Lua primitives can send messages to the broker without dedicated channels.
pub type SharedBrokerConnection = Arc<Mutex<Option<BrokerConnection>>>;

/// Hub-side connection to the PTY broker process.
///
/// Wraps a `UnixStream` and provides typed methods for the broker protocol.
///
/// # Reading model
///
/// Before [`install_forwarder`] is called, `read_response()` reads directly
/// from the socket (used for `set_timeout` and `ping` during initial setup).
///
/// After [`install_forwarder`] is called, a demux thread owns all socket
/// reads and routes frames to two destinations:
///
/// - Output frames (`PtyOutput`, `PtyExited`) → `HubEvent` channel
/// - Control frames (`Registered`, `Snapshot`, `Ack`, `Pong`, `Error`) →
///   internal `mpsc` channel consumed by `read_response()`
///
/// This eliminates the race where the forwarder and `read_response()` both
/// competed to read `Registered` frames off the same socket receive buffer.
///
/// [`install_forwarder`]: BrokerConnection::install_forwarder
pub struct BrokerConnection {
    /// Socket used for writing Hub → Broker frames.
    ///
    /// After `install_forwarder()`, this FD is write-only in practice:
    /// all reads are delegated to the demux thread via the dup'd FD.
    stream: UnixStream,
    /// Direct-read decoder — only used before `install_forwarder()`.
    decoder: BrokerFrameDecoder,
    /// Overflow buffer for the direct-read path (pre-forwarder only).
    frame_buffer: VecDeque<BrokerFrame>,
    /// Channel for control responses delivered by the demux reader thread.
    ///
    /// `None` until `install_forwarder()` is called. Once set, `read_response()`
    /// blocks on this channel instead of the socket to avoid racing with the
    /// forwarder thread.
    response_rx: Option<std::sync::mpsc::Receiver<BrokerFrame>>,
    /// Flag indicating the demux reader thread is still alive.
    ///
    /// Set to `true` by `install_forwarder()`, cleared by the demux thread
    /// on exit (EOF, decode error, or dropped response channel). The Hub
    /// checks this via `is_demux_alive()` during the cleanup tick to detect
    /// silent forwarder death and trigger broker reconnection.
    demux_alive: Arc<AtomicBool>,
}

impl std::fmt::Debug for BrokerConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrokerConnection").finish_non_exhaustive()
    }
}

impl BrokerConnection {
    /// Connect to the broker socket at `path`.
    ///
    /// Sets a 5-second read timeout to bound synchronous response polling.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket does not exist, the connection is refused,
    /// or the read timeout cannot be configured.
    pub fn connect(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path)
            .with_context(|| format!("connect to broker socket: {}", path.display()))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .context("set broker socket read timeout")?;
        Ok(Self {
            stream,
            decoder: BrokerFrameDecoder::new(),
            frame_buffer: VecDeque::new(),
            response_rx: None,
            demux_alive: Arc::new(AtomicBool::new(false)),
        })
    }

    // ── Registration ────────────────────────────────────────────────────────

    /// Register a PTY master FD with the broker via `sendmsg` + SCM_RIGHTS.
    ///
    /// Sends an `FdTransfer` frame whose payload carries the agent key,
    /// PTY index, child PID, and terminal dimensions. The master FD itself
    /// is attached as SCM_RIGHTS ancillary data in the same `sendmsg` call —
    /// the broker receives an independent descriptor pointing to the same
    /// open file description, so the Hub's own copy is unaffected.
    ///
    /// Returns the `session_id` the broker assigned. Store this alongside
    /// the agent's metadata so it can be passed to [`get_snapshot`] on
    /// Hub reconnect.
    ///
    /// # Errors
    ///
    /// Returns an error if the `sendmsg` syscall fails or the broker replies
    /// with an error frame.
    pub fn register_pty(
        &mut self,
        agent_key: &str,
        pty_index: usize,
        child_pid: u32,
        rows: u16,
        cols: u16,
        fd: RawFd,
    ) -> Result<u32> {
        let reg = FdTransferPayload {
            agent_key: agent_key.to_owned(),
            pty_index,
            child_pid,
            rows,
            cols,
        };
        let frame_bytes = encode_fd_transfer(&reg).context("encode FdTransfer")?;
        send_with_fd(&self.stream, &frame_bytes, fd)
            .context("sendmsg FdTransfer to broker")?;

        match self.read_response()? {
            BrokerFrame::BrokerControl(BrokerMessage::Registered { session_id, .. }) => {
                Ok(session_id)
            }
            BrokerFrame::BrokerControl(BrokerMessage::Error { message }) => {
                bail!("broker registration error: {message}")
            }
            other => bail!("unexpected broker response to FdTransfer: {other:?}"),
        }
    }

    // ── Snapshot retrieval ──────────────────────────────────────────────────

    /// Retrieve the raw ring-buffer snapshot for a session.
    ///
    /// After a Hub restart, replay the returned bytes into a fresh
    /// `vt100::Parser` to reconstruct the shadow screen:
    ///
    /// ```rust,ignore
    /// let bytes = conn.get_snapshot(session_id)?;
    /// shadow_screen.lock().unwrap().process(&bytes);
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the send or read fails, or the broker replies
    /// with an error frame.
    pub fn get_snapshot(&mut self, session_id: u32) -> Result<Vec<u8>> {
        let frame = encode_hub_control(&HubMessage::GetSnapshot { session_id });
        self.stream.write_all(&frame).context("send GetSnapshot")?;

        match self.read_response()? {
            BrokerFrame::Snapshot(sid, data) if sid == session_id => Ok(data),
            BrokerFrame::BrokerControl(BrokerMessage::Error { message }) => {
                bail!("broker snapshot error: {message}")
            }
            other => bail!("unexpected broker response to GetSnapshot: {other:?}"),
        }
    }

    // ── PTY control ─────────────────────────────────────────────────────────

    /// Resize a PTY session via the broker.
    ///
    /// Fire-and-forget — the broker issues `ioctl(TIOCSWINSZ)` and sends
    /// no acknowledgment.
    ///
    /// # Errors
    ///
    /// Returns an error if the write to the socket fails.
    pub fn resize_pty(&mut self, session_id: u32, rows: u16, cols: u16) -> Result<()> {
        let frame = encode_hub_control(&HubMessage::ResizePty { session_id, rows, cols });
        self.stream.write_all(&frame).context("send ResizePty")?;
        Ok(())
    }

    /// Write raw bytes to a PTY session via the broker (relay mode after restart).
    ///
    /// Use this only when the Hub no longer holds the master PTY FD directly
    /// (i.e., after a Hub restart before the agent process terminates).
    ///
    /// # Errors
    ///
    /// Returns an error if the write to the socket fails.
    pub fn write_pty_input(&mut self, session_id: u32, data: &[u8]) -> Result<()> {
        let frame = encode_data(frame_type::PTY_INPUT, session_id, data);
        self.stream.write_all(&frame).context("send PtyInput")?;
        Ok(())
    }

    /// Unregister a session whose PTY process has already exited.
    ///
    /// Tells the broker to close the FD and discard the ring buffer.
    /// Best-effort — timeouts on the Ack are silently ignored.
    ///
    /// # Errors
    ///
    /// Returns an error if the write to the socket fails.
    pub fn unregister_pty(&mut self, session_id: u32) -> Result<()> {
        let frame = encode_hub_control(&HubMessage::UnregisterPty { session_id });
        self.stream.write_all(&frame).context("send UnregisterPty")?;
        let _ = self.read_response(); // Ack — best-effort, ignore timeout
        Ok(())
    }

    // ── Lifetime control ─────────────────────────────────────────────────────

    /// Configure the broker reconnect timeout window (seconds).
    ///
    /// Call this immediately after connecting so the broker uses the correct
    /// window when the Hub connection drops.
    ///
    /// # Errors
    ///
    /// Returns an error if the write or response read fails.
    pub fn set_timeout(&mut self, seconds: u64) -> Result<()> {
        let frame = encode_hub_control(&HubMessage::SetTimeout { seconds });
        self.stream.write_all(&frame).context("send SetTimeout")?;
        let _ = self.read_response(); // Ack — best-effort
        Ok(())
    }

    /// Graceful disconnect: close the connection so the broker starts its
    /// configured timeout window.
    ///
    /// Use this when the Hub is **restarting** — the broker will keep PTY
    /// children alive until the new Hub instance reconnects, or the timeout
    /// expires.
    ///
    /// # Shutdown vs. drop
    ///
    /// `shutdown(Both)` is called explicitly before dropping the stream.
    /// Simply dropping the `UnixStream` FD would leave the demux reader thread's
    /// `try_clone()` dup alive, preventing the broker from detecting Hub
    /// disconnect: the broker's `recvmsg_fds` would block indefinitely because
    /// the dup still keeps the socket open.
    ///
    /// `shutdown()` operates on the underlying kernel socket object — it
    /// affects **all** file descriptors that share that socket (including
    /// dup'd copies held by the demux thread). The broker's `recvmsg_fds`
    /// then receives empty data (EOF) and exits `handle_connection`; the
    /// demux thread's `read()` returns an error and the thread exits cleanly.
    pub fn disconnect_graceful(self) {
        // Signal EOF on the kernel socket so the broker detects disconnect
        // immediately, even if the demux thread holds a dup of this socket.
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        drop(self);
    }

    /// Kill all PTY children and exit the broker immediately.
    ///
    /// Use this when the Hub is shutting down **without** a restart — no
    /// reconnect is expected, so there is no reason to keep the broker alive.
    pub fn kill_all(mut self) {
        let frame = encode_hub_control(&HubMessage::KillAll);
        let _ = self.stream.write_all(&frame);
        // Drop closes the socket; broker cleans up and exits.
        drop(self);
    }

    /// Send a keepalive ping and wait for pong.
    ///
    /// Useful for verifying the broker is still responsive before
    /// attempting a registration.
    ///
    /// # Errors
    ///
    /// Returns an error if the write, read, or response frame is unexpected.
    pub fn ping(&mut self) -> Result<()> {
        let frame = encode_hub_control(&HubMessage::Ping);
        self.stream.write_all(&frame).context("send Ping")?;
        match self.read_response()? {
            BrokerFrame::BrokerControl(BrokerMessage::Pong) => Ok(()),
            other => bail!("unexpected ping response: {other:?}"),
        }
    }

    /// Construct a `BrokerConnection` from an existing `UnixStream`.
    ///
    /// Only available in tests — production code always uses [`connect`].
    #[cfg(test)]
    pub(crate) fn from_stream(stream: UnixStream) -> Self {
        Self {
            stream,
            decoder: BrokerFrameDecoder::new(),
            frame_buffer: VecDeque::new(),
            response_rx: None,
            demux_alive: Arc::new(AtomicBool::new(false)),
        }
    }

    // ── Internal ─────────────────────────────────────────────────────────────

    /// Read one complete control frame from the broker.
    ///
    /// # Two reading modes
    ///
    /// **Pre-forwarder** (before `install_forwarder()`): reads directly from
    /// the socket. Used during initial setup for `set_timeout` / `ping`.
    ///
    /// **Post-forwarder** (after `install_forwarder()`): reads from the
    /// internal `mpsc` channel fed by the demux reader thread. The demux
    /// thread is the sole socket reader, so there is no race between this
    /// method and the output forwarder path.
    ///
    /// # Errors
    ///
    /// Returns `Err` on socket EOF, I/O error, decode failure (pre-forwarder),
    /// or channel disconnect (post-forwarder — the demux thread exited).
    fn read_response(&mut self) -> Result<BrokerFrame> {
        if let Some(ref rx) = self.response_rx {
            // Post-forwarder: demux thread owns all socket reads and sends
            // control frames here. No socket contention.
            return rx.recv().context("broker demux reader thread disconnected");
        }

        // Pre-forwarder: read directly from the socket (no race yet).
        if let Some(frame) = self.frame_buffer.pop_front() {
            return Ok(frame);
        }
        let mut buf = [0u8; 4096];
        loop {
            let n = self.stream.read(&mut buf).context("read from broker")?;
            if n == 0 {
                bail!("broker closed connection unexpectedly");
            }
            let mut frames = self.decoder.feed(&buf[..n])?.into_iter();
            if let Some(frame) = frames.next() {
                self.frame_buffer.extend(frames);
                return Ok(frame);
            }
        }
    }

    /// Install the demux reader thread and switch `read_response()` to channel mode.
    ///
    /// Spawns a background thread that reads **all** frames from a dup of the
    /// broker socket and routes them:
    ///
    /// - `PtyOutput` / `PtyExited` → `event_tx` (Hub event loop)
    /// - All other frames (`Registered`, `Snapshot`, `Ack`, `Pong`, `Error`) →
    ///   internal channel consumed by [`read_response()`]
    ///
    /// After this call, `read_response()` never reads the socket directly, so
    /// there is no race between registration calls and the forwarder.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket dup fails or the thread cannot be spawned.
    /// On error the forwarder is not installed — `read_response()` falls back to
    /// direct socket reads (output forwarding will be absent but commands work).
    pub fn install_forwarder(
        &mut self,
        event_tx: tokio::sync::mpsc::UnboundedSender<crate::hub::events::HubEvent>,
    ) -> Result<()> {
        let reader_stream = self.stream.try_clone()
            .context("dup broker socket for demux reader")?;
        let (response_tx, response_rx) = std::sync::mpsc::channel::<BrokerFrame>();
        self.response_rx = Some(response_rx);

        // Mark demux as alive before spawning — the thread clears this on exit.
        self.demux_alive.store(true, Ordering::Release);
        let alive_flag = Arc::clone(&self.demux_alive);

        std::thread::Builder::new()
            .name("broker-demux".to_owned())
            .spawn(move || {
                demux_reader(reader_stream, response_tx, event_tx);
                alive_flag.store(false, Ordering::Release);
            })
            .context("spawn broker-demux thread")?;
        Ok(())
    }

    /// Check whether the demux reader thread is still running.
    ///
    /// Returns `false` if the thread exited (socket EOF, decode error,
    /// or dropped response channel). The Hub should trigger a broker
    /// reconnect when this returns `false` after `install_forwarder()`
    /// was called.
    pub fn is_demux_alive(&self) -> bool {
        self.demux_alive.load(Ordering::Acquire)
    }

    /// Whether `install_forwarder()` has been called on this connection.
    ///
    /// Returns `true` once the demux thread has been started and `response_rx`
    /// is set. Used by the Hub's health check to distinguish "forwarder never
    /// installed" (skip check) from "forwarder died" (fire event).
    pub fn has_forwarder(&self) -> bool {
        self.response_rx.is_some()
    }
}

// ── SCM_RIGHTS send ──────────────────────────────────────────────────────────

/// Send `data` bytes with `fd` attached via `sendmsg` + SCM_RIGHTS.
///
/// This is the standard POSIX mechanism for passing a file descriptor between
/// processes over a Unix domain socket. The kernel duplicates the FD into the
/// receiving process — the sender retains its own copy and the FD remains
/// valid in both processes after the call returns.
///
/// `O_CLOEXEC` is a per-process attribute and does **not** block SCM_RIGHTS
/// transfers; no special handling is required for cloexec-flagged FDs.
fn send_with_fd(stream: &UnixStream, data: &[u8], fd: RawFd) -> Result<()> {
    use std::os::unix::io::AsRawFd;

    let sock_fd = stream.as_raw_fd();
    let fd_size = std::mem::size_of::<libc::c_int>();
    // CMSG_SPACE includes the cmsghdr header overhead.
    let cmsg_space = unsafe { libc::CMSG_SPACE(fd_size as u32) } as usize;
    let mut cmsg_buf = vec![0u8; cmsg_space];

    let mut iov = libc::iovec {
        iov_base: data.as_ptr() as *mut libc::c_void,
        iov_len: data.len(),
    };

    let msg = libc::msghdr {
        msg_name: std::ptr::null_mut(),
        msg_namelen: 0,
        msg_iov: &mut iov,
        msg_iovlen: 1,
        msg_control: cmsg_buf.as_mut_ptr() as *mut libc::c_void,
        msg_controllen: cmsg_space as _,
        msg_flags: 0,
    };

    // Populate cmsghdr with SOL_SOCKET / SCM_RIGHTS and the FD value.
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg as *const _);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(fd_size as libc::c_uint);
        let data_ptr = libc::CMSG_DATA(cmsg) as *mut libc::c_int;
        std::ptr::write_unaligned(data_ptr, fd);
    }

    let n = unsafe { libc::sendmsg(sock_fd, &msg, 0) };
    if n < 0 {
        return Err(anyhow::anyhow!(
            "sendmsg failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(all(test, unix))]
mod tests {
    use super::send_with_fd;
    use super::super::broker_socket_path;
    use std::os::unix::io::{AsRawFd, FromRawFd};
    use std::os::unix::net::UnixStream;

    // ── SCM_RIGHTS helpers ────────────────────────────────────────────────────

    /// Receive one message from `sock`, extracting any SCM_RIGHTS ancillary FDs.
    unsafe fn recv_with_fd(sock: libc::c_int) -> (Vec<u8>, Vec<libc::c_int>) {
        unsafe {
            let mut data_buf = vec![0u8; 4096];
            let fd_size = std::mem::size_of::<libc::c_int>();
            let cmsg_space = libc::CMSG_SPACE(fd_size as u32) as usize;
            let mut cmsg_buf = vec![0u8; cmsg_space * 4];

            let mut iov = libc::iovec {
                iov_base: data_buf.as_mut_ptr() as *mut libc::c_void,
                iov_len: data_buf.len(),
            };
            let msg = libc::msghdr {
                msg_name: std::ptr::null_mut(),
                msg_namelen: 0,
                msg_iov: &mut iov,
                msg_iovlen: 1,
                msg_control: cmsg_buf.as_mut_ptr() as *mut libc::c_void,
                msg_controllen: cmsg_buf.len() as _,
                msg_flags: 0,
            };
            // msghdr must be mut for recvmsg; shadow with a mutable binding.
            let mut msg = msg;

            let n = libc::recvmsg(sock, &mut msg, 0);
            assert!(n >= 0, "recvmsg failed: {}", std::io::Error::last_os_error());
            data_buf.truncate(n as usize);

            let mut fds = Vec::new();
            let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                    let data = libc::CMSG_DATA(cmsg);
                    let count = ((*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize)
                        / std::mem::size_of::<libc::c_int>();
                    for i in 0..count {
                        let fd: libc::c_int = std::ptr::read_unaligned(
                            data.add(i * std::mem::size_of::<libc::c_int>()) as *const libc::c_int,
                        );
                        fds.push(fd);
                    }
                }
                cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
            }

            (data_buf, fds)
        }
    }

    // ── SCM_RIGHTS send/receive ───────────────────────────────────────────────

    /// Verify that `send_with_fd` transfers an FD via SCM_RIGHTS and the
    /// received descriptor refers to the same open file description.
    #[test]
    fn test_fd_passthrough_via_scm_rights() {
        // Create a socketpair for the control channel.
        let mut sv: [libc::c_int; 2] = [0; 2];
        let ret = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) };
        assert_eq!(ret, 0, "socketpair: {}", std::io::Error::last_os_error());
        let (sender_sock, receiver_sock) = (sv[0], sv[1]);

        // Create a pipe — we will pass the read end via SCM_RIGHTS.
        let mut pipefd: [libc::c_int; 2] = [0; 2];
        let ret = unsafe { libc::pipe(pipefd.as_mut_ptr()) };
        assert_eq!(ret, 0, "pipe: {}", std::io::Error::last_os_error());
        let (pipe_read, pipe_write) = (pipefd[0], pipefd[1]);

        // Build a UnixStream wrapper around sender_sock for send_with_fd.
        let stream = unsafe { UnixStream::from_raw_fd(sender_sock) };

        let sentinel = b"fd-xfer-test";
        send_with_fd(&stream, sentinel, pipe_read).expect("send_with_fd should succeed");

        // stream drop would close sender_sock — prevent that.
        let sender_sock_fd = stream.as_raw_fd();
        std::mem::forget(stream);

        // Receive on the other end.
        let (data, fds) = unsafe { recv_with_fd(receiver_sock) };

        // Frame bytes arrived intact.
        assert_eq!(&data, sentinel);

        // Exactly one FD was transferred.
        assert_eq!(fds.len(), 1, "expected 1 received FD");
        let received_fd = fds[0];

        // Write to the write end; read from the received (duplicated) read end.
        let msg = b"hello through SCM_RIGHTS";
        let written = unsafe { libc::write(pipe_write, msg.as_ptr() as *const libc::c_void, msg.len()) };
        assert_eq!(written as usize, msg.len());

        let mut read_buf = vec![0u8; msg.len()];
        let n = unsafe { libc::read(received_fd, read_buf.as_mut_ptr() as *mut libc::c_void, read_buf.len()) };
        assert_eq!(n as usize, msg.len());
        assert_eq!(&read_buf, msg);

        // Cleanup.
        unsafe {
            libc::close(received_fd);
            libc::close(pipe_read);
            libc::close(pipe_write);
            libc::close(sender_sock_fd);
            libc::close(receiver_sock);
        }
    }

    /// The kernel duplicates the FD into the receiver's process; closing the
    /// sender's original copy must not affect the receiver's descriptor.
    #[test]
    fn test_received_fd_valid_after_sender_closes_original() {
        let mut sv: [libc::c_int; 2] = [0; 2];
        let ret = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) };
        assert_eq!(ret, 0, "socketpair: {}", std::io::Error::last_os_error());
        let (sender_sock, receiver_sock) = (sv[0], sv[1]);

        let mut pipefd: [libc::c_int; 2] = [0; 2];
        let ret = unsafe { libc::pipe(pipefd.as_mut_ptr()) };
        assert_eq!(ret, 0, "pipe: {}", std::io::Error::last_os_error());
        let (pipe_read, pipe_write) = (pipefd[0], pipefd[1]);

        let stream = unsafe { UnixStream::from_raw_fd(sender_sock) };
        send_with_fd(&stream, b"x", pipe_read).expect("send_with_fd");
        let sender_sock_fd = stream.as_raw_fd();
        std::mem::forget(stream);

        let (_data, fds) = unsafe { recv_with_fd(receiver_sock) };
        assert_eq!(fds.len(), 1);
        let received_fd = fds[0];

        // Close the sender's original read end — receiver must still work.
        unsafe { libc::close(pipe_read) };

        let msg = b"independent copy";
        unsafe { libc::write(pipe_write, msg.as_ptr() as *const libc::c_void, msg.len()) };

        let mut read_buf = vec![0u8; msg.len()];
        let n = unsafe {
            libc::read(received_fd, read_buf.as_mut_ptr() as *mut libc::c_void, read_buf.len())
        };
        assert_eq!(n as usize, msg.len(), "received FD should be readable after sender closes original");
        assert_eq!(&read_buf, msg);

        unsafe {
            libc::close(received_fd);
            libc::close(pipe_write);
            libc::close(sender_sock_fd);
            libc::close(receiver_sock);
        }
    }

    // ── Socket bind ──────────────────────────────────────────────────────────

    /// `broker_socket_path` must produce a path under /tmp with the hub_id embedded.
    #[test]
    fn test_broker_socket_path_format() {
        let path = broker_socket_path("abc123").expect("path should be valid");
        let s = path.to_string_lossy();
        assert!(s.contains("broker-abc123.sock"), "path should contain hub_id: {s}");
        assert!(s.starts_with("/tmp/"), "path should be under /tmp: {s}");
    }

    /// A hub_id that would push the path over the macOS 104-byte limit should fail.
    #[test]
    fn test_broker_socket_path_too_long_fails() {
        let long_id = "x".repeat(200);
        let result = broker_socket_path(&long_id);
        assert!(result.is_err(), "excessively long hub_id should produce an error");
    }
}

/// Demux reader loop — sole consumer of the broker socket receive buffer.
///
/// Reads all frames from `stream` and routes them:
///
/// - `PtyOutput` / `PtyExited` → `event_tx` (Hub async event loop)
/// - All other frames (control responses: `Registered`, `Snapshot`, `Ack`,
///   `Pong`, `Error`) → `response_tx` (consumed by `read_response()`)
///
/// Running as the sole reader eliminates the race condition that existed when
/// both `read_response()` and a separate forwarder thread competed to read
/// from dup'd file descriptors sharing the same kernel receive buffer.
fn demux_reader(
    stream: UnixStream,
    response_tx: std::sync::mpsc::Sender<BrokerFrame>,
    event_tx: tokio::sync::mpsc::UnboundedSender<crate::hub::events::HubEvent>,
) {
    let mut decoder = BrokerFrameDecoder::new();
    let mut stream = stream;
    // Block indefinitely — the Hub sends data only when needed, and the PTY
    // sends output continuously. A timeout here would cause spurious errors.
    let _ = stream.set_read_timeout(None);
    let mut buf = [0u8; 8192];

    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let frames = match decoder.feed(&buf[..n]) {
            Ok(f) => f,
            Err(e) => {
                log::warn!("[broker-demux] decode error: {e}");
                break;
            }
        };
        for frame in frames {
            match frame {
                BrokerFrame::PtyOutput(session_id, data) => {
                    let _ = event_tx.send(
                        crate::hub::events::HubEvent::BrokerPtyOutput { session_id, data },
                    );
                }
                BrokerFrame::BrokerControl(BrokerMessage::PtyExited {
                    session_id,
                    agent_key,
                    pty_index,
                    exit_code,
                }) => {
                    let _ = event_tx.send(
                        crate::hub::events::HubEvent::BrokerPtyExited {
                            session_id,
                            agent_key,
                            pty_index,
                            exit_code,
                        },
                    );
                }
                other => {
                    // Control response: Registered, Snapshot, Ack, Pong, Error, etc.
                    // Route to BrokerConnection::read_response() via channel.
                    if response_tx.send(other).is_err() {
                        // BrokerConnection was dropped — no one is waiting.
                        break;
                    }
                }
            }
        }
    }
    log::debug!("[broker-demux] thread exiting");
}

// ─── Integration tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod integration_tests {
    use super::*;
    use super::super::protocol::encode_broker_control;
    use crate::hub::events::HubEvent;
    use std::os::unix::io::AsRawFd;

    /// Receive one message from `sock`, extracting any SCM_RIGHTS ancillary FDs.
    unsafe fn recv_with_fd(sock: libc::c_int) -> (Vec<u8>, Vec<libc::c_int>) {
        unsafe {
            let mut data_buf = vec![0u8; 4096];
            let fd_size = std::mem::size_of::<libc::c_int>();
            let cmsg_space = libc::CMSG_SPACE(fd_size as u32) as usize;
            let mut cmsg_buf = vec![0u8; cmsg_space * 4];

            let mut iov = libc::iovec {
                iov_base: data_buf.as_mut_ptr() as *mut libc::c_void,
                iov_len: data_buf.len(),
            };
            let msg = libc::msghdr {
                msg_name: std::ptr::null_mut(),
                msg_namelen: 0,
                msg_iov: &mut iov,
                msg_iovlen: 1,
                msg_control: cmsg_buf.as_mut_ptr() as *mut libc::c_void,
                msg_controllen: cmsg_buf.len() as _,
                msg_flags: 0,
            };
            let mut msg = msg;

            let n = libc::recvmsg(sock, &mut msg, 0);
            assert!(n >= 0, "recvmsg failed: {}", std::io::Error::last_os_error());
            data_buf.truncate(n as usize);

            let mut fds = Vec::new();
            let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET
                    && (*cmsg).cmsg_type == libc::SCM_RIGHTS
                {
                    let data = libc::CMSG_DATA(cmsg);
                    let count = ((*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize)
                        / std::mem::size_of::<libc::c_int>();
                    for i in 0..count {
                        let fd: libc::c_int = std::ptr::read_unaligned(
                            data.add(i * std::mem::size_of::<libc::c_int>()) as *const libc::c_int,
                        );
                        fds.push(fd);
                    }
                }
                cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
            }

            (data_buf, fds)
        }
    }

    /// Create a pipe and return (read_fd, write_fd). Caller must close both.
    fn make_pipe() -> (i32, i32) {
        let mut pipe_fds = [0i32; 2];
        let ret = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
        assert_eq!(ret, 0, "pipe: {}", std::io::Error::last_os_error());
        (pipe_fds[0], pipe_fds[1])
    }

    // ── Test 1: THE REGRESSION TEST ─────────────────────────────────────────

    /// Proves the race condition fix: `Registered` reaches `read_response()`
    /// through the demux channel, not by racing on the socket receive buffer.
    ///
    /// Before the fix, `install_forwarder()` used `try_clone_stream()` which
    /// created a dup FD. Both the forwarder thread and `read_response()` competed
    /// on the same kernel socket receive buffer — if the forwarder won, `Registered`
    /// was consumed as a `PtyOutput` candidate and `read_response()` blocked forever.
    #[test]
    fn test_register_pty_with_forwarder_running() {
        let (broker_stream, client_stream) = UnixStream::pair().unwrap();
        let broker_fd = broker_stream.as_raw_fd();

        // Create a pipe FD to pass via SCM_RIGHTS in register_pty.
        let (pipe_read, pipe_write) = make_pipe();

        // Mock broker thread: receive FdTransfer, send back Registered.
        let broker_handle = std::thread::spawn(move || {
            // Receive the FdTransfer frame + SCM_RIGHTS FD.
            let (data, fds) = unsafe { recv_with_fd(broker_fd) };
            assert!(!data.is_empty(), "should receive FdTransfer frame bytes");
            assert_eq!(fds.len(), 1, "should receive exactly 1 FD via SCM_RIGHTS");

            // Close the received FD — we don't need it.
            unsafe { libc::close(fds[0]) };

            // Decode the frame to verify it's FdTransfer.
            let mut decoder = BrokerFrameDecoder::new();
            let frames = decoder.feed(&data).unwrap();
            assert_eq!(frames.len(), 1);
            assert!(
                matches!(&frames[0], BrokerFrame::FdTransfer(_)),
                "expected FdTransfer frame"
            );

            // Send Registered response.
            let resp = encode_broker_control(&BrokerMessage::Registered {
                agent_key: "test-agent".to_string(),
                pty_index: 0,
                session_id: 42,
            });
            use std::io::Write;
            let mut bs = broker_stream;
            bs.write_all(&resp).unwrap();
        });

        let mut conn = BrokerConnection::from_stream(client_stream);

        // Create event channel and install forwarder BEFORE registration.
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<HubEvent>();
        conn.install_forwarder(event_tx).unwrap();

        // Register PTY — this must succeed (Registered routed to read_response via channel).
        let session_id = conn.register_pty("test-agent", 0, 0, 24, 80, pipe_read).unwrap();
        assert_eq!(session_id, 42, "register_pty should return broker-assigned session_id");

        // Registered must NOT have leaked to the event channel.
        assert!(
            event_rx.try_recv().is_err(),
            "Registered frame must not appear in event channel"
        );

        broker_handle.join().unwrap();

        // Cleanup pipe FDs.
        unsafe {
            libc::close(pipe_read);
            libc::close(pipe_write);
        }
    }

    // ── Test 2: PtyOutput routes to event channel ───────────────────────────

    /// Verifies that `PtyOutput` frames are routed to the event channel (not
    /// consumed by `read_response()`).
    #[test]
    fn test_pty_output_routes_to_event_channel() {
        let (broker_stream, client_stream) = UnixStream::pair().unwrap();

        let mut conn = BrokerConnection::from_stream(client_stream);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<HubEvent>();
        conn.install_forwarder(event_tx).unwrap();

        // Mock broker: send a PtyOutput frame.
        std::thread::spawn(move || {
            use std::io::Write;
            let frame = encode_data(frame_type::PTY_OUTPUT, 7, b"hello world");
            let mut bs = broker_stream;
            bs.write_all(&frame).unwrap();
            // Drop closes the socket, which will cause the demux reader to exit.
        });

        // Receive the event with a timeout.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await
        });

        match result {
            Ok(Some(HubEvent::BrokerPtyOutput { session_id, data })) => {
                assert_eq!(session_id, 7);
                assert_eq!(data, b"hello world");
            }
            other => panic!("expected BrokerPtyOutput event, got: {other:?}"),
        }
    }

    // ── Test 3: set_timeout works before forwarder ──────────────────────────

    /// Verifies `set_timeout()` works in pre-forwarder mode (direct socket reads).
    #[test]
    fn test_set_timeout_works_before_forwarder() {
        let (broker_stream, client_stream) = UnixStream::pair().unwrap();

        // Mock broker: read SetTimeout frame, send Ack.
        let broker_handle = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let mut bs = broker_stream;
            let mut buf = [0u8; 4096];
            let n = bs.read(&mut buf).unwrap();
            assert!(n > 0, "should receive SetTimeout frame");

            let mut decoder = BrokerFrameDecoder::new();
            let frames = decoder.feed(&buf[..n]).unwrap();
            assert_eq!(frames.len(), 1);
            assert!(
                matches!(
                    &frames[0],
                    BrokerFrame::HubControl(super::super::protocol::HubMessage::SetTimeout {
                        seconds: 60
                    })
                ),
                "expected SetTimeout(60)"
            );

            let resp = encode_broker_control(&BrokerMessage::Ack);
            bs.write_all(&resp).unwrap();
        });

        let mut conn = BrokerConnection::from_stream(client_stream);
        // No install_forwarder — tests the pre-forwarder direct-read path.
        conn.set_timeout(60).unwrap();

        broker_handle.join().unwrap();
    }

    // ── Test 4: ping works before forwarder ─────────────────────────────────

    /// Verifies `ping()` works in pre-forwarder mode.
    #[test]
    fn test_ping_works_before_forwarder() {
        let (broker_stream, client_stream) = UnixStream::pair().unwrap();

        // Mock broker: read Ping, send Pong.
        let broker_handle = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let mut bs = broker_stream;
            let mut buf = [0u8; 4096];
            let n = bs.read(&mut buf).unwrap();
            assert!(n > 0, "should receive Ping frame");

            let mut decoder = BrokerFrameDecoder::new();
            let frames = decoder.feed(&buf[..n]).unwrap();
            assert_eq!(frames.len(), 1);
            assert!(
                matches!(&frames[0], BrokerFrame::HubControl(super::super::protocol::HubMessage::Ping)),
                "expected Ping"
            );

            let resp = encode_broker_control(&BrokerMessage::Pong);
            bs.write_all(&resp).unwrap();
        });

        let mut conn = BrokerConnection::from_stream(client_stream);
        conn.ping().unwrap();

        broker_handle.join().unwrap();
    }

    // ── Test 5: get_snapshot with forwarder running ─────────────────────────

    /// Verifies `get_snapshot()` works after `install_forwarder()` — the
    /// Snapshot response is routed through the demux channel to `read_response()`,
    /// not to the event channel.
    #[test]
    fn test_get_snapshot_with_forwarder_running() {
        let (broker_stream, client_stream) = UnixStream::pair().unwrap();

        // Mock broker: read GetSnapshot, send Snapshot data frame.
        let broker_handle = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let mut bs = broker_stream;
            let mut buf = [0u8; 4096];
            let n = bs.read(&mut buf).unwrap();
            assert!(n > 0, "should receive GetSnapshot frame");

            let mut decoder = BrokerFrameDecoder::new();
            let frames = decoder.feed(&buf[..n]).unwrap();
            assert_eq!(frames.len(), 1);
            assert!(
                matches!(
                    &frames[0],
                    BrokerFrame::HubControl(super::super::protocol::HubMessage::GetSnapshot {
                        session_id: 1
                    })
                ),
                "expected GetSnapshot(1)"
            );

            let resp = encode_data(frame_type::SNAPSHOT, 1, b"screen-data");
            bs.write_all(&resp).unwrap();
        });

        let mut conn = BrokerConnection::from_stream(client_stream);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<HubEvent>();
        conn.install_forwarder(event_tx).unwrap();

        let snapshot = conn.get_snapshot(1).unwrap();
        assert_eq!(snapshot, b"screen-data", "snapshot data should match");

        // Snapshot must NOT have leaked to the event channel.
        assert!(
            event_rx.try_recv().is_err(),
            "Snapshot frame must not appear in event channel"
        );

        broker_handle.join().unwrap();
    }
}
