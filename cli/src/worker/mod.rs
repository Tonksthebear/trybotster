//! Worker actor contracts.
//!
//! These modules define the typed message boundaries for the workerized hub
//! architecture. The hub remains the orchestration state owner; workers request
//! state changes through typed messages and bounded queues.

pub mod client;
pub mod hub_control;
pub mod plugin;
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
    use super::plugin::{
        PluginHandlerKind, PluginHandlerRef, PluginLoadSpec, PluginWorkerMessage,
        PLUGIN_WORKER_QUEUE,
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
            PLUGIN_WORKER_QUEUE,
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
    fn plugin_worker_messages_use_stable_handler_refs_not_lua_functions() {
        let load = PluginWorkerMessage::Load {
            spec: PluginLoadSpec {
                plugin_key: "repo:demo".to_string(),
                display_name: "demo".to_string(),
                init_path: "/tmp/demo/init.lua".into(),
                source: Some("repo".to_string()),
                repo_root: Some("/tmp/demo".into()),
            },
        };
        let invoke = PluginWorkerMessage::Invoke {
            request_id: "plugin-req-1".to_string(),
            handler: PluginHandlerRef {
                kind: PluginHandlerKind::UiAction,
                id: "botster.demo.run".to_string(),
                name: Some("main".to_string()),
            },
            payload: serde_json::json!({ "session_uuid": "sess-1" }),
            timeout_ms: 250,
        };

        let rendered = format!("{load:?} {invoke:?}");
        assert!(rendered.contains("PluginLoadSpec"));
        assert!(rendered.contains("PluginHandlerRef"));
        assert!(!rendered.contains("Function"));
        assert!(!rendered.contains("mlua"));
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
            super::transport::ingress_to_client_message(ingress)
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

    #[test]
    fn static_webrtc_receiver_access_stays_test_only_and_registry_owned() {
        let webrtc = include_str!("webrtc.rs");
        let server_comms = concat!(
            include_str!("../hub/server_comms.rs"),
            include_str!("../hub/server_comms/webrtc_transport.rs")
        );

        assert!(webrtc.contains("fn start_queue_forwarders"));
        for event in [
            "HubEvent::WebRtcOutgoingSignal",
            "HubEvent::WebRtcStreamFrame",
        ] {
            assert!(
                webrtc.contains(event),
                "WebRtcPeerRegistry forwarders must emit typed {event} events"
            );
        }
        assert!(
            webrtc.contains("ClientWorkerMessage::SessionInput")
                && webrtc.contains("ClientWorkerMessage::PasteFile"),
            "WebRTC PTY/file input must route directly to the owning ClientWorker"
        );

        assert!(
            preceding_cfg_test(webrtc, "fn poll_received_messages"),
            "raw WebRTC receiver polling must remain a test-only helper"
        );
        for helper in [
            "fn lease_outgoing_signal_receiver_for_test",
            "fn lease_stream_frame_receiver_for_test",
        ] {
            assert!(
                preceding_cfg_test(webrtc, helper),
                "{helper} must remain cfg(test)"
            );
        }

        for usage in [
            "poll_received_messages(",
            "lease_outgoing_signal_receiver_for_test(",
            "lease_stream_frame_receiver_for_test(",
        ] {
            assert!(
                all_occurrences_in_cfg_test_functions(server_comms, usage),
                "{usage} may only be used by cfg(test) hub polling helpers"
            );
        }
    }

    #[test]
    fn static_browser_webrtc_ingress_crosses_client_worker_boundary() {
        let webrtc = include_str!("webrtc.rs");
        let server_comms = concat!(
            include_str!("../hub/server_comms.rs"),
            include_str!("../hub/server_comms/webrtc_transport.rs")
        );

        for typed_mapping in [
            "Some(\"subscribe\")",
            "TransportIngress::Subscribe",
            "Some(\"unsubscribe\")",
            "TransportIngress::Unsubscribe",
            "Some(\"focus_changed\")",
            "TransportIngress::FocusChanged",
        ] {
            assert!(
                webrtc.contains(typed_mapping),
                "WebRTC plaintext classification must keep {typed_mapping} typed"
            );
        }

        let client_worker_branch = source_window(
            server_comms,
            "WebRtcIngressOutcome::ClientWorker(other)",
            "fn call_lua_webrtc_message",
        );
        assert!(client_worker_branch.contains("browser_client_workers.get(browser_identity)"));
        assert!(client_worker_branch.contains("worker.try_send(other)"));
        assert!(
            !client_worker_branch.contains("call_lua_webrtc_message(browser_identity, other"),
            "typed browser terminal ingress must not fall back to Lua routing"
        );

        for bypass in [
            "send_pty_raw",
            "send_pty(",
            "WebRtcAdapterCommand::Pty",
            "WebRtcAdapterCommand::Binary",
        ] {
            assert!(
                !source_window(
                    server_comms,
                    "fn process_webrtc_plaintext_payload",
                    "fn call_lua_webrtc_message",
                )
                .contains(bypass),
                "process_webrtc_plaintext_payload must not encode browser terminal traffic directly with {bypass}"
            );
        }
    }

    #[test]
    fn static_session_io_docs_only_claim_executable_mailbox_work() {
        let docs = include_str!("../../../docs/worker-actor-contracts.md");
        let session_io = include_str!("session_io.rs");
        let runtime = include_str!("session_io_runtime.rs");

        for variant in [
            "PtyInput",
            "Resize",
            "GetSnapshot",
            "PasteFile",
            "PrepareSnapshot",
            "GetModeFlags",
            "GetScreen",
            "SetColorProfile",
            "Shutdown",
        ] {
            assert!(
                session_io.contains(&format!("    {variant}")),
                "SessionIoRequest::{variant} must exist in the contract"
            );
            assert!(
                runtime.contains(&format!("SessionIoRequest::{variant}")),
                "SessionIoRequest::{variant} must have executable runtime handling before docs claim worker ownership"
            );
        }

        assert!(docs.contains("current production mailbox work"));
        assert!(docs.contains("Hub-owned policy"));
        assert!(docs.contains("synchronous"));
        assert!(docs.contains("compatibility work"));
        assert!(
            !docs.contains("scaffold-only SessionIoRequest"),
            "docs should not describe scaffold-only SessionIoRequest variants as production-owned"
        );
    }

    #[test]
    fn static_boundary_json_remains_lua_plugin_or_relay_boundary() {
        let docs = include_str!("../../../docs/worker-actor-contracts.md");
        let transport = include_str!("transport.rs");
        let webrtc = include_str!("webrtc.rs");
        let server_comms = concat!(
            include_str!("../hub/server_comms.rs"),
            include_str!("../hub/server_comms/webrtc_transport.rs"),
            include_str!("../hub/server_comms/terminal_attach.rs"),
            include_str!("../hub/server_comms/terminal_snapshot.rs"),
            include_str!("../hub/server_comms/terminal_stream.rs"),
            include_str!("../hub/server_comms/terminal_clients.rs"),
            include_str!("../hub/server_comms/terminal_client_adapters.rs"),
            include_str!("../hub/server_comms/terminal_cleanup.rs")
        );

        assert!(docs.contains("JSON remains limited to Lua/plugin/relay boundaries"));
        assert!(transport.contains("TransportIngress::BoundaryJson"));
        assert!(transport.contains("ClientControlFrame::BoundaryJson"));
        assert!(webrtc.contains("_ => TransportIngress::BoundaryJson(msg)"));

        let stable_ingress = source_window(
            transport,
            "pub(crate) fn ingress_to_client_message",
            "fn client_message_to_egress",
        );
        for typed in [
            "TransportIngress::Subscribe",
            "TransportIngress::Unsubscribe",
            "TransportIngress::TerminalInput",
            "TransportIngress::FocusChanged",
            "TransportIngress::DcPing",
            "TransportIngress::DcPong",
        ] {
            assert!(
                stable_ingress.contains(typed),
                "{typed} must stay typed before BoundaryJson fallback"
            );
        }

        let hub_boundary_json_count = server_comms
            .matches("ClientControlFrame::BoundaryJson")
            .count();
        assert!(
            hub_boundary_json_count <= 1,
            "hub-side BoundaryJson should not grow beyond the documented subscribe ack exception"
        );
        if hub_boundary_json_count == 1 {
            assert!(server_comms.contains("\"type\": \"subscribed\""));
        }
    }

    fn source_window<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
        let start_idx = source.find(start).expect("source start marker missing");
        let after_start = &source[start_idx..];
        let end_idx = after_start.find(end).unwrap_or(after_start.len());
        &after_start[..end_idx]
    }

    fn preceding_cfg_test(source: &str, marker: &str) -> bool {
        let marker_idx = source.find(marker).expect("source marker missing");
        let prefix = &source[..marker_idx];
        prefix
            .lines()
            .rev()
            .take(4)
            .any(|line| line.trim() == "#[cfg(test)]")
    }

    fn all_occurrences_in_cfg_test_functions(source: &str, needle: &str) -> bool {
        let mut rest = source;
        let mut offset = 0;
        let mut found = false;

        while let Some(relative_idx) = rest.find(needle) {
            found = true;
            let idx = offset + relative_idx;
            if !inside_preceding_cfg_test_function(source, idx) {
                return false;
            }
            let next = relative_idx + needle.len();
            offset += next;
            rest = &rest[next..];
        }

        found
    }

    fn inside_preceding_cfg_test_function(source: &str, idx: usize) -> bool {
        let prefix = &source[..idx];
        let fn_idx = prefix
            .rfind("\n    pub(super) fn ")
            .or_else(|| prefix.rfind("\n    fn "));
        let Some(fn_idx) = fn_idx else { return false };
        prefix[fn_idx..]
            .lines()
            .take(4)
            .any(|line| line.contains("_for_tests") || line.contains("poll_"))
            && prefix[..fn_idx]
                .lines()
                .rev()
                .take(4)
                .any(|line| line.trim() == "#[cfg(test)]")
    }
}
