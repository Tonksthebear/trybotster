//! Transport adapter boundary.
//!
//! A transport adapter is the only layer that may translate browser, TUI,
//! socket, or future transport details into the generic client-worker contract.
//! Session I/O workers and the session process stay unaware of WebRTC,
//! browser-specific framing, TUI IPC, and socket wire details.

use crate::client::ClientId;

use super::client::ClientWorkerMessage;
use super::{BoundedQueueConfig, SessionUuid, SubscriptionId};

/// Default bounded mailbox config for transport-adapter input.
pub const TRANSPORT_ADAPTER_QUEUE: BoundedQueueConfig =
    BoundedQueueConfig::new("worker.transport", 512);

/// Generic inbound frame after transport-specific decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportIngress {
    /// Client requested a terminal subscription.
    Subscribe {
        /// Session to subscribe to.
        session_uuid: SessionUuid,
        /// Transport-local subscription identifier.
        subscription_id: SubscriptionId,
    },
    /// Client requested subscription removal.
    Unsubscribe {
        /// Session to unsubscribe from.
        session_uuid: SessionUuid,
        /// Transport-local subscription identifier.
        subscription_id: SubscriptionId,
    },
    /// Client sent raw PTY input bytes.
    TerminalInput {
        /// Target session.
        session_uuid: SessionUuid,
        /// Raw input bytes.
        data: Vec<u8>,
    },
    /// Client sent a transport-neutral JSON command.
    Json(serde_json::Value),
    /// Transport reported a health change.
    Health(super::client::ClientConnectionHealth),
}

/// Generic outbound frame before transport-specific encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportEgress {
    /// Raw terminal bytes for a subscription.
    TerminalBytes {
        /// Transport-local subscription identifier.
        subscription_id: SubscriptionId,
        /// Raw terminal bytes.
        data: Vec<u8>,
    },
    /// Generic control message for the client.
    Control(serde_json::Value),
    /// Close the transport.
    Close {
        /// Human-readable reason for diagnostics.
        reason: String,
    },
}

/// Trait implemented by transport-specific adapters.
pub trait TransportAdapter: std::fmt::Debug + Send {
    /// Return the client identity represented by this adapter.
    fn client_id(&self) -> &ClientId;

    /// Convert decoded transport ingress into a client-worker message.
    fn ingress_to_client(&self, ingress: TransportIngress) -> ClientWorkerMessage;

    /// Convert a client-worker message into transport-specific egress when
    /// possible.
    fn client_to_egress(&self, message: ClientWorkerMessage) -> Option<TransportEgress>;
}
