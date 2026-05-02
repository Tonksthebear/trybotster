//! Hub control actor contract.
//!
//! `HubControlMessage` is the mailbox shape for requests that require
//! hub-owned orchestration state changes. Client, transport, and session I/O
//! workers may send these messages, but the hub remains the only component that
//! mutates session registries, client routing tables, reconnect state, and
//! shutdown coordination.

use crate::client::ClientId;

use super::{BoundedQueueConfig, SessionUuid};

/// Default bounded mailbox config for hub-control requests.
pub const HUB_CONTROL_QUEUE: BoundedQueueConfig =
    BoundedQueueConfig::new("worker.hub_control", 512);

/// Origin of a hub-control request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubControlOrigin {
    /// Request originated from a connected client worker.
    Client(ClientId),
    /// Request originated from a session I/O worker.
    SessionIo(SessionUuid),
    /// Request originated from internal hub coordination.
    Internal,
}

/// Lifecycle phase observed for a session process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionLifecycleState {
    /// The session process is expected but not yet connected.
    Starting,
    /// The hub has an active connection to the session process.
    Connected,
    /// Reconnect is pending after a disconnect or hub restart.
    Reconnecting {
        /// Monotonic generation used to ignore stale reconnect completions.
        generation: u64,
    },
    /// The session process exited or was intentionally removed.
    Exited {
        /// Exit code when available.
        exit_code: Option<i32>,
    },
}

/// Backpressure notice raised by a bounded worker queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerBackpressure {
    /// Queue/source label.
    pub source: &'static str,
    /// Number of messages the queue can hold.
    pub capacity: usize,
    /// Optional session UUID when the overloaded path is session-scoped.
    pub session_uuid: Option<SessionUuid>,
    /// Optional client identity when the overloaded path is client-scoped.
    pub client_id: Option<ClientId>,
}

/// Requests that cross into the hub-owned orchestration boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubControlMessage {
    /// A client wants to attach to a session stream.
    AttachClient {
        /// Client requesting the attach.
        client_id: ClientId,
        /// Session to attach.
        session_uuid: SessionUuid,
        /// Transport-local subscription identifier.
        subscription_id: String,
    },
    /// A client wants to detach from a session stream.
    DetachClient {
        /// Client requesting the detach.
        client_id: ClientId,
        /// Session to detach.
        session_uuid: SessionUuid,
        /// Transport-local subscription identifier.
        subscription_id: String,
    },
    /// A bounded worker queue reached capacity or rejected a message.
    Backpressure(WorkerBackpressure),
    /// A session process lifecycle state changed.
    SessionLifecycle {
        /// Session whose lifecycle changed.
        session_uuid: SessionUuid,
        /// New lifecycle state.
        state: SessionLifecycleState,
    },
    /// Coordinate reconnect for a client or session.
    Reconnect {
        /// Request origin.
        origin: HubControlOrigin,
        /// Optional session scope for reconnect.
        session_uuid: Option<SessionUuid>,
        /// Monotonic generation used to ignore stale reconnect completions.
        generation: u64,
    },
    /// Coordinate orderly shutdown across workers.
    Shutdown {
        /// Request origin.
        origin: HubControlOrigin,
        /// Human-readable reason for diagnostics.
        reason: String,
    },
}
