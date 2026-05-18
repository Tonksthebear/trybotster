//! Hub-side connection to a per-session process.
//!
//! Each `SessionConnection` owns a Unix socket stream to one session process.
//! After `install_reader()`, a SessionIoWorker reads all frames from the socket:
//!
//! - `PtyOutput` → coalesced/fanned out by SessionIo to subscribed ClientWorkers
//! - Structured events (0x10-0x15) → mapped to `PtyEvent` variants, atomics updated
//! - `ProcessExited` → sent as `HubEvent::SessionProcessExited`
//! - Control responses (Snapshot, Screen, ModeFlags, Pong) → routed to `response_rx`
//!
//! RPCs (get_snapshot, get_screen, etc.) send their request on the write stream
//! and receive the response via `response_rx`. No socket read contention.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::sync::broadcast;

use crate::agent::pty::PtyEvent;
use crate::worker::session_io::{SessionIoRequest, SESSION_IO_WORKER_QUEUE};
use crate::worker::session_io_runtime::{SessionIoWorker, SessionIoWorkerConfig};

use super::protocol::*;
use super::SpawnConfig;

/// Response timeout for RPCs after reader is installed.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Shared session connection for concurrent access from PtyHandle.
pub type SharedSessionConnection = Arc<Mutex<Option<SessionConnection>>>;

/// Hub-side connection to a single session process.
pub struct SessionConnection {
    stream: UnixStream,
    /// Pre-reader: frame decoder for direct socket reads.
    /// Post-reader: unused (reader thread owns decoding).
    decoder: FrameDecoder,
    /// Post-reader: RPC responses arrive here.
    response_rx: Option<std::sync::mpsc::Receiver<Frame>>,
    /// Whether the session I/O worker is alive.
    reader_alive: Arc<AtomicBool>,
    /// Mailbox for session I/O requests owned by the worker companion thread.
    session_io_tx: Option<tokio::sync::mpsc::Sender<SessionIoRequest>>,
    /// Protocol version negotiated during handshake.
    pub protocol_version: u8,
    /// Session metadata received during handshake.
    pub metadata: SessionMetadata,
}

impl std::fmt::Debug for SessionConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionConnection")
            .field("protocol_version", &self.protocol_version)
            .field("metadata", &self.metadata)
            .field("reader_alive", &self.reader_alive)
            .finish_non_exhaustive()
    }
}

impl SessionConnection {
    /// Build a test connection with an injected Session I/O mailbox.
    #[cfg(test)]
    pub(crate) fn test_with_session_io_sender(
        session_io_tx: tokio::sync::mpsc::Sender<SessionIoRequest>,
    ) -> Self {
        Self::test_with_session_io_sender_and_snapshot(session_io_tx, None)
    }

    /// Build a test connection with an injected Session I/O mailbox and
    /// optional snapshot response.
    #[cfg(test)]
    pub(crate) fn test_with_session_io_sender_and_snapshot(
        session_io_tx: tokio::sync::mpsc::Sender<SessionIoRequest>,
        snapshot: Option<Vec<u8>>,
    ) -> Self {
        let (stream, _peer) = UnixStream::pair().expect("test session connection pair");
        let (response_tx, response_rx) = std::sync::mpsc::channel::<Frame>();
        if let Some(payload) = snapshot {
            response_tx
                .send(Frame {
                    frame_type: FRAME_SNAPSHOT,
                    payload,
                })
                .expect("seed test snapshot response");
        }
        let reader_alive = Arc::new(AtomicBool::new(true));
        Self {
            stream,
            decoder: FrameDecoder::new(),
            response_rx: Some(response_rx),
            reader_alive,
            session_io_tx: Some(session_io_tx),
            protocol_version: PROTOCOL_VERSION,
            metadata: SessionMetadata {
                session_uuid: "test-session".to_string(),
                pid: std::process::id(),
                rows: 24,
                cols: 80,
                last_output_at: 0,
                title: None,
                cwd: None,
                port: None,
                mode_flags: ModeFlags::default(),
                recovery_identity: None,
            },
        }
    }

    /// Connect to a session process socket and perform handshake.
    pub fn connect(socket_path: &Path) -> Result<Self> {
        let mut stream = UnixStream::connect(socket_path)
            .with_context(|| format!("connect to session: {}", socket_path.display()))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .context("set session socket read timeout")?;

        let (version, metadata) = handshake_hub(&mut stream).context("session handshake")?;

        Ok(Self {
            stream,
            decoder: FrameDecoder::new(),
            response_rx: None,
            reader_alive: Arc::new(AtomicBool::new(false)),
            session_io_tx: None,
            protocol_version: version,
            metadata,
        })
    }

    /// Send spawn configuration to the session process.
    pub fn send_spawn_config(&mut self, config: &SpawnConfig) -> Result<()> {
        let frame = encode_json(FRAME_PTY_INPUT, config)?;
        self.stream.write_all(&frame).context("send spawn config")?;
        self.stream.flush().context("flush spawn config")?;
        Ok(())
    }

    /// Install the session I/O worker.
    ///
    /// Spawns a background worker that reads all frames from a dup of the
    /// session socket and routes them:
    ///
    /// - `PtyOutput` → broadcasts `PtyEvent::Output` (no shadow screen parsing)
    /// - Structured events (0x10-0x15) → mapped to `PtyEvent` variants
    /// - `ProcessExited` → `hub_event_tx` as `SessionProcessExited`
    /// - Control responses → `response_rx` for RPC callers
    ///
    /// After this call, `read_response()` uses the channel instead of
    /// reading the socket directly.
    pub(crate) fn install_reader(
        &mut self,
        session_uuid: String,
        event_tx: broadcast::Sender<PtyEvent>,
        kitty_enabled: Arc<AtomicBool>,
        cursor_visible: Arc<AtomicBool>,
        resize_pending: Arc<AtomicBool>,
        last_output_at: Arc<AtomicU64>,
        last_human_input_ms: Arc<std::sync::atomic::AtomicI64>,
        hub_event_tx: crate::hub::events::HubEventTx,
    ) -> Result<()> {
        let reader_stream = self
            .stream
            .try_clone()
            .context("dup session socket for reader")?;
        let write_stream = self
            .stream
            .try_clone()
            .context("dup session socket for request processor")?;
        let (session_io_tx, request_rx) =
            tokio::sync::mpsc::channel(SESSION_IO_WORKER_QUEUE.capacity);
        let (response_tx, response_rx) = std::sync::mpsc::channel::<Frame>();
        let terminal_subscriptions = Arc::new(Mutex::new(std::collections::HashMap::new()));
        self.response_rx = Some(response_rx);
        self.session_io_tx = Some(session_io_tx);
        self.reader_alive.store(true, Ordering::Release);
        let _handle = SessionIoWorker::spawn(SessionIoWorkerConfig {
            stream: reader_stream,
            write_stream,
            request_rx: Some(request_rx),
            session_uuid,
            event_tx,
            kitty_enabled,
            cursor_visible,
            resize_pending,
            last_output_at,
            last_human_input_ms,
            response_tx,
            hub_event_tx,
            reader_alive: Arc::clone(&self.reader_alive),
            pending_snapshot_requests: Arc::new(Mutex::new(VecDeque::new())),
            terminal_subscriptions,
        })?;

        Ok(())
    }

    /// Enqueue a request for the session I/O worker companion mailbox.
    pub fn enqueue_session_io_request(
        &self,
        request: SessionIoRequest,
    ) -> Result<(), SessionIoRequestEnqueueError> {
        if !self.has_reader() {
            return Err(SessionIoRequestEnqueueError::ReaderMissing);
        }
        if !self.is_reader_alive() {
            return Err(SessionIoRequestEnqueueError::ReaderClosed);
        }
        let Some(tx) = &self.session_io_tx else {
            return Err(SessionIoRequestEnqueueError::MailboxMissing);
        };

        match tx.try_send(request) {
            Ok(()) => Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                Err(SessionIoRequestEnqueueError::MailboxFull)
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                Err(SessionIoRequestEnqueueError::MailboxClosed)
            }
        }
    }

    /// Clone the session I/O worker mailbox for client workers.
    pub fn session_io_sender(&self) -> Option<tokio::sync::mpsc::Sender<SessionIoRequest>> {
        self.session_io_tx.clone()
    }

    /// Whether the session I/O worker has been installed.
    pub fn has_reader(&self) -> bool {
        self.response_rx.is_some()
    }

    /// Whether the session I/O worker is still running.
    pub fn is_reader_alive(&self) -> bool {
        self.reader_alive.load(Ordering::Acquire)
    }

    /// Write raw PTY input bytes.
    pub fn write_input(&mut self, data: &[u8]) -> Result<()> {
        let frame = encode_frame(FRAME_PTY_INPUT, data);
        self.stream.write_all(&frame).context("send PTY input")?;
        Ok(())
    }

    /// Send a resize command.
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        let frame = encode_json(
            FRAME_RESIZE,
            &serde_json::json!({"rows": rows, "cols": cols}),
        )?;
        self.stream.write_all(&frame).context("send resize")?;
        Ok(())
    }

    /// Request and receive an opaque terminal snapshot from the session process.
    ///
    /// Returns an opaque blob produced by `ghostty_terminal_snapshot_export`.
    /// Clients import it via `terminal.snapshot_import()`.
    pub fn get_snapshot(&mut self) -> Result<Vec<u8>> {
        let req = encode_empty(FRAME_GET_SNAPSHOT);
        self.stream.write_all(&req).context("send GetSnapshot")?;
        self.stream.flush()?;
        let frame = self.read_response(FRAME_SNAPSHOT)?;
        Ok(frame.payload)
    }

    /// Request plain text screen contents from the session process.
    ///
    /// Uses the dedicated FRAME_GET_SCREEN/FRAME_SCREEN RPC which returns
    /// plain text directly from the session's parser — no binary snapshot
    /// decoding or ANSI stripping needed.
    pub fn get_screen(&mut self) -> Result<String> {
        let req = encode_empty(FRAME_GET_SCREEN);
        self.stream.write_all(&req).context("send GetScreen")?;
        self.stream.flush()?;
        let frame = self.read_response(FRAME_SCREEN)?;
        String::from_utf8(frame.payload).context("FRAME_SCREEN payload is not valid UTF-8")
    }

    /// Request terminal mode flags from the session process.
    ///
    /// Used on reconnect to initialize the hub's state.
    pub fn get_mode_flags(&mut self) -> Result<ModeFlags> {
        let req = encode_empty(FRAME_GET_MODE_FLAGS);
        self.stream.write_all(&req).context("send GetModeFlags")?;
        self.stream.flush()?;
        let frame = self.read_response(FRAME_MODE_FLAGS)?;
        frame.json()
    }

    /// Replace the session parser's current terminal color profile.
    pub fn set_color_profile(
        &mut self,
        colors: &std::collections::HashMap<usize, crate::terminal::Rgb>,
    ) -> Result<()> {
        let frame = encode_json(
            FRAME_SET_COLOR_PROFILE,
            &TerminalColorProfile {
                colors: colors.clone(),
            },
        )?;
        self.stream
            .write_all(&frame)
            .context("send SetColorProfile")?;
        self.stream.flush().context("flush SetColorProfile")?;
        Ok(())
    }

    /// Connect to a session process and seed reconnect state from handshake metadata.
    pub fn connect_and_seed(socket_path: &Path) -> Result<(Self, SessionMetadata)> {
        let conn = Self::connect(socket_path)?;
        let metadata = conn.metadata.clone();
        Ok((conn, metadata))
    }

    /// Send a ping and wait for pong.
    pub fn ping(&mut self) -> Result<()> {
        let req = encode_empty(FRAME_PING);
        self.stream.write_all(&req).context("send ping")?;
        self.stream.flush()?;
        let _ = self.read_response(FRAME_PONG)?;
        Ok(())
    }

    /// Request clean shutdown.
    pub fn shutdown(&mut self) -> Result<()> {
        let req = encode_empty(FRAME_SHUTDOWN);
        self.stream.write_all(&req).context("send shutdown")?;
        Ok(())
    }

    /// Arm the tee log.
    pub fn arm_tee(&mut self, log_path: &str, cap_bytes: u64) -> Result<()> {
        let frame = encode_json(
            FRAME_ARM_TEE,
            &serde_json::json!({"log_path": log_path, "cap_bytes": cap_bytes}),
        )?;
        self.stream.write_all(&frame).context("send ArmTee")?;
        Ok(())
    }

    /// Read the next response frame of the expected type.
    ///
    /// Post-reader: receives from response channel (reader routes control frames here).
    /// Pre-reader: reads directly from socket, skipping async frames.
    fn read_response(&mut self, expected_type: u8) -> Result<Frame> {
        if let Some(ref rx) = self.response_rx {
            // Post-reader: responses arrive via channel from reader thread
            return rx
                .recv_timeout(RESPONSE_TIMEOUT)
                .context("timed out waiting for session control response via reader");
        }

        // Pre-reader: direct socket read (used during initial handshake/setup)
        let mut buf = [0u8; 8192];
        let deadline = std::time::Instant::now() + RESPONSE_TIMEOUT;

        loop {
            if std::time::Instant::now() >= deadline {
                bail!(
                    "timeout waiting for frame 0x{:02x} from session",
                    expected_type
                );
            }

            let n = self.stream.read(&mut buf).context("read from session")?;
            if n == 0 {
                bail!("session disconnected");
            }

            for frame in self.decoder.feed(&buf[..n]) {
                if frame.frame_type == FRAME_PTY_OUTPUT || frame.frame_type == FRAME_PROCESS_EXITED
                {
                    continue;
                }
                // Skip proactive event frames during pre-reader phase
                if (FRAME_TITLE_CHANGED..=FRAME_NOTIFICATION).contains(&frame.frame_type) {
                    continue;
                }
                if frame.frame_type == expected_type {
                    return Ok(frame);
                }
            }
        }
    }
}

/// Stable enqueue failures for hub policy and logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionIoRequestEnqueueError {
    /// The session reader has not been installed yet.
    ReaderMissing,
    /// The session reader has exited.
    ReaderClosed,
    /// The request mailbox sender was not installed.
    MailboxMissing,
    /// The request mailbox receiver has closed.
    MailboxClosed,
    /// The request mailbox is at capacity.
    MailboxFull,
    /// The shared session connection is absent.
    ConnectionMissing,
    /// The shared session connection mutex is poisoned.
    ConnectionLockPoisoned,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_session_connection_type_compiles() {
        let _conn: SharedSessionConnection = Arc::new(Mutex::new(None));
    }
}
