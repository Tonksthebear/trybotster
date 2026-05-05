use super::test_support::*;

#[test]
pub(super) fn test_noisy_session_io_replay_keeps_hot_handler_latency_bounded() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-noisy-replay";
    hub.handle_cache
        .add_session(test_local_session_handle(session_uuid));

    let mut elapsed_samples = Vec::with_capacity(1001);
    let mut max_elapsed = std::time::Duration::ZERO;
    for i in 0..=1000 {
        let data = format!("\x1b]2;botster replay {i}\x07payload-{i:04}\r\n").into_bytes();
        let event = crate::hub::events::HubEvent::SessionIoBatch(
            crate::worker::session_io::SessionIoBatch {
                session_uuid: session_uuid.to_string(),
                output: Some(data),
            },
        );
        let started = Instant::now();
        hub.handle_hub_event(event);
        let elapsed = started.elapsed();
        max_elapsed = max_elapsed.max(elapsed);
        elapsed_samples.push(elapsed);
        hub.hub_event_metrics
            .record_handler_time("session_io_batch", elapsed);
    }

    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["pty_output.messages"], 1001);
    assert!(snapshot.counters["pty_output.bytes"] > 32_000);
    let session_io = snapshot
        .by_type
        .get("session_io_batch")
        .expect("session_io_batch handler metrics");
    assert_eq!(
        session_io.handler_time_max_ns,
        max_elapsed.as_nanos() as u64
    );
    elapsed_samples.sort_unstable();
    let p99_elapsed = elapsed_samples[elapsed_samples.len() * 99 / 100];
    let slow_samples = elapsed_samples
        .iter()
        .filter(|elapsed| **elapsed >= Hub::HOT_SUBHANDLER_SLOW)
        .count();
    assert!(
            p99_elapsed < Hub::HOT_SUBHANDLER_SLOW,
            "observed-log-shaped SessionIoBatch replay p99 exceeded hot-path budget: p99={p99_elapsed:?}, max={max_elapsed:?}, slow_samples={slow_samples}"
        );
    assert!(snapshot.slow_samples.is_empty());
}

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
