//! Session I/O worker contract.
//!
//! The session I/O worker mirrors the durable per-session process boundary in
//! Rust actor-message form. It does not replace the Unix-socket wire protocol;
//! it gives future hub code a typed mailbox for PTY input, terminal snapshots,
//! mode updates, plain-screen reads, color profile updates, and process
//! lifecycle events.

use super::{BoundedQueueConfig, RequestId, SessionUuid};

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
