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
//!
//! PTY spawn ──register_pty(key, idx, pid, rows, cols, fd)──► BrokerMessage::Registered
//!
//! Hub restart (graceful) ──disconnect_graceful()──► broker starts timeout window
//! Hub shutdown  (clean)  ──kill_all()──────────────► broker kills children + exits
//! ```
//!
//! # Relay mode after restart
//!
//! When the Hub reconnects after a restart it no longer holds the master PTY
//! FDs. The broker continues reading the PTYs and forwards output via
//! `PtyOutput` frames. The Hub feeds those bytes into a fresh `vt100::Parser`
//! to reconstruct the shadow screen, and routes PTY input back via `PtyInput`
//! frames until the agent processes terminate.
//!
//! The initial reconnect always calls `get_snapshot()` per session to obtain
//! the ring-buffer contents for immediate shadow-screen reconstruction.

// Rust guideline compliant 2026-02

use std::io::{Read, Write};
use std::os::unix::io::RawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
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
/// Operates in **blocking mode** with a short read timeout — suitable for
/// synchronous request/response flows (registration, snapshot, control).
///
/// Broker-initiated frames (`PtyOutput`, `PtyExited`) are delivered via a
/// background reader thread started by [`BrokerConnection::start_output_forwarder`].
pub struct BrokerConnection {
    stream: UnixStream,
    decoder: BrokerFrameDecoder,
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
        let frame_bytes = encode_fd_transfer(&reg);
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
    pub fn disconnect_graceful(self) {
        // Dropping self closes the UnixStream, which the broker detects as
        // Hub disconnect and starts the reconnect countdown.
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

    /// Clone the underlying socket for use in a background reader thread.
    ///
    /// The background thread reads `PtyOutput` and `PtyExited` frames from
    /// the broker and feeds them into the Hub's event bus.  The main connection
    /// object retains write access for control messages.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket clone fails.
    pub fn try_clone_stream(&self) -> Result<UnixStream> {
        self.stream.try_clone().context("clone broker socket for reader thread")
    }

    // ── Internal ─────────────────────────────────────────────────────────────

    /// Read one complete control or data frame from the broker socket.
    ///
    /// Accumulates bytes until the frame decoder produces a complete frame.
    /// Returns `Err` on EOF, I/O error, or decode failure.
    fn read_response(&mut self) -> Result<BrokerFrame> {
        let mut buf = [0u8; 4096];
        loop {
            let n = self.stream.read(&mut buf).context("read from broker")?;
            if n == 0 {
                bail!("broker closed connection unexpectedly");
            }
            let frames = self.decoder.feed(&buf[..n])?;
            if let Some(frame) = frames.into_iter().next() {
                return Ok(frame);
            }
        }
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

    let mut msg = libc::msghdr {
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
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(fd_size as u32) as _;
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
        let mut data_buf = vec![0u8; 4096];
        let fd_size = std::mem::size_of::<libc::c_int>();
        let cmsg_space = libc::CMSG_SPACE(fd_size as u32) as usize;
        let mut cmsg_buf = vec![0u8; cmsg_space * 4];

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
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) };
        let (sender_sock, receiver_sock) = (sv[0], sv[1]);

        let mut pipefd: [libc::c_int; 2] = [0; 2];
        unsafe { libc::pipe(pipefd.as_mut_ptr()) };
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

/// Start a background thread that reads broker-initiated frames from `stream`
/// and routes them to `event_tx`.
///
/// Handles:
/// - `BrokerFrame::PtyOutput(session_id, bytes)` → `HubEvent::BrokerPtyOutput`
/// - `BrokerFrame::BrokerControl(BrokerMessage::PtyExited { .. })` → `HubEvent::BrokerPtyExited`
///
/// The thread exits silently when the stream closes.
pub(crate) fn start_output_forwarder(
    stream: UnixStream,
    event_tx: tokio::sync::mpsc::UnboundedSender<crate::hub::events::HubEvent>,
) {
    std::thread::Builder::new()
        .name("broker-reader".to_owned())
        .spawn(move || {
            let mut decoder = BrokerFrameDecoder::new();
            let mut stream = stream;
            // Remove read timeout for the reader thread — it should block until data arrives.
            let _ = stream.set_read_timeout(None);
            let mut buf = [0u8; 8192];
            loop {
                let n = match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break, // broker closed or error
                    Ok(n) => n,
                };
                let frames = match decoder.feed(&buf[..n]) {
                    Ok(f) => f,
                    Err(e) => {
                        log::warn!("[broker-reader] decode error: {e}");
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
                        _ => {
                            log::debug!("[broker-reader] ignoring unexpected frame");
                        }
                    }
                }
            }
            log::debug!("[broker-reader] thread exiting");
        })
        .ok(); // thread spawn failure is non-fatal
}
