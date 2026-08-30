use super::*;

impl Hub {
    pub(super) fn handle_webrtc_message_event(
        &mut self,
        browser_identity: String,
        payload: Vec<u8>,
    ) {
        let started = Instant::now();
        if !self.webrtc.is_authenticated(&browser_identity) {
            self.handle_browser_authentication(&browser_identity, &payload);
            return;
        }
        self.process_webrtc_plaintext_payload(&browser_identity, &payload);
        self.record_hot_span(
            "webrtc_message.total",
            started,
            payload.len(),
            &browser_identity,
        );
    }

    fn handle_browser_authentication(&mut self, browser_identity: &str, payload: &[u8]) {
        let request = match serde_json::from_slice::<crate::relay::BrowserAuthRequest>(payload) {
            Ok(request) => request,
            Err(error) => {
                log::warn!("[WebRTC-Auth] Invalid authentication request: {error}");
                self.cleanup_webrtc_peer(browser_identity, "invalid_authentication_request");
                return;
            }
        };
        let admission = self
            .browser
            .pairing_authority
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("pairing authority is unavailable"))
            .and_then(|authority| authority.verify(browser_identity, request));
        let admission = match admission {
            Ok(admission) => admission,
            Err(error) => {
                log::warn!("[WebRTC-Auth] Browser admission rejected: {error}");
                self.cleanup_webrtc_peer(browser_identity, "browser_authentication_rejected");
                return;
            }
        };
        if !self.webrtc.mark_authenticated(browser_identity) {
            return;
        }
        let response = serde_json::to_vec(&admission.response)
            .expect("authentication response serialization cannot fail");
        self.queue_webrtc_peer_command(
            browser_identity,
            crate::worker::webrtc::WebRtcAdapterCommand::Json { data: response },
        );
        self.browser.handle_connected(&admission.credential_id);
        self.complete_authenticated_webrtc_peer(browser_identity);
    }

    pub(super) fn handle_dc_opened_event(&mut self, browser_identity: String, generation: u64) {
        let opened_started = Instant::now();
        let current_generation = self.webrtc.current_offer_generation(&browser_identity);
        let Some(_peer_state) = self
            .webrtc
            .mark_data_channel_open(&browser_identity, generation)
        else {
            if current_generation != generation {
                self.hub_event_metrics
                    .record_counter("webrtc_open.stale_generation", 1);
            } else {
                self.hub_event_metrics
                    .record_counter("webrtc_open.unknown_peer", 1);
            }
            self.hub_event_metrics.record_span_with_threshold(
                "webrtc_open.total",
                opened_started.elapsed(),
                0,
                Self::HOT_SUBHANDLER_SLOW,
                &browser_identity,
            );
            log::warn!(
                "[WebRTC] DcOpened for peer {} generation {} ignored (current generation {})",
                &browser_identity[..browser_identity.len().min(8)],
                generation,
                current_generation
            );
            return;
        };
        log::info!(
            "[WebRTC] DataChannel opened for {}, starting authentication",
            &browser_identity[..browser_identity.len().min(8)],
        );

        if self.webrtc.start_recv_forwarder(
            &browser_identity,
            &self.tokio_runtime,
            self.hub_event_tx.clone(),
        ) {
            self.spawn_webrtc_peer_sender(&browser_identity);
            let Some(authority) = self.browser.pairing_authority.as_mut() else {
                self.cleanup_webrtc_peer(&browser_identity, "pairing_authority_unavailable");
                return;
            };
            let Some(dtls_binding) = self.webrtc.dtls_binding(&browser_identity) else {
                self.cleanup_webrtc_peer(&browser_identity, "dtls_binding_unavailable");
                return;
            };
            let challenge = authority.begin_challenge(&browser_identity, dtls_binding);
            let data = serde_json::to_vec(&challenge)
                .expect("authentication challenge serialization cannot fail");
            self.queue_webrtc_peer_command(
                &browser_identity,
                crate::worker::webrtc::WebRtcAdapterCommand::Json { data },
            );
        } else {
            self.hub_event_metrics
                .record_counter("webrtc_open.recv_forwarder_failed", 1);
        }
        self.hub_event_metrics.record_span_with_threshold(
            "webrtc_open.total",
            opened_started.elapsed(),
            0,
            Self::HOT_SUBHANDLER_SLOW,
            &browser_identity,
        );
    }

    fn complete_authenticated_webrtc_peer(&mut self, browser_identity: &str) {
        let ready_path_started = Instant::now();
        let Some(peer_command_tx) = self.webrtc.peer_command_sender(browser_identity) else {
            self.cleanup_webrtc_peer(browser_identity, "peer_sender_missing_after_auth");
            return;
        };
        self.queue_webrtc_peer_command(
            browser_identity,
            crate::worker::webrtc::WebRtcAdapterCommand::Json {
                data: serde_json::to_vec(&serde_json::json!({ "type": "dc_ready" }))
                    .expect("static JSON serialization cannot fail"),
            },
        );
        let worker = self
            .spawn_webrtc_client_worker_adapter(browser_identity.to_string(), peer_command_tx);
        self.webrtc
            .register_client_worker_route(browser_identity.to_string(), worker.clone());
        self.browser_client_workers
            .insert(browser_identity.to_string(), worker);
        self.spawn_dc_ping_task(browser_identity);
        let generation = self.webrtc.current_offer_generation(browser_identity);
        self.handle_transport_control_message(
            crate::worker::hub_control::HubControlMessage::TransportPeerStateChanged {
                client_id: crate::client::ClientId::browser(browser_identity.to_string()),
                browser_identity: browser_identity.to_string(),
                state: crate::worker::hub_control::TransportPeerState::Connected {
                    generation,
                    mode: crate::worker::hub_control::TransportConnectionMode::Unknown,
                },
            },
        );
        self.hub_event_metrics.record_span_with_threshold(
            "webrtc_auth.ready_path",
            ready_path_started.elapsed(),
            0,
            Self::HOT_SUBHANDLER_SLOW,
            browser_identity,
        );
        if let Err(error) = self.lua.call_peer_connected(browser_identity) {
            log::warn!("[WebRTC] Lua peer_connected callback error: {error}");
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
                    move |gen, candidate_str, sdp_mid, sdp_mline_index, e| {
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
                self.hub_event_metrics
                    .record_counter("webrtc_offer.start_failed", 1);
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
