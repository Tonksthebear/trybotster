use super::*;
use crate::worker::session_io::{TerminalAttachDeliveryFailureReason, TerminalAttachDeliveryPhase};

impl Hub {
    /// Remove paste files tracked for a closed session.
    pub fn cleanup_paste_files(&mut self, session_uuid: &str) {
        if let Some(files) = self.paste_files.remove(session_uuid) {
            for path in &files {
                if let Err(e) = std::fs::remove_file(path) {
                    log::warn!(
                        "[FILE-INPUT] Failed to clean up paste file {}: {e}",
                        path.display()
                    );
                }
            }
            if !files.is_empty() {
                log::info!(
                    "[FILE-INPUT] Cleaned up {} paste file(s) for {session_uuid}",
                    files.len()
                );
            }
        }
    }

    pub(super) fn handle_session_io_event(
        &mut self,
        event: crate::worker::session_io::SessionIoEvent,
    ) {
        use crate::worker::session_io::SessionIoEvent;

        match event {
            SessionIoEvent::PasteFileWritten {
                session_uuid,
                path,
                bytes,
                ..
            } => {
                log::info!(
                    "[FILE-INPUT] Wrote {} bytes to {} (session={})",
                    bytes,
                    path.display(),
                    session_uuid,
                );
                self.paste_files.entry(session_uuid).or_default().push(path);
            }
            SessionIoEvent::PasteFileFailed {
                session_uuid,
                reason,
                detail,
                ..
            } => {
                log::error!(
                    "[FILE-INPUT] Paste failed for session {} reason={reason:?}: {detail}",
                    session_uuid
                );
            }
            SessionIoEvent::PreparedSnapshot {
                request_id,
                session_uuid,
                uncompressed_len,
                payload,
                recovery,
            } => {
                self.route_prepared_session_io_snapshot(
                    request_id,
                    session_uuid,
                    uncompressed_len,
                    payload,
                    recovery,
                );
            }
            SessionIoEvent::TerminalAttachTiming(timing) => {
                self.record_terminal_attach_timing(timing);
            }
            SessionIoEvent::TerminalAttachDeliveryFailed { phase, reason, .. } => {
                self.record_terminal_attach_delivery_failure(phase, reason);
            }
            SessionIoEvent::Snapshot {
                request_id,
                session_uuid,
                payload,
            } => {
                self.route_terminal_client_initial_snapshot(request_id, session_uuid, payload);
            }
            _ => {}
        }
    }

    fn record_terminal_attach_timing(
        &self,
        timing: crate::worker::session_io::TerminalAttachTiming,
    ) {
        const TERMINAL_ATTACH_SLOW: std::time::Duration = std::time::Duration::from_millis(100);

        let label = timing.subscription_key.as_str();
        if let Some(elapsed) = timing.attach_to_client_worker_subscribed {
            self.hub_event_metrics.record_span_with_threshold(
                "terminal_attach.client_worker_subscribe",
                elapsed,
                0,
                TERMINAL_ATTACH_SLOW,
                label,
            );
        }
        if let Some(elapsed) = timing.attach_to_session_io_queued {
            self.hub_event_metrics.record_span_with_threshold(
                "terminal_attach.session_io_queue",
                elapsed,
                0,
                TERMINAL_ATTACH_SLOW,
                label,
            );
        }
        if let Some(elapsed) = timing.attach_to_session_io_accepted {
            self.hub_event_metrics.record_span_with_threshold(
                "terminal_attach.session_io_accept",
                elapsed,
                0,
                TERMINAL_ATTACH_SLOW,
                label,
            );
        }
        if let Some(elapsed) = timing.attach_to_snapshot_ready {
            self.hub_event_metrics.record_span_with_threshold(
                "terminal_attach.snapshot_ready",
                elapsed,
                timing.snapshot_bytes,
                TERMINAL_ATTACH_SLOW,
                label,
            );
        }
        self.hub_event_metrics.record_span_with_threshold(
            "terminal_attach.client_worker_accept",
            timing.snapshot_ready_to_client_worker_accepted,
            timing.snapshot_bytes,
            TERMINAL_ATTACH_SLOW,
            label,
        );
        if let Some(elapsed) = timing.attach_to_client_worker_accepted {
            self.hub_event_metrics.record_span_with_threshold(
                "terminal_attach.total_to_client_worker",
                elapsed,
                timing.snapshot_bytes,
                TERMINAL_ATTACH_SLOW,
                label,
            );
        }
    }

    fn record_terminal_attach_delivery_failure(
        &self,
        phase: TerminalAttachDeliveryPhase,
        reason: TerminalAttachDeliveryFailureReason,
    ) {
        self.hub_event_metrics
            .record_counter("terminal_attach.delivery_failed", 1);
        self.hub_event_metrics.record_counter(
            match phase {
                TerminalAttachDeliveryPhase::Snapshot => {
                    "terminal_attach.delivery_failed.phase.snapshot"
                }
                TerminalAttachDeliveryPhase::SubscribeAck => {
                    "terminal_attach.delivery_failed.phase.subscribe_ack"
                }
                TerminalAttachDeliveryPhase::AttachState => {
                    "terminal_attach.delivery_failed.phase.attach_state"
                }
            },
            1,
        );
        self.hub_event_metrics.record_counter(
            match reason {
                TerminalAttachDeliveryFailureReason::QueueFull => {
                    "terminal_attach.delivery_failed.reason.queue_full"
                }
                TerminalAttachDeliveryFailureReason::QueueClosed => {
                    "terminal_attach.delivery_failed.reason.queue_closed"
                }
            },
            1,
        );
    }
}
