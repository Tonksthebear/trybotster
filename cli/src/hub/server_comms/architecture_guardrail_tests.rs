use super::test_support::*;

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
