use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SnapshotAttachState {
    Ready,
    Reconnecting,
    Exited,
}

impl Hub {
    pub(super) fn classify_snapshot_attach_state(
        pty_handle: &crate::hub::agent_handle::PtyHandle,
        session_uuid: &str,
        snapshot: &[u8],
    ) -> SnapshotAttachState {
        if !snapshot.is_empty() || !pty_handle.is_session_backed() {
            return SnapshotAttachState::Ready;
        }

        if crate::session::session_process_is_live(session_uuid) {
            SnapshotAttachState::Reconnecting
        } else {
            SnapshotAttachState::Exited
        }
    }
    pub(super) fn cleanup_pending_session_io_snapshots_for_session(&mut self, session_uuid: &str) {
        self.pending_session_io_snapshots
            .retain(|_, pending| pending.session_uuid != session_uuid);
    }

    pub(super) fn cleanup_pending_session_io_snapshots_for_peer(&mut self, peer_id: &str) {
        self.pending_session_io_snapshots
            .retain(|_, pending| match &pending.target {
                crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                    peer_id: owner, ..
                } => owner != peer_id,
                crate::hub::PendingSessionIoSnapshotTarget::WebRtcPeerRecovery { request } => {
                    request.browser_identity != peer_id
                }
                crate::hub::PendingSessionIoSnapshotTarget::TerminalClientInitial { .. } => true,
            });
    }

    pub(super) fn cleanup_pending_session_io_snapshots_for_forwarder(
        &mut self,
        forwarder_id: &str,
    ) {
        self.pending_session_io_snapshots
            .retain(|_, pending| match &pending.target {
                crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                    forwarder_key, ..
                } => forwarder_key.as_deref() != Some(forwarder_id),
                crate::hub::PendingSessionIoSnapshotTarget::WebRtcPeerRecovery { .. } => true,
                crate::hub::PendingSessionIoSnapshotTarget::TerminalClientInitial {
                    forwarder_key,
                    ..
                } => forwarder_key != forwarder_id,
            });
    }

    pub(super) fn next_session_io_request_id(prefix: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{prefix}-{nanos}")
    }
    pub(super) fn insert_pending_session_io_snapshot(
        &mut self,
        request_id: String,
        pending: crate::hub::PendingSessionIoSnapshot,
    ) -> bool {
        if self.pending_session_io_snapshots.len()
            >= crate::worker::session_io::SESSION_IO_WORKER_QUEUE.capacity
        {
            self.hub_event_metrics
                .record_counter("snapshot.queue_full", 1);
            log::warn!(
                "[SessionIo] Snapshot pending map full; dropping request {} for session {}",
                request_id,
                pending.session_uuid
            );
            return false;
        }

        self.pending_session_io_snapshots
            .insert(request_id, pending);
        true
    }

    pub(super) fn route_prepared_session_io_snapshot(
        &mut self,
        request_id: String,
        session_uuid: String,
        uncompressed_len: usize,
        payload: Vec<u8>,
        recovery: bool,
    ) {
        let Some(pending) = self.pending_session_io_snapshots.remove(&request_id) else {
            log::debug!(
                "[SessionIo] Dropping prepared snapshot for unknown request {} session {}",
                request_id,
                session_uuid
            );
            return;
        };

        if payload.is_empty() {
            let counter = if recovery {
                "snapshot.backpressure_recovery.empty"
            } else {
                "snapshot.empty"
            };
            self.hub_event_metrics.record_counter(counter, 1);
            return;
        }

        self.hub_event_metrics.record_span_with_threshold(
            "snapshot.gzip_queue",
            pending.started_at.elapsed(),
            uncompressed_len + payload.len(),
            Hub::SNAPSHOT_SLOW,
            &session_uuid,
        );

        match pending.target {
            crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                peer_id,
                rows,
                cols,
                kitty_enabled,
                forwarder_key,
                active_flag,
            } => {
                let Some(worker) = self.browser_client_workers.get(&peer_id) else {
                    self.hub_event_metrics
                        .record_counter("snapshot.worker_missing", 1);
                    log::warn!(
                        "[WebRTC] Dropping prepared snapshot for missing browser worker {}",
                        &peer_id[..peer_id.len().min(8)]
                    );
                    return;
                };
                let message = crate::worker::client::ClientWorkerMessage::ControlFrame(
                    crate::worker::client::ClientControlFrame::Scrollback {
                        session_uuid,
                        rows,
                        cols,
                        kitty_enabled,
                        data: payload,
                    },
                );
                match worker.try_send(message) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        self.hub_event_metrics
                            .record_counter("snapshot.queue_full", 1);
                        if let Some(flag) = active_flag {
                            if let Ok(mut active) = flag.lock() {
                                *active = false;
                            }
                        }
                        if let Some(key) = forwarder_key {
                            self.stop_lua_pty_forwarder(&key);
                        }
                        let _ = self.hub_event_tx.send(
                            crate::hub::events::HubEvent::WebRtcIngressBackpressure {
                                browser_identity: peer_id,
                                source: "worker_snapshot_queue_full",
                            },
                        );
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                        self.hub_event_metrics
                            .record_counter("snapshot.queue_closed", 1);
                        if let Some(flag) = active_flag {
                            if let Ok(mut active) = flag.lock() {
                                *active = false;
                            }
                        }
                        if let Some(key) = forwarder_key {
                            self.stop_lua_pty_forwarder(&key);
                        }
                    }
                }
            }
            crate::hub::PendingSessionIoSnapshotTarget::WebRtcPeerRecovery { request } => {
                let _ = self.webrtc.complete_recovery_snapshot(
                    request,
                    crate::worker::webrtc::WebRtcRecoverySnapshotResult::PreparedSnapshot {
                        uncompressed_len,
                        payload,
                    },
                    &self.hub_event_metrics,
                );
            }
            crate::hub::PendingSessionIoSnapshotTarget::TerminalClientInitial { .. } => {
                log::debug!(
                    "[SessionIo] Ignoring prepared snapshot for terminal-client initial request {} session {}",
                    request_id,
                    session_uuid
                );
            }
        }
    }

    pub(super) fn route_terminal_client_initial_snapshot(
        &mut self,
        request_id: String,
        session_uuid: String,
        snapshot: Vec<u8>,
    ) {
        let Some(pending) = self.pending_session_io_snapshots.remove(&request_id) else {
            log::debug!(
                "[SessionIo] Dropping terminal snapshot for unknown request {} session {}",
                request_id,
                session_uuid
            );
            return;
        };

        if matches!(
            &pending.target,
            crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput { .. }
        ) {
            self.hub_event_metrics.record_span_with_threshold(
                "snapshot.rpc_get",
                pending.started_at.elapsed(),
                snapshot.len(),
                Hub::SNAPSHOT_SLOW,
                &session_uuid,
            );
            let Some(session_handle) = self.handle_cache.get_session(&session_uuid) else {
                self.hub_event_metrics
                    .record_counter("snapshot.session_missing", 1);
                log::warn!(
                    "[SessionIo] Dropping WebRTC snapshot for missing session {}",
                    session_uuid
                );
                return;
            };
            let pty_handle = session_handle.pty().clone();
            self.pending_session_io_snapshots
                .insert(request_id.clone(), pending);
            Self::queue_webrtc_terminal_snapshot(
                &self.hub_event_metrics,
                &self.hub_event_tx,
                &pty_handle,
                Some(request_id),
                &session_uuid,
                snapshot,
            );
            return;
        }

        let crate::hub::PendingSessionIoSnapshotTarget::TerminalClientInitial {
            worker,
            forwarder_key,
            subscription_id,
            rows,
            cols,
            kitty_enabled,
            pty_handle,
        } = pending.target
        else {
            self.pending_session_io_snapshots
                .insert(request_id, pending);
            return;
        };

        self.hub_event_metrics.record_span_with_threshold(
            "snapshot.rpc_get",
            pending.started_at.elapsed(),
            snapshot.len(),
            Hub::SNAPSHOT_SLOW,
            &session_uuid,
        );

        let attach_state =
            Self::classify_snapshot_attach_state(&pty_handle, &session_uuid, &snapshot);
        match attach_state {
            SnapshotAttachState::Ready => {}
            SnapshotAttachState::Exited => {
                log::warn!(
                    "[SessionIo] Session RPC died before terminal snapshot for {}; sending ProcessExited",
                    session_uuid
                );
                let _ = worker.try_send(crate::worker::client::ClientWorkerMessage::ControlFrame(
                    crate::worker::client::ClientControlFrame::ProcessExited {
                        session_uuid: session_uuid.clone(),
                        exit_code: None,
                    },
                ));
                let _ = worker.try_send(
                    crate::worker::client::ClientWorkerMessage::UnregisterSessionIoSender {
                        session_uuid,
                    },
                );
                self.stop_lua_pty_forwarder(&forwarder_key);
                return;
            }
            SnapshotAttachState::Reconnecting => {
                let _ = worker.try_send(crate::worker::client::ClientWorkerMessage::ControlFrame(
                    crate::worker::client::ClientControlFrame::TerminalAttach {
                        subscription_id,
                        session_uuid,
                        state: crate::worker::client::TerminalAttachState::Reconnecting,
                    },
                ));
                return;
            }
        }

        if worker
            .try_send(crate::worker::client::ClientWorkerMessage::ControlFrame(
                crate::worker::client::ClientControlFrame::Scrollback {
                    session_uuid: session_uuid.clone(),
                    rows,
                    cols,
                    kitty_enabled,
                    data: snapshot,
                },
            ))
            .is_err()
        {
            self.hub_event_metrics
                .record_counter("snapshot.queue_closed", 1);
            self.stop_lua_pty_forwarder(&forwarder_key);
        }
    }

    pub(super) fn cleanup_stale_session_io_snapshots(&mut self) {
        let now = Instant::now();
        let stale: Vec<String> = self
            .pending_session_io_snapshots
            .iter()
            .filter_map(|(request_id, pending)| {
                (now.duration_since(pending.started_at)
                    > crate::hub::SESSION_IO_SNAPSHOT_PENDING_TTL)
                    .then(|| request_id.clone())
            })
            .collect();

        for request_id in stale {
            if let Some(pending) = self.pending_session_io_snapshots.remove(&request_id) {
                self.hub_event_metrics
                    .record_counter("snapshot.pending_stale_drop", 1);
                log::warn!(
                    "[SessionIo] Dropped stale prepared-snapshot request {} for session {}",
                    request_id,
                    pending.session_uuid
                );
            }
        }
    }

    pub(super) fn refresh_lua_terminal_snapshot(
        &mut self,
        req: crate::lua::RefreshSnapshotRequest,
    ) {
        let Some(session_handle) = self.handle_cache.get_session(&req.session_uuid) else {
            log::debug!(
                "[Lua] Snapshot refresh ignored for missing session {}",
                req.session_uuid
            );
            return;
        };

        let pty_handle = session_handle.pty().clone();
        let pty_for_prepare = pty_handle.clone();
        let hub_event_tx = self.hub_event_tx.clone();
        let peer_id = req.peer_id.clone();
        let session_uuid = req.session_uuid.clone();
        let target_rows = req.rows;
        let target_cols = req.cols;
        let metrics = Arc::clone(&self.hub_event_metrics);
        let snapshot_request_id = if pty_handle.is_session_backed() {
            let request_id = Self::next_session_io_request_id("snapshot");
            if !self.insert_pending_session_io_snapshot(
                request_id.clone(),
                crate::hub::PendingSessionIoSnapshot {
                    session_uuid: session_uuid.clone(),
                    started_at: Instant::now(),
                    target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                        peer_id,
                        rows: target_rows,
                        cols: target_cols,
                        kitty_enabled: pty_handle.kitty_enabled(),
                        forwarder_key: None,
                        active_flag: None,
                    },
                },
            ) {
                return;
            }
            Some(request_id)
        } else {
            None
        };

        let _guard = self.tokio_runtime.enter();
        tokio::spawn(async move {
            let rpc_started = Instant::now();
            let snapshot = match tokio::task::spawn_blocking(move || {
                if pty_handle.is_session_backed() {
                    pty_handle.resize_direct(target_rows, target_cols);
                }
                pty_handle.get_snapshot()
            })
            .await
            {
                Ok(snapshot) => snapshot,
                Err(e) => {
                    log::warn!(
                        "[Lua] Snapshot refresh task failed for session {}: {}",
                        session_uuid,
                        e
                    );
                    Vec::new()
                }
            };
            metrics.record_span_with_threshold(
                "snapshot.rpc_get",
                rpc_started.elapsed(),
                snapshot.len(),
                Hub::SNAPSHOT_SLOW,
                &session_uuid,
            );

            Self::queue_webrtc_terminal_snapshot(
                &metrics,
                &hub_event_tx,
                &pty_for_prepare,
                snapshot_request_id,
                &session_uuid,
                snapshot,
            );
        });
    }

    pub(super) fn queue_webrtc_terminal_snapshot(
        metrics: &crate::hub::events::HubEventMetrics,
        hub_event_tx: &crate::hub::events::HubEventTx,
        pty_handle: &crate::hub::agent_handle::PtyHandle,
        request_id: Option<String>,
        session_uuid: &str,
        snapshot: Vec<u8>,
    ) -> bool {
        if snapshot.is_empty() {
            if let Some(request_id) = request_id {
                let _ =
                    hub_event_tx.send(crate::hub::events::HubEvent::DropPendingSessionIoSnapshot {
                        request_id,
                    });
            }
            metrics.record_counter("snapshot.empty", 1);
            return true;
        }

        let Some(request_id) = request_id else {
            log::warn!(
                "[SessionIo] Non-session-backed snapshot for session {} cannot be prepared",
                session_uuid
            );
            return false;
        };

        match pty_handle.enqueue_session_io_request(
            crate::worker::session_io::SessionIoRequest::PrepareSnapshot {
                request_id: request_id.clone(),
                snapshot,
                recovery: false,
            },
        ) {
            Ok(()) => true,
            Err(e) => {
                if matches!(
                    e,
                    crate::session::connection::SessionIoRequestEnqueueError::MailboxFull
                ) {
                    metrics.record_counter("snapshot.queue_full", 1);
                }
                log::warn!(
                    "[SessionIo] Failed to enqueue snapshot prepare for session {}: {e:?}",
                    session_uuid
                );
                let _ =
                    hub_event_tx.send(crate::hub::events::HubEvent::DropPendingSessionIoSnapshot {
                        request_id,
                    });
                false
            }
        }
    }

    pub(super) fn queue_backpressure_recovery_snapshot(
        metrics: &crate::hub::events::HubEventMetrics,
        hub_event_tx: &crate::hub::events::HubEventTx,
        pty_handle: &crate::hub::agent_handle::PtyHandle,
        request_id: String,
        session_uuid: &str,
        snapshot: Vec<u8>,
    ) -> bool {
        match pty_handle.enqueue_session_io_request(
            crate::worker::session_io::SessionIoRequest::PrepareSnapshot {
                request_id: request_id.clone(),
                snapshot,
                recovery: true,
            },
        ) {
            Ok(()) => true,
            Err(e) => {
                metrics.record_counter("snapshot.backpressure_recovery.failed", 1);
                if matches!(
                    e,
                    crate::session::connection::SessionIoRequestEnqueueError::MailboxFull
                ) {
                    metrics.record_counter("snapshot.queue_full", 1);
                }
                log::warn!(
                    "[SessionIo] Failed to enqueue recovery snapshot prepare for session {}: {e:?}",
                    session_uuid
                );
                let _ =
                    hub_event_tx.send(crate::hub::events::HubEvent::DropPendingSessionIoSnapshot {
                        request_id,
                    });
                false
            }
        }
    }

    pub(super) fn dispatch_webrtc_recovery_snapshot_requests(&mut self) {
        let now = Instant::now();

        // Collect entries that have cooled down.
        let ready = self.webrtc.drain_recovery_requests(now);

        for request in ready {
            let Some(session_handle) = self.handle_cache.get_session(&request.session_uuid) else {
                let _ = self.webrtc.complete_recovery_snapshot(
                    request,
                    crate::worker::webrtc::WebRtcRecoverySnapshotResult::Failed,
                    &self.hub_event_metrics,
                );
                continue;
            };

            let pty_handle = session_handle.pty().clone();

            if pty_handle.is_session_backed() {
                // Session snapshot requires blocking I/O — spawn off the tick loop.
                let session_uuid = request.session_uuid.clone();
                let browser_identity = request.browser_identity.clone();
                let request_for_task = request.clone();
                let metrics = Arc::clone(&self.hub_event_metrics);
                let pty_for_prepare = pty_handle.clone();
                let hub_event_tx = self.hub_event_tx.clone();
                let request_id = Self::next_session_io_request_id("snapshot-recovery");
                if !self.insert_pending_session_io_snapshot(
                    request_id.clone(),
                    crate::hub::PendingSessionIoSnapshot {
                        session_uuid: session_uuid.clone(),
                        started_at: Instant::now(),
                        target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcPeerRecovery {
                            request: request.clone(),
                        },
                    },
                ) {
                    continue;
                }

                let _guard = self.tokio_runtime.enter();
                tokio::spawn(async move {
                    let rpc_started = Instant::now();
                    let snapshot = match tokio::task::spawn_blocking(move || {
                        pty_handle.get_snapshot()
                    })
                    .await
                    {
                        Ok(snapshot) => snapshot,
                        Err(e) => {
                            log::warn!(
                                "[WebRTC] Backpressure recovery snapshot task failed for session {}: {}",
                                &session_uuid[..session_uuid.len().min(8)],
                                e
                            );
                            let _ = hub_event_tx.send(
                                crate::hub::events::HubEvent::DropPendingSessionIoSnapshot {
                                    request_id: request_id.clone(),
                                },
                            );
                            let _ = hub_event_tx.send(
                                crate::hub::events::HubEvent::WebRtcRecoverySnapshotReady {
                                    request: request_for_task.clone(),
                                    result:
                                        crate::worker::webrtc::WebRtcRecoverySnapshotResult::Failed,
                                },
                            );
                            return;
                        }
                    };
                    metrics.record_span_with_threshold(
                        "snapshot.rpc_get",
                        rpc_started.elapsed(),
                        snapshot.len(),
                        Hub::SNAPSHOT_SLOW,
                        &session_uuid,
                    );

                    if snapshot.is_empty() {
                        let _ = hub_event_tx.send(
                            crate::hub::events::HubEvent::DropPendingSessionIoSnapshot {
                                request_id: request_id.clone(),
                            },
                        );
                        let _ = hub_event_tx.send(
                            crate::hub::events::HubEvent::WebRtcRecoverySnapshotReady {
                                request: request_for_task.clone(),
                                result: crate::worker::webrtc::WebRtcRecoverySnapshotResult::Empty,
                            },
                        );
                        return;
                    }

                    log::info!(
                        "[WebRTC] Sending async backpressure recovery snapshot ({} bytes) to {} for session {}",
                        snapshot.len(),
                        &browser_identity[..browser_identity.len().min(8)],
                        &session_uuid[..session_uuid.len().min(8)]
                    );

                    if !Self::queue_backpressure_recovery_snapshot(
                        &metrics,
                        &hub_event_tx,
                        &pty_for_prepare,
                        request_id,
                        &session_uuid,
                        snapshot,
                    ) {
                        let _ = hub_event_tx.send(
                            crate::hub::events::HubEvent::WebRtcRecoverySnapshotReady {
                                request: request_for_task.clone(),
                                result: crate::worker::webrtc::WebRtcRecoverySnapshotResult::Failed,
                            },
                        );
                    }
                });
                continue;
            }

            // Snapshot via RPC — run on blocking thread to avoid stalling the event loop.
            let pty_handle = session_handle.pty().clone();
            let browser_identity = request.browser_identity.clone();
            let session_uuid = request.session_uuid.clone();
            let request_for_task = request;
            let metrics = Arc::clone(&self.hub_event_metrics);
            let hub_event_tx = self.hub_event_tx.clone();
            tokio::spawn(async move {
                let rpc_started = Instant::now();
                let snapshot = match tokio::task::spawn_blocking(move || pty_handle.get_snapshot())
                    .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        log::warn!(
                            "[WebRTC] Backpressure recovery snapshot task failed for session {}: {}",
                            &session_uuid[..session_uuid.len().min(8)],
                            e
                        );
                        let _ = hub_event_tx.send(
                            crate::hub::events::HubEvent::WebRtcRecoverySnapshotReady {
                                request: request_for_task.clone(),
                                result: crate::worker::webrtc::WebRtcRecoverySnapshotResult::Failed,
                            },
                        );
                        return;
                    }
                };
                metrics.record_span_with_threshold(
                    "snapshot.rpc_get",
                    rpc_started.elapsed(),
                    snapshot.len(),
                    Hub::SNAPSHOT_SLOW,
                    &session_uuid,
                );

                if snapshot.is_empty() {
                    let _ = hub_event_tx.send(
                        crate::hub::events::HubEvent::WebRtcRecoverySnapshotReady {
                            request: request_for_task.clone(),
                            result: crate::worker::webrtc::WebRtcRecoverySnapshotResult::Empty,
                        },
                    );
                    return;
                }

                log::info!(
                    "[WebRTC] Sending backpressure recovery snapshot ({} bytes) to {} for session {}",
                    snapshot.len(),
                    &browser_identity[..browser_identity.len().min(8)],
                    &session_uuid[..session_uuid.len().min(8)]
                );

                let _ =
                    hub_event_tx.send(crate::hub::events::HubEvent::WebRtcRecoverySnapshotReady {
                        request: request_for_task,
                        result: crate::worker::webrtc::WebRtcRecoverySnapshotResult::Snapshot(
                            snapshot,
                        ),
                    });
            });
        }
    }
}
