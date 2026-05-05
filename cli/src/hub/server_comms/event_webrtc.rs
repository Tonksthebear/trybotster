use super::*;

impl Hub {
    pub(super) fn handle_webrtc_message_event(
        &mut self,
        browser_identity: String,
        payload: Vec<u8>,
    ) {
        let started = Instant::now();
        self.process_webrtc_plaintext_payload(&browser_identity, &payload);
        self.record_hot_span(
            "webrtc_message.total",
            started,
            payload.len(),
            &browser_identity,
        );
        for restart_peer in self.webrtc.drain_decrypt_failure_triggers() {
            self.request_transport_ratchet_restart(&restart_peer);
        }
    }

    pub(super) fn handle_dc_opened_event(&mut self, browser_identity: String) {
        let generation = self.webrtc.current_offer_generation(&browser_identity);
        let Some(peer_state) = self
            .webrtc
            .mark_data_channel_open(&browser_identity, generation)
        else {
            log::warn!(
                "[WebRTC] DcOpened for unknown peer {}, ignoring stale open event",
                &browser_identity[..browser_identity.len().min(8)]
            );
            return;
        };
        self.handle_transport_control_message(peer_state);
        log::info!(
            "[WebRTC] DataChannel opened for {}, firing peer_connected",
            &browser_identity[..browser_identity.len().min(8)],
        );

        if self.webrtc.start_recv_forwarder(
            &browser_identity,
            &self.tokio_runtime,
            self.hub_event_tx.clone(),
        ) {
            self.spawn_webrtc_peer_sender(&browser_identity);
            self.queue_webrtc_peer_command(
                &browser_identity,
                crate::worker::webrtc::WebRtcAdapterCommand::Json {
                    data: serde_json::to_vec(&serde_json::json!({
                        "type": "dc_ready",
                    }))
                    .expect("static JSON serialization cannot fail"),
                },
            );
            let worker = self.spawn_webrtc_client_worker_adapter(browser_identity.clone());
            self.browser_client_workers
                .insert(browser_identity.clone(), worker);

            self.spawn_dc_ping_task(&browser_identity);
            if let Err(e) = self.lua.call_peer_connected(&browser_identity) {
                log::warn!("[WebRTC] Lua peer_connected callback error: {e}");
            }
        }
    }

    pub(super) fn handle_webrtc_ingress_backpressure_event(
        &mut self,
        browser_identity: String,
        source: &'static str,
    ) {
        log::warn!(
            "[WebRTC] Ingress backpressure from {} for {}; cleaning up peer",
            source,
            &browser_identity[..browser_identity.len().min(8)]
        );
        self.cleanup_webrtc_peer(&browser_identity, source);
    }

    pub(super) fn handle_webrtc_send_event(
        &mut self,
        send_req: crate::lua::primitives::WebRtcSendRequest,
    ) {
        use crate::lua::primitives::WebRtcSendRequest;

        match send_req {
            WebRtcSendRequest::Json { peer_id, data } => {
                let payload = match serde_json::to_vec(&data) {
                    Ok(p) => p,
                    Err(e) => {
                        log::warn!("[WebRTC] Lua send failed to serialize: {e}");
                        return;
                    }
                };
                self.queue_webrtc_peer_command(
                    &peer_id,
                    crate::worker::webrtc::WebRtcAdapterCommand::Json { data: payload },
                );
            }
            WebRtcSendRequest::Binary { peer_id, data } => {
                self.queue_webrtc_peer_command(
                    &peer_id,
                    crate::worker::webrtc::WebRtcAdapterCommand::Binary { data },
                );
            }
        }
    }

    pub(super) fn handle_webrtc_offer_negotiated_event(
        &mut self,
        completion: crate::worker::webrtc::WebRtcOfferCompletion,
    ) {
        match self.webrtc.complete_offer(completion, &self.tokio_runtime) {
            crate::worker::webrtc::WebRtcOfferCompletionOutcome::AnswerReady {
                browser_identity,
                generation,
                envelope,
                queued_ice,
            } => {
                self.handle_transport_control_message(
                    crate::worker::hub_control::HubControlMessage::TransportSignalReady {
                        client_id: crate::client::ClientId::browser(browser_identity.clone()),
                        signal: crate::worker::hub_control::TransportSignal::Answer {
                            browser_identity: browser_identity.clone(),
                            envelope,
                        },
                    },
                );

                let browser_id_short =
                    browser_identity[..browser_identity.len().min(8)].to_string();
                self.webrtc.apply_queued_ice_for_offer(
                    &browser_identity,
                    generation,
                    queued_ice,
                    &self.tokio_runtime,
                    |gen, candidate_str, sdp_mid, sdp_mline_index, e| {
                        log::warn!(
                            "[WebRTC] Failed to apply queued ICE candidate for {}: {} (gen={}, mid={:?}, mline={:?}, candidate='{}')",
                            browser_id_short,
                            e,
                            gen,
                            sdp_mid,
                            sdp_mline_index,
                            Self::ice_candidate_preview(candidate_str),
                        );
                    },
                );
            }
            crate::worker::webrtc::WebRtcOfferCompletionOutcome::StaleDropped {
                browser_identity,
                completed_generation,
                current_generation,
            } => {
                log::info!(
                    "[WebRTC] Discarding stale offer completion for {} (got gen {}, current gen {})",
                    &browser_identity[..browser_identity.len().min(8)],
                    completed_generation,
                    current_generation
                );
            }
            crate::worker::webrtc::WebRtcOfferCompletionOutcome::FailedCleaned {
                browser_identity,
                generation,
            } => {
                log::warn!(
                    "[WebRTC] Offer handling failed for {} at generation {} — registry discarded channel so the next retry can start cleanly",
                    &browser_identity[..browser_identity.len().min(8)],
                    generation
                );
            }
        }
    }

    pub(super) fn handle_webrtc_recovery_snapshot_ready(
        &mut self,
        request: crate::worker::webrtc::WebRtcRecoverySnapshotRequest,
        result: crate::worker::webrtc::WebRtcRecoverySnapshotResult,
    ) {
        let _ = self
            .webrtc
            .complete_recovery_snapshot(request, result, &self.hub_event_metrics);
    }
}
