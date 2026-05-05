use super::*;

impl Hub {
    pub(super) fn handle_cleanup_tick(&mut self) {
        self.repair_missing_socket_path();
        self.cleanup_webrtc_peer_registry();
        self.cleanup_stale_session_io_snapshots();
        self.poll_stream_frames_outgoing();
        self.dispatch_webrtc_recovery_snapshot_requests();
        self.webrtc.clear_ratchet_restart_dedupe();
        self.log_hub_event_metrics_if_due();
        self.retry_pending_session_reconnects();
    }

    fn log_hub_event_metrics_if_due(&mut self) {
        if self.hub_event_metrics_last_log.elapsed() < Duration::from_secs(30) {
            return;
        }

        let m = self.hub_event_metrics.snapshot();
        let by_type = m
            .by_type
            .iter()
            .filter(|(_, s)| s.enqueue_ok > 0 || s.pending > 0)
            .map(|(kind, s)| {
                let avg_us = if s.dequeue > 0 {
                    s.handler_time_total_ns / s.dequeue / 1_000
                } else {
                    0
                };
                let max_us = s.handler_time_max_ns / 1_000;
                format!(
                    "{kind}:ok={} fail={} deq={} pend={} hwm={} bytes={} bytes_hwm={} avg_us={} max_us={}",
                    s.enqueue_ok,
                    s.enqueue_failed,
                    s.dequeue,
                    s.pending,
                    s.pending_high_water,
                    s.bytes_pending,
                    s.bytes_high_water,
                    avg_us,
                    max_us
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let avg_us = if m.dequeue_total > 0 {
            m.handler_time_total_ns / m.dequeue_total / 1_000
        } else {
            0
        };
        let max_us = m.handler_time_max_ns / 1_000;
        let spans = Self::format_metrics_spans(&m.spans);
        let counters = Self::format_metrics_counters(&m.counters);
        let slow_samples = Self::format_metrics_slow_samples(&m.slow_samples);
        log::info!(
            "[HubEventMetrics] enqueue_ok={} dequeue={} failed={} pending={} pending_hwm={} bytes_pending={} bytes_hwm={} avg_us={} max_us={} by_type=[{}] counters=[{}] spans=[{}] slow_samples=[{}]",
            m.enqueue_ok_total,
            m.dequeue_total,
            m.enqueue_failed_total,
            m.pending_total,
            m.pending_high_water_total,
            m.bytes_pending_total,
            m.bytes_high_water_total,
            avg_us,
            max_us,
            by_type,
            counters,
            spans,
            slow_samples
        );
        self.hub_event_metrics_last_log = Instant::now();
    }

    fn retry_pending_session_reconnects(&mut self) {
        if self.pending_reconnects.is_empty() {
            return;
        }

        self.hub_event_metrics
            .record_high_water("reconnect.pending", self.pending_reconnects.len() as u64);
        let now = Instant::now();
        let reconnect_deadline = Duration::from_secs(110);
        let in_flight_timeout = Duration::from_secs(10);

        let mut expired = Vec::new();
        let mut retryable = Vec::new();

        for (uuid, state) in &mut self.pending_reconnects {
            if now.duration_since(state.started_at) > reconnect_deadline {
                expired.push(uuid.clone());
            } else if state.in_flight
                && state
                    .attempt_started_at
                    .is_some_and(|t| now.duration_since(t) > in_flight_timeout)
            {
                state.in_flight = false;
                state.attempt_started_at = None;
                retryable.push((uuid.clone(), state.generation));
            } else if !state.in_flight {
                retryable.push((uuid.clone(), state.generation));
            }
        }

        for uuid in expired {
            log::warn!(
                "[Session] Reconnect expired for '{}'",
                &uuid[..uuid.len().min(16)]
            );
            self.hub_event_metrics
                .record_counter("reconnect.expired", 1);
            self.pending_reconnects.remove(&uuid);
            if let Some(sh) = self.handle_cache.get_session(&uuid) {
                sh.pty().notify_process_exited(None);
            }
            let data = serde_json::json!({
                "session_uuid": uuid,
                "exit_code": null,
            });
            let _ = self.lua.fire_json_event("session_process_exited", &data);
        }

        for (uuid, generation) in retryable {
            self.spawn_session_reconnect(uuid, generation);
            self.hub_event_metrics.record_counter("reconnect.retry", 1);
        }
    }

    pub(super) fn handle_pty_osc_event(
        &mut self,
        session_uuid: String,
        session_name: String,
        event: crate::agent::pty::PtyEvent,
    ) {
        let counter = match &event {
            crate::agent::pty::PtyEvent::TitleChanged(_) => "pty_osc.title",
            crate::agent::pty::PtyEvent::CwdChanged(_) => "pty_osc.cwd",
            crate::agent::pty::PtyEvent::PromptMark(_) => "pty_osc.prompt",
            crate::agent::pty::PtyEvent::CursorVisibilityChanged(_) => "pty_osc.cursor",
            _ => "pty_osc.other",
        };
        self.record_volume_guardrail(counter, "pty_osc.volume_burst");
        self.lua
            .notify_pty_osc_event(&session_uuid, &session_name, &event);
    }

    pub(super) fn handle_pty_process_exited(
        &mut self,
        session_uuid: String,
        session_name: String,
        exit_code: Option<i32>,
    ) {
        log::info!(
            "[Hub] PTY process exited for {}:{} (code={:?})",
            session_uuid,
            session_name,
            exit_code
        );
        let data = serde_json::json!({
            "session_uuid": session_uuid,
            "session_name": session_name,
            "exit_code": exit_code,
        });
        if let Err(e) = self.lua.fire_json_event("process_exited", &data) {
            log::error!("Failed to fire process_exited event: {e}");
        }
    }
}
