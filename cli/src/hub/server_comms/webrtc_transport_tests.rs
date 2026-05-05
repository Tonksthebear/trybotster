use super::test_support::*;

#[test]
pub(super) fn test_inactive_webrtc_forwarder_strips_probe_queries() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-filter-inactive-webrtc";
    let session = test_session_handle(session_uuid);
    let event_tx = session.pty().event_tx_clone();
    hub.handle_cache.add_session(session);
    let mut command_rx =
        install_test_browser_worker(&mut hub, "browser-a", session_uuid, "terminal_sub");

    assert!(hub.try_attach_terminal_forwarder(&test_forwarder_request(
        "browser-a",
        session_uuid,
        "terminal_sub"
    )));
    hub.set_active_terminal_peer(session_uuid, "tui", true);
    // No snapshot message (0x02) — test PtyHandle has no session process,
    // so get_snapshot() returns empty and the snapshot send is skipped.
    // Allow forwarder task to start the live loop.
    hub.tokio_runtime.block_on(async {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    });

    let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(
        b"before\x1b]11;?\x07after".to_vec(),
    ));

    let (_subscription_id, data) = recv_next_webrtc_pty_command(&mut hub, &mut command_rx, 0x01);
    assert_eq!(data, b"\x01beforeafter");
}

#[test]
pub(super) fn test_active_webrtc_forwarder_keeps_probe_queries() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-filter-active-webrtc";
    let session = test_session_handle(session_uuid);
    let event_tx = session.pty().event_tx_clone();
    hub.handle_cache.add_session(session);
    let mut command_rx =
        install_test_browser_worker(&mut hub, "browser-a", session_uuid, "terminal_sub");

    assert!(hub.try_attach_terminal_forwarder(&test_forwarder_request(
        "browser-a",
        session_uuid,
        "terminal_sub"
    )));
    hub.set_active_terminal_peer(session_uuid, "browser-a", true);
    // No snapshot message — empty snapshot from test PtyHandle.
    hub.tokio_runtime.block_on(async {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    });

    let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(
        b"\x1b]11;?\x07after".to_vec(),
    ));

    let (_subscription_id, data) = recv_next_webrtc_pty_command(&mut hub, &mut command_rx, 0x01);
    assert_eq!(data, b"\x01\x1b]11;?\x07after");
}

#[test]
pub(super) fn test_webrtc_worker_live_output_runs_per_browser_pty_hooks() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-webrtc-live-hooks";
    let session = test_session_handle(session_uuid);
    let event_tx = session.pty().event_tx_clone();
    hub.handle_cache.add_session(session);
    let mut command_rx =
        install_test_browser_worker(&mut hub, "browser-hooks", session_uuid, "terminal_hooks");

    hub.lua
        .lua()
        .load(
            r#"
                _test_webrtc_pty_hook_peer = nil
                hooks.intercept("pty_output", "test.webrtc_worker_live", function(ctx, data)
                    _test_webrtc_pty_hook_peer = ctx.peer_id
                    return data .. "-hooked"
                end)
                "#,
        )
        .exec()
        .expect("register test pty_output interceptor");

    assert!(hub.try_attach_terminal_forwarder(&test_forwarder_request(
        "browser-hooks",
        session_uuid,
        "terminal_hooks"
    )));
    hub.tokio_runtime.block_on(async {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    });

    let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"live".to_vec()));

    let (_subscription_id, data) = recv_next_webrtc_pty_command(&mut hub, &mut command_rx, 0x01);
    assert_eq!(data, b"\x01live-hooked");
    let observed_peer: String = hub
        .lua
        .lua()
        .load("return _test_webrtc_pty_hook_peer")
        .eval()
        .expect("read pty hook peer");
    assert_eq!(observed_peer, "browser-hooks");
}

#[test]
pub(super) fn test_webrtc_focus_message_updates_active_terminal_peer() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-webrtc-focus";
    let payload = serde_json::to_vec(&serde_json::json!({
        "type": "focus_changed",
        "session_uuid": session_uuid,
        "focused": true,
    }))
    .expect("focus payload");

    hub.process_webrtc_plaintext_payload("browser-a", &payload);

    assert_eq!(
        hub.active_terminal_peers
            .lock()
            .expect("active peers mutex")
            .get(session_uuid)
            .cloned(),
        Some("browser-a".to_string())
    );
}

#[test]
pub(super) fn test_backpressure_recovery_routes_prepared_snapshot_to_peer() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let mut peer_rx = hub
        .webrtc
        .install_test_recovery_sender("browser-recovery", &hub.tokio_runtime);
    let request_id = "snapshot-recovery-test".to_string();
    hub.insert_pending_session_io_snapshot(
        request_id.clone(),
        crate::hub::PendingSessionIoSnapshot {
            session_uuid: "sess-recovery".to_string(),
            started_at: Instant::now(),
            target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcPeerRecovery {
                request: crate::worker::webrtc::WebRtcRecoverySnapshotRequest {
                    request_id: "browser-recovery:sess-recovery".to_string(),
                    browser_identity: "browser-recovery".to_string(),
                    session_uuid: "sess-recovery".to_string(),
                    subscription_id: "sub-recovery".to_string(),
                },
            },
        },
    );

    hub.handle_session_io_event(
        crate::worker::session_io::SessionIoEvent::PreparedSnapshot {
            request_id,
            session_uuid: "sess-recovery".to_string(),
            uncompressed_len: 128,
            payload: vec![0x1f, 0x8b, 0x08],
            recovery: true,
        },
    );

    match peer_rx.try_recv().expect("recovery snapshot command") {
        crate::worker::webrtc::WebRtcAdapterCommand::Pty {
            subscription_id,
            data,
        } => {
            assert_eq!(subscription_id, "sub-recovery");
            assert!(data.starts_with(&[0x1f, 0x8b]));
        }
        other => panic!("expected PTY recovery command, got {other:?}"),
    }

    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["snapshot.backpressure_recovery.sent"], 1);
    assert!(snapshot.spans.contains_key("snapshot.gzip_queue"));
}

#[test]
pub(super) fn test_backpressure_recovery_missing_session_counts_failed_without_dispatch() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-recovery-missing";
    let browser_identity = "browser-recovery-missing";
    let mut peer_rx = hub
        .webrtc
        .install_test_recovery_sender(browser_identity, &hub.tokio_runtime);
    let key = format!("{browser_identity}:{session_uuid}");
    hub.webrtc.record_backpressure_recovery(
        key,
        crate::worker::webrtc::BackpressureRecoveryEntry {
            browser_identity: browser_identity.to_string(),
            session_uuid: session_uuid.to_string(),
            subscription_id: "sub-recovery-missing".to_string(),
            last_drop: Instant::now() - crate::worker::webrtc::BACKPRESSURE_SNAPSHOT_COOLDOWN,
        },
    );

    hub.dispatch_webrtc_recovery_snapshot_requests();

    assert!(peer_rx.try_recv().is_err());
    let metrics = hub.hub_event_metrics.snapshot();
    assert!(!metrics
        .counters
        .contains_key("snapshot.backpressure_recovery.sent"));
    assert!(!metrics
        .counters
        .contains_key("snapshot.backpressure_recovery.empty"));
    assert_eq!(metrics.counters["snapshot.backpressure_recovery.failed"], 1);
}

#[test]
pub(super) fn test_unknown_peer_burst_guardrail_is_bounded_and_rate_limited() {
    let (hub, _request_tx, _output_rx) = e2e_hub();

    for _ in 0..crate::worker::webrtc::PeerBurstState::THRESHOLD {
        hub.queue_webrtc_peer_command(
            "peer-alpha-abcdefghijklmnopqrstuvwxyz",
            crate::worker::webrtc::WebRtcAdapterCommand::Json { data: vec![1] },
        );
    }
    for i in 0..32 {
        hub.queue_webrtc_peer_command(
            &format!("peer-distinct-{i}"),
            crate::worker::webrtc::WebRtcAdapterCommand::Json { data: vec![1] },
        );
    }

    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(
        snapshot.counters["webrtc_send.unknown_peer_burst"], 1,
        "same peer should warn once per window"
    );
    assert_eq!(
        snapshot.counters["webrtc_send.unknown_peer"],
        (crate::worker::webrtc::PeerBurstState::THRESHOLD + 32) as u64
    );
    let peer_count = hub.webrtc.unknown_peer_distinct_count();
    assert!(peer_count <= crate::worker::webrtc::PeerBurstState::PEER_CAP);
}

#[test]
pub(super) fn test_terminal_attach_intent_resolves_when_session_appears() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let key = "peer-attach:sess-attach".to_string();

    let req = test_forwarder_request("peer-attach", "sess-attach", "terminal_sess-attach");
    hub.create_lua_pty_forwarder(req);

    assert!(
        hub.pending_terminal_attaches.contains_key(&key),
        "missing session should create pending attach intent"
    );
    assert!(
        !hub.pty_forwarders.contains_key(&key),
        "forwarder should not start until session is registered"
    );

    hub.handle_cache
        .add_session(test_session_handle("sess-attach"));
    let _command_rx = install_test_browser_worker(
        &mut hub,
        "peer-attach",
        "sess-attach",
        "terminal_sess-attach",
    );
    hub.tick();

    assert!(
        !hub.pending_terminal_attaches.contains_key(&key),
        "pending attach intent should clear once session exists"
    );
    assert!(
        hub.pty_forwarders.contains_key(&key),
        "forwarder should start after session registration"
    );
    assert!(
        hub.browser_client_workers.contains_key("peer-attach"),
        "WebRTC peer should register a browser ClientWorker handle"
    );
}

#[test]
pub(super) fn test_webrtc_worker_handle_registered_and_removed_with_forwarder() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-webrtc-worker-lifecycle";
    let key = format!("browser-worker:{session_uuid}");

    hub.handle_cache
        .add_session(test_session_handle(session_uuid));
    let _command_rx =
        install_test_browser_worker(&mut hub, "browser-worker", session_uuid, "terminal_sub");

    assert!(hub.try_attach_terminal_forwarder(&test_forwarder_request(
        "browser-worker",
        session_uuid,
        "terminal_sub"
    )));
    assert!(
        hub.browser_client_workers.contains_key("browser-worker"),
        "WebRTC peer should register a browser ClientWorker handle"
    );

    hub.stop_lua_pty_forwarder(&key);

    assert!(
        hub.browser_client_workers.contains_key("browser-worker"),
        "stopping one WebRTC forwarder should keep the peer-level ClientWorker handle"
    );
    assert!(
        !hub.pty_forwarders.contains_key(&key),
        "stopping WebRTC forwarder should remove the task"
    );
}

#[test]
pub(super) fn test_webrtc_terminal_subscribe_routes_attach_through_browser_worker() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let browser_identity = "browser-subscribe-worker";
    let session_uuid = "sess-webrtc-subscribe-worker";
    let forwarder_key = format!("{browser_identity}:{session_uuid}");

    hub.handle_cache
        .add_session(test_session_handle(session_uuid));
    let mut command_rx = install_test_browser_worker_unsubscribed(&mut hub, browser_identity);

    let payload = serde_json::json!({
        "type": "subscribe",
        "channel": "terminal",
        "subscriptionId": "terminal_sub_worker",
        "params": {
            "session_uuid": session_uuid,
            "rows": 33,
            "cols": 120,
        }
    });
    hub.process_webrtc_plaintext_payload(browser_identity, payload.to_string().as_bytes());

    hub.tokio_runtime.block_on(async {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    });
    hub.poll_hub_events();

    assert!(
        hub.pty_forwarders.contains_key(&forwarder_key),
        "browser subscribe should attach through the peer-level ClientWorker"
    );

    let mut saw_subscribed_ack = false;
    for _ in 0..20 {
        hub.tokio_runtime.block_on(async {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        });
        hub.poll_hub_events();
        while let Ok(command) = command_rx.try_recv() {
            if matches!(
                command,
                crate::worker::webrtc::WebRtcAdapterCommand::Json { data }
                    if serde_json::from_slice::<serde_json::Value>(&data)
                        .ok()
                        .and_then(|value| value.get("type").and_then(|v| v.as_str()).map(str::to_owned))
                        .as_deref()
                        == Some("subscribed")
            ) {
                saw_subscribed_ack = true;
                break;
            }
        }
        if saw_subscribed_ack {
            break;
        }
    }
    assert!(saw_subscribed_ack, "expected subscribed ack");
}

#[test]
pub(super) fn test_webrtc_peer_cleanup_removes_terminal_client_worker() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let browser_identity = "browser-cleanup";
    let session_uuid = "sess-webrtc-cleanup";
    let key = format!("{browser_identity}:{session_uuid}");

    hub.handle_cache
        .add_session(test_session_handle(session_uuid));
    let _command_rx =
        install_test_browser_worker(&mut hub, browser_identity, session_uuid, "terminal_sub");
    assert!(hub.try_attach_terminal_forwarder(&test_forwarder_request(
        browser_identity,
        session_uuid,
        "terminal_sub"
    )));
    assert!(
        hub.browser_client_workers.contains_key(browser_identity),
        "WebRTC peer should register a browser ClientWorker handle"
    );

    let channel = crate::channel::WebRtcChannel::builder()
        .server_url(hub.config.server_url.clone())
        .api_key(hub.config.get_api_key().to_string())
        .signal_tx(hub.webrtc.outgoing_signal_tx())
        .stream_frame_tx(hub.webrtc.stream_frame_tx())
        .pty_input_tx(hub.webrtc.pty_input_tx())
        .file_input_tx(hub.webrtc.file_input_tx())
        .crypto_service(
            hub.browser
                .crypto_service
                .clone()
                .expect("crypto service required"),
        )
        .build();
    let generation = hub.webrtc.next_offer_generation(browser_identity);
    let _ = hub.webrtc.complete_offer(
        crate::worker::webrtc::WebRtcOfferCompletion {
            browser_identity: browser_identity.to_string(),
            generation,
            channel,
            encrypted_answer: Some(serde_json::json!({"type": "answer"})),
        },
        &hub.tokio_runtime,
    );

    hub.cleanup_webrtc_peer(browser_identity, "disconnected");

    assert!(
        !hub.browser_client_workers.contains_key(browser_identity),
        "WebRTC peer cleanup should remove the browser ClientWorker handle"
    );
    assert!(
        !hub.pty_forwarders.contains_key(&key),
        "WebRTC peer cleanup should remove the PTY forwarder task"
    );
}

#[test]
pub(super) fn test_terminal_attach_intent_times_out_to_not_found() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let key = "peer-timeout:sess-timeout".to_string();

    let req = test_forwarder_request("peer-timeout", "sess-timeout", "terminal_sess-timeout");
    let active_flag = Arc::clone(&req.active_flag);
    hub.create_lua_pty_forwarder(req);

    {
        let intent = hub
            .pending_terminal_attaches
            .get_mut(&key)
            .expect("pending attach intent should exist");
        intent.requested_at =
            Instant::now() - (Hub::TERMINAL_ATTACH_NOT_FOUND_TIMEOUT + Duration::from_millis(1));
    }

    hub.tick();

    assert!(
        !hub.pending_terminal_attaches.contains_key(&key),
        "stale pending attach should be removed"
    );
    assert!(
        !*active_flag
            .lock()
            .expect("Forwarder active_flag mutex poisoned"),
        "not_found transition should deactivate forwarder handle"
    );
}

#[test]
pub(super) fn test_terminal_attach_intent_replaces_previous_pending_request() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let key = "peer-replace:sess-replace".to_string();

    let req1 = test_forwarder_request("peer-replace", "sess-replace", "terminal_old");
    let req1_active = Arc::clone(&req1.active_flag);
    hub.create_lua_pty_forwarder(req1);

    let req2 = test_forwarder_request("peer-replace", "sess-replace", "terminal_new");
    let req2_active = Arc::clone(&req2.active_flag);
    hub.create_lua_pty_forwarder(req2);

    let pending = hub
        .pending_terminal_attaches
        .get(&key)
        .expect("pending attach should still exist for missing session");
    let subscription_id = match &pending.request {
        PendingTerminalAttachRequest::WebRtc(req) => req.subscription_id.as_str(),
        other => panic!("expected WebRTC pending attach, got {other:?}"),
    };
    assert_eq!(
        subscription_id, "terminal_new",
        "latest subscribe should replace previous pending attach"
    );
    assert!(
        !*req1_active
            .lock()
            .expect("Forwarder active_flag mutex poisoned"),
        "previous pending attach should be deactivated"
    );
    assert!(
        *req2_active
            .lock()
            .expect("Forwarder active_flag mutex poisoned"),
        "replacement attach should remain active"
    );
}
