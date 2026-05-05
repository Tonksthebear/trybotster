use super::*;

impl Hub {
    pub(super) fn record_hot_span(
        &self,
        span: &'static str,
        started: Instant,
        bytes: usize,
        label: &str,
    ) {
        self.hub_event_metrics.record_span_with_threshold(
            span,
            started.elapsed(),
            bytes,
            Self::HOT_SUBHANDLER_SLOW,
            label,
        );
    }

    pub(super) fn record_volume_guardrail(
        &self,
        counter: &'static str,
        burst_counter: &'static str,
    ) {
        self.hub_event_metrics.record_counter(counter, 1);
        let Some(count) = self
            .volume_bursts
            .lock()
            .ok()
            .and_then(|mut guard| guard.record(counter, Instant::now()))
        else {
            return;
        };
        self.hub_event_metrics.record_counter(burst_counter, 1);
        log::warn!(
            "[HubEvent-Guardrail] event=volume_burst subtype={} count={} window_ms=30000",
            counter,
            count
        );
    }

    pub(super) fn format_metrics_spans(
        spans: &std::collections::BTreeMap<&'static str, crate::hub::events::HubEventSpanSnapshot>,
    ) -> String {
        spans
            .iter()
            .filter(|(_, s)| s.count > 0)
            .map(|(span, s)| {
                let avg_us = if s.count > 0 {
                    s.total_ns / s.count / 1_000
                } else {
                    0
                };
                format!(
                    "{span}:count={} avg_us={} max_us={} slow={} bytes={}",
                    s.count,
                    avg_us,
                    s.max_ns / 1_000,
                    s.slow_count,
                    s.bytes_total
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    pub(super) fn format_metrics_counters(
        counters: &std::collections::BTreeMap<&'static str, u64>,
    ) -> String {
        counters
            .iter()
            .filter(|(_, value)| **value > 0)
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub(super) fn format_metrics_slow_samples(
        samples: &[crate::hub::events::HubEventSlowSample],
    ) -> String {
        samples
            .iter()
            .map(|sample| {
                format!(
                    "{}:elapsed_us={} bytes={} label={}",
                    sample.span, sample.elapsed_us, sample.bytes, sample.label
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}
