use super::test_support::*;

fn drain_initial_terminal_attach_requests(
    rx: &mut tokio::sync::mpsc::Receiver<crate::worker::session_io::SessionIoRequest>,
) -> (
    crate::worker::session_io::TerminalOutputSubscription,
    crate::worker::session_io::TerminalInitialSnapshotDelivery,
) {
    let _ = recv_session_io_request_matching(rx, |request| {
        matches!(
            request,
            crate::worker::session_io::SessionIoRequest::Resize { .. }
        )
    });
    let subscription = match recv_session_io_request_matching(rx, |request| {
        matches!(
            request,
            crate::worker::session_io::SessionIoRequest::SubscribeTerminal { .. }
        )
    }) {
        crate::worker::session_io::SessionIoRequest::SubscribeTerminal { subscription } => {
            subscription
        }
        other => panic!("expected SubscribeTerminal request, got {other:?}"),
    };
    let delivery = recv_terminal_initial_snapshot_delivery(rx);
    (subscription, delivery)
}

#[test]
pub(super) fn test_tui_focus_request_updates_active_terminal_peer() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-tui-focus";

    hub.handle_tui_request(TuiRequest::FocusChanged {
        session_uuid: session_uuid.to_string(),
        focused: true,
    });

    assert_eq!(
        hub.active_terminal_peers
            .lock()
            .expect("active peers mutex")
            .get(session_uuid)
            .cloned(),
        Some("tui".to_string())
    );

    hub.handle_tui_request(TuiRequest::FocusChanged {
        session_uuid: session_uuid.to_string(),
        focused: false,
    });

    assert!(hub
        .active_terminal_peers
        .lock()
        .expect("active peers mutex")
        .get(session_uuid)
        .is_none());
}

#[test]
pub(super) fn test_tui_attach_intent_resolves_when_session_appears() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let key = "tui:sess-tui-attach".to_string();

    let req = crate::lua::primitives::TuiTerminalSubscriptionRequest {
        session_uuid: "sess-tui-attach".to_string(),
        subscription_id: "tui:sess-tui-attach".to_string(),
        rows: 24,
        cols: 80,
        active_flag: Arc::new(Mutex::new(true)),
    };
    hub.create_tui_terminal_subscription(req);

    assert!(
        hub.pending_terminal_attaches.contains_key(&key),
        "missing session should create pending TUI attach intent"
    );

    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(8);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            "sess-tui-attach",
            session_io_tx,
        ));
    hub.tick();

    assert!(
        !hub.pending_terminal_attaches.contains_key(&key),
        "pending TUI attach should clear once session exists"
    );
    assert!(
        hub.terminal_client_workers.contains_key(&key),
        "TUI subscription should register a ClientWorker after session registration"
    );
    let (subscription, _delivery) = drain_initial_terminal_attach_requests(&mut session_io_rx);
    assert_eq!(subscription.subscription_key, key);
}

#[test]
pub(super) fn test_tui_input_routes_through_client_worker_to_session_io_mailbox() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-tui-input-mailbox";
    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(8);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            session_uuid,
            session_io_tx,
        ));

    hub.create_tui_terminal_subscription(crate::lua::primitives::TuiTerminalSubscriptionRequest {
        session_uuid: session_uuid.to_string(),
        subscription_id: format!("tui:{session_uuid}"),
        rows: 24,
        cols: 80,
        active_flag: Arc::new(Mutex::new(true)),
    });
    settle_worker_subscription();

    let worker = hub
        .tui_session_input_routes
        .lock()
        .expect("tui client routes")
        .get(session_uuid)
        .cloned()
        .expect("tui session input route");
    worker
        .try_send(crate::worker::client::ClientWorkerMessage::SessionInput {
            session_uuid: session_uuid.to_string(),
            data: b"mailbox\n".to_vec(),
        })
        .expect("direct tui input enqueue");

    let mut routed = None;
    for _ in 0..20 {
        if let Ok(request) = session_io_rx.try_recv() {
            if matches!(
                request,
                crate::worker::session_io::SessionIoRequest::PtyInput { .. }
            ) {
                routed = Some(request);
                break;
            }
        }
        hub.tokio_runtime.block_on(async {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        });
    }

    assert!(matches!(
        routed,
        Some(crate::worker::session_io::SessionIoRequest::PtyInput { data })
            if data == b"mailbox\n"
    ));
}

#[test]
pub(super) fn test_tui_initial_scrollback_routes_snapshot_through_session_io_mailbox() {
    let (mut hub, _request_tx, mut output_rx) = e2e_hub();
    let session_uuid = "sess-tui-snapshot-mailbox";
    let snapshot = b"tui mailbox snapshot".to_vec();
    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(8);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            session_uuid,
            session_io_tx,
        ));

    hub.create_tui_terminal_subscription(crate::lua::primitives::TuiTerminalSubscriptionRequest {
        session_uuid: session_uuid.to_string(),
        subscription_id: format!("tui:{session_uuid}"),
        rows: 30,
        cols: 100,
        active_flag: Arc::new(Mutex::new(true)),
    });

    let (_subscription, delivery) = drain_initial_terminal_attach_requests(&mut session_io_rx);
    assert_eq!(delivery.subscription_key, format!("tui:{session_uuid}"));
    assert_eq!(delivery.rows, 30);
    assert_eq!(delivery.cols, 100);
    assert!(matches!(
        delivery.payload_mode,
        crate::worker::session_io::TerminalSnapshotPayloadMode::Raw
    ));

    settle_worker_subscription();
    deliver_terminal_initial_snapshot(delivery, snapshot.clone());

    let scrollback = shared_test_runtime()
        .block_on(async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            while tokio::time::Instant::now() < deadline {
                if let Ok(Some(TuiOutput::Scrollback {
                    session_uuid: frame_session,
                    rows,
                    cols,
                    data,
                    ..
                })) = tokio::time::timeout(Duration::from_millis(50), output_rx.recv()).await
                {
                    if frame_session == session_uuid {
                        return Some((rows, cols, data));
                    }
                }
            }
            None
        })
        .expect("TUI scrollback from SessionIo snapshot");

    assert_eq!(scrollback, (30, 100, snapshot));
}

#[test]
pub(super) fn test_tui_snapshot_mailbox_failure_emits_not_ready() {
    let (mut hub, _request_tx, mut output_rx) = e2e_hub();
    let session_uuid = "sess-tui-snapshot-mailbox-missing";
    hub.handle_cache
        .add_session(test_session_backed_handle(session_uuid, 24, 80));

    hub.create_tui_terminal_subscription(crate::lua::primitives::TuiTerminalSubscriptionRequest {
        session_uuid: session_uuid.to_string(),
        subscription_id: format!("tui:{session_uuid}"),
        rows: 24,
        cols: 80,
        active_flag: Arc::new(Mutex::new(true)),
    });

    let outputs = shared_test_runtime().block_on(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let mut outputs = Vec::new();
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(output)) =
                tokio::time::timeout(Duration::from_millis(50), output_rx.recv()).await
            {
                outputs.push(output);
                if outputs.iter().any(|output| {
                    matches!(
                        output,
                        TuiOutput::Message(json)
                            if json.get("type").and_then(|v| v.as_str()) == Some("terminal_attach")
                                && json.get("state").and_then(|v| v.as_str()) == Some("not_ready")
                                && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid)
                    )
                }) {
                    break;
                }
            }
        }
        outputs
    });

    assert!(
        outputs.iter().any(|output| matches!(
            output,
            TuiOutput::Message(json)
                if json.get("type").and_then(|v| v.as_str()) == Some("terminal_attach")
                    && json.get("state").and_then(|v| v.as_str()) == Some("not_ready")
                    && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid)
        )),
        "mailbox failure should emit a deterministic not_ready attach state"
    );
}

#[test]
pub(super) fn test_tui_attach_intent_times_out_to_not_found() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let key = "tui:sess-tui-timeout".to_string();

    let req = crate::lua::primitives::TuiTerminalSubscriptionRequest {
        session_uuid: "sess-tui-timeout".to_string(),
        subscription_id: "tui:sess-tui-timeout".to_string(),
        rows: 24,
        cols: 80,
        active_flag: Arc::new(Mutex::new(true)),
    };
    let active_flag = Arc::clone(&req.active_flag);
    hub.create_tui_terminal_subscription(req);

    {
        let intent = hub
            .pending_terminal_attaches
            .get_mut(&key)
            .expect("pending TUI attach intent should exist");
        intent.requested_at =
            Instant::now() - (Hub::TERMINAL_ATTACH_NOT_FOUND_TIMEOUT + Duration::from_millis(1));
    }

    hub.tick();

    assert!(
        !hub.pending_terminal_attaches.contains_key(&key),
        "stale pending TUI attach should be removed"
    );
    assert!(
        !*active_flag
            .lock()
            .expect("Subscription active_flag mutex poisoned"),
        "not_found transition should deactivate TUI subscription handle"
    );
}

#[test]
pub(super) fn test_socket_attach_intent_times_out_to_not_found() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let key = "socket:dead:sess-socket-timeout".to_string();

    let req = crate::lua::primitives::SocketTerminalSubscriptionRequest {
        client_id: "socket:dead".to_string(),
        session_uuid: "sess-socket-timeout".to_string(),
        subscription_id: "socket:sess-socket-timeout".to_string(),
        rows: 24,
        cols: 80,
        active_flag: Arc::new(Mutex::new(true)),
    };
    let active_flag = Arc::clone(&req.active_flag);
    hub.create_socket_terminal_subscription(req);

    assert!(
        hub.pending_terminal_attaches.contains_key(&key),
        "missing socket client/session should create pending socket attach intent"
    );

    {
        let intent = hub
            .pending_terminal_attaches
            .get_mut(&key)
            .expect("pending socket attach intent should exist");
        intent.requested_at =
            Instant::now() - (Hub::TERMINAL_ATTACH_NOT_FOUND_TIMEOUT + Duration::from_millis(1));
    }

    hub.tick();

    assert!(
        !hub.pending_terminal_attaches.contains_key(&key),
        "stale pending socket attach should be removed"
    );
    assert!(
        !*active_flag
            .lock()
            .expect("Subscription active_flag mutex poisoned"),
        "not_found transition should deactivate socket subscription handle"
    );
}

#[test]
pub(super) fn test_socket_attach_intent_resolves_when_session_and_client_appear() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let client_id = "socket:live";
    let key = format!("{client_id}:sess-socket-attach");

    let req = crate::lua::primitives::SocketTerminalSubscriptionRequest {
        client_id: client_id.to_string(),
        session_uuid: "sess-socket-attach".to_string(),
        subscription_id: "socket:sess-socket-attach".to_string(),
        rows: 24,
        cols: 80,
        active_flag: Arc::new(Mutex::new(true)),
    };
    hub.create_socket_terminal_subscription(req);

    assert!(
        hub.pending_terminal_attaches.contains_key(&key),
        "missing socket client/session should create pending socket attach intent"
    );

    let _client_stream = register_test_socket_client(&mut hub, client_id);
    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(8);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            "sess-socket-attach",
            session_io_tx,
        ));
    hub.tick();

    assert!(
        !hub.pending_terminal_attaches.contains_key(&key),
        "pending socket attach should clear once session and client exist"
    );
    assert!(
        hub.terminal_client_workers.contains_key(&key),
        "socket subscription should register a ClientWorker after prerequisites are available"
    );
    let (subscription, _delivery) = drain_initial_terminal_attach_requests(&mut session_io_rx);
    assert_eq!(subscription.subscription_key, key);
}

#[test]
pub(super) fn test_socket_initial_scrollback_routes_snapshot_through_session_io_mailbox() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-socket-snapshot-mailbox";
    let client_id = "socket:snapshot-mailbox";
    let snapshot = b"socket mailbox snapshot".to_vec();
    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(8);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            session_uuid,
            session_io_tx,
        ));
    let mut client_stream = register_test_socket_client(&mut hub, client_id);

    hub.create_socket_terminal_subscription(
        crate::lua::primitives::SocketTerminalSubscriptionRequest {
            client_id: client_id.to_string(),
            session_uuid: session_uuid.to_string(),
            subscription_id: format!("socket:{session_uuid}"),
            rows: 31,
            cols: 101,
            active_flag: Arc::new(Mutex::new(true)),
        },
    );

    let (_subscription, delivery) = drain_initial_terminal_attach_requests(&mut session_io_rx);
    assert_eq!(
        delivery.subscription_key,
        format!("{client_id}:{session_uuid}")
    );
    assert_eq!(delivery.rows, 31);
    assert_eq!(delivery.cols, 101);
    assert!(matches!(
        delivery.payload_mode,
        crate::worker::session_io::TerminalSnapshotPayloadMode::Raw
    ));

    settle_worker_subscription();
    deliver_terminal_initial_snapshot(delivery, snapshot.clone());

    let socket_scrollback =
        read_test_socket_frame_matching(&mut client_stream, Duration::from_secs(2), |frame| {
            matches!(
                frame,
                Frame::Scrollback {
                    session_uuid: frame_session,
                    rows: 31,
                    cols: 101,
                    data,
                    ..
                } if frame_session == session_uuid && data == &snapshot
            )
        });

    assert!(
        socket_scrollback.is_some(),
        "socket initial scrollback should be delivered from SessionIo snapshot response"
    );
}

#[test]
pub(super) fn test_tui_worker_handle_registered_and_removed_with_subscription() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-tui-worker-lifecycle";
    let key = format!("tui:{session_uuid}");

    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(8);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            session_uuid,
            session_io_tx,
        ));

    let req = crate::lua::primitives::TuiTerminalSubscriptionRequest {
        session_uuid: session_uuid.to_string(),
        subscription_id: format!("tui:{session_uuid}"),
        rows: 24,
        cols: 80,
        active_flag: Arc::new(Mutex::new(true)),
    };
    hub.create_tui_terminal_subscription(req);
    hub.tick();

    assert!(
        hub.terminal_client_workers.contains_key(&key),
        "TUI subscription should register a ClientWorker handle"
    );
    assert!(
        matches!(
            drain_initial_terminal_attach_requests(&mut session_io_rx).0,
            crate::worker::session_io::TerminalOutputSubscription { subscription_key, .. }
                if subscription_key == key
        ),
        "TUI terminal subscription should be registered with SessionIo"
    );

    hub.stop_terminal_subscription(&key);

    assert!(
        !hub.terminal_client_workers.contains_key(&key),
        "stopping the subscription should remove the ClientWorker handle"
    );
}

#[test]
pub(super) fn test_socket_worker_handle_registered_and_removed_with_subscription() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-socket-worker-lifecycle";
    let client_id = "socket:worker-lifecycle";
    let key = format!("{client_id}:{session_uuid}");

    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(8);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            session_uuid,
            session_io_tx,
        ));
    let _client_stream = register_test_socket_client(&mut hub, client_id);

    let req = crate::lua::primitives::SocketTerminalSubscriptionRequest {
        client_id: client_id.to_string(),
        session_uuid: session_uuid.to_string(),
        subscription_id: format!("socket:{session_uuid}"),
        rows: 24,
        cols: 80,
        active_flag: Arc::new(Mutex::new(true)),
    };
    hub.create_socket_terminal_subscription(req);
    hub.tick();

    assert!(
        hub.terminal_client_workers.contains_key(&key),
        "socket subscription should register a ClientWorker handle"
    );
    assert!(
        matches!(
            drain_initial_terminal_attach_requests(&mut session_io_rx).0,
            crate::worker::session_io::TerminalOutputSubscription { subscription_key, .. }
                if subscription_key == key
        ),
        "socket terminal subscription should be registered with SessionIo"
    );

    hub.stop_terminal_subscription(&key);

    assert!(
        !hub.terminal_client_workers.contains_key(&key),
        "stopping the socket subscription should remove the ClientWorker handle"
    );
}

#[test]
pub(super) fn test_socket_workerized_live_output_reaches_socket_frame() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-socket-worker-output".to_string();
    let client_id = "socket:worker-output".to_string();
    let key = format!("{client_id}:{session_uuid}");

    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(8);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            &session_uuid,
            session_io_tx,
        ));
    let mut client_stream = register_test_socket_client(&mut hub, &client_id);

    let req = crate::lua::primitives::SocketTerminalSubscriptionRequest {
        client_id,
        session_uuid: session_uuid.to_string(),
        subscription_id: format!("socket:{session_uuid}"),
        rows: 24,
        cols: 80,
        active_flag: Arc::new(Mutex::new(true)),
    };
    hub.create_socket_terminal_subscription(req);
    hub.tick();

    assert!(
        hub.terminal_client_workers.contains_key(&key),
        "socket subscription should register a ClientWorker handle"
    );

    let (subscription, _delivery) = drain_initial_terminal_attach_requests(&mut session_io_rx);
    subscription
        .worker
        .try_send(crate::worker::client::ClientWorkerMessage::TerminalBytes {
            session_uuid: session_uuid.clone(),
            data: b"worker-live".to_vec(),
        })
        .expect("send live terminal bytes through client worker");

    let found =
        read_test_socket_frame_matching(&mut client_stream, Duration::from_secs(5), |frame| {
            matches!(
                frame,
                Frame::PtyOutput { session_uuid: frame_session, data }
                    if frame_session == &session_uuid && data == b"worker-live"
            )
        })
        .is_some();
    assert!(
        found,
        "live socket PTY output should flow through worker egress"
    );
}

#[test]
pub(super) fn test_terminal_stream_forwards_equivalent_scrollback_to_tui_and_socket() {
    let (mut hub, _request_tx, mut output_rx) = e2e_hub();
    let session_uuid = "sess-shared-scrollback".to_string();
    let socket_client_id = "socket:shared-scrollback".to_string();
    let snapshot = b"non-empty shared scrollback snapshot".to_vec();

    let (tui_session_io_tx, mut tui_session_io_rx) = tokio::sync::mpsc::channel(8);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            &session_uuid,
            tui_session_io_tx,
        ));
    let mut client_stream = register_test_socket_client(&mut hub, &socket_client_id);

    hub.create_tui_terminal_subscription(crate::lua::primitives::TuiTerminalSubscriptionRequest {
        session_uuid: session_uuid.clone(),
        subscription_id: format!("tui:{session_uuid}"),
        rows: 24,
        cols: 80,
        active_flag: Arc::new(Mutex::new(true)),
    });
    hub.create_socket_terminal_subscription(
        crate::lua::primitives::SocketTerminalSubscriptionRequest {
            client_id: socket_client_id.clone(),
            session_uuid: session_uuid.clone(),
            subscription_id: format!("socket:{session_uuid}"),
            rows: 24,
            cols: 80,
            active_flag: Arc::new(Mutex::new(true)),
        },
    );
    let (_tui_subscription, tui_delivery) =
        drain_initial_terminal_attach_requests(&mut tui_session_io_rx);
    let (_socket_subscription, socket_delivery) =
        drain_initial_terminal_attach_requests(&mut tui_session_io_rx);
    settle_worker_subscription();
    deliver_terminal_initial_snapshot(tui_delivery, snapshot.clone());
    deliver_terminal_initial_snapshot(socket_delivery, snapshot.clone());

    let tui_scrollback = shared_test_runtime()
        .block_on(async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            while tokio::time::Instant::now() < deadline {
                if let Ok(Some(TuiOutput::Scrollback {
                    session_uuid: frame_session,
                    rows,
                    cols,
                    data,
                    kitty_enabled,
                })) = tokio::time::timeout(Duration::from_millis(50), output_rx.recv()).await
                {
                    if frame_session == session_uuid {
                        return Some((rows, cols, data, kitty_enabled));
                    }
                }
            }
            None
        })
        .expect("TUI scrollback");

    let socket_frames = read_test_socket_frames(&mut client_stream, 8, Duration::from_secs(2));
    let socket_scrollback = socket_frames
        .into_iter()
        .find_map(|frame| match frame {
            Frame::Scrollback {
                session_uuid: frame_session,
                rows,
                cols,
                kitty_enabled,
                data,
            } if frame_session == session_uuid => Some((rows, cols, data, kitty_enabled)),
            _ => None,
        })
        .expect("socket scrollback");

    assert_eq!(tui_scrollback, socket_scrollback);
    assert_eq!(
        tui_scrollback,
        (24, 80, snapshot, false),
        "both transports should receive the same non-empty snapshot metadata and payload"
    );
}

#[test]
pub(super) fn test_tui_first_scrollback_latency_budget_session_backed() {
    let (mut hub, _request_tx, mut output_rx) = e2e_hub();
    let snapshot: Vec<u8> = (0..500)
        .map(|idx| format!("scrollback line {idx:03}\r\n"))
        .collect::<String>()
        .into_bytes();
    let mut elapsed_ms = Vec::new();

    for idx in 0..40 {
        let session_uuid = unique_session_uuid(&format!("sess-tui-latency-{idx}"));
        register_live_session_identity(&session_uuid);

        let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(8);
        hub.handle_cache
            .add_session(test_session_backed_handle_with_mailbox(
                &session_uuid,
                session_io_tx,
            ));

        let started = Instant::now();
        hub.create_tui_terminal_subscription(
            crate::lua::primitives::TuiTerminalSubscriptionRequest {
                session_uuid: session_uuid.clone(),
                subscription_id: format!("tui:{session_uuid}"),
                rows: 24,
                cols: 80,
                active_flag: Arc::new(Mutex::new(true)),
            },
        );
        let (_subscription, delivery) = drain_initial_terminal_attach_requests(&mut session_io_rx);
        settle_worker_subscription();
        deliver_terminal_initial_snapshot(delivery, snapshot.clone());

        let received = shared_test_runtime().block_on(async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            while tokio::time::Instant::now() < deadline {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if let Ok(Some(TuiOutput::Scrollback {
                    session_uuid: frame_session,
                    data,
                    ..
                })) =
                    tokio::time::timeout(remaining.min(Duration::from_millis(50)), output_rx.recv())
                        .await
                {
                    if frame_session == session_uuid {
                        return Some(data);
                    }
                }
            }
            None
        });

        let elapsed = started.elapsed();
        assert_eq!(
            received.as_deref(),
            Some(snapshot.as_slice()),
            "TUI first scrollback should deliver the live session snapshot"
        );
        elapsed_ms.push(elapsed.as_secs_f64() * 1000.0);
        cleanup_live_session_identity(&session_uuid);
    }

    elapsed_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p95 = elapsed_ms[((elapsed_ms.len() as f64 * 0.95).ceil() as usize).saturating_sub(1)];
    let p99 = elapsed_ms[((elapsed_ms.len() as f64 * 0.99).ceil() as usize).saturating_sub(1)];

    assert!(
        p95 < 250.0,
        "TUI first scrollback p95 {p95:.2}ms must stay under 250ms; samples={elapsed_ms:?}"
    );
    assert!(
        p99 < 500.0,
        "TUI first scrollback p99 {p99:.2}ms must stay under 500ms; samples={elapsed_ms:?}"
    );
    eprintln!("TUI first scrollback timing p95={p95:.2}ms p99={p99:.2}ms samples={elapsed_ms:?}");
}

#[test]
pub(super) fn test_terminal_stream_forwards_live_modes_and_exit_to_tui_and_socket() {
    let (mut hub, _request_tx, mut output_rx) = e2e_hub();
    let session_uuid = "sess-shared-terminal-runtime".to_string();
    let socket_client_id = "socket:shared-runtime".to_string();

    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(16);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            &session_uuid,
            session_io_tx,
        ));
    let mut client_stream = register_test_socket_client(&mut hub, &socket_client_id);

    hub.create_tui_terminal_subscription(crate::lua::primitives::TuiTerminalSubscriptionRequest {
        session_uuid: session_uuid.clone(),
        subscription_id: format!("tui:{session_uuid}"),
        rows: 24,
        cols: 80,
        active_flag: Arc::new(Mutex::new(true)),
    });
    hub.create_socket_terminal_subscription(
        crate::lua::primitives::SocketTerminalSubscriptionRequest {
            client_id: socket_client_id.clone(),
            session_uuid: session_uuid.clone(),
            subscription_id: format!("socket:{session_uuid}"),
            rows: 24,
            cols: 80,
            active_flag: Arc::new(Mutex::new(true)),
        },
    );
    let (tui_subscription, _tui_delivery) =
        drain_initial_terminal_attach_requests(&mut session_io_rx);
    let (socket_subscription, _socket_delivery) =
        drain_initial_terminal_attach_requests(&mut session_io_rx);
    settle_worker_subscription();

    for subscription in [&tui_subscription, &socket_subscription] {
        subscription
            .worker
            .try_send(crate::worker::client::ClientWorkerMessage::TerminalBytes {
                session_uuid: session_uuid.clone(),
                data: b"one".to_vec(),
            })
            .expect("send terminal bytes");
        subscription
            .worker
            .try_send(crate::worker::client::ClientWorkerMessage::ControlFrame(
                crate::worker::client::ClientControlFrame::KittyChanged {
                    session_uuid: session_uuid.clone(),
                    enabled: true,
                },
            ))
            .expect("send kitty change");
        subscription
            .worker
            .try_send(crate::worker::client::ClientWorkerMessage::ControlFrame(
                crate::worker::client::ClientControlFrame::FocusReportingChanged {
                    session_uuid: session_uuid.clone(),
                    enabled: true,
                },
            ))
            .expect("send focus change");
        subscription
            .worker
            .try_send(crate::worker::client::ClientWorkerMessage::ControlFrame(
                crate::worker::client::ClientControlFrame::ProcessExited {
                    session_uuid: session_uuid.clone(),
                    exit_code: Some(7),
                },
            ))
            .expect("send process exit");
    }

    let tui_outputs = shared_test_runtime().block_on(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let mut outputs = Vec::new();
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(output)) =
                tokio::time::timeout(Duration::from_millis(50), output_rx.recv()).await
            {
                outputs.push(output);
                if outputs.iter().any(|output| {
                    matches!(
                        output,
                        TuiOutput::ProcessExited {
                            session_uuid: frame_session,
                            exit_code: Some(7),
                        } if frame_session == &session_uuid
                    )
                }) {
                    break;
                }
            }
        }
        outputs
    });

    assert!(
        tui_outputs.iter().any(|output| matches!(
            output,
            TuiOutput::Output { session_uuid: frame_session, data }
                if frame_session == &session_uuid && data == b"one"
        )),
        "TUI should receive first live chunk through shared runtime"
    );
    assert!(
            tui_outputs.iter().any(|output| matches!(
                output,
                TuiOutput::Message(json)
                    if json.get("type").and_then(|v| v.as_str()) == Some("kitty_changed")
                        && json.get("enabled").and_then(|v| v.as_bool()) == Some(true)
                        && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
            )),
            "TUI should receive kitty mode changes through shared runtime"
        );
    assert!(
            tui_outputs.iter().any(|output| matches!(
                output,
                TuiOutput::Message(json)
                    if json.get("type").and_then(|v| v.as_str()) == Some("focus_reporting_changed")
                        && json.get("enabled").and_then(|v| v.as_bool()) == Some(true)
                        && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
            )),
            "TUI should receive focus mode changes through shared runtime"
        );
    assert!(
        tui_outputs.iter().any(|output| matches!(
            output,
            TuiOutput::ProcessExited {
                session_uuid: frame_session,
                exit_code: Some(7),
            } if frame_session == &session_uuid
        )),
        "TUI should receive process exit through shared runtime"
    );

    let socket_frames = read_test_socket_frames(&mut client_stream, 8, Duration::from_secs(2));
    assert!(
        socket_frames.iter().any(|frame| matches!(
            frame,
            Frame::PtyOutput { session_uuid: frame_session, data }
                if frame_session == &session_uuid && data == b"one"
        )),
        "socket should receive first live chunk through shared runtime"
    );
    assert!(
            socket_frames.iter().any(|frame| matches!(
                frame,
                Frame::Json(json)
                    if json.get("type").and_then(|v| v.as_str()) == Some("kitty_changed")
                        && json.get("enabled").and_then(|v| v.as_bool()) == Some(true)
                        && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
            )),
            "socket should receive kitty mode changes through shared runtime"
        );
    assert!(
            socket_frames.iter().any(|frame| matches!(
                frame,
                Frame::Json(json)
                    if json.get("type").and_then(|v| v.as_str()) == Some("focus_reporting_changed")
                        && json.get("enabled").and_then(|v| v.as_bool()) == Some(true)
                        && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
            )),
            "socket should receive focus mode changes through shared runtime"
        );
    assert!(
        socket_frames.iter().any(|frame| matches!(
            frame,
            Frame::ProcessExited {
                session_uuid: frame_session,
                exit_code: Some(7),
            } if frame_session == &session_uuid
        )),
        "socket should receive process exit through shared runtime"
    );
}

#[test]
pub(super) fn test_terminal_stream_continues_after_sessionio_delivery_for_tui_and_socket() {
    let (mut hub, _request_tx, mut output_rx) = e2e_hub();
    let session_uuid = "sess-shared-lag".to_string();
    let socket_client_id = "socket:shared-lag".to_string();

    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(16);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            &session_uuid,
            session_io_tx,
        ));
    let mut client_stream = register_test_socket_client(&mut hub, &socket_client_id);

    hub.create_tui_terminal_subscription(crate::lua::primitives::TuiTerminalSubscriptionRequest {
        session_uuid: session_uuid.clone(),
        subscription_id: format!("tui:{session_uuid}"),
        rows: 24,
        cols: 80,
        active_flag: Arc::new(Mutex::new(true)),
    });
    hub.create_socket_terminal_subscription(
        crate::lua::primitives::SocketTerminalSubscriptionRequest {
            client_id: socket_client_id.clone(),
            session_uuid: session_uuid.clone(),
            subscription_id: format!("socket:{session_uuid}"),
            rows: 24,
            cols: 80,
            active_flag: Arc::new(Mutex::new(true)),
        },
    );
    let (tui_subscription, _tui_delivery) =
        drain_initial_terminal_attach_requests(&mut session_io_rx);
    let (socket_subscription, _socket_delivery) =
        drain_initial_terminal_attach_requests(&mut session_io_rx);
    settle_worker_subscription();
    for subscription in [&tui_subscription, &socket_subscription] {
        subscription
            .worker
            .try_send(crate::worker::client::ClientWorkerMessage::TerminalBytes {
                session_uuid: session_uuid.clone(),
                data: b"after-lag".to_vec(),
            })
            .expect("send post-lag terminal bytes");
    }

    let tui_seen = shared_test_runtime().block_on(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(TuiOutput::Output {
                session_uuid: frame_session,
                data,
            })) = tokio::time::timeout(Duration::from_millis(50), output_rx.recv()).await
            {
                if frame_session == session_uuid && data == b"after-lag" {
                    return true;
                }
            }
        }
        false
    });

    let socket_frames = read_test_socket_frames(&mut client_stream, 8, Duration::from_secs(2));
    let socket_seen = socket_frames.iter().any(|frame| {
        matches!(
            frame,
            Frame::PtyOutput {
                session_uuid: frame_session,
                data,
            } if frame_session == &session_uuid && data == b"after-lag"
        )
    });

    assert!(
        tui_seen,
        "TUI shared runtime should continue forwarding after SessionIo delivery"
    );
    assert!(
        socket_seen,
        "socket shared runtime should continue forwarding after SessionIo delivery"
    );
}

#[test]
pub(super) fn test_socket_shared_runtime_batches_outputs_but_filters_osc_queries_per_chunk() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-socket-filter-batch".to_string();
    let client_id = "socket:filter-batch".to_string();

    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(8);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            &session_uuid,
            session_io_tx,
        ));
    let mut client_stream = register_test_socket_client(&mut hub, &client_id);
    hub.active_terminal_peers
        .lock()
        .expect("active terminal peers")
        .insert(session_uuid.clone(), "socket:owner".to_string());

    hub.create_socket_terminal_subscription(
        crate::lua::primitives::SocketTerminalSubscriptionRequest {
            client_id: client_id.clone(),
            session_uuid: session_uuid.clone(),
            subscription_id: format!("socket:{session_uuid}"),
            rows: 24,
            cols: 80,
            active_flag: Arc::new(Mutex::new(true)),
        },
    );
    let (subscription, _delivery) = drain_initial_terminal_attach_requests(&mut session_io_rx);
    settle_worker_subscription();

    let mut filter_buffer = Vec::new();
    for chunk in [
        b"A\x1b]11;?".as_slice(),
        b"\x07B".as_slice(),
        b"C".as_slice(),
    ] {
        let filtered = subscription
            .filter
            .filter_chunk(&session_uuid, &mut filter_buffer, chunk);
        if filtered.is_empty() {
            continue;
        }
        subscription
            .worker
            .try_send(crate::worker::client::ClientWorkerMessage::TerminalBytes {
                session_uuid: session_uuid.clone(),
                data: filtered,
            })
            .expect("send filtered terminal bytes");
    }

    let frames = read_test_socket_frames(&mut client_stream, 8, Duration::from_secs(2));
    let mut chunks = Vec::new();
    for frame in frames {
        if let Frame::PtyOutput {
            session_uuid: frame_session,
            data,
        } = frame
        {
            if frame_session == session_uuid {
                chunks.push(data);
                if chunks.len() == 3 {
                    break;
                }
            }
        }
    }

    assert_eq!(
            chunks,
            vec![b"A".to_vec(), b"B".to_vec(), b"C".to_vec()],
            "socket filtering must preserve per-output-chunk boundaries while stripping split OSC queries"
        );
}
