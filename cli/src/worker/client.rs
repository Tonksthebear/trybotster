//! Transport-neutral client worker contract.
//!
//! Client workers own per-client session subscriptions and terminal stream
//! delivery, but they do not know whether the client is a browser, TUI, socket,
//! or future transport. Transport-specific encoding and send mechanics belong
//! behind `TransportAdapter`.

use crate::client::ClientId;

use super::{BoundedQueueConfig, RequestId, SessionUuid, SubscriptionId};

/// Default bounded mailbox config for client-worker input.
pub const CLIENT_WORKER_QUEUE: BoundedQueueConfig = BoundedQueueConfig::new("worker.client", 1024);

/// Health reported for a connected client transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientConnectionHealth {
    /// Transport is ready to send and receive work.
    Ready,
    /// Transport is alive but currently unable to keep up.
    Backpressured {
        /// Queue/source label that reported pressure.
        source: &'static str,
    },
    /// Transport is reconnecting.
    Reconnecting {
        /// Monotonic generation used to ignore stale reconnect completions.
        generation: u64,
    },
    /// Transport disconnected.
    Disconnected {
        /// Human-readable disconnect reason for diagnostics.
        reason: String,
    },
}

/// Control frame emitted by the session/hub side toward a client worker.
#[derive(Debug, Clone)]
pub enum ClientControlFrame {
    /// Initial or recovery snapshot for a session.
    Snapshot {
        /// Session the snapshot belongs to.
        session_uuid: SessionUuid,
        /// Opaque snapshot bytes owned by terminal/session code.
        payload: Vec<u8>,
    },
    /// Terminal mode flags changed.
    ModeChanged {
        /// Session whose mode changed.
        session_uuid: SessionUuid,
        /// Sparse mode update.
        mode: crate::session::protocol::ModeChanged,
    },
    /// Session process exited.
    ProcessExited {
        /// Session that exited.
        session_uuid: SessionUuid,
        /// Exit code when available.
        exit_code: Option<i32>,
    },
    /// Transport-neutral JSON control payload from hub-owned policy.
    Json(serde_json::Value),
}

/// Messages accepted by a client worker.
#[derive(Debug, Clone)]
pub enum ClientWorkerMessage {
    /// Subscribe this client to a session.
    SubscribeSession {
        /// Session to subscribe to.
        session_uuid: SessionUuid,
        /// Transport-local subscription identifier.
        subscription_id: SubscriptionId,
    },
    /// Unsubscribe this client from a session.
    UnsubscribeSession {
        /// Session to unsubscribe from.
        session_uuid: SessionUuid,
        /// Transport-local subscription identifier.
        subscription_id: SubscriptionId,
    },
    /// Raw terminal bytes for a subscribed session.
    TerminalBytes {
        /// Session that produced the bytes.
        session_uuid: SessionUuid,
        /// Raw terminal bytes.
        data: Vec<u8>,
    },
    /// Control frame for a subscribed session or client.
    ControlFrame(ClientControlFrame),
    /// Client transport health changed.
    Health(ClientConnectionHealth),
    /// Worker ingress queue hit backpressure.
    Backpressure {
        /// Queue/source label.
        source: &'static str,
        /// Queue capacity.
        capacity: usize,
    },
    /// Request/response correlation ping.
    Ping {
        /// Request identifier.
        request_id: RequestId,
    },
    /// Shut down this client worker.
    Shutdown {
        /// Human-readable reason for diagnostics.
        reason: String,
    },
}

/// Handle shape for future code that owns a bounded client-worker mailbox.
#[derive(Debug, Clone)]
pub struct ClientWorkerHandle {
    /// Client identity represented by this worker.
    pub client_id: ClientId,
    /// Bounded mailbox sender.
    pub tx: tokio::sync::mpsc::Sender<ClientWorkerMessage>,
}
