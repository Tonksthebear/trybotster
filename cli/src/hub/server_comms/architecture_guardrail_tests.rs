use super::test_support::*;

#[test]
pub(super) fn test_browser_attach_delegates_to_terminal_stream_runtime() {
    let source = include_str!("terminal_attach.rs");
    let body = function_body(source, "try_attach_terminal_forwarder");
    assert!(
        body.contains("spawn_terminal_client_forwarder_runtime"),
        "browser attach must use the shared ClientWorker/SessionIoWorker terminal runtime"
    );
    for forbidden in ["snapshot_and_subscribe", "PtyEvent", "TerminalBytes"] {
        assert!(
            !body.contains(forbidden),
            "browser attach must not own WebRTC-specific PTY data-plane logic: {forbidden}"
        );
    }
}

#[test]
pub(super) fn test_tui_and_socket_attach_handlers_delegate_to_terminal_stream_runtime() {
    let source = include_str!("terminal_clients.rs");
    for function in [
        "create_lua_tui_pty_forwarder",
        "try_attach_tui_terminal_forwarder",
        "create_lua_socket_pty_forwarder",
        "try_attach_socket_terminal_forwarder",
    ] {
        let body = function_body(source, function);
        assert!(
            !body.contains("snapshot_and_subscribe"),
            "{function} must not own snapshot subscription logic"
        );
        assert!(
            !body.contains("PtyEvent"),
            "{function} must not own a transport-specific PTY event loop"
        );
    }
}

#[test]
pub(super) fn test_browser_snapshot_refresh_uses_session_io_mailbox() {
    let source = include_str!("terminal_snapshot.rs");
    let body = function_body(source, "refresh_lua_terminal_snapshot");
    assert!(
        body.contains("SessionIoRequest::Resize") && body.contains("SessionIoRequest::GetSnapshot"),
        "browser snapshot refresh must request resize/snapshot through SessionIoWorker"
    );
    for forbidden in ["resize_direct", ".get_snapshot()", "snapshot_and_subscribe"] {
        assert!(
            !body.contains(forbidden),
            "browser snapshot refresh must not use direct terminal data-plane calls: {forbidden}"
        );
    }
}

#[test]
pub(super) fn test_terminal_stream_session_snapshots_use_session_io_mailbox() {
    let source = include_str!("terminal_stream.rs");
    let body = function_body(source, "spawn_terminal_client_forwarder_runtime");
    assert!(
        body.contains("SessionIoRequest::GetSnapshot"),
        "session-backed TUI/socket snapshots must be requested through SessionIoWorker"
    );
    for forbidden in ["resize_direct", ".get_snapshot()"] {
        assert!(
            !body.contains(forbidden),
            "shared TUI/socket runtime must not use direct session snapshot calls: {forbidden}"
        );
    }
}

#[test]
pub(super) fn test_intentional_direct_snapshot_exceptions_are_documented() {
    let allowed = [(
        "terminal_stream.rs",
        "spawn_terminal_client_forwarder_runtime",
    )];
    let source = include_str!("terminal_stream.rs");
    let body = function_body(source, "spawn_terminal_client_forwarder_runtime");

    assert_eq!(
        body.matches("snapshot_and_subscribe").count(),
        1,
        "the only shared-runtime direct snapshot helper is the documented non-session-backed fallback"
    );
    assert_eq!(
        allowed,
        [(
            "terminal_stream.rs",
            "spawn_terminal_client_forwarder_runtime"
        )],
        "update the documented exception list before adding any direct snapshot helper"
    );

    for (file, source) in [
        ("terminal_attach.rs", include_str!("terminal_attach.rs")),
        ("terminal_clients.rs", include_str!("terminal_clients.rs")),
        ("terminal_snapshot.rs", include_str!("terminal_snapshot.rs")),
    ] {
        assert!(
            !source.contains("snapshot_and_subscribe"),
            "{file} must not contain direct snapshot subscription helpers"
        );
    }
}

#[test]
pub(super) fn test_shared_runtime_snapshot_and_subscribe_is_non_session_backed_only() {
    let source = include_str!("terminal_stream.rs");
    let body = function_body(source, "spawn_terminal_client_forwarder_runtime");

    let session_branch = body
        .find("if let Some(request_id) = snapshot_request_id")
        .expect("shared runtime should branch on session-backed snapshot_request_id");
    let direct_snapshot = body
        .find("snapshot_and_subscribe")
        .expect("documented non-session-backed direct snapshot helper should exist");
    let non_session_branch = body[session_branch..direct_snapshot]
        .find("} else {")
        .map(|offset| session_branch + offset)
        .expect("shared runtime should isolate non-session-backed fallback in else branch");

    assert!(
        direct_snapshot > non_session_branch,
        "snapshot_and_subscribe must stay in the non-session-backed else branch"
    );
    let session_body = &body[session_branch..non_session_branch];
    assert!(
        session_body.contains("SessionIoRequest::Resize")
            && session_body.contains("SessionIoRequest::GetSnapshot"),
        "session-backed shared runtime must queue resize/snapshot through SessionIoWorker"
    );
}

#[test]
pub(super) fn test_webrtc_recovery_session_backed_snapshot_uses_session_io_mailbox() {
    let source = include_str!("terminal_snapshot.rs");
    let body = function_body(source, "dispatch_webrtc_recovery_snapshot_requests");
    let session_branch = body
        .find("if pty_handle.is_session_backed()")
        .expect("recovery dispatch should branch on session-backed PTY handles");
    let fallback_idx = body[session_branch..]
        .find("// Snapshot via RPC")
        .map(|offset| session_branch + offset)
        .expect("session-backed recovery branch should precede non-session fallback");
    let session_body = &body[session_branch..fallback_idx];

    assert!(
        session_body.contains("SessionIoRequest::GetSnapshot"),
        "session-backed WebRTC recovery snapshots must request data through SessionIoWorker"
    );
    assert!(
        !session_body.contains(".get_snapshot()"),
        "session-backed WebRTC recovery snapshots must not read directly from the PTY handle"
    );
}

#[test]
pub(super) fn test_terminal_attach_snapshot_paths_have_no_fixed_sleep_settle_windows() {
    let source = concat!(
        include_str!("terminal_attach.rs"),
        include_str!("terminal_snapshot.rs"),
        include_str!("terminal_stream.rs")
    );
    for function in [
        "create_lua_pty_forwarder",
        "refresh_lua_terminal_snapshot",
        "spawn_terminal_client_forwarder_runtime",
    ] {
        let body = function_body(source, function);
        assert!(
            !body.contains("thread::sleep"),
            "{function} must not use fixed sleep windows on the first-attach snapshot path"
        );
        assert!(
            !body.contains("from_millis(125)"),
            "{function} must not reintroduce the former 125ms attach delay"
        );
    }
}

#[test]
pub(super) fn test_server_comms_dispatcher_does_not_own_terminal_hot_paths() {
    let source = include_str!("../server_comms.rs");
    let body = function_body(source, "handle_hub_event");
    for forbidden in [
        "write_input_direct",
        "snapshot_and_subscribe",
        "WebRtcAdapterCommand::Pty",
    ] {
        assert!(
            !source.contains(forbidden),
            "server_comms.rs dispatcher must not contain hot-path terminal code: {forbidden}"
        );
    }
    for forbidden in [
        "spawn_blocking",
        "std::process::Command",
        "serde_json::json!",
        "enqueue_session_io_request",
        "pending_reconnects",
        "terminal_client_workers",
        "WorktreeManager::new",
    ] {
        assert!(
            !body.contains(forbidden),
            "handle_hub_event must stay thin and delegate domain logic; found {forbidden}"
        );
    }
    let body_lines = body.lines().count();
    assert!(
        body_lines <= 310,
        "handle_hub_event should stay visibly thin; saw {body_lines} lines"
    );
}
