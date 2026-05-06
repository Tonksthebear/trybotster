use super::test_support::*;

#[test]
pub(super) fn test_queue_webrtc_terminal_snapshot_returns_false_when_mailbox_full() {
    let (hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-snapshot-mailbox-full";
    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(1);
    session_io_tx
        .try_send(crate::worker::session_io::SessionIoRequest::PtyInput {
            data: b"queued".to_vec(),
        })
        .expect("fill mailbox");
    let session = test_session_backed_handle_with_mailbox(session_uuid, session_io_tx);
    let pty = session.pty().clone();
    hub.handle_cache.add_session(session);

    assert!(!Hub::queue_webrtc_terminal_snapshot(
        &hub.hub_event_metrics,
        &hub.hub_event_tx,
        &pty,
        Some("snapshot-full".to_string()),
        session_uuid,
        b"snapshot".to_vec(),
    ));

    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["snapshot.queue_full"], 1);
    assert!(matches!(
        session_io_rx.try_recv().expect("filled request"),
        crate::worker::session_io::SessionIoRequest::PtyInput { .. }
    ));
}

#[test]
pub(super) fn test_browser_initial_snapshot_uses_direct_session_io_delivery() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-browser-initial-direct-snapshot";
    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(4);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            session_uuid,
            session_io_tx,
        ));
    let mut req = test_browser_subscription_request(
        "browser-direct-snapshot",
        session_uuid,
        "terminal_direct_snapshot",
    );
    req.rows = 23;
    let _command_rx = install_test_browser_worker(
        &mut hub,
        "browser-direct-snapshot",
        session_uuid,
        "terminal_direct_snapshot",
    );

    assert!(hub.try_attach_browser_terminal_subscription(&req));
    assert!(
        hub.pending_session_io_snapshots.is_empty(),
        "initial terminal scrollback must not allocate a hub pending snapshot"
    );

    let _ = recv_session_io_request_matching(&mut session_io_rx, |request| {
        matches!(
            request,
            crate::worker::session_io::SessionIoRequest::Resize { rows: 23, cols: 80 }
        )
    });
    let delivery = recv_terminal_initial_snapshot_delivery(&mut session_io_rx);
    assert_eq!(
        delivery.subscription_key,
        "browser-direct-snapshot:sess-browser-initial-direct-snapshot"
    );
    assert_eq!(delivery.subscription_id, "terminal_direct_snapshot");
    assert!(matches!(
        delivery.payload_mode,
        crate::worker::session_io::TerminalSnapshotPayloadMode::PrefixedGzip
    ));
    hub.stop_terminal_subscription("browser-direct-snapshot:sess-browser-initial-direct-snapshot");
}

#[test]
pub(super) fn test_snapshot_enqueue_failure_cleans_existing_pending_request() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-snapshot-enqueue-cleanup";
    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(1);
    session_io_tx
        .try_send(crate::worker::session_io::SessionIoRequest::PtyInput {
            data: b"queued".to_vec(),
        })
        .expect("fill mailbox");
    let session = test_session_backed_handle_with_mailbox(session_uuid, session_io_tx);
    let pty = session.pty().clone();
    hub.handle_cache.add_session(session);
    let request_id = "snapshot-cleanup-on-full".to_string();
    assert!(hub.insert_pending_session_io_snapshot(
        request_id.clone(),
        crate::hub::PendingSessionIoSnapshot {
            session_uuid: session_uuid.to_string(),
            started_at: Instant::now(),
            target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                peer_id: "browser-cleanup".to_string(),
                rows: 24,
                cols: 80,
                kitty_enabled: false,
                subscription_key: Some("browser-cleanup:sess-snapshot-enqueue-cleanup".to_string()),
                active_flag: None,
            },
        },
    ));

    assert!(!Hub::queue_webrtc_terminal_snapshot(
        &hub.hub_event_metrics,
        &hub.hub_event_tx,
        &pty,
        Some(request_id.clone()),
        session_uuid,
        b"snapshot".to_vec(),
    ));
    assert!(hub.pending_session_io_snapshots.contains_key(&request_id));

    hub.poll_hub_events();
    assert!(!hub.pending_session_io_snapshots.contains_key(&request_id));
    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["snapshot.queue_full"], 1);
    assert!(matches!(
        session_io_rx.try_recv().expect("filled request"),
        crate::worker::session_io::SessionIoRequest::PtyInput { .. }
    ));
}

#[test]
pub(super) fn test_prepared_snapshot_routes_through_browser_worker_with_metrics() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let request_id = "snapshot-output-test".to_string();
    let mut command_rx =
        install_test_browser_worker(&mut hub, "browser-output", "sess-output", "sub-output");
    hub.insert_pending_session_io_snapshot(
        request_id.clone(),
        crate::hub::PendingSessionIoSnapshot {
            session_uuid: "sess-output".to_string(),
            started_at: Instant::now(),
            target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                peer_id: "browser-output".to_string(),
                rows: 24,
                cols: 80,
                kitty_enabled: false,
                subscription_key: None,
                active_flag: None,
            },
        },
    );

    hub.handle_session_io_event(
        crate::worker::session_io::SessionIoEvent::PreparedSnapshot {
            request_id,
            session_uuid: "sess-output".to_string(),
            uncompressed_len: 256,
            payload: vec![0x1f, 0x8b, 0x08, 0x00],
            recovery: false,
        },
    );
    let (subscription_id, data) = recv_next_webrtc_pty_command(&mut hub, &mut command_rx, 0x1f);
    assert_eq!(subscription_id, "sub-output");
    assert!(data.starts_with(&[0x1f, 0x8b]));

    let snapshot = hub.hub_event_metrics.snapshot();
    assert!(snapshot.spans.contains_key("snapshot.gzip_queue"));
    assert!(hub.pending_session_io_snapshots.is_empty());
}

#[test]
pub(super) fn test_pending_session_io_snapshot_cleanup_paths() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    hub.insert_pending_session_io_snapshot(
        "by-peer".to_string(),
        crate::hub::PendingSessionIoSnapshot {
            session_uuid: "sess-a".to_string(),
            started_at: Instant::now(),
            target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                peer_id: "browser-a".to_string(),
                rows: 24,
                cols: 80,
                kitty_enabled: false,
                subscription_key: Some("browser-a:sess-a".to_string()),
                active_flag: None,
            },
        },
    );
    hub.insert_pending_session_io_snapshot(
        "by-subscription".to_string(),
        crate::hub::PendingSessionIoSnapshot {
            session_uuid: "sess-b".to_string(),
            started_at: Instant::now(),
            target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                peer_id: "browser-b".to_string(),
                rows: 24,
                cols: 80,
                kitty_enabled: false,
                subscription_key: Some("browser-b:sess-b".to_string()),
                active_flag: None,
            },
        },
    );
    hub.insert_pending_session_io_snapshot(
        "by-session".to_string(),
        crate::hub::PendingSessionIoSnapshot {
            session_uuid: "sess-c".to_string(),
            started_at: Instant::now(),
            target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcPeerRecovery {
                request: crate::worker::webrtc::WebRtcRecoverySnapshotRequest {
                    request_id: "recovery-by-session".to_string(),
                    browser_identity: "browser-c".to_string(),
                    session_uuid: "sess-c".to_string(),
                    subscription_id: "sub-c".to_string(),
                },
            },
        },
    );

    hub.cleanup_pending_session_io_snapshots_for_peer("browser-a");
    assert!(!hub.pending_session_io_snapshots.contains_key("by-peer"));
    hub.cleanup_pending_session_io_snapshots_for_subscription("browser-b:sess-b");
    assert!(!hub
        .pending_session_io_snapshots
        .contains_key("by-subscription"));
    hub.handle_hub_event(crate::hub::events::HubEvent::SessionUnregistered {
        session_uuid: "sess-c".to_string(),
    });
    assert!(!hub.pending_session_io_snapshots.contains_key("by-session"));

    hub.insert_pending_session_io_snapshot(
        "stale".to_string(),
        crate::hub::PendingSessionIoSnapshot {
            session_uuid: "sess-stale".to_string(),
            started_at: Instant::now()
                - crate::hub::SESSION_IO_SNAPSHOT_PENDING_TTL
                - Duration::from_secs(1),
            target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                peer_id: "browser-stale".to_string(),
                rows: 24,
                cols: 80,
                kitty_enabled: false,
                subscription_key: None,
                active_flag: None,
            },
        },
    );
    hub.cleanup_stale_session_io_snapshots();
    assert!(!hub.pending_session_io_snapshots.contains_key("stale"));
    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["snapshot.pending_stale_drop"], 1);
}
