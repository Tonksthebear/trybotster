//! Worker actor contract scaffolding.
//!
//! These modules define the typed message boundaries for the workerized hub
//! architecture without moving production traffic onto those actors yet. The
//! hub remains the orchestration state owner; workers request state changes
//! through typed messages and bounded queues.

pub mod client;
pub mod hub_control;
pub mod session_io;
pub(crate) mod session_io_runtime;
pub mod transport;
pub(crate) mod webrtc;

/// Stable identifier for a Botster session.
pub type SessionUuid = String;

/// Stable identifier for a terminal subscription within one client transport.
pub type SubscriptionId = String;

/// Stable identifier for an actor request that expects a later response.
pub type RequestId = String;

/// Bounded queue configuration for a worker mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundedQueueConfig {
    /// Human-readable queue label used in logs and diagnostics.
    pub name: &'static str,
    /// Maximum number of messages allowed in the mailbox.
    pub capacity: usize,
}

impl BoundedQueueConfig {
    /// Construct a bounded queue config.
    #[must_use]
    pub const fn new(name: &'static str, capacity: usize) -> Self {
        Self { name, capacity }
    }

    /// Whether the config can back a bounded Tokio mpsc channel.
    #[must_use]
    pub const fn is_bounded(self) -> bool {
        self.capacity > 0
    }
}

#[cfg(test)]
mod tests {
    use super::client::{
        ClientConnectionHealth, ClientControlFrame, ClientWorkerHandle, ClientWorkerMessage,
        CLIENT_WORKER_QUEUE,
    };
    use super::hub_control::{
        HubControlMessage, HubControlOrigin, SessionLifecycleState, TransportConnectionMode,
        TransportPeerState, TransportSignal, WorkerBackpressure, HUB_CONTROL_QUEUE,
    };
    use super::session_io::{SessionIoEvent, SessionIoRequest, SESSION_IO_WORKER_QUEUE};
    use super::transport::{
        TransportAdapter, TransportEgress, TransportIngress, TRANSPORT_ADAPTER_QUEUE,
    };
    use super::BoundedQueueConfig;
    use crate::client::ClientId;

    #[test]
    fn worker_queue_configs_are_bounded() {
        let configs = [
            HUB_CONTROL_QUEUE,
            CLIENT_WORKER_QUEUE,
            TRANSPORT_ADAPTER_QUEUE,
            SESSION_IO_WORKER_QUEUE,
        ];

        for config in configs {
            assert!(config.is_bounded(), "{} must be bounded", config.name);
            assert!(config.capacity >= 512, "{} is too small", config.name);
        }
    }

    #[test]
    fn hub_control_messages_capture_hub_owned_mutation_requests() {
        let attach = HubControlMessage::AttachClient {
            client_id: ClientId::Tui,
            session_uuid: "sess-1".to_string(),
            subscription_id: "tui:sess-1".to_string(),
        };
        let lifecycle = HubControlMessage::SessionLifecycle {
            session_uuid: "sess-1".to_string(),
            state: SessionLifecycleState::Reconnecting { generation: 3 },
        };
        let pressure = HubControlMessage::Backpressure(WorkerBackpressure {
            source: "worker.client",
            capacity: 1024,
            session_uuid: Some("sess-1".to_string()),
            client_id: Some(ClientId::Tui),
        });
        let shutdown = HubControlMessage::Shutdown {
            origin: HubControlOrigin::Internal,
            reason: "test".to_string(),
        };
        let peer_state = HubControlMessage::TransportPeerStateChanged {
            client_id: ClientId::browser("browser-1"),
            browser_identity: "browser-1".to_string(),
            state: TransportPeerState::Connected {
                generation: 2,
                mode: TransportConnectionMode::Unknown,
            },
        };
        let signal = HubControlMessage::TransportSignalReady {
            client_id: ClientId::browser("browser-1"),
            signal: TransportSignal::Answer {
                browser_identity: "browser-1".to_string(),
                envelope: serde_json::json!({ "t": 1 }),
            },
        };

        assert!(format!("{attach:?}").contains("AttachClient"));
        assert!(format!("{lifecycle:?}").contains("Reconnecting"));
        assert!(format!("{pressure:?}").contains("worker.client"));
        assert!(format!("{shutdown:?}").contains("Shutdown"));
        assert!(format!("{peer_state:?}").contains("TransportPeerStateChanged"));
        assert!(format!("{signal:?}").contains("TransportSignalReady"));
    }

    #[test]
    fn client_worker_messages_are_transport_neutral() {
        let messages = [
            ClientWorkerMessage::SubscribeSession {
                session_uuid: "sess-1".to_string(),
                subscription_id: "client:sess-1".to_string(),
            },
            ClientWorkerMessage::TerminalBytes {
                session_uuid: "sess-1".to_string(),
                data: b"hello".to_vec(),
            },
            ClientWorkerMessage::ControlFrame(ClientControlFrame::ProcessExited {
                session_uuid: "sess-1".to_string(),
                exit_code: Some(0),
            }),
            ClientWorkerMessage::Health(ClientConnectionHealth::Ready),
        ];

        for message in messages {
            let rendered = format!("{message:?}");
            assert!(!rendered.contains("WebRtc"));
            assert!(!rendered.contains("Browser("));
        }
    }

    #[test]
    fn client_worker_handle_uses_bounded_tokio_sender() {
        let (tx, _rx) = tokio::sync::mpsc::channel(CLIENT_WORKER_QUEUE.capacity);
        let handle = ClientWorkerHandle {
            client_id: ClientId::Socket("socket:a1".to_string()),
            tx,
        };

        assert_eq!(handle.client_id.to_string(), "socket:a1");
        assert_eq!(handle.tx.capacity(), CLIENT_WORKER_QUEUE.capacity);
    }

    #[derive(Debug)]
    struct EchoAdapter {
        client_id: ClientId,
    }

    impl TransportAdapter for EchoAdapter {
        fn client_id(&self) -> &ClientId {
            &self.client_id
        }

        fn ingress_to_client(&self, ingress: TransportIngress) -> ClientWorkerMessage {
            match ingress {
                TransportIngress::Subscribe {
                    session_uuid,
                    subscription_id,
                } => ClientWorkerMessage::SubscribeSession {
                    session_uuid,
                    subscription_id,
                },
                TransportIngress::TerminalInput { session_uuid, data } => {
                    ClientWorkerMessage::SessionInput { session_uuid, data }
                }
                TransportIngress::Health(health) => ClientWorkerMessage::Health(health),
                TransportIngress::Unsubscribe {
                    session_uuid,
                    subscription_id,
                } => ClientWorkerMessage::UnsubscribeSession {
                    session_uuid,
                    subscription_id,
                },
                TransportIngress::Json(value) => {
                    ClientWorkerMessage::ControlFrame(ClientControlFrame::Json(value))
                }
                TransportIngress::Binary(data) => {
                    ClientWorkerMessage::ControlFrame(ClientControlFrame::Binary(data))
                }
            }
        }

        fn client_to_egress(&self, message: ClientWorkerMessage) -> Option<TransportEgress> {
            match message {
                ClientWorkerMessage::TerminalBytes { data, .. } => {
                    Some(TransportEgress::TerminalBytes {
                        subscription_id: "echo".to_string(),
                        session_uuid: "sess-1".to_string(),
                        data,
                    })
                }
                ClientWorkerMessage::Shutdown { reason } => Some(TransportEgress::Close { reason }),
                _ => None,
            }
        }
    }

    #[test]
    fn transport_adapter_boundary_converts_without_concrete_transport_types() {
        let adapter = EchoAdapter {
            client_id: ClientId::Tui,
        };

        let message = adapter.ingress_to_client(TransportIngress::Subscribe {
            session_uuid: "sess-1".to_string(),
            subscription_id: "tui:sess-1".to_string(),
        });
        let egress = adapter.client_to_egress(ClientWorkerMessage::TerminalBytes {
            session_uuid: "sess-1".to_string(),
            data: b"x".to_vec(),
        });

        assert!(format!("{message:?}").contains("SubscribeSession"));
        assert_eq!(adapter.client_id(), &ClientId::Tui);
        assert!(matches!(
            egress,
            Some(TransportEgress::TerminalBytes { data, .. }) if data == b"x"
        ));
        assert!(!std::any::type_name::<EchoAdapter>().contains("WebRtc"));
    }

    #[test]
    fn session_io_contract_mirrors_session_process_boundary() {
        let requests = [
            SessionIoRequest::PtyInput {
                data: b"ls\n".to_vec(),
            },
            SessionIoRequest::Resize { rows: 24, cols: 80 },
            SessionIoRequest::GetSnapshot {
                request_id: "req-1".to_string(),
            },
            SessionIoRequest::GetScreen {
                request_id: "req-2".to_string(),
            },
            SessionIoRequest::PasteFile {
                request_id: "req-3".to_string(),
                filename: "screenshot.png".to_string(),
                data: b"png".to_vec(),
            },
            SessionIoRequest::PrepareSnapshot {
                request_id: "req-4".to_string(),
                snapshot: vec![1, 2, 3],
                recovery: false,
            },
        ];

        let events = [
            format!(
                "{:?}",
                SessionIoEvent::ProcessExited {
                    session_uuid: "sess-1".to_string(),
                    exit_code: None,
                }
            ),
            format!(
                "{:?}",
                SessionIoEvent::PasteFileFailed {
                    request_id: "req-3".to_string(),
                    session_uuid: "sess-1".to_string(),
                    reason: super::session_io::PasteFileErrorReason::Inject,
                    detail: "closed".to_string(),
                }
            ),
            format!(
                "{:?}",
                SessionIoEvent::PreparedSnapshot {
                    request_id: "req-4".to_string(),
                    session_uuid: "sess-1".to_string(),
                    uncompressed_len: 3,
                    payload: vec![0x02, 1, 2, 3],
                    recovery: false,
                }
            ),
        ];

        for request in requests {
            let rendered = format!("{request:?}");
            assert!(!rendered.contains("WebRtc"));
            assert!(!rendered.contains("Browser"));
        }
        for rendered in events {
            assert!(!rendered.contains("WebRtc"));
            assert!(!rendered.contains("Browser"));
        }
    }

    #[test]
    fn bounded_queue_config_reports_zero_capacity_as_unbounded_invalid() {
        let config = BoundedQueueConfig::new("test.zero", 0);

        assert!(!config.is_bounded());
    }
}
