use super::test_support::*;

#[test]
pub(super) fn test_lua_write_and_resize_pty_are_session_io_data_plane() {
    let source = include_str!("event_socket_terminal.rs");
    let body = function_body(source, "handle_lua_pty_request_event");
    for request in ["WritePty", "ResizePty"] {
        let start = body
            .find(request)
            .unwrap_or_else(|| panic!("missing {request} arm"));
        let excerpt = &body[start..body.len().min(start + 500)];
        assert!(
            excerpt.contains("enqueue_session_io_request"),
            "{request} must route through SessionIoRequest"
        );
        assert!(
            !excerpt.contains("write_input_direct") && !excerpt.contains("resize_direct"),
            "{request} must not use direct hub PTY data-plane calls"
        );
    }
}
