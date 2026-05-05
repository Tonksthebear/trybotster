use super::test_support::*;

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

    let req = crate::lua::primitives::CreateTuiForwarderRequest {
        session_uuid: "sess-tui-attach".to_string(),
        subscription_id: "tui:sess-tui-attach".to_string(),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    };
    hub.create_lua_tui_pty_forwarder(req);

    assert!(
        hub.pending_terminal_attaches.contains_key(&key),
        "missing session should create pending TUI attach intent"
    );
    assert!(
        !hub.pty_forwarders.contains_key(&key),
        "TUI forwarder should not start until session is registered"
    );

    hub.handle_cache
        .add_session(test_session_handle("sess-tui-attach"));
    hub.tick();

    assert!(
        !hub.pending_terminal_attaches.contains_key(&key),
        "pending TUI attach should clear once session exists"
    );
    assert!(
        hub.pty_forwarders.contains_key(&key),
        "TUI forwarder should start after session registration"
    );
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

    hub.create_lua_tui_pty_forwarder(crate::lua::primitives::CreateTuiForwarderRequest {
        session_uuid: session_uuid.to_string(),
        subscription_id: format!("tui:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    });
    settle_worker_subscription();

    hub.handle_tui_request(crate::client::TuiRequest::PtyInput {
        session_uuid: session_uuid.to_string(),
        data: b"mailbox\n".to_vec(),
    });

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

    hub.create_lua_tui_pty_forwarder(crate::lua::primitives::CreateTuiForwarderRequest {
        session_uuid: session_uuid.to_string(),
        subscription_id: format!("tui:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 30,
        cols: 100,
    });

    assert!(matches!(
        recv_session_io_request_matching(&mut session_io_rx, |request| matches!(
            request,
            crate::worker::session_io::SessionIoRequest::Resize {
                rows: 30,
                cols: 100
            }
        )),
        crate::worker::session_io::SessionIoRequest::Resize { .. }
    ));
    let request_id = match recv_session_io_request_matching(&mut session_io_rx, |request| {
        matches!(
            request,
            crate::worker::session_io::SessionIoRequest::GetSnapshot { .. }
        )
    }) {
        crate::worker::session_io::SessionIoRequest::GetSnapshot { request_id } => request_id,
        other => panic!("expected GetSnapshot request, got {other:?}"),
    };

    settle_worker_subscription();
    hub.handle_session_io_event(crate::worker::session_io::SessionIoEvent::Snapshot {
        request_id,
        session_uuid: session_uuid.to_string(),
        payload: snapshot.clone(),
    });

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

    hub.create_lua_tui_pty_forwarder(crate::lua::primitives::CreateTuiForwarderRequest {
        session_uuid: session_uuid.to_string(),
        subscription_id: format!("tui:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
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

    let req = crate::lua::primitives::CreateTuiForwarderRequest {
        session_uuid: "sess-tui-timeout".to_string(),
        subscription_id: "tui:sess-tui-timeout".to_string(),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    };
    let active_flag = Arc::clone(&req.active_flag);
    hub.create_lua_tui_pty_forwarder(req);

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
            .expect("Forwarder active_flag mutex poisoned"),
        "not_found transition should deactivate TUI forwarder handle"
    );
}

#[test]
pub(super) fn test_socket_attach_intent_times_out_to_not_found() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let key = "socket:dead:sess-socket-timeout".to_string();

    let req = crate::lua::primitives::CreateSocketForwarderRequest {
        client_id: "socket:dead".to_string(),
        session_uuid: "sess-socket-timeout".to_string(),
        subscription_id: "socket:sess-socket-timeout".to_string(),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    };
    let active_flag = Arc::clone(&req.active_flag);
    hub.create_lua_socket_pty_forwarder(req);

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
            .expect("Forwarder active_flag mutex poisoned"),
        "not_found transition should deactivate socket forwarder handle"
    );
}

#[test]
pub(super) fn test_socket_attach_intent_resolves_when_session_and_client_appear() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let client_id = "socket:live";
    let key = format!("{client_id}:sess-socket-attach");

    let req = crate::lua::primitives::CreateSocketForwarderRequest {
        client_id: client_id.to_string(),
        session_uuid: "sess-socket-attach".to_string(),
        subscription_id: "socket:sess-socket-attach".to_string(),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    };
    hub.create_lua_socket_pty_forwarder(req);

    assert!(
        hub.pending_terminal_attaches.contains_key(&key),
        "missing socket client/session should create pending socket attach intent"
    );
    assert!(
        !hub.pty_forwarders.contains_key(&key),
        "socket forwarder should not start until session and client are ready"
    );

    let _client_stream = register_test_socket_client(&mut hub, client_id);
    hub.handle_cache
        .add_session(test_session_handle("sess-socket-attach"));
    hub.tick();

    assert!(
        !hub.pending_terminal_attaches.contains_key(&key),
        "pending socket attach should clear once session and client exist"
    );
    assert!(
        hub.pty_forwarders.contains_key(&key),
        "socket forwarder should start after prerequisites are available"
    );
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

    hub.create_lua_socket_pty_forwarder(crate::lua::primitives::CreateSocketForwarderRequest {
        client_id: client_id.to_string(),
        session_uuid: session_uuid.to_string(),
        subscription_id: format!("socket:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 31,
        cols: 101,
    });

    let _ = recv_session_io_request_matching(&mut session_io_rx, |request| {
        matches!(
            request,
            crate::worker::session_io::SessionIoRequest::Resize {
                rows: 31,
                cols: 101
            }
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

    settle_worker_subscription();
    hub.handle_session_io_event(crate::worker::session_io::SessionIoEvent::Snapshot {
        request_id,
        session_uuid: session_uuid.to_string(),
        payload: snapshot.clone(),
    });

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
pub(super) fn test_tui_worker_handle_registered_and_removed_with_forwarder() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-tui-worker-lifecycle";
    let key = format!("tui:{session_uuid}");

    hub.handle_cache
        .add_session(test_session_handle(session_uuid));

    let req = crate::lua::primitives::CreateTuiForwarderRequest {
        session_uuid: session_uuid.to_string(),
        subscription_id: format!("tui:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    };
    hub.create_lua_tui_pty_forwarder(req);
    hub.tick();

    assert!(
        hub.terminal_client_workers.contains_key(&key),
        "TUI forwarder should register a ClientWorker handle"
    );
    assert!(
        hub.pty_forwarders.contains_key(&key),
        "TUI forwarder task should be tracked by the hub"
    );

    hub.stop_lua_pty_forwarder(&key);

    assert!(
        !hub.terminal_client_workers.contains_key(&key),
        "stopping the forwarder should remove the ClientWorker handle"
    );
    assert!(
        !hub.pty_forwarders.contains_key(&key),
        "stopping the forwarder should remove the task"
    );
}

#[test]
pub(super) fn test_socket_worker_handle_registered_and_removed_with_forwarder() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-socket-worker-lifecycle";
    let client_id = "socket:worker-lifecycle";
    let key = format!("{client_id}:{session_uuid}");

    hub.handle_cache
        .add_session(test_session_handle(session_uuid));
    let _client_stream = register_test_socket_client(&mut hub, client_id);

    let req = crate::lua::primitives::CreateSocketForwarderRequest {
        client_id: client_id.to_string(),
        session_uuid: session_uuid.to_string(),
        subscription_id: format!("socket:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    };
    hub.create_lua_socket_pty_forwarder(req);
    hub.tick();

    assert!(
        hub.terminal_client_workers.contains_key(&key),
        "socket forwarder should register a ClientWorker handle"
    );
    assert!(
        hub.pty_forwarders.contains_key(&key),
        "socket forwarder task should be tracked by the hub"
    );

    hub.stop_lua_pty_forwarder(&key);

    assert!(
        !hub.terminal_client_workers.contains_key(&key),
        "stopping the socket forwarder should remove the ClientWorker handle"
    );
    assert!(
        !hub.pty_forwarders.contains_key(&key),
        "stopping the socket forwarder should remove the task"
    );
}

#[test]
pub(super) fn test_socket_workerized_live_output_reaches_socket_frame() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-socket-worker-output".to_string();
    let client_id = "socket:worker-output".to_string();
    let key = format!("{client_id}:{session_uuid}");

    let session = test_session_handle(&session_uuid);
    let event_tx = session.pty().event_tx_clone();
    hub.handle_cache.add_session(session);
    let mut client_stream = register_test_socket_client(&mut hub, &client_id);

    let req = crate::lua::primitives::CreateSocketForwarderRequest {
        client_id,
        session_uuid: session_uuid.to_string(),
        subscription_id: format!("socket:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    };
    hub.create_lua_socket_pty_forwarder(req);
    hub.tick();

    assert!(
        hub.terminal_client_workers.contains_key(&key),
        "socket forwarder should register a ClientWorker handle"
    );

    let subscribed = hub.tokio_runtime.block_on(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if event_tx.receiver_count() > 0 {
                break true;
            }
            if tokio::time::Instant::now() >= deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    assert!(
        subscribed,
        "socket forwarder should subscribe to PTY output before test emits live bytes"
    );

    let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"worker-live".to_vec()));

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
pub(super) fn test_shared_terminal_runtime_forwards_equivalent_scrollback_to_tui_and_socket() {
    let (mut hub, _request_tx, mut output_rx) = e2e_hub();
    let session_uuid = "sess-shared-scrollback".to_string();
    let socket_client_id = "socket:shared-scrollback".to_string();
    let snapshot = b"non-empty shared scrollback snapshot".to_vec();

    hub.handle_cache
        .add_session(test_session_handle_with_snapshot(&session_uuid, &snapshot));
    let mut client_stream = register_test_socket_client(&mut hub, &socket_client_id);

    hub.create_lua_tui_pty_forwarder(crate::lua::primitives::CreateTuiForwarderRequest {
        session_uuid: session_uuid.clone(),
        subscription_id: format!("tui:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    });
    hub.create_lua_socket_pty_forwarder(crate::lua::primitives::CreateSocketForwarderRequest {
        client_id: socket_client_id.clone(),
        session_uuid: session_uuid.clone(),
        subscription_id: format!("socket:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    });

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
        hub.create_lua_tui_pty_forwarder(crate::lua::primitives::CreateTuiForwarderRequest {
            session_uuid: session_uuid.clone(),
            subscription_id: format!("tui:{session_uuid}"),
            active_flag: Arc::new(Mutex::new(true)),
            rows: 24,
            cols: 80,
        });
        let _ = recv_session_io_request_matching(&mut session_io_rx, |request| {
            matches!(
                request,
                crate::worker::session_io::SessionIoRequest::Resize { .. }
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
        settle_worker_subscription();
        hub.handle_session_io_event(crate::worker::session_io::SessionIoEvent::Snapshot {
            request_id,
            session_uuid: session_uuid.clone(),
            payload: snapshot.clone(),
        });

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
pub(super) fn test_shared_terminal_runtime_forwards_live_modes_and_exit_to_tui_and_socket() {
    let (mut hub, _request_tx, mut output_rx) = e2e_hub();
    let session_uuid = "sess-shared-terminal-runtime".to_string();
    let socket_client_id = "socket:shared-runtime".to_string();

    let session = test_session_handle(&session_uuid);
    let event_tx = session.pty().event_tx_clone();
    hub.handle_cache.add_session(session);
    let mut client_stream = register_test_socket_client(&mut hub, &socket_client_id);

    hub.create_lua_tui_pty_forwarder(crate::lua::primitives::CreateTuiForwarderRequest {
        session_uuid: session_uuid.clone(),
        subscription_id: format!("tui:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    });
    hub.create_lua_socket_pty_forwarder(crate::lua::primitives::CreateSocketForwarderRequest {
        client_id: socket_client_id.clone(),
        session_uuid: session_uuid.clone(),
        subscription_id: format!("socket:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    });
    wait_for_receiver_count(&event_tx, 2);
    settle_worker_subscription();

    let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"one".to_vec()));
    let _ = event_tx.send(crate::agent::pty::PtyEvent::kitty_changed(true));
    let _ = event_tx.send(crate::agent::pty::PtyEvent::focus_reporting_changed(true));
    let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"two".to_vec()));
    let _ = event_tx.send(crate::agent::pty::PtyEvent::process_exited(Some(7)));

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
pub(super) fn test_shared_terminal_runtime_continues_after_broadcast_lag_for_tui_and_socket() {
    let (mut hub, _request_tx, mut output_rx) = e2e_hub();
    let session_uuid = "sess-shared-lag".to_string();
    let socket_client_id = "socket:shared-lag".to_string();

    let session = test_session_handle_with_broadcast_capacity(&session_uuid, 1);
    let event_tx = session.pty().event_tx_clone();
    hub.handle_cache.add_session(session);
    let mut client_stream = register_test_socket_client(&mut hub, &socket_client_id);

    hub.create_lua_tui_pty_forwarder(crate::lua::primitives::CreateTuiForwarderRequest {
        session_uuid: session_uuid.clone(),
        subscription_id: format!("tui:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    });
    hub.create_lua_socket_pty_forwarder(crate::lua::primitives::CreateSocketForwarderRequest {
        client_id: socket_client_id.clone(),
        session_uuid: session_uuid.clone(),
        subscription_id: format!("socket:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    });
    wait_for_receiver_count(&event_tx, 2);

    for i in 0..128 {
        let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(
            format!("dropped-{i}").into_bytes(),
        ));
    }
    settle_worker_subscription();
    let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"after-lag".to_vec()));

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
        "TUI shared runtime should continue forwarding after a broadcast lag"
    );
    assert!(
        socket_seen,
        "socket shared runtime should continue forwarding after a broadcast lag"
    );
}

#[test]
pub(super) fn test_socket_shared_runtime_batches_outputs_but_filters_osc_queries_per_chunk() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-socket-filter-batch".to_string();
    let client_id = "socket:filter-batch".to_string();

    let session = test_session_handle(&session_uuid);
    let event_tx = session.pty().event_tx_clone();
    hub.handle_cache.add_session(session);
    let mut client_stream = register_test_socket_client(&mut hub, &client_id);
    hub.active_terminal_peers
        .lock()
        .expect("active terminal peers")
        .insert(session_uuid.clone(), "socket:owner".to_string());

    hub.create_lua_socket_pty_forwarder(crate::lua::primitives::CreateSocketForwarderRequest {
        client_id: client_id.clone(),
        session_uuid: session_uuid.clone(),
        subscription_id: format!("socket:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    });
    wait_for_receiver_count(&event_tx, 1);
    settle_worker_subscription();

    let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"A\x1b]11;?".to_vec()));
    let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"\x07B".to_vec()));
    let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"C".to_vec()));

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
