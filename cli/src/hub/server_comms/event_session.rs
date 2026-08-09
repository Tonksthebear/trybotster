use super::*;

impl Hub {
    pub(super) fn handle_session_process_exited_event(
        &mut self,
        session_uuid: String,
        exit_code: Option<i32>,
    ) {
        log::info!(
            "[Session] ProcessExited uuid='{}' exit={:?}",
            session_uuid,
            exit_code
        );

        if exit_code.is_none() {
            if let Some(session_handle) = self.handle_cache.get_session(&session_uuid) {
                let pty = session_handle.pty();
                if pty.is_session_backed() {
                    let cleared = pty.clear_session_connection();
                    log::info!(
                        "[Session] Reader died for '{}', cleared old connection={}, initiating reconnect",
                        &session_uuid[..session_uuid.len().min(16)],
                        cleared
                    );

                    self.reconnect_generation += 1;
                    let generation = self.reconnect_generation;
                    self.pending_reconnects.insert(
                        session_uuid.clone(),
                        crate::hub::ReconnectState {
                            started_at: Instant::now(),
                            attempt_started_at: None,
                            generation,
                            in_flight: false,
                        },
                    );

                    self.hub_event_metrics.record_counter("reconnect.retry", 1);
                    self.spawn_session_reconnect(session_uuid, generation);
                    return;
                }
            }
        }

        self.cleanup_pending_session_io_snapshots_for_session(&session_uuid);
        self.cleanup_paste_files(&session_uuid);
        if let Some(session_handle) = self.handle_cache.get_session(&session_uuid) {
            session_handle.pty().notify_process_exited(exit_code);
        }
        let data = serde_json::json!({
            "session_uuid": session_uuid,
            "exit_code": exit_code,
        });
        if let Err(e) = self.lua.fire_json_event("session_process_exited", &data) {
            log::error!("[Session] Failed to fire session_process_exited event: {e}");
        }
    }

    pub(super) fn handle_session_reconnect_ready_event(
        &mut self,
        session_uuid: String,
        generation: u64,
        mut conn: crate::session::connection::SessionConnection,
        metadata: crate::session::protocol::SessionMetadata,
    ) {
        let valid = self
            .pending_reconnects
            .get(&session_uuid)
            .is_some_and(|s| s.generation == generation);

        if !valid {
            log::info!(
                "[Session] Dropping stale reconnect for '{}' gen={}",
                &session_uuid[..session_uuid.len().min(16)],
                generation
            );
            self.hub_event_metrics
                .record_counter("reconnect.stale_generation", 1);
            drop(conn);
            return;
        }

        let Some(session_handle) = self.handle_cache.get_session(&session_uuid) else {
            log::warn!(
                "[Session] Session '{}' disappeared during reconnect",
                &session_uuid[..session_uuid.len().min(16)]
            );
            self.hub_event_metrics.record_counter("reconnect.failed", 1);
            self.pending_reconnects.remove(&session_uuid);
            return;
        };
        let pty = session_handle.pty();

        if let Err(e) = conn.install_reader(
            session_uuid.clone(),
            pty.event_tx_clone(),
            pty.kitty_enabled_arc(),
            pty.cursor_visible_arc(),
            pty.resize_pending_arc(),
            Arc::clone(pty.last_output_at_atomic()),
            pty.last_human_input_atomic(),
            self.hub_event_tx.clone(),
        ) {
            log::error!(
                "[Session] Failed to install reader after reconnect for '{}': {e}",
                &session_uuid[..session_uuid.len().min(16)]
            );
            self.hub_event_metrics.record_counter("reconnect.failed", 1);
            self.pending_reconnects.remove(&session_uuid);
            pty.notify_process_exited(None);
            let data = serde_json::json!({
                "session_uuid": session_uuid,
                "exit_code": null,
            });
            let _ = self.lua.fire_json_event("session_process_exited", &data);
            return;
        }

        if let Some(shared) = pty.shared_session_connection() {
            if let Ok(mut guard) = shared.lock() {
                *guard = Some(conn);
            }
        }

        pty.kitty_enabled_arc().store(
            metadata.mode_flags.kitty_enabled,
            std::sync::atomic::Ordering::Relaxed,
        );
        pty.cursor_visible_arc().store(
            metadata.mode_flags.cursor_visible,
            std::sync::atomic::Ordering::Relaxed,
        );

        self.pending_reconnects.remove(&session_uuid);
        self.hub_event_metrics.record_counter("reconnect.ready", 1);

        log::info!(
            "[Session] Reconnected to '{}' successfully",
            &session_uuid[..session_uuid.len().min(16)]
        );

        let data = serde_json::json!({
            "session_uuid": session_uuid,
            "title": metadata.title,
            "cwd": metadata.cwd,
        });
        if let Err(e) = self.lua.fire_json_event("session_reconnected", &data) {
            log::error!("[Session] Failed to fire session_reconnected: {e}");
        }

        // Resume any client attaches that waited out the reader reconnect.
        self.process_pending_terminal_attaches();
    }

    pub(super) fn handle_session_unregistered_event(&mut self, session_uuid: String) {
        self.cleanup_pending_session_io_snapshots_for_session(&session_uuid);
        self.cleanup_paste_files(&session_uuid);
        self.terminal_profiles.clear_session(&session_uuid);
        self.terminal_session_peers.remove(&session_uuid);
        self.terminal_subscription_peers
            .retain(|_, (tracked_session, _)| tracked_session != &session_uuid);
        let suffix = format!(":{session_uuid}");
        let worker_keys: Vec<String> = self
            .terminal_client_workers
            .keys()
            .filter(|key| key.ends_with(&suffix))
            .cloned()
            .collect();
        for key in worker_keys {
            self.remove_terminal_client_worker(&key, &session_uuid, "Session");
        }
        for worker in self.browser_client_workers.values() {
            Self::unregister_worker_session_io_sender(worker, &session_uuid, "Session");
        }
        let suffix = format!(":{session_uuid}");
        self.browser_terminal_attach_sizes
            .retain(|key, _| !key.ends_with(&suffix));
        if let Ok(mut active) = self.active_terminal_peers.lock() {
            active.remove(&session_uuid);
        }

        log::debug!("[Session] Unregistered '{}'", session_uuid);
    }
}
