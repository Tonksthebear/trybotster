//! Transport adapter boundary.
//!
//! A transport adapter is the only layer that may translate browser, TUI,
//! socket, or future transport details into the generic client-worker contract.
//! Session I/O workers and the session process stay unaware of WebRTC,
//! browser-specific framing, TUI IPC, and socket wire details.

use crate::client::{ClientId, TuiOutput, TuiRequest};
use crate::socket::framing::Frame;

use super::client::{ClientControlFrame, ClientWorkerMessage, TerminalAttachState};
use super::{BoundedQueueConfig, RequestId, SessionUuid, SubscriptionId};

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
    /// Client requested a PTY resize.
    TerminalResize {
        /// Target session.
        session_uuid: SessionUuid,
        /// Requested terminal rows.
        rows: u16,
        /// Requested terminal columns.
        cols: u16,
    },
    /// Client requested a fresh terminal snapshot.
    RequestSnapshot {
        /// Target session.
        session_uuid: SessionUuid,
        /// Requested terminal rows.
        rows: u16,
        /// Requested terminal columns.
        cols: u16,
    },
    /// Client focus state changed.
    FocusChanged {
        /// Session whose focus changed.
        session_uuid: SessionUuid,
        /// Whether the client is focused.
        focused: bool,
    },
    /// WebRTC data-channel ping heartbeat.
    DcPing,
    /// WebRTC data-channel pong heartbeat.
    DcPong,
    /// Client sent JSON whose shape is owned by Lua/plugin/relay code.
    BoundaryJson(serde_json::Value),
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
    /// Request/response correlation pong.
    Pong {
        /// Request identifier from the ping.
        request_id: RequestId,
    },
    /// Terminal attach state.
    TerminalAttach {
        /// Transport-local subscription identifier.
        subscription_id: SubscriptionId,
        /// Session whose attach state changed.
        session_uuid: SessionUuid,
        /// Known attach state.
        state: TerminalAttachState,
    },
    /// Initial or recovery snapshot for a session.
    Snapshot {
        /// Session the snapshot belongs to.
        session_uuid: SessionUuid,
        /// Opaque snapshot bytes.
        payload: Vec<u8>,
    },
    /// Terminal mode flags changed.
    ModeChanged {
        /// Session whose mode changed.
        session_uuid: SessionUuid,
        /// Sparse mode update.
        mode: crate::session::protocol::ModeChanged,
    },
    /// Kitty keyboard mode changed.
    KittyChanged {
        /// Session whose mode changed.
        session_uuid: SessionUuid,
        /// Whether kitty keyboard mode is enabled.
        enabled: bool,
    },
    /// Focus reporting mode changed.
    FocusReportingChanged {
        /// Session whose mode changed.
        session_uuid: SessionUuid,
        /// Whether focus reporting is enabled.
        enabled: bool,
    },
    /// Client focus state changed.
    FocusChanged {
        /// Session whose focus changed.
        session_uuid: SessionUuid,
        /// Whether the client is focused.
        focused: bool,
    },
    /// WebRTC data-channel pong heartbeat.
    DcPong,
    /// Boundary JSON whose shape is owned by Lua/plugin/relay code.
    BoundaryJson(serde_json::Value),
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
                Some("dc_ping") => Some(TransportIngress::DcPing),
                Some("dc_pong") => Some(TransportIngress::DcPong),
                _ => Some(TransportIngress::BoundaryJson(value)),
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
            TransportEgress::Pong { request_id } => Some(Frame::Json(egress_pong(request_id))),
            TransportEgress::TerminalAttach {
                subscription_id,
                session_uuid,
                state,
            } => Some(Frame::Json(egress_terminal_attach(
                subscription_id,
                session_uuid,
                state,
            ))),
            TransportEgress::Snapshot {
                session_uuid,
                payload,
            } => Some(Frame::Json(egress_snapshot(session_uuid, payload))),
            TransportEgress::ModeChanged { session_uuid, mode } => {
                Some(Frame::Json(egress_mode_changed(session_uuid, mode)))
            }
            TransportEgress::KittyChanged {
                session_uuid,
                enabled,
            } => Some(Frame::Json(egress_kitty_changed(session_uuid, enabled))),
            TransportEgress::FocusReportingChanged {
                session_uuid,
                enabled,
            } => Some(Frame::Json(egress_focus_reporting_changed(
                session_uuid,
                enabled,
            ))),
            TransportEgress::FocusChanged {
                session_uuid,
                focused,
            } => Some(Frame::Json(egress_focus_changed(session_uuid, focused))),
            TransportEgress::DcPong => Some(Frame::Json(egress_dc_pong())),
            TransportEgress::BoundaryJson(value) => Some(Frame::Json(value)),
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
            TuiRequest::LuaMessage(value) => TransportIngress::BoundaryJson(value),
            TuiRequest::FocusChanged {
                session_uuid,
                focused,
            } => TransportIngress::FocusChanged {
                session_uuid,
                focused,
            },
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
            TransportEgress::Pong { request_id } => {
                Some(TuiOutput::Message(egress_pong(request_id)))
            }
            TransportEgress::TerminalAttach {
                subscription_id,
                session_uuid,
                state,
            } => Some(TuiOutput::Message(egress_terminal_attach(
                subscription_id,
                session_uuid,
                state,
            ))),
            TransportEgress::Snapshot {
                session_uuid,
                payload,
            } => Some(TuiOutput::Message(egress_snapshot(session_uuid, payload))),
            TransportEgress::ModeChanged { session_uuid, mode } => {
                Some(TuiOutput::Message(egress_mode_changed(session_uuid, mode)))
            }
            TransportEgress::KittyChanged {
                session_uuid,
                enabled,
            } => Some(TuiOutput::Message(egress_kitty_changed(
                session_uuid,
                enabled,
            ))),
            TransportEgress::FocusReportingChanged {
                session_uuid,
                enabled,
            } => Some(TuiOutput::Message(egress_focus_reporting_changed(
                session_uuid,
                enabled,
            ))),
            TransportEgress::FocusChanged {
                session_uuid,
                focused,
            } => Some(TuiOutput::Message(egress_focus_changed(
                session_uuid,
                focused,
            ))),
            TransportEgress::DcPong => Some(TuiOutput::Message(egress_dc_pong())),
            TransportEgress::BoundaryJson(value) => Some(TuiOutput::Message(value)),
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

pub(crate) fn ingress_to_client_message(ingress: TransportIngress) -> ClientWorkerMessage {
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
        TransportIngress::TerminalResize {
            session_uuid,
            rows,
            cols,
        } => ClientWorkerMessage::SessionResize {
            session_uuid,
            rows,
            cols,
        },
        TransportIngress::RequestSnapshot {
            session_uuid,
            rows,
            cols,
        } => ClientWorkerMessage::RequestSnapshot {
            session_uuid,
            rows,
            cols,
        },
        TransportIngress::FocusChanged {
            session_uuid,
            focused,
        } => ClientWorkerMessage::ControlFrame(ClientControlFrame::FocusChanged {
            session_uuid,
            focused,
        }),
        TransportIngress::DcPing => ClientWorkerMessage::ControlFrame(ClientControlFrame::DcPong),
        TransportIngress::DcPong => {
            ClientWorkerMessage::ControlFrame(ClientControlFrame::DcPongReceived)
        }
        TransportIngress::BoundaryJson(value) => {
            ClientWorkerMessage::ControlFrame(ClientControlFrame::BoundaryJson(value))
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
        ClientWorkerMessage::ControlFrame(ClientControlFrame::Pong { request_id }) => {
            Some(TransportEgress::Pong { request_id })
        }
        ClientWorkerMessage::ControlFrame(ClientControlFrame::TerminalAttach {
            subscription_id: frame_subscription_id,
            session_uuid,
            state,
        }) => Some(TransportEgress::TerminalAttach {
            subscription_id: frame_subscription_id,
            session_uuid,
            state,
        }),
        ClientWorkerMessage::ControlFrame(ClientControlFrame::Snapshot {
            session_uuid,
            payload,
        }) => Some(TransportEgress::Snapshot {
            session_uuid,
            payload,
        }),
        ClientWorkerMessage::ControlFrame(ClientControlFrame::ModeChanged {
            session_uuid,
            mode,
        }) => Some(TransportEgress::ModeChanged { session_uuid, mode }),
        ClientWorkerMessage::ControlFrame(ClientControlFrame::KittyChanged {
            session_uuid,
            enabled,
        }) => Some(TransportEgress::KittyChanged {
            session_uuid,
            enabled,
        }),
        ClientWorkerMessage::ControlFrame(ClientControlFrame::FocusReportingChanged {
            session_uuid,
            enabled,
        }) => Some(TransportEgress::FocusReportingChanged {
            session_uuid,
            enabled,
        }),
        ClientWorkerMessage::ControlFrame(ClientControlFrame::FocusChanged {
            session_uuid,
            focused,
        }) => Some(TransportEgress::FocusChanged {
            session_uuid,
            focused,
        }),
        ClientWorkerMessage::ControlFrame(ClientControlFrame::DcPong) => {
            Some(TransportEgress::DcPong)
        }
        ClientWorkerMessage::ControlFrame(ClientControlFrame::DcPongReceived) => None,
        ClientWorkerMessage::ControlFrame(ClientControlFrame::BoundaryJson(value)) => {
            Some(TransportEgress::BoundaryJson(value))
        }
        ClientWorkerMessage::ControlFrame(ClientControlFrame::Binary(data)) => {
            Some(TransportEgress::Binary(data))
        }
        ClientWorkerMessage::ControlFrame(ClientControlFrame::Scrollback {
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
        ClientWorkerMessage::ControlFrame(ClientControlFrame::ProcessExited {
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

#[must_use]
pub(crate) fn egress_pong(request_id: RequestId) -> serde_json::Value {
    serde_json::json!({
        "type": "pong",
        "request_id": request_id,
    })
}

#[must_use]
pub(crate) fn egress_terminal_attach(
    subscription_id: SubscriptionId,
    session_uuid: SessionUuid,
    state: TerminalAttachState,
) -> serde_json::Value {
    serde_json::json!({
        "type": "terminal_attach",
        "subscriptionId": subscription_id,
        "session_uuid": session_uuid,
        "state": state.as_str(),
    })
}

#[must_use]
pub(crate) fn egress_snapshot(session_uuid: SessionUuid, payload: Vec<u8>) -> serde_json::Value {
    serde_json::json!({
        "type": "snapshot",
        "session_uuid": session_uuid,
        "payload": payload,
    })
}

#[must_use]
pub(crate) fn egress_mode_changed(
    session_uuid: SessionUuid,
    mode: crate::session::protocol::ModeChanged,
) -> serde_json::Value {
    serde_json::json!({
        "type": "mode_changed",
        "session_uuid": session_uuid,
        "mode": mode,
    })
}

#[must_use]
pub(crate) fn egress_kitty_changed(session_uuid: SessionUuid, enabled: bool) -> serde_json::Value {
    serde_json::json!({
        "type": "kitty_changed",
        "enabled": enabled,
        "session_uuid": session_uuid,
    })
}

#[must_use]
pub(crate) fn egress_focus_reporting_changed(
    session_uuid: SessionUuid,
    enabled: bool,
) -> serde_json::Value {
    serde_json::json!({
        "type": "focus_reporting_changed",
        "enabled": enabled,
        "session_uuid": session_uuid,
    })
}

#[must_use]
pub(crate) fn egress_focus_changed(session_uuid: SessionUuid, focused: bool) -> serde_json::Value {
    serde_json::json!({
        "type": "focus_changed",
        "session_uuid": session_uuid,
        "focused": focused,
    })
}

#[must_use]
pub(crate) fn egress_dc_pong() -> serde_json::Value {
    serde_json::json!({ "type": "dc_pong" })
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
    fn socket_adapter_encodes_typed_control_egress_losslessly() {
        let pong = SocketFrameAdapter::egress_to_frame(TransportEgress::Pong {
            request_id: "req-1".to_string(),
        })
        .expect("pong frame");
        assert!(matches!(
            pong,
            Frame::Json(value) if value == serde_json::json!({
                "type": "pong",
                "request_id": "req-1",
            })
        ));

        let attach = SocketFrameAdapter::egress_to_frame(TransportEgress::TerminalAttach {
            subscription_id: "socket:sess-1".to_string(),
            session_uuid: "sess-1".to_string(),
            state: TerminalAttachState::Reconnecting,
        })
        .expect("terminal attach frame");
        assert!(matches!(
            attach,
            Frame::Json(value) if value == serde_json::json!({
                "type": "terminal_attach",
                "subscriptionId": "socket:sess-1",
                "session_uuid": "sess-1",
                "state": "reconnecting",
            })
        ));

        let kitty = SocketFrameAdapter::egress_to_frame(TransportEgress::KittyChanged {
            session_uuid: "sess-1".to_string(),
            enabled: true,
        })
        .expect("kitty frame");
        assert!(matches!(
            kitty,
            Frame::Json(value) if value == serde_json::json!({
                "type": "kitty_changed",
                "enabled": true,
                "session_uuid": "sess-1",
            })
        ));

        let focus = SocketFrameAdapter::egress_to_frame(TransportEgress::FocusReportingChanged {
            session_uuid: "sess-1".to_string(),
            enabled: false,
        })
        .expect("focus reporting frame");
        assert!(matches!(
            focus,
            Frame::Json(value) if value == serde_json::json!({
                "type": "focus_reporting_changed",
                "enabled": false,
                "session_uuid": "sess-1",
            })
        ));

        let dc_pong =
            SocketFrameAdapter::egress_to_frame(TransportEgress::DcPong).expect("dc pong frame");
        assert!(matches!(
            dc_pong,
            Frame::Json(value) if value == egress_dc_pong()
        ));
    }

    #[test]
    fn socket_adapter_decodes_known_json_controls_to_typed_ingress() {
        assert!(matches!(
            SocketFrameAdapter::frame_to_ingress(Frame::Json(
                serde_json::json!({ "type": "dc_ping" })
            )),
            Some(TransportIngress::DcPing)
        ));
        assert!(matches!(
            SocketFrameAdapter::frame_to_ingress(Frame::Json(egress_dc_pong())),
            Some(TransportIngress::DcPong)
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

    #[test]
    fn tui_adapter_maps_focus_ingress_to_typed_frame() {
        let ingress = TuiTransportAdapter::request_to_ingress(TuiRequest::FocusChanged {
            session_uuid: "sess-1".to_string(),
            focused: true,
        });

        assert!(matches!(
            ingress,
            TransportIngress::FocusChanged {
                ref session_uuid,
                focused: true,
            } if session_uuid == "sess-1"
        ));

        let adapter = TuiTransportAdapter::new();
        assert!(matches!(
            adapter.ingress_to_client(ingress),
            ClientWorkerMessage::ControlFrame(ClientControlFrame::FocusChanged {
                session_uuid,
                focused: true,
            }) if session_uuid == "sess-1"
        ));
    }

    #[test]
    fn typed_snapshot_mode_and_boundary_json_encode_to_existing_json_outputs() {
        let snapshot = TuiTransportAdapter::egress_to_output(TransportEgress::Snapshot {
            session_uuid: "sess-1".to_string(),
            payload: b"abc".to_vec(),
        })
        .expect("snapshot output");
        assert!(matches!(
            snapshot,
            TuiOutput::Message(value) if value == serde_json::json!({
                "type": "snapshot",
                "session_uuid": "sess-1",
                "payload": [97, 98, 99],
            })
        ));

        let mode = TuiTransportAdapter::egress_to_output(TransportEgress::ModeChanged {
            session_uuid: "sess-1".to_string(),
            mode: crate::session::protocol::ModeChanged {
                kitty_enabled: Some(true),
                ..Default::default()
            },
        })
        .expect("mode output");
        assert!(matches!(
            mode,
            TuiOutput::Message(value) if value == serde_json::json!({
                "type": "mode_changed",
                "session_uuid": "sess-1",
                "mode": { "kitty_enabled": true },
            })
        ));

        let boundary = serde_json::json!({ "plugin": "defined" });
        let output =
            TuiTransportAdapter::egress_to_output(TransportEgress::BoundaryJson(boundary.clone()))
                .expect("boundary output");
        assert!(matches!(output, TuiOutput::Message(value) if value == boundary));
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
