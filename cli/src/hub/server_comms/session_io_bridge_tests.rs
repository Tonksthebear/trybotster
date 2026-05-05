use super::test_support::*;

#[test]
pub(super) fn test_session_io_batch_preserves_output_metrics_and_probe_learning() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-worker-batch-probe";

    hub.handle_cache
        .add_session(test_local_session_handle(session_uuid));

    hub.handle_hub_event(crate::hub::events::HubEvent::SessionIoBatch(
        crate::worker::session_io::SessionIoBatch {
            session_uuid: session_uuid.to_string(),
            output: Some(b"\x1b]11;?\x07payload".to_vec()),
        },
    ));

    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["pty_output.messages"], 1);
    assert_eq!(snapshot.counters["pty_output.bytes"], 14);

    hub.learn_terminal_probe_replies(session_uuid, "browser-a", b"\x1b]11;rgb:1234/5678/9abc\x07");

    assert_eq!(
        hub.terminal_profiles.headless_reply(
            session_uuid,
            crate::hub::terminal_profile::TerminalProbe::DefaultBackground
        ),
        None
    );
}

#[test]
pub(super) fn test_file_input_enqueues_paste_and_written_event_registers_cleanup() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-file-paste-mailbox";
    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(4);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            session_uuid,
            session_io_tx,
        ));

    hub.handle_file_input(crate::channel::webrtc::FileInputIncoming {
        session_uuid: session_uuid.to_string(),
        filename: "drop.PNG".to_string(),
        data: b"image-bytes".to_vec(),
    });

    match session_io_rx.try_recv().expect("paste request") {
        crate::worker::session_io::SessionIoRequest::PasteFile {
            request_id,
            filename,
            data,
        } => {
            assert!(request_id.starts_with("paste-"));
            assert_eq!(filename, "drop.PNG");
            assert_eq!(data, b"image-bytes");
            let path = std::path::PathBuf::from("/tmp/botster-paste-test.png");
            hub.handle_session_io_event(
                crate::worker::session_io::SessionIoEvent::PasteFileWritten {
                    request_id,
                    session_uuid: session_uuid.to_string(),
                    path: path.clone(),
                    bytes: 11,
                },
            );
            assert_eq!(
                hub.paste_files.get(session_uuid).expect("paste cleanup"),
                &vec![path]
            );
        }
        other => panic!("expected PasteFile request, got {other:?}"),
    }
}

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
pub(super) fn test_empty_initial_snapshot_cleans_pending_session_io_request() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-empty-initial-snapshot";
    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(4);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox_and_snapshot(
            session_uuid,
            session_io_tx,
            Some(Vec::new()),
        ));
    let mut req = test_forwarder_request(
        "browser-empty-snapshot",
        session_uuid,
        "terminal_empty_snapshot",
    );
    req.rows = 23;
    let _command_rx = install_test_browser_worker(
        &mut hub,
        "browser-empty-snapshot",
        session_uuid,
        "terminal_empty_snapshot",
    );

    assert!(hub.try_attach_terminal_forwarder(&req));
    assert_eq!(hub.pending_session_io_snapshots.len(), 1);

    let _ = recv_session_io_request_matching(&mut session_io_rx, |request| {
        matches!(
            request,
            crate::worker::session_io::SessionIoRequest::Resize { rows: 23, cols: 80 }
        )
    });
    let request_id = match recv_session_io_request_matching(&mut session_io_rx, |request| {
        matches!(
            request,
            crate::worker::session_io::SessionIoRequest::GetSnapshot { .. }
        )
    }) {
        crate::worker::session_io::SessionIoRequest::GetSnapshot { request_id } => request_id,
        other => panic!("expected GetSnapshot request, got {other:?}"),
    };

    hub.handle_session_io_event(crate::worker::session_io::SessionIoEvent::Snapshot {
        request_id,
        session_uuid: session_uuid.to_string(),
        payload: Vec::new(),
    });

    for _ in 0..20 {
        hub.tokio_runtime.block_on(async {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        });
        hub.poll_hub_events();
        if hub.pending_session_io_snapshots.is_empty() {
            break;
        }
    }

    assert!(hub.pending_session_io_snapshots.is_empty());
    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["snapshot.empty"], 1);
    hub.stop_lua_pty_forwarder("browser-empty-snapshot:sess-empty-initial-snapshot");
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
                forwarder_key: Some("browser-cleanup:sess-snapshot-enqueue-cleanup".to_string()),
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
                forwarder_key: None,
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
                forwarder_key: Some("browser-a:sess-a".to_string()),
                active_flag: None,
            },
        },
    );
    hub.insert_pending_session_io_snapshot(
        "by-forwarder".to_string(),
        crate::hub::PendingSessionIoSnapshot {
            session_uuid: "sess-b".to_string(),
            started_at: Instant::now(),
            target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                peer_id: "browser-b".to_string(),
                rows: 24,
                cols: 80,
                kitty_enabled: false,
                forwarder_key: Some("browser-b:sess-b".to_string()),
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
    hub.cleanup_pending_session_io_snapshots_for_forwarder("browser-b:sess-b");
    assert!(!hub
        .pending_session_io_snapshots
        .contains_key("by-forwarder"));
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
                forwarder_key: None,
                active_flag: None,
            },
        },
    );
    hub.cleanup_stale_session_io_snapshots();
    assert!(!hub.pending_session_io_snapshots.contains_key("stale"));
    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["snapshot.pending_stale_drop"], 1);
}

#[test]
pub(super) fn test_session_io_batch_notifies_lua_pty_output_observers_in_byte_order() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-worker-batch-lua";

    hub.handle_cache
        .add_session(test_local_session_handle(session_uuid));

    hub.lua
        .lua()
        .load(
            r#"
                _test_pty_output_chunks = {}
                hooks.on("pty_output", "test.session_io_batch_order", function(ctx, data)
                    table.insert(_test_pty_output_chunks, {
                        session_uuid = ctx.session_uuid,
                        data = data,
                        len = #data,
                    })
                end)
                "#,
        )
        .exec()
        .expect("register test pty_output observer");

    let chunks = [
        b"coalesced-one-".to_vec(),
        b"two-and-three-".to_vec(),
        b"four".to_vec(),
    ];
    let expected = chunks.concat();

    for chunk in chunks {
        hub.handle_hub_event(crate::hub::events::HubEvent::SessionIoBatch(
            crate::worker::session_io::SessionIoBatch {
                session_uuid: session_uuid.to_string(),
                output: Some(chunk),
            },
        ));
    }

    let observed: String = hub
        .lua
        .lua()
        .load(
            r#"
                local out = {}
                for _, chunk in ipairs(_test_pty_output_chunks) do
                    table.insert(out, chunk.data)
                end
                return table.concat(out)
                "#,
        )
        .eval()
        .expect("read observed pty output bytes");
    let observed_total: usize = hub
        .lua
        .lua()
        .load(
            r#"
                local total = 0
                for _, chunk in ipairs(_test_pty_output_chunks) do
                    total = total + chunk.len
                end
                return total
                "#,
        )
        .eval()
        .expect("read observed pty output byte count");
    let observed_count: usize = hub
        .lua
        .lua()
        .load("return #_test_pty_output_chunks")
        .eval()
        .expect("read observed pty output chunk count");
    let observed_session_uuid: String = hub
        .lua
        .lua()
        .load("return _test_pty_output_chunks[1].session_uuid")
        .eval()
        .expect("read observed pty output context");

    assert_eq!(observed.as_bytes(), expected.as_slice());
    assert_eq!(observed_total, expected.len());
    assert_eq!(observed_count, 3);
    assert_eq!(observed_session_uuid, session_uuid);

    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["pty_output.messages"], 3);
    assert_eq!(snapshot.counters["pty_output.bytes"], expected.len() as u64);
}

#[test]
pub(super) fn test_browser_focus_input_updates_active_terminal_peer() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-browser-focus";

    hub.handle_pty_input(crate::channel::webrtc::PtyInputIncoming {
        session_uuid: session_uuid.to_string(),
        browser_identity: "browser-a".to_string(),
        data: b"\x1b[I".to_vec(),
    });

    assert_eq!(
        hub.active_terminal_peers
            .lock()
            .expect("active peers mutex")
            .get(session_uuid)
            .cloned(),
        Some("browser-a".to_string())
    );

    hub.handle_pty_input(crate::channel::webrtc::PtyInputIncoming {
        session_uuid: session_uuid.to_string(),
        browser_identity: "browser-a".to_string(),
        data: b"\x1b[O".to_vec(),
    });

    assert!(hub
        .active_terminal_peers
        .lock()
        .expect("active peers mutex")
        .get(session_uuid)
        .is_none());
}
