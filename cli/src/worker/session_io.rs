//! Session I/O worker contract.
//!
//! The session I/O worker mirrors the durable per-session process boundary in
//! Rust actor-message form. It does not replace the Unix-socket wire protocol;
//! it gives future hub code a typed mailbox for PTY input, terminal snapshots,
//! mode updates, plain-screen reads, color profile updates, and process
//! lifecycle events.

use super::{BoundedQueueConfig, RequestId, SessionUuid};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Default bounded mailbox config for session-I/O worker input.
pub const SESSION_IO_WORKER_QUEUE: BoundedQueueConfig =
    BoundedQueueConfig::new("worker.session_io", 1024);

/// Request sent to a session I/O worker.
#[derive(Debug, Clone)]
pub enum SessionIoRequest {
    /// Write raw PTY input to the session process.
    PtyInput {
        /// Raw input bytes.
        data: Vec<u8>,
    },
    /// Resize the PTY.
    Resize {
        /// Terminal row count.
        rows: u16,
        /// Terminal column count.
        cols: u16,
    },
    /// Request an opaque terminal snapshot.
    GetSnapshot {
        /// Request identifier for correlating the response.
        request_id: RequestId,
    },
    /// Request and deliver an initial terminal snapshot directly to a client worker.
    GetInitialSnapshot {
        /// Client-worker delivery configuration.
        delivery: TerminalInitialSnapshotDelivery,
    },
    /// Register a client-worker terminal output subscriber.
    SubscribeTerminal {
        /// Subscription delivery configuration.
        subscription: TerminalOutputSubscription,
    },
    /// Remove a client-worker terminal output subscriber.
    UnsubscribeTerminal {
        /// Stable subscription route key.
        subscription_key: String,
    },
    /// Write an already-authorized file paste/drop payload for this session
    /// and inject the resulting local path as PTY input.
    PasteFile {
        /// Request identifier for correlating success/failure.
        request_id: RequestId,
        /// Original client filename. Used only to preserve a safe extension.
        filename: String,
        /// Raw file bytes after transport/client authorization.
        data: Vec<u8>,
    },
    /// Prepare an opaque terminal snapshot for client delivery.
    ///
    /// Snapshot coalescing is scoped by `(session_uuid, request_id)`. The hub
    /// keeps request_id opaque and uses it to route the worker output back to a
    /// peer/subscription or Lua refresh caller; browser identities never enter
    /// the session I/O contract.
    PrepareSnapshot {
        /// Request identifier for hub-side routing.
        request_id: RequestId,
        /// Opaque snapshot bytes from the session process.
        snapshot: Vec<u8>,
        /// Whether this payload is for backpressure recovery.
        recovery: bool,
    },
    /// Request current terminal mode flags.
    GetModeFlags {
        /// Request identifier for correlating the response.
        request_id: RequestId,
    },
    /// Request plain text screen contents.
    GetScreen {
        /// Request identifier for correlating the response.
        request_id: RequestId,
    },
    /// Replace the session parser color profile.
    SetColorProfile(crate::session::protocol::TerminalColorProfile),
    /// Ask the session process to shut down cleanly.
    Shutdown {
        /// Human-readable reason for diagnostics.
        reason: String,
    },
}

/// Optional output transform for a terminal subscriber.
#[derive(Debug, Clone)]
pub enum TerminalOutputFilter {
    /// Deliver bytes unchanged.
    None,
    /// Suppress OSC color-query bytes when another peer owns active focus for
    /// the session. This prevents inactive clients from answering color probes.
    StripOscQueriesWhenInactive {
        /// Active peer per session UUID.
        active_terminal_peers: Arc<Mutex<HashMap<String, String>>>,
        /// Peer represented by this subscriber.
        peer_id: String,
    },
}

/// Client-worker terminal output subscriber owned by a session I/O actor.
#[derive(Debug, Clone)]
pub struct TerminalOutputSubscription {
    /// Stable route key (`peer:session`, `tui:session`, `socket:session`).
    pub subscription_key: String,
    /// Transport-local subscription id.
    pub subscription_id: String,
    /// Client worker that owns transport-neutral delivery/backpressure.
    pub worker: super::client::ClientWorkerHandle,
    /// Optional bytes to prepend before transport encoding.
    pub output_prefix: Vec<u8>,
    /// Per-subscriber output transform.
    pub filter: TerminalOutputFilter,
}

/// Transport payload preparation required for an initial snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalSnapshotPayloadMode {
    /// Deliver the session snapshot bytes unchanged.
    Raw,
    /// Prefix as a binary terminal snapshot and gzip before delivery.
    PrefixedGzip,
}

/// Initial scrollback target owned by the session I/O data plane.
#[derive(Debug, Clone)]
pub struct TerminalInitialSnapshotDelivery {
    /// Request identifier for diagnostics and FIFO correlation.
    pub request_id: RequestId,
    /// Stable route key (`peer:session`, `tui:session`, `socket:session`).
    pub subscription_key: String,
    /// Session that produced the snapshot.
    pub session_uuid: SessionUuid,
    /// Transport-local subscription id.
    pub subscription_id: String,
    /// Client worker that owns transport delivery/backpressure.
    pub worker: super::client::ClientWorkerHandle,
    /// Authoritative terminal row count for the snapshot.
    pub rows: u16,
    /// Authoritative terminal column count for the snapshot.
    pub cols: u16,
    /// Whether kitty keyboard mode is enabled in the inner PTY.
    pub kitty_enabled: bool,
    /// Payload preparation expected by the transport adapter.
    pub payload_mode: TerminalSnapshotPayloadMode,
    /// Whether the transport needs a subscription confirmation after the
    /// initial snapshot has been delivered.
    pub confirm_subscription: bool,
    /// Live output subscription to activate immediately after the snapshot
    /// barrier has been delivered. PTY output before the snapshot frame is
    /// represented by the snapshot; output after the frame follows live.
    pub live_subscription: Option<TerminalOutputSubscription>,
    /// Hub-side time when terminal attach orchestration started.
    pub attach_requested_at: Option<Instant>,
    /// Hub-side time when the client-worker subscription was accepted.
    pub client_worker_subscribed_at: Option<Instant>,
    /// Hub-side time when the initial snapshot request was queued to SessionIo.
    pub session_io_snapshot_queued_at: Option<Instant>,
    /// SessionIo-worker time when it accepted the initial snapshot request.
    pub session_io_accepted_at: Option<Instant>,
}

/// Timing summary for one initial terminal attach barrier.
#[derive(Debug, Clone)]
pub struct TerminalAttachTiming {
    /// Request identifier for the initial snapshot barrier.
    pub request_id: RequestId,
    /// Stable route key for the terminal subscription.
    pub subscription_key: String,
    /// Session that accepted the attach.
    pub session_uuid: SessionUuid,
    /// Transport-local subscription id.
    pub subscription_id: String,
    /// Raw snapshot byte length before transport preparation.
    pub snapshot_bytes: usize,
    /// Duration from hub attach start to client-worker subscription acceptance.
    pub attach_to_client_worker_subscribed: Option<Duration>,
    /// Duration from hub attach start to session-I/O snapshot enqueue.
    pub attach_to_session_io_queued: Option<Duration>,
    /// Duration from hub attach start to session-I/O worker acceptance.
    pub attach_to_session_io_accepted: Option<Duration>,
    /// Duration from hub attach start to snapshot response availability.
    pub attach_to_snapshot_ready: Option<Duration>,
    /// Duration from snapshot response availability to client-worker queue acceptance.
    pub snapshot_ready_to_client_worker_accepted: Duration,
    /// Duration from hub attach start to client-worker queue acceptance.
    pub attach_to_client_worker_accepted: Option<Duration>,
}

/// Initial attach delivery step that failed while queueing frames to a client worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalAttachDeliveryPhase {
    /// Initial scrollback snapshot frame.
    Snapshot,
    /// Optional subscribe acknowledgement frame.
    SubscribeAck,
    /// Terminal attach state control frame.
    AttachState,
}

/// Why an initial attach delivery step failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalAttachDeliveryFailureReason {
    /// Client worker queue was full.
    QueueFull,
    /// Client worker queue was closed.
    QueueClosed,
}

impl TerminalOutputFilter {
    /// Apply this filter to a chunk, preserving incomplete OSC query fragments
    /// in `buffer` across calls.
    #[must_use]
    pub fn filter_chunk(&self, session_uuid: &str, buffer: &mut Vec<u8>, data: &[u8]) -> Vec<u8> {
        match self {
            Self::None => data.to_vec(),
            Self::StripOscQueriesWhenInactive {
                active_terminal_peers,
                peer_id,
            } => {
                if active_terminal_peers
                    .lock()
                    .ok()
                    .and_then(|active| active.get(session_uuid).cloned())
                    .is_some_and(|active_peer| active_peer != peer_id.as_str())
                {
                    strip_osc_queries_from_output(buffer, data)
                } else {
                    buffer.clear();
                    data.to_vec()
                }
            }
        }
    }
}

const OSC_ESC: u8 = 0x1b;
const OSC_BEL: u8 = 0x07;
const OSC_BUFFER_LIMIT: usize = 512;

fn append_with_limit(buffer: &mut Vec<u8>, data: &[u8]) {
    buffer.extend_from_slice(data);
    if buffer.len() > OSC_BUFFER_LIMIT {
        let drop_count = buffer.len() - OSC_BUFFER_LIMIT;
        buffer.drain(..drop_count);
    }
}

fn is_color_query_osc(seq: &[u8]) -> bool {
    if seq.len() < 4 || seq[0] != OSC_ESC || seq[1] != b']' {
        return false;
    }

    let body = if seq.ends_with(&[OSC_BEL]) {
        &seq[2..seq.len() - 1]
    } else if seq.len() >= 4 && seq.ends_with(&[OSC_ESC, b'\\']) {
        &seq[2..seq.len() - 2]
    } else {
        return false;
    };

    matches!(
        body,
        [b'1', b'0', b';', b'?'] | [b'1', b'1', b';', b'?'] | [b'1', b'2', b';', b'?']
    ) || body.starts_with(b"4;") && body.ends_with(b";?")
}

fn strip_osc_queries_from_output(buffer: &mut Vec<u8>, data: &[u8]) -> Vec<u8> {
    append_with_limit(buffer, data);
    if buffer.is_empty() {
        return Vec::new();
    }

    let input = std::mem::take(buffer);
    let mut output = Vec::with_capacity(input.len());
    let mut idx = 0usize;

    while idx < input.len() {
        if input[idx] == OSC_ESC {
            if idx + 1 >= input.len() {
                buffer.extend_from_slice(&input[idx..]);
                break;
            }

            if input[idx + 1] == b']' {
                let mut scan = idx + 2;
                let mut end = None;

                while scan < input.len() {
                    if input[scan] == OSC_BEL {
                        end = Some(scan + 1);
                        break;
                    }
                    if input[scan] == OSC_ESC {
                        if scan + 1 >= input.len() {
                            break;
                        }
                        if input[scan + 1] == b'\\' {
                            end = Some(scan + 2);
                            break;
                        }
                    }
                    scan += 1;
                }

                let Some(end_idx) = end else {
                    buffer.extend_from_slice(&input[idx..]);
                    break;
                };

                let seq = &input[idx..end_idx];
                if !is_color_query_osc(seq) {
                    output.extend_from_slice(seq);
                }
                idx = end_idx;
                continue;
            }
        }

        output.push(input[idx]);
        idx += 1;
    }

    output
}

/// Event emitted by a session I/O worker.
#[derive(Debug, Clone)]
pub enum SessionIoEvent {
    /// Raw PTY output bytes from the session process.
    PtyOutput {
        /// Session that produced the output.
        session_uuid: SessionUuid,
        /// Raw terminal bytes.
        data: Vec<u8>,
    },
    /// Opaque terminal snapshot response.
    Snapshot {
        /// Request identifier from `GetSnapshot`.
        request_id: RequestId,
        /// Session that produced the snapshot.
        session_uuid: SessionUuid,
        /// Opaque snapshot bytes.
        payload: Vec<u8>,
    },
    /// File paste/drop completed and the path was injected into the session.
    PasteFileWritten {
        /// Request identifier from `PasteFile`.
        request_id: RequestId,
        /// Session that received the paste.
        session_uuid: SessionUuid,
        /// Local path written for cleanup.
        path: PathBuf,
        /// Number of bytes written.
        bytes: usize,
    },
    /// File paste/drop failed in the session I/O data plane.
    PasteFileFailed {
        /// Request identifier from `PasteFile`.
        request_id: RequestId,
        /// Session that received the paste request.
        session_uuid: SessionUuid,
        /// Stable reason code for hub policy/logging.
        reason: PasteFileErrorReason,
        /// Human-readable detail for diagnostics.
        detail: String,
    },
    /// Snapshot bytes prepared for transport delivery.
    PreparedSnapshot {
        /// Request identifier from `PrepareSnapshot`.
        request_id: RequestId,
        /// Session that produced the snapshot.
        session_uuid: SessionUuid,
        /// Raw snapshot byte length before prefixing/compression.
        uncompressed_len: usize,
        /// Payload ready for transport.
        payload: Vec<u8>,
        /// Whether this payload is for backpressure recovery.
        recovery: bool,
    },
    /// Initial terminal attach timing emitted after snapshot delivery is queued
    /// to the client worker.
    TerminalAttachTiming(TerminalAttachTiming),
    /// Initial terminal attach delivery failed before a complete
    /// snapshot/ack/attach sequence could be queued.
    TerminalAttachDeliveryFailed {
        /// Request identifier for the initial snapshot barrier.
        request_id: RequestId,
        /// Stable route key for the terminal subscription.
        subscription_key: String,
        /// Session being attached.
        session_uuid: SessionUuid,
        /// Transport-local subscription id.
        subscription_id: String,
        /// Delivery phase that failed.
        phase: TerminalAttachDeliveryPhase,
        /// Failure reason.
        reason: TerminalAttachDeliveryFailureReason,
    },
    /// Terminal mode flags response.
    ModeFlags {
        /// Request identifier from `GetModeFlags`.
        request_id: RequestId,
        /// Session that produced the flags.
        session_uuid: SessionUuid,
        /// Current mode flags.
        flags: crate::session::protocol::ModeFlags,
    },
    /// Plain text screen response.
    Screen {
        /// Request identifier from `GetScreen`.
        request_id: RequestId,
        /// Session that produced the screen.
        session_uuid: SessionUuid,
        /// Plain text screen contents.
        text: String,
    },
    /// Sparse terminal mode change pushed by the session process.
    ModeChanged {
        /// Session whose mode changed.
        session_uuid: SessionUuid,
        /// Sparse mode update.
        mode: crate::session::protocol::ModeChanged,
    },
    /// Window title changed.
    TitleChanged {
        /// Session whose title changed.
        session_uuid: SessionUuid,
        /// New title.
        title: String,
    },
    /// Bell received.
    Bell {
        /// Session that emitted the bell.
        session_uuid: SessionUuid,
    },
    /// Working directory changed.
    CwdChanged {
        /// Session whose CWD changed.
        session_uuid: SessionUuid,
        /// New working directory.
        cwd: String,
    },
    /// Semantic prompt mark detected.
    PromptMark {
        /// Session that emitted the prompt mark.
        session_uuid: SessionUuid,
        /// Prompt mark name.
        mark: String,
    },
    /// OSC notification detected.
    Notification {
        /// Session that emitted the notification.
        session_uuid: SessionUuid,
        /// Notification title.
        title: String,
        /// Notification body.
        body: String,
    },
    /// Session process exited.
    ProcessExited {
        /// Session that exited.
        session_uuid: SessionUuid,
        /// Exit code when available.
        exit_code: Option<i32>,
    },
}

/// Stable failure reasons for file paste/drop processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteFileErrorReason {
    /// Temp directory creation failed.
    TempDir,
    /// File creation failed.
    Create,
    /// File write failed.
    Write,
    /// PTY path injection failed.
    Inject,
}

/// Successful file paste/drop write result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PasteFileWrite {
    /// Local path written for cleanup.
    pub path: PathBuf,
    /// Number of bytes written.
    pub bytes: usize,
}

/// Snapshot payload prepared by the session I/O data plane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedSnapshot {
    /// Raw snapshot byte length before prefixing/compression.
    pub uncompressed_len: usize,
    /// Payload ready for transport.
    pub payload: Vec<u8>,
}

/// Resolve the paste temp directory for a session.
///
/// Resolution order is:
/// 1. session manifest `worktree_path` when available
/// 2. Botster data dir
/// 3. OS temp dir
pub(crate) fn paste_temp_dir(session_uuid: &str) -> PathBuf {
    session_worktree_path(session_uuid)
        .map(|path| path.join(".botster").join("pastes").join(session_uuid))
        .or_else(|| crate::env::data_dir().map(|path| path.join("pastes").join(session_uuid)))
        .unwrap_or_else(|| {
            std::env::temp_dir()
                .join("botster")
                .join("pastes")
                .join(session_uuid)
        })
}

/// Write a file paste/drop payload and inject the resulting local path.
pub(crate) fn write_paste_file<F>(
    session_uuid: &str,
    filename: &str,
    data: &[u8],
    mut inject_path: F,
) -> Result<PasteFileWrite, (PasteFileErrorReason, String)>
where
    F: FnMut(&[u8]) -> Result<(), String>,
{
    let dir = paste_temp_dir(session_uuid);
    write_paste_file_to_dir(&dir, filename, data, &mut inject_path)
}

fn write_paste_file_to_dir<F>(
    dir: &Path,
    filename: &str,
    data: &[u8],
    mut inject_path: F,
) -> Result<PasteFileWrite, (PasteFileErrorReason, String)>
where
    F: FnMut(&[u8]) -> Result<(), String>,
{
    std::fs::create_dir_all(&dir).map_err(|e| (PasteFileErrorReason::TempDir, e.to_string()))?;

    let path = dir.join(paste_filename(filename, data));
    std::fs::write(&path, data).map_err(|e| {
        let reason = if path.exists() {
            PasteFileErrorReason::Write
        } else {
            PasteFileErrorReason::Create
        };
        (reason, e.to_string())
    })?;

    let input = format!("{} ", path.display());
    inject_path(input.as_bytes()).map_err(|e| (PasteFileErrorReason::Inject, e))?;

    Ok(PasteFileWrite {
        path,
        bytes: data.len(),
    })
}

/// Prepare a snapshot payload for transport delivery.
///
/// The metric names remain owned by callers so existing observability
/// vocabulary (`snapshot.gzip_queue`) stays stable while the byte manipulation
/// lives with the session I/O data-plane contract.
pub(crate) fn prepare_snapshot_payload(snapshot: &[u8]) -> Option<PreparedSnapshot> {
    if snapshot.is_empty() {
        return None;
    }

    let uncompressed_len = snapshot.len();
    let mut plain = Vec::with_capacity(1 + uncompressed_len);
    plain.push(0x02);
    plain.extend_from_slice(snapshot);

    let payload = {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;

        let mut encoder = GzEncoder::new(
            Vec::with_capacity(uncompressed_len / 4),
            Compression::fast(),
        );
        encoder
            .write_all(&plain)
            .and_then(|()| encoder.finish())
            .unwrap_or(plain)
    };

    Some(PreparedSnapshot {
        uncompressed_len,
        payload,
    })
}

fn paste_filename(filename: &str, data: &[u8]) -> String {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|ext| ext.chars().all(|ch| ch.is_ascii_alphanumeric()))
        .unwrap_or("png");

    let hash = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        data.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    };

    format!("botster-paste-{hash}.{ext}")
}

fn session_worktree_path(session_uuid: &str) -> Option<PathBuf> {
    let manifest_path = crate::env::session_manifest_path(session_uuid)?;
    let content = std::fs::read_to_string(manifest_path).ok()?;
    let manifest = serde_json::from_str::<serde_json::Value>(&content).ok()?;
    manifest
        .get("worktree_path")
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_file_write_uses_session_scoped_temp_dir_and_injects_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paste_dir = temp.path().join("pastes").join("sess-paste-test");

        let mut injected = Vec::new();
        let result = write_paste_file_to_dir(
            &paste_dir,
            "../../screenshot.PNG",
            b"image-bytes",
            |bytes| {
                injected.extend_from_slice(bytes);
                Ok(())
            },
        )
        .expect("paste write");

        assert_eq!(result.bytes, 11);
        assert!(result.path.starts_with(temp.path()));
        assert!(result.path.to_string_lossy().contains("sess-paste-test"));
        assert!(result.path.extension().is_some_and(|ext| ext == "PNG"));
        assert!(std::fs::read(&result.path).is_ok_and(|bytes| bytes == b"image-bytes"));
        assert_eq!(
            String::from_utf8(injected).expect("utf8"),
            format!("{} ", result.path.display())
        );
    }

    #[test]
    fn paste_file_write_reports_inject_failure_with_stable_reason() {
        let temp = tempfile::tempdir().expect("tempdir");

        let err =
            write_paste_file_to_dir(temp.path(), "a.png", b"data", |_| Err("closed".to_string()))
                .expect_err("inject failure");

        assert_eq!(err.0, PasteFileErrorReason::Inject);
        assert_eq!(err.1, "closed");
    }

    #[test]
    fn prepared_snapshot_prefixes_and_compresses_nonempty_snapshots() {
        let snapshot = vec![b'x'; 4096];
        let prepared = prepare_snapshot_payload(&snapshot).expect("prepared");

        assert_eq!(prepared.uncompressed_len, snapshot.len());
        assert!(prepared.payload.starts_with(&[0x1f, 0x8b]));
        assert!(prepared.payload.len() < snapshot.len());
        assert!(prepare_snapshot_payload(&[]).is_none());
    }
}
