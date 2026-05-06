use super::test_support::*;

#[test]
pub(super) fn test_multiple_live_clients_do_not_update_terminal_profile_cache() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-multi-client";

    hub.register_terminal_subscription_peer(&format!("tui:{session_uuid}"), session_uuid, "tui");
    hub.register_terminal_subscription_peer(
        &format!("browser-a:{session_uuid}"),
        session_uuid,
        "browser-a",
    );

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

#[test]
pub(super) fn test_terminal_color_profile_reports_merge_for_same_peer() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();

    let mut first = std::collections::HashMap::new();
    first.insert(7usize, crate::terminal::Rgb::new(1, 2, 3));

    let mut second = std::collections::HashMap::new();
    second.insert(257usize, crate::terminal::Rgb::new(17, 34, 51));

    hub.update_terminal_client_profile("browser-a", first);
    hub.update_terminal_client_profile("browser-a", second);

    let colors = hub
        .terminal_client_profiles
        .get("browser-a")
        .expect("profile cached");
    assert_eq!(
        colors.get(&7usize).copied(),
        Some(crate::terminal::Rgb::new(1, 2, 3))
    );
    assert_eq!(
        colors.get(&257usize).copied(),
        Some(crate::terminal::Rgb::new(17, 34, 51))
    );
}

#[test]
pub(super) fn test_effective_terminal_colors_overlay_active_peer_on_boot_cache() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-overlay-profile";

    hub.shared_color_cache
        .lock()
        .expect("shared cache")
        .insert(7usize, crate::terminal::Rgb::new(1, 2, 3));

    let mut browser = std::collections::HashMap::new();
    browser.insert(257usize, crate::terminal::Rgb::new(17, 34, 51));

    hub.update_terminal_client_profile("browser-a", browser);
    hub.set_active_terminal_peer(session_uuid, "browser-a", true);

    let colors = hub.effective_terminal_colors(session_uuid);
    assert_eq!(
        colors.get(&7usize).copied(),
        Some(crate::terminal::Rgb::new(1, 2, 3))
    );
    assert_eq!(
        colors.get(&257usize).copied(),
        Some(crate::terminal::Rgb::new(17, 34, 51))
    );
}
