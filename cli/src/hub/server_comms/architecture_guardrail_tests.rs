use super::test_support::*;

#[test]
pub(super) fn test_browser_attach_delegates_to_terminal_stream_runtime() {
    let source = include_str!("terminal_attach.rs");
    let body = function_body(source, "try_attach_browser_terminal_subscription");
    assert!(
        body.contains("start_terminal_client_subscription"),
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
        "create_tui_terminal_subscription",
        "try_attach_tui_terminal_subscription",
        "create_socket_terminal_subscription",
        "try_attach_socket_terminal_subscription",
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
        assert!(
            body.contains("start_terminal_client_subscription")
                || (function.starts_with("create_")
                    && body.contains("try_attach_")
                    && body.contains("terminal_subscription")),
            "{function} must use the shared terminal subscription runtime"
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
    let body = function_body(source, "start_session_io_terminal_subscription");
    assert!(
        body.contains("SessionIoRequest::GetInitialSnapshot"),
        "session-backed terminal initial snapshots must be delivered through SessionIoWorker"
    );
    for forbidden in [
        "resize_direct",
        ".get_snapshot()",
        "insert_pending_session_io_snapshot",
        "PendingSessionIoSnapshotTarget",
    ] {
        assert!(
            !body.contains(forbidden),
            "shared terminal runtime must not use hub-owned initial snapshot calls: {forbidden}"
        );
    }
}

#[test]
pub(super) fn test_direct_snapshot_subscription_helpers_are_not_used_for_terminal_attach() {
    for (file, source) in [
        ("terminal_attach.rs", include_str!("terminal_attach.rs")),
        ("terminal_clients.rs", include_str!("terminal_clients.rs")),
        ("terminal_snapshot.rs", include_str!("terminal_snapshot.rs")),
        ("terminal_stream.rs", include_str!("terminal_stream.rs")),
    ] {
        assert!(
            !source.contains("snapshot_and_subscribe"),
            "{file} must not contain direct snapshot subscription helpers"
        );
    }
}

#[test]
pub(super) fn test_terminal_stream_rejects_non_session_backed_data_plane() {
    let source = include_str!("terminal_stream.rs");
    let attach_body = function_body(source, "start_terminal_client_subscription");
    assert!(
        attach_body.contains("!spec.pty_handle.is_session_backed()")
            && attach_body.contains("Refusing non-session-backed terminal subscription")
            && attach_body.contains("start_session_io_terminal_subscription"),
        "session-backed terminal attach must route into SessionIoWorker subscription handling"
    );
    assert!(
        !source.contains("spawn_terminal_subscription_runtime"),
        "terminal attach must not keep a hub-owned broadcast fallback runtime"
    );
}

#[test]
pub(super) fn test_terminal_stream_dead_session_attach_classifies_process_exited() {
    let source = include_str!("terminal_stream.rs");
    let subscribe_body = function_body(source, "start_session_io_terminal_subscription");
    assert!(
        subscribe_body.contains("session_process_is_live")
            && subscribe_body.contains("terminal_attach_failure_kind")
            && subscribe_body.contains("emit_terminal_attach_failure"),
        "attach enqueue failure must classify dead vs live session process"
    );
    assert!(
        source.contains("TerminalAttachFailureKind::ProcessExited")
            && source.contains("ClientControlFrame::ProcessExited"),
        "dead sessions must emit ProcessExited rather than not_ready thrash"
    );
}

#[test]
pub(super) fn test_terminal_attach_pending_only_for_missing_sessions() {
    let attach_source = include_str!("terminal_attach.rs");
    let clients_source = include_str!("terminal_clients.rs");
    let create_browser = function_body(attach_source, "create_browser_terminal_subscription");
    let process_pending = function_body(attach_source, "process_pending_terminal_attaches");
    let create_tui = function_body(clients_source, "create_tui_terminal_subscription");
    let create_socket = function_body(clients_source, "create_socket_terminal_subscription");

    for (name, body) in [
        ("create_browser_terminal_subscription", create_browser),
        ("create_tui_terminal_subscription", create_tui),
        ("create_socket_terminal_subscription", create_socket),
    ] {
        assert!(
            body.contains("should_queue_pending_terminal_attach"),
            "{name} must gate pending attach on missing HandleCache sessions only"
        );
    }
    assert!(
        process_pending.contains("should_queue_pending_terminal_attach")
            && process_pending.contains("Dropping pending attach"),
        "process_pending must not requeue failed attaches for registered sessions"
    );
    let should_queue = function_body(attach_source, "should_queue_pending_terminal_attach");
    assert!(
        should_queue.contains("pending_reconnects"),
        "pending attach must allow SessionIo reader reconnect windows"
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
        "create_browser_terminal_subscription",
        "refresh_lua_terminal_snapshot",
        "start_session_io_terminal_subscription",
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
