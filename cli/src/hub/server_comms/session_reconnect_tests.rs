use super::test_support::*;

#[test]
pub(super) fn test_tui_attach_reconnecting_does_not_fake_attached_state() {
    let session_uuid = unique_session_uuid("sess-tui-reconnecting");
    register_live_session_identity(&session_uuid);

    let (mut hub, _request_tx, mut output_rx) = e2e_hub();
    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(8);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            &session_uuid,
            session_io_tx,
        ));

    let req = crate::lua::primitives::TuiTerminalSubscriptionRequest {
        session_uuid: session_uuid.clone(),
        subscription_id: format!("tui:{session_uuid}"),
        rows: 24,
        cols: 80,
        active_flag: Arc::new(Mutex::new(true)),
    };
    hub.create_tui_terminal_subscription(req);
    let _ = recv_session_io_request_matching(&mut session_io_rx, |request| {
        matches!(
            request,
            crate::worker::session_io::SessionIoRequest::Resize { .. }
        )
    });
    let delivery = recv_terminal_initial_snapshot_delivery(&mut session_io_rx);
    settle_worker_subscription();
    deliver_terminal_attach_state(
        delivery,
        crate::worker::client::TerminalAttachState::Reconnecting,
    );

    let rt = shared_test_runtime();
    let outputs = rt.block_on(async {
        let mut outputs = Vec::new();
        if let Some(output) = tokio::time::timeout(Duration::from_secs(2), output_rx.recv())
            .await
            .expect("timed out waiting for TUI output")
        {
            outputs.push(output);
        }
        outputs
    });

    assert!(
        outputs.iter().any(|output| matches!(
            output,
            TuiOutput::Message(json)
                if json.get("type").and_then(|v| v.as_str()) == Some("terminal_attach")
                    && json.get("state").and_then(|v| v.as_str()) == Some("reconnecting")
                    && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
        )),
        "reconnect-pending attach should emit explicit reconnecting state"
    );
    assert!(
        !outputs.iter().any(|output| matches!(
            output,
            TuiOutput::Message(json)
                if json.get("type").and_then(|v| v.as_str()) == Some("terminal_attach")
                    && json.get("state").and_then(|v| v.as_str()) == Some("attached")
                    && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
        )),
        "reconnect-pending attach must not fake attached state"
    );
    assert!(
        !outputs
            .iter()
            .any(|output| matches!(output, TuiOutput::Scrollback { data, .. } if data.is_empty())),
        "reconnect-pending attach must not fake an empty scrollback"
    );

    cleanup_live_session_identity(&session_uuid);
}

#[test]
pub(super) fn test_socket_attach_reconnecting_does_not_fake_attached_state() {
    let session_uuid = unique_session_uuid("sess-socket-reconnecting");
    register_live_session_identity(&session_uuid);

    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let client_id = "socket:reconnecting";
    let mut client_stream = register_test_socket_client(&mut hub, client_id);
    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(8);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            &session_uuid,
            session_io_tx,
        ));

    let req = crate::lua::primitives::SocketTerminalSubscriptionRequest {
        client_id: client_id.to_string(),
        session_uuid: session_uuid.clone(),
        subscription_id: format!("socket:{session_uuid}"),
        rows: 24,
        cols: 80,
        active_flag: Arc::new(Mutex::new(true)),
    };
    hub.create_socket_terminal_subscription(req);
    let _ = recv_session_io_request_matching(&mut session_io_rx, |request| {
        matches!(
            request,
            crate::worker::session_io::SessionIoRequest::Resize { .. }
        )
    });
    let delivery = recv_terminal_initial_snapshot_delivery(&mut session_io_rx);
    settle_worker_subscription();
    deliver_terminal_attach_state(
        delivery,
        crate::worker::client::TerminalAttachState::Reconnecting,
    );

    let frames = read_test_socket_frames(&mut client_stream, 1, Duration::from_secs(5));

    assert!(
        frames.iter().any(|frame| matches!(
            frame,
            Frame::Json(value)
                if value.get("type").and_then(|v| v.as_str()) == Some("terminal_attach")
                    && value.get("state").and_then(|v| v.as_str()) == Some("reconnecting")
                    && value.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
        )),
        "reconnect-pending socket attach should emit explicit reconnecting state"
    );
    assert!(
        !frames.iter().any(|frame| matches!(
            frame,
            Frame::Json(value)
                if value.get("type").and_then(|v| v.as_str()) == Some("terminal_attach")
                    && value.get("state").and_then(|v| v.as_str()) == Some("attached")
                    && value.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
        )),
        "reconnect-pending socket attach must not fake attached state"
    );
    assert!(
        !frames
            .iter()
            .any(|frame| matches!(frame, Frame::Scrollback { data, .. } if data.is_empty())),
        "reconnect-pending socket attach must not fake an empty scrollback"
    );

    cleanup_live_session_identity(&session_uuid);
}
