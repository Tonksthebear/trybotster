use super::test_support::*;

#[test]
pub(super) fn test_missing_session_io_sender_control_records_observable_metric() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();

    hub.handle_client_worker_control(crate::worker::hub_control::HubControlMessage::Backpressure(
        crate::worker::hub_control::WorkerBackpressure {
            source: "worker.client.session_io_missing",
            capacity: 0,
            session_uuid: Some("sess-missing-sender".to_string()),
            client_id: Some(ClientId::Tui),
        },
    ));

    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["client_worker.backpressure"], 1);
    assert_eq!(snapshot.counters["client_worker.session_io_missing"], 1);
}

#[test]
pub(super) fn test_worker_session_io_registration_uses_real_session_io_mailbox() {
    let source = include_str!("terminal_client_adapters.rs");
    let body = function_body(source, "register_worker_session_io_sender");
    assert!(
        body.contains("pty_handle.session_io_sender()"),
        "ClientWorker registration must use the real SessionIoWorker mailbox"
    );
    assert!(
        !body.contains("write_input_direct"),
        "ClientWorker registration must not reintroduce hub-owned PTY writes"
    );
    assert!(
        !body.contains("tokio::spawn"),
        "ClientWorker registration must not create a hub-owned PTY bridge task"
    );
}
