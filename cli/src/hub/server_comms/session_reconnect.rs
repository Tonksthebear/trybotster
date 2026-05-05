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
                // Socket gone — session truly dead. Clean up and fire deferred exit.
                self.pending_reconnects.remove(&session_uuid);
                if let Some(session_handle) = self.handle_cache.get_session(&session_uuid) {
                    session_handle.pty().notify_process_exited(None);
                }
                let data = serde_json::json!({
                    "session_uuid": session_uuid,
                    "exit_code": null,
                });
                if let Err(e) = self.lua.fire_json_event("session_process_exited", &data) {
                    log::error!("[Session] Failed to fire deferred session_process_exited: {e}");
                }
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
                Ok((conn, mode_flags)) => {
                    log::info!(
                        "[Session] Reconnect handshake succeeded for '{}'",
                        &session_uuid[..session_uuid.len().min(16)]
                    );
                    let _ = tx.send(crate::hub::events::HubEvent::SessionReconnectReady {
                        session_uuid,
                        generation,
                        conn,
                        mode_flags,
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
