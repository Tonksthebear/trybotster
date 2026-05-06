use super::test_support::*;

#[test]
pub(super) fn test_pty_osc_cursor_volume_burst_guardrail_matches_observed_logs() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();

    for i in 0..=crate::hub::VolumeBurstState::THRESHOLD {
        hub.handle_hub_event(crate::hub::events::HubEvent::PtyOscEvent {
            session_uuid: "sess-osc-replay".to_string(),
            session_name: "test-agent".to_string(),
            event: crate::agent::pty::PtyEvent::cursor_visibility_changed(i % 2 == 0),
        });
    }

    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["pty_osc.cursor"], 1001);
    assert_eq!(snapshot.counters["pty_osc.volume_burst"], 1);
}
