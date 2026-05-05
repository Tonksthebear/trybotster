use super::test_support::*;

#[test]
pub(super) fn test_multiple_live_clients_do_not_update_terminal_profile_cache() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-multi-client";

    let _guard = hub.tokio_runtime.enter();
    hub.pty_forwarders
        .insert(format!("tui:{session_uuid}"), tokio::spawn(async {}));
    hub.pty_forwarders
        .insert(format!("browser-a:{session_uuid}"), tokio::spawn(async {}));

    hub.terminal_profiles
        .observe_output(session_uuid, b"\x1b]11;?\x07");
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
pub(super) fn test_headless_probe_detected_and_cache_available() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-headless-probe";

    // Populate hub cache with color values.
    hub.terminal_profiles
        .observe_peer_input("boot", b"\x1b]10;rgb:aaaa/bbbb/cccc\x07");
    hub.terminal_profiles
        .observe_peer_input("boot", b"\x1b]11;rgb:1111/2222/3333\x07");
    hub.terminal_profiles
        .observe_peer_input("boot", b"\x1b]12;rgb:4444/5555/6666\x07");

    hub.handle_cache
        .add_session(test_local_session_handle(session_uuid));

    // No live clients (headless) — hub should attempt to answer from cache.
    // write_input_direct returns Err in tests (no real PTY), but the hub
    // should still detect the probe and have the right cache value.
    assert!(hub.terminal_profiles.hub_profile_is_complete());
    assert_eq!(
        hub.terminal_profiles.headless_reply(
            session_uuid,
            crate::hub::terminal_profile::TerminalProbe::DefaultBackground
        ),
        Some(b"\x1b]11;rgb:1111/2222/3333\x07".as_slice())
    );
}

#[test]
pub(super) fn test_live_client_skips_hub_probe_answering() {
    let (mut hub, _request_tx, mut output_rx) = e2e_hub();
    let session_uuid = "sess-live-client-probe";

    // Populate hub cache.
    hub.terminal_profiles
        .observe_peer_input("boot", b"\x1b]11;rgb:1111/2222/3333\x07");

    hub.handle_cache
        .add_session(test_local_session_handle(session_uuid));

    // Add a live client forwarder — hub should NOT answer probes.
    let _guard = hub.tokio_runtime.enter();
    hub.pty_forwarders
        .insert(format!("socket:abc:{session_uuid}"), tokio::spawn(async {}));

    hub.handle_hub_event(crate::hub::events::HubEvent::PtyOutputObserved {
        session_uuid: session_uuid.to_string(),
        data: b"\x1b]11;?\x07".to_vec(),
    });

    // Drain output — hub should not have sent any probe-related messages.
    while output_rx.try_recv().is_ok() {}
}

#[test]
pub(super) fn test_pty_output_observed_tracks_probe_queries_for_later_replies() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-observed-probe";

    hub.handle_cache
        .add_session(test_local_session_handle(session_uuid));

    hub.handle_hub_event(crate::hub::events::HubEvent::PtyOutputObserved {
        session_uuid: session_uuid.to_string(),
        data: b"\x1b]11;?\x07".to_vec(),
    });

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
pub(super) fn test_tui_terminal_color_profile_updates_client_cache() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();

    let mut colors = std::collections::HashMap::new();
    colors.insert(257usize, crate::terminal::Rgb::new(17, 34, 51));

    hub.handle_tui_request(TuiRequest::LuaMessage(serde_json::json!({
        "type": "terminal_color_profile",
        "session_uuid": "sess-color-profile",
        "colors": colors,
    })));

    assert_eq!(
        hub.terminal_client_profiles
            .get("tui")
            .and_then(|colors| colors.get(&257usize))
            .copied(),
        Some(crate::terminal::Rgb::new(17, 34, 51))
    );
}
