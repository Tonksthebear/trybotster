use super::*;

impl Hub {
    pub(super) fn spawn_session_reconnect(&mut self, session_uuid: String, generation: u64) {
        // Mark in-flight with current timestamp
        if let Some(state) = self.pending_reconnects.get_mut(&session_uuid) {
            state.in_flight = true;
            state.attempt_started_at = Some(Instant::now());
        }

        // Check socket exists before spawning (cheap sync check)
        let socket_path = match crate::session::session_socket_path(&session_uuid) {
            Ok(p) if crate::session::session_process_is_live(&session_uuid) => p,
            _ => {
                log::warn!(
                    "[Session] Live session transport gone for '{}', aborting reconnect",
                    &session_uuid[..session_uuid.len().min(16)]
                );
                // Socket/process gone — session truly dead. Reap if tracked and
                // fire deferred permanent exit with any OS status we can get.
                let mut exit_code = None;
                let mut signal = None;
                if let Some(status) = self
                    .handle_cache
                    .try_reap_session_process(&session_uuid)
                    .or_else(|| self.handle_cache.wait_reap_session_process(&session_uuid))
                {
                    log::info!(
                        "[Session] Reaped session process on reconnect abort: {}",
                        status.summary()
                    );
                    exit_code = status.effective_exit_code();
                    signal = status.signal;
                }
                let progress = crate::session::read_session_progress(&session_uuid);
                let vt_artifacts = crate::session::summarize_vt_crash_artifacts(&session_uuid);
                Self::log_vt_crash_artifacts(&session_uuid, signal, &vt_artifacts);
                self.finalize_session_process_exit(session_uuid, exit_code, signal, progress);
                return;
            }
        };

        let tx = self.hub_event_tx.clone();
        tokio::task::spawn_blocking(move || {
            log::info!(
                "[Session] Reconnect attempt for '{}' gen={}",
                &session_uuid[..session_uuid.len().min(16)],
                generation
            );
            match crate::session::connection::SessionConnection::connect_and_seed(&socket_path) {
                Ok((conn, metadata)) => {
                    log::info!(
                        "[Session] Reconnect handshake succeeded for '{}'",
                        &session_uuid[..session_uuid.len().min(16)]
                    );
                    let _ = tx.send(crate::hub::events::HubEvent::SessionReconnectReady {
                        session_uuid,
                        generation,
                        conn,
                        metadata,
                    });
                }
                Err(e) => {
                    log::warn!(
                        "[Session] Reconnect failed for '{}': {e}",
                        &session_uuid[..session_uuid.len().min(16)]
                    );
                    // Don't send anything — CleanupTick will detect that
                    // in_flight has been true for >10s and reset it for retry.
                }
            }
        });
    }
}
