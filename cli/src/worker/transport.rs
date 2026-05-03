//! Transport adapter boundary.
//!
//! A transport adapter is the only layer that may translate browser, TUI,
//! socket, or future transport details into the generic client-worker contract.
//! Session I/O workers and the session process stay unaware of WebRTC,
//! browser-specific framing, TUI IPC, and socket wire details.

use crate::client::{ClientId, TuiOutput, TuiRequest};
use crate::socket::framing::Frame;

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
    /// Client sent plugin-level binary data outside PTY routing.
    Binary(Vec<u8>),
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
        /// Session that produced the bytes.
        session_uuid: SessionUuid,
        /// Raw terminal bytes.
        data: Vec<u8>,
    },
    /// Initial or recovery scrollback snapshot for a subscription.
    Scrollback {
        /// Transport-local subscription identifier.
        subscription_id: SubscriptionId,
        /// Session that produced the snapshot.
        session_uuid: SessionUuid,
        /// Authoritative row count used to produce the snapshot.
        rows: u16,
        /// Authoritative column count used to produce the snapshot.
        cols: u16,
        /// Whether kitty keyboard mode is enabled in the inner PTY.
        kitty_enabled: bool,
        /// Opaque snapshot bytes.
        data: Vec<u8>,
    },
    /// Session process exit notice.
    ProcessExited {
        /// Transport-local subscription identifier.
        subscription_id: SubscriptionId,
        /// Session that exited.
        session_uuid: SessionUuid,
        /// Exit code when available.
        exit_code: Option<i32>,
    },
    /// Generic control message for the client.
    Control(serde_json::Value),
    /// Plugin-level binary data outside PTY routing.
    Binary(Vec<u8>),
    /// Close the transport.
    Close {
        /// Human-readable reason for diagnostics.
        reason: String,
    },
}

/// Adapter for length-prefixed Unix socket frames.
#[derive(Debug, Clone)]
pub struct SocketFrameAdapter {
    client_id: ClientId,
}

impl SocketFrameAdapter {
    /// Build an adapter for a socket client id.
    #[must_use]
    pub fn new(client_id: impl Into<String>) -> Self {
        Self {
            client_id: ClientId::Socket(client_id.into()),
        }
    }

    /// Convert a decoded socket frame into generic ingress.
    #[must_use]
    pub fn frame_to_ingress(frame: Frame) -> Option<TransportIngress> {
        match frame {
            Frame::Json(value) => match value.get("type").and_then(|v| v.as_str()) {
                Some("subscribe") => {
                    let session_uuid = value
                        .get("session_uuid")
                        .or_else(|| value.pointer("/data/session_uuid"))
                        .and_then(|v| v.as_str())?
                        .to_string();
                    let subscription_id = value
                        .get("subscriptionId")
                        .or_else(|| value.get("subscription_id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(&session_uuid)
                        .to_string();
                    Some(TransportIngress::Subscribe {
                        session_uuid,
                        subscription_id,
                    })
                }
                Some("unsubscribe") => {
                    let session_uuid = value
                        .get("session_uuid")
                        .or_else(|| value.pointer("/data/session_uuid"))
                        .and_then(|v| v.as_str())?
                        .to_string();
                    let subscription_id = value
                        .get("subscriptionId")
                        .or_else(|| value.get("subscription_id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(&session_uuid)
                        .to_string();
                    Some(TransportIngress::Unsubscribe {
                        session_uuid,
                        subscription_id,
                    })
                }
                _ => Some(TransportIngress::Json(value)),
            },
            Frame::PtyInput { session_uuid, data } => {
                Some(TransportIngress::TerminalInput { session_uuid, data })
            }
            Frame::Binary(data) => Some(TransportIngress::Binary(data)),
            Frame::PtyOutput { .. } | Frame::Scrollback { .. } | Frame::ProcessExited { .. } => {
                None
            }
        }
    }

    /// Convert generic egress to a socket frame.
    #[must_use]
    pub fn egress_to_frame(egress: TransportEgress) -> Option<Frame> {
        match egress {
            TransportEgress::TerminalBytes {
                session_uuid, data, ..
            } => Some(Frame::PtyOutput { session_uuid, data }),
            TransportEgress::Scrollback {
                session_uuid,
                rows,
                cols,
                kitty_enabled,
                data,
                ..
            } => Some(Frame::Scrollback {
                session_uuid,
                rows,
                cols,
                kitty_enabled,
                data,
            }),
            TransportEgress::ProcessExited {
                session_uuid,
                exit_code,
                ..
            } => Some(Frame::ProcessExited {
                session_uuid,
                exit_code,
            }),
            TransportEgress::Control(value) => Some(Frame::Json(value)),
            TransportEgress::Binary(data) => Some(Frame::Binary(data)),
            TransportEgress::Close { .. } => None,
        }
    }
}

impl TransportAdapter for SocketFrameAdapter {
    fn client_id(&self) -> &ClientId {
        &self.client_id
    }

    fn ingress_to_client(&self, ingress: TransportIngress) -> ClientWorkerMessage {
        ingress_to_client_message(ingress)
    }

    fn client_to_egress(&self, message: ClientWorkerMessage) -> Option<TransportEgress> {
        client_message_to_egress(message, "socket")
    }
}

/// Adapter for in-process TUI request/output channels.
#[derive(Debug, Clone)]
pub struct TuiTransportAdapter {
    client_id: ClientId,
}

impl TuiTransportAdapter {
    /// Build an adapter for the in-process TUI client.
    #[must_use]
    pub fn new() -> Self {
        Self {
            client_id: ClientId::Tui,
        }
    }

    /// Convert a TUI request into generic ingress.
    #[must_use]
    pub fn request_to_ingress(request: TuiRequest) -> TransportIngress {
        match request {
            TuiRequest::LuaMessage(value) => TransportIngress::Json(value),
            TuiRequest::PtyInput { session_uuid, data } => {
                TransportIngress::TerminalInput { session_uuid, data }
            }
            TuiRequest::FocusChanged {
                session_uuid,
                focused,
            } => TransportIngress::Json(serde_json::json!({
                "type": "focus_changed",
                "session_uuid": session_uuid,
                "focused": focused,
            })),
        }
    }

    /// Convert generic egress to TUI output.
    #[must_use]
    pub fn egress_to_output(egress: TransportEgress) -> Option<TuiOutput> {
        match egress {
            TransportEgress::TerminalBytes {
                session_uuid, data, ..
            } => Some(TuiOutput::Output { session_uuid, data }),
            TransportEgress::Scrollback {
                session_uuid,
                rows,
                cols,
                kitty_enabled,
                data,
                ..
            } => Some(TuiOutput::Scrollback {
                session_uuid,
                rows,
                cols,
                data,
                kitty_enabled,
            }),
            TransportEgress::ProcessExited {
                session_uuid,
                exit_code,
                ..
            } => Some(TuiOutput::ProcessExited {
                session_uuid,
                exit_code,
            }),
            TransportEgress::Control(value) => Some(TuiOutput::Message(value)),
            TransportEgress::Binary(data) => Some(TuiOutput::Binary(data)),
            TransportEgress::Close { .. } => None,
        }
    }
}

impl TransportAdapter for TuiTransportAdapter {
    fn client_id(&self) -> &ClientId {
        &self.client_id
    }

    fn ingress_to_client(&self, ingress: TransportIngress) -> ClientWorkerMessage {
        ingress_to_client_message(ingress)
    }

    fn client_to_egress(&self, message: ClientWorkerMessage) -> Option<TransportEgress> {
        client_message_to_egress(message, "tui")
    }
}

fn ingress_to_client_message(ingress: TransportIngress) -> ClientWorkerMessage {
    match ingress {
        TransportIngress::Subscribe {
            session_uuid,
            subscription_id,
        } => ClientWorkerMessage::SubscribeSession {
            session_uuid,
            subscription_id,
        },
        TransportIngress::Unsubscribe {
            session_uuid,
            subscription_id,
        } => ClientWorkerMessage::UnsubscribeSession {
            session_uuid,
            subscription_id,
        },
        TransportIngress::TerminalInput { session_uuid, data } => {
            ClientWorkerMessage::SessionInput { session_uuid, data }
        }
        TransportIngress::Json(value) => {
            ClientWorkerMessage::ControlFrame(super::client::ClientControlFrame::Json(value))
        }
        TransportIngress::Binary(data) => {
            ClientWorkerMessage::ControlFrame(super::client::ClientControlFrame::Binary(data))
        }
        TransportIngress::Health(health) => ClientWorkerMessage::Health(health),
    }
}

fn client_message_to_egress(
    message: ClientWorkerMessage,
    subscription_id: impl Into<String>,
) -> Option<TransportEgress> {
    let subscription_id = subscription_id.into();
    match message {
        ClientWorkerMessage::TerminalBytes { session_uuid, data } => {
            Some(TransportEgress::TerminalBytes {
                subscription_id,
                session_uuid,
                data,
            })
        }
        ClientWorkerMessage::ControlFrame(super::client::ClientControlFrame::Json(value)) => {
            Some(TransportEgress::Control(value))
        }
        ClientWorkerMessage::ControlFrame(super::client::ClientControlFrame::Binary(data)) => {
            Some(TransportEgress::Binary(data))
        }
        ClientWorkerMessage::ControlFrame(super::client::ClientControlFrame::Scrollback {
            session_uuid,
            rows,
            cols,
            kitty_enabled,
            data,
        }) => Some(TransportEgress::Scrollback {
            subscription_id,
            session_uuid,
            rows,
            cols,
            kitty_enabled,
            data,
        }),
        ClientWorkerMessage::ControlFrame(super::client::ClientControlFrame::ProcessExited {
            session_uuid,
            exit_code,
        }) => Some(TransportEgress::ProcessExited {
            subscription_id,
            session_uuid,
            exit_code,
        }),
        ClientWorkerMessage::Shutdown { reason } => Some(TransportEgress::Close { reason }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_adapter_round_trips_lossless_binary_frames() {
        let scrollback = TransportEgress::Scrollback {
            subscription_id: "socket:sess-1".to_string(),
            session_uuid: "sess-1".to_string(),
            rows: 44,
            cols: 132,
            kitty_enabled: true,
            data: vec![0, 1, 2, 255],
        };
        let frame = SocketFrameAdapter::egress_to_frame(scrollback).expect("scrollback frame");
        assert!(matches!(
            frame,
            Frame::Scrollback {
                session_uuid,
                rows: 44,
                cols: 132,
                kitty_enabled: true,
                data,
            } if session_uuid == "sess-1" && data == vec![0, 1, 2, 255]
        ));

        let binary = TransportEgress::Binary(vec![7, 8, 9]);
        assert!(matches!(
            SocketFrameAdapter::egress_to_frame(binary),
            Some(Frame::Binary(data)) if data == vec![7, 8, 9]
        ));
    }

    #[test]
    fn socket_adapter_maps_frame_ingress_to_worker_messages() {
        let adapter = SocketFrameAdapter::new("sock-1");

        let subscribe = SocketFrameAdapter::frame_to_ingress(Frame::Json(serde_json::json!({
            "type": "subscribe",
            "subscriptionId": "sock:sess-1",
            "session_uuid": "sess-1",
        })))
        .expect("subscribe ingress");
        assert!(matches!(
            adapter.ingress_to_client(subscribe),
            ClientWorkerMessage::SubscribeSession {
                session_uuid,
                subscription_id,
            } if session_uuid == "sess-1" && subscription_id == "sock:sess-1"
        ));

        let input = SocketFrameAdapter::frame_to_ingress(Frame::PtyInput {
            session_uuid: "sess-1".to_string(),
            data: b"abc".to_vec(),
        })
        .expect("pty input ingress");
        assert!(matches!(
            adapter.ingress_to_client(input),
            ClientWorkerMessage::SessionInput { session_uuid, data }
                if session_uuid == "sess-1" && data == b"abc"
        ));
    }

    #[test]
    fn tui_adapter_preserves_output_metadata() {
        let output = TuiTransportAdapter::egress_to_output(TransportEgress::Scrollback {
            subscription_id: "tui:sess-1".to_string(),
            session_uuid: "sess-1".to_string(),
            rows: 30,
            cols: 120,
            kitty_enabled: false,
            data: b"snapshot".to_vec(),
        })
        .expect("tui output");

        assert!(matches!(
            output,
            TuiOutput::Scrollback {
                session_uuid,
                rows: 30,
                cols: 120,
                kitty_enabled: false,
                data,
            } if session_uuid == "sess-1" && data == b"snapshot"
        ));
    }
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
