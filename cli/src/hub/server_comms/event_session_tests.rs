use super::test_support::*;

#[test]
pub(super) fn test_session_unregistered_clears_terminal_profile_state() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-clear-profile";

    hub.terminal_profiles
        .observe_output(session_uuid, b"\x1b]11;?\x07");

    hub.handle_hub_event(crate::hub::events::HubEvent::SessionUnregistered {
        session_uuid: session_uuid.to_string(),
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
