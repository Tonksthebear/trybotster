use super::*;

impl Hub {
    pub(super) fn send_terminal_attach_state(
        &self,
        peer_id: &str,
        subscription_id: &str,
        session_uuid: &str,
        state: &str,
    ) {
        let Ok(attach_state) = crate::worker::client::TerminalAttachState::try_from(state) else {
            log::warn!(
                "[WebRTC] Refusing unknown terminal_attach state '{}' for {}",
                state,
                session_uuid
            );
            return;
        };
        let payload = crate::worker::transport::egress_terminal_attach(
            subscription_id.to_string(),
            session_uuid.to_string(),
            attach_state,
        );
        match serde_json::to_vec(&payload) {
            Ok(data) => self.queue_webrtc_peer_command(
                peer_id,
                crate::worker::webrtc::WebRtcAdapterCommand::Json { data },
            ),
            Err(e) => {
                log::warn!(
                    "[WebRTC] Failed to serialize terminal_attach state '{}': {}",
                    state,
                    e
                );
            }
        }
    }

    pub(super) fn send_worker_terminal_attach_state(
        worker: &crate::worker::client::ClientWorkerHandle,
        subscription_id: &str,
        session_uuid: &str,
        state: &str,
    ) {
        let Ok(state) = crate::worker::client::TerminalAttachState::try_from(state) else {
            log::warn!(
                "[ClientWorker] Refusing unknown terminal_attach state '{}' for {}",
                state,
                session_uuid
            );
            return;
        };
        if let Err(e) = worker.try_send(crate::worker::client::ClientWorkerMessage::ControlFrame(
            crate::worker::client::ClientControlFrame::TerminalAttach {
                subscription_id: subscription_id.to_string(),
                session_uuid: session_uuid.to_string(),
                state,
            },
        )) {
            log::warn!(
                "[ClientWorker] Failed to queue terminal_attach state '{}' for {}: {}",
                state.as_str(),
                session_uuid,
                e
            );
        }
    }

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

    pub(super) fn try_attach_terminal_forwarder(
        &mut self,
        req: &crate::lua::CreateForwarderRequest,
    ) -> bool {
        let forwarder_key = format!("{}:{}", req.peer_id, req.session_uuid);

        let Some(session_handle) = self.handle_cache.get_session(&req.session_uuid) else {
            return false;
        };

        let pty_handle = session_handle.pty().clone();

        // Abort any existing forwarder for this key.
        if let Some(old_task) = self.pty_forwarders.remove(&forwarder_key) {
            old_task.abort();
            self.unregister_terminal_forwarder_peer(&forwarder_key, false);
            log::debug!("[Lua] Aborted existing PTY forwarder for {}", forwarder_key);
        }

        // Snapshot retrieval and subscription setup can block.
        // Run it inside the spawned forwarder task so Hub event processing stays
        // responsive while attach state is being prepared.
        let pty_for_snapshot = pty_handle.clone();
        let pty_for_prepare = pty_handle.clone();

        // Spawn forwarder task.
        let peer_id = req.peer_id.clone();
        let session_uuid = req.session_uuid.clone();
        let target_rows = req.rows;
        let target_cols = req.cols;
        let prefix = req.prefix.clone().unwrap_or_else(|| vec![0x01]);
        let active_flag = req.active_flag.clone();
        let active_terminal_peers = Arc::clone(&self.active_terminal_peers);
        let metrics = Arc::clone(&self.hub_event_metrics);
        let hub_event_tx = self.hub_event_tx.clone();

        // Use browser-provided subscription ID for message routing.
        let subscription_id = req.subscription_id.clone();
        let snapshot_request_id = if pty_handle.is_session_backed() {
            let request_id = Self::next_session_io_request_id("snapshot");
            if !self.insert_pending_session_io_snapshot(
                request_id.clone(),
                crate::hub::PendingSessionIoSnapshot {
                    session_uuid: session_uuid.clone(),
                    started_at: Instant::now(),
                    target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                        peer_id: peer_id.clone(),
                        rows: target_rows,
                        cols: target_cols,
                        kitty_enabled: pty_handle.kitty_enabled(),
                        forwarder_key: Some(forwarder_key.clone()),
                        active_flag: Some(active_flag.clone()),
                    },
                },
            ) {
                return false;
            }
            Some(request_id)
        } else {
            None
        };

        let _guard = self.tokio_runtime.enter();
        let Some(worker) = self.browser_client_workers.get(&peer_id).cloned() else {
            log::warn!(
                "[WebRTC] Cannot attach terminal for peer {} without browser worker",
                &peer_id[..peer_id.len().min(8)]
            );
            return false;
        };
        Self::register_worker_session_io_sender(
            &worker,
            &req.session_uuid,
            pty_handle.clone(),
            "WebRTC",
        );
        let task = tokio::spawn(async move {
            use crate::agent::pty::PtyEvent;
            use crate::worker::client::{ClientControlFrame, ClientWorkerMessage};

            log::info!(
                "[Lua] Started PTY forwarder for peer {} session {}",
                &peer_id[..peer_id.len().min(8)],
                session_uuid
            );
            let mut query_filter_buffer = Vec::new();
            let mut dumped_live_chunks = 0usize;

            let rpc_started = Instant::now();
            let (snapshot, mut pty_rx) = match tokio::task::spawn_blocking(move || {
                if pty_for_snapshot.is_session_backed() {
                    pty_for_snapshot.resize_direct(target_rows, target_cols);
                }
                let (snapshot, _kitty_enabled, _rows, _cols, pty_rx) =
                    pty_for_snapshot.snapshot_and_subscribe();
                (snapshot, pty_rx)
            })
            .await
            {
                Ok(result) => result,
                Err(e) => {
                    log::warn!(
                        "[Lua] Snapshot fetch task failed for session {}: {}",
                        session_uuid,
                        e
                    );
                    (Vec::new(), pty_handle.subscribe())
                }
            };
            metrics.record_span_with_threshold(
                "snapshot.rpc_get",
                rpc_started.elapsed(),
                snapshot.len(),
                Hub::SNAPSHOT_SLOW,
                &session_uuid,
            );

            log::debug!(
                "[Lua] Snapshot bytes for peer {} session {}: {}",
                &peer_id[..peer_id.len().min(8)],
                session_uuid,
                snapshot.len()
            );

            Self::reset_restty_fixture_capture(
                &session_uuid,
                &peer_id,
                &subscription_id,
                target_rows,
                target_cols,
                snapshot.len(),
            );
            if !snapshot.is_empty() {
                Self::dump_restty_snapshot_fixture(&session_uuid, &snapshot);
            }

            if !Self::queue_webrtc_terminal_snapshot(
                &metrics,
                &hub_event_tx,
                &pty_for_prepare,
                snapshot_request_id,
                &session_uuid,
                snapshot,
            ) {
                return;
            }

            loop {
                // Check if forwarder was stopped by Lua.
                {
                    let active = active_flag
                        .lock()
                        .expect("Forwarder active_flag mutex poisoned");
                    if !*active {
                        log::debug!("[Lua] PTY forwarder stopped by Lua");
                        break;
                    }
                }

                match pty_rx.recv().await {
                    Ok(PtyEvent::Output(data)) => {
                        let filtered = if active_terminal_peers
                            .lock()
                            .ok()
                            .and_then(|active| active.get(&session_uuid).cloned())
                            .is_some_and(|active_peer| active_peer != peer_id.as_str())
                        {
                            crate::hub::terminal_profile::strip_osc_queries_from_output(
                                &mut query_filter_buffer,
                                &data,
                            )
                        } else {
                            query_filter_buffer.clear();
                            data
                        };

                        if filtered.is_empty() {
                            continue;
                        }

                        if dumped_live_chunks < Self::RESTTY_FIXTURE_LIVE_CHUNK_LIMIT {
                            Self::dump_restty_live_fixture_chunk(
                                &session_uuid,
                                dumped_live_chunks,
                                &filtered,
                            );
                            dumped_live_chunks += 1;
                        }

                        let mut raw_message = Vec::with_capacity(prefix.len() + filtered.len());
                        raw_message.extend(&prefix);
                        raw_message.extend(&filtered);

                        if worker
                            .send(ClientWorkerMessage::TerminalBytes {
                                session_uuid: session_uuid.clone(),
                                data: raw_message,
                            })
                            .await
                            .is_err()
                        {
                            log::trace!("[Lua] Worker channel closed, stopping forwarder");
                            break;
                        }
                    }
                    Ok(PtyEvent::ProcessExited { exit_code }) => {
                        log::info!(
                            "[Lua] PTY process exited (code={:?}) for session {}",
                            exit_code,
                            session_uuid
                        );
                        let _ = worker
                            .send(ClientWorkerMessage::ControlFrame(
                                ClientControlFrame::ProcessExited {
                                    session_uuid: session_uuid.clone(),
                                    exit_code,
                                },
                            ))
                            .await;
                        let _ = worker
                            .send(ClientWorkerMessage::UnregisterSessionIoSender {
                                session_uuid: session_uuid.clone(),
                            })
                            .await;
                        break;
                    }
                    Ok(_other_event) => {
                        // Ignore other events.
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!(
                            "[Lua] PTY forwarder lagged by {} events for session {}",
                            n,
                            session_uuid
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        log::info!("[Lua] PTY channel closed for session {}", session_uuid);
                        break;
                    }
                }
            }

            // Mark forwarder as inactive.
            *active_flag
                .lock()
                .expect("Forwarder active_flag mutex poisoned") = false;
            let _ = worker
                .send(ClientWorkerMessage::UnregisterSessionIoSender {
                    session_uuid: session_uuid.clone(),
                })
                .await;

            log::info!(
                "[Lua] Stopped PTY forwarder for peer {} session {}",
                &peer_id[..peer_id.len().min(8)],
                session_uuid
            );
        });

        self.register_terminal_forwarder_peer(&forwarder_key, &req.session_uuid, &req.peer_id);
        self.pty_forwarders.insert(forwarder_key, task);
        true
    }

    pub(super) fn process_pending_terminal_attaches(&mut self) {
        if self.pending_terminal_attaches.is_empty() {
            return;
        }

        let now = Instant::now();
        let mut ready_keys = Vec::new();
        let mut stale_keys = Vec::new();
        let mut inactive_keys = Vec::new();

        for (key, intent) in &self.pending_terminal_attaches {
            if !intent.request.is_active() {
                inactive_keys.push(key.clone());
                continue;
            }

            if self
                .handle_cache
                .get_session(intent.request.session_uuid())
                .is_some()
            {
                ready_keys.push(key.clone());
                continue;
            }

            if now.duration_since(intent.requested_at) >= Self::TERMINAL_ATTACH_NOT_FOUND_TIMEOUT {
                stale_keys.push(key.clone());
            }
        }

        for key in inactive_keys {
            self.pending_terminal_attaches.remove(&key);
        }

        for key in ready_keys {
            let Some(intent) = self.pending_terminal_attaches.remove(&key) else {
                continue;
            };
            if self.try_attach_pending_terminal_request(&intent.request) {
            } else {
                // Session may have disappeared between lookup and attach attempt.
                self.pending_terminal_attaches.insert(key, intent);
            }
        }

        for key in stale_keys {
            let Some(intent) = self.pending_terminal_attaches.remove(&key) else {
                continue;
            };
            intent.request.deactivate();
            self.send_pending_terminal_attach_state(&intent.request, "not_found");
            let session_uuid = intent.request.session_uuid().to_string();
            self.remove_terminal_client_worker(&key, &session_uuid, "PendingAttach");
        }
    }

    pub(super) fn try_attach_pending_terminal_request(
        &mut self,
        request: &PendingTerminalAttachRequest,
    ) -> bool {
        match request {
            PendingTerminalAttachRequest::WebRtc(req) => self.try_attach_terminal_forwarder(req),
            PendingTerminalAttachRequest::Tui(req) => self.try_attach_tui_terminal_forwarder(req),
            PendingTerminalAttachRequest::Socket(req) => {
                self.try_attach_socket_terminal_forwarder(req)
            }
        }
    }

    pub(super) fn send_pending_terminal_attach_state(
        &self,
        request: &PendingTerminalAttachRequest,
        state: &str,
    ) {
        match request {
            PendingTerminalAttachRequest::WebRtc(req) => {
                self.send_terminal_attach_state(
                    &req.peer_id,
                    &req.subscription_id,
                    &req.session_uuid,
                    state,
                );
            }
            PendingTerminalAttachRequest::Tui(req) => {
                let forwarder_key = format!("tui:{}", req.session_uuid);
                if let Some(worker) = self.terminal_client_workers.get(&forwarder_key) {
                    Self::send_worker_terminal_attach_state(
                        worker,
                        &req.subscription_id,
                        &req.session_uuid,
                        state,
                    );
                }
            }
            PendingTerminalAttachRequest::Socket(req) => {
                let forwarder_key = format!("{}:{}", req.client_id, req.session_uuid);
                if let Some(worker) = self.terminal_client_workers.get(&forwarder_key) {
                    Self::send_worker_terminal_attach_state(
                        worker,
                        &req.subscription_id,
                        &req.session_uuid,
                        state,
                    );
                }
            }
        }
    }

    pub(super) fn replace_pending_terminal_attach(
        &mut self,
        forwarder_key: &str,
        request: PendingTerminalAttachRequest,
    ) {
        if let Some(prev) = self.pending_terminal_attaches.remove(forwarder_key) {
            prev.request.deactivate();
        }

        self.pending_terminal_attaches.insert(
            forwarder_key.to_string(),
            PendingTerminalAttach {
                request,
                requested_at: Instant::now(),
            },
        );
    }

    pub(super) fn create_lua_pty_forwarder(&mut self, req: crate::lua::CreateForwarderRequest) {
        let forwarder_key = format!("{}:{}", req.peer_id, req.session_uuid);

        if self.try_attach_terminal_forwarder(&req) {
            self.send_terminal_attach_state(
                &req.peer_id,
                &req.subscription_id,
                &req.session_uuid,
                "attached",
            );
            return;
        }

        self.replace_pending_terminal_attach(
            &forwarder_key,
            PendingTerminalAttachRequest::WebRtc(req.clone()),
        );
        self.send_terminal_attach_state(
            &req.peer_id,
            &req.subscription_id,
            &req.session_uuid,
            "pending",
        );
    }

    pub(super) fn spawn_terminal_client_forwarder_runtime(
        pty_handle: crate::hub::agent_handle::PtyHandle,
        worker: crate::worker::client::ClientWorkerHandle,
        session_uuid: String,
        subscription_id: String,
        target_rows: u16,
        target_cols: u16,
        active_flag: Arc<std::sync::Mutex<bool>>,
        snapshot_request_id: Option<String>,
        hub_event_tx: crate::hub::events::HubEventTx,
        log_prefix: &'static str,
        client_label: String,
        filter: TerminalStreamFilter,
    ) -> tokio::task::JoinHandle<()> {
        let pty_for_snapshot = pty_handle.clone();

        tokio::spawn(async move {
            use crate::agent::pty::PtyEvent;
            use crate::worker::client::{
                ClientControlFrame, ClientWorkerMessage, TerminalAttachState,
            };

            log::info!(
                "[{}] Started PTY forwarder for {} session {}",
                log_prefix,
                client_label,
                session_uuid
            );
            let _ = worker
                .send(ClientWorkerMessage::SubscribeSession {
                    session_uuid: session_uuid.clone(),
                    subscription_id: subscription_id.clone(),
                })
                .await;
            let mut query_filter_buffer = Vec::new();

            let mut pty_rx = if let Some(request_id) = snapshot_request_id {
                let pty_rx = pty_handle.subscribe();
                let resize_result = pty_handle.enqueue_session_io_request(
                    crate::worker::session_io::SessionIoRequest::Resize {
                        rows: target_rows,
                        cols: target_cols,
                    },
                );
                let snapshot_result = pty_handle.enqueue_session_io_request(
                    crate::worker::session_io::SessionIoRequest::GetSnapshot {
                        request_id: request_id.clone(),
                    },
                );

                if resize_result.is_err() || snapshot_result.is_err() {
                    log::warn!(
                        "[{}] Session I/O snapshot request failed for {} session {}: resize={:?} snapshot={:?}",
                        log_prefix,
                        client_label,
                        session_uuid,
                        resize_result.err(),
                        snapshot_result.err()
                    );
                    let _ = hub_event_tx.send(
                        crate::hub::events::HubEvent::DropPendingSessionIoSnapshot { request_id },
                    );
                    let _ = worker
                        .send(ClientWorkerMessage::ControlFrame(
                            ClientControlFrame::TerminalAttach {
                                subscription_id: subscription_id.clone(),
                                session_uuid: session_uuid.clone(),
                                state: TerminalAttachState::NotReady,
                            },
                        ))
                        .await;
                    let _ = worker
                        .send(ClientWorkerMessage::UnregisterSessionIoSender {
                            session_uuid: session_uuid.clone(),
                        })
                        .await;
                    return;
                }

                pty_rx
            } else {
                let (snapshot, kitty_enabled, snapshot_rows, snapshot_cols, pty_rx) =
                    match tokio::task::spawn_blocking(move || {
                        pty_for_snapshot.snapshot_and_subscribe()
                    })
                    .await
                    {
                        Ok(result) => result,
                        Err(e) => {
                            log::warn!(
                                "[{}] Snapshot fetch task failed for {} session {}: {}",
                                log_prefix,
                                client_label,
                                session_uuid,
                                e
                            );
                            (
                                Vec::new(),
                                false,
                                target_rows,
                                target_cols,
                                pty_handle.subscribe(),
                            )
                        }
                    };

                log::debug!(
                    "[{}] Snapshot bytes for {} session {}: {}",
                    log_prefix,
                    client_label,
                    session_uuid,
                    snapshot.len()
                );

                let attach_state =
                    Self::classify_snapshot_attach_state(&pty_handle, &session_uuid, &snapshot);
                match attach_state {
                    SnapshotAttachState::Ready => {}
                    SnapshotAttachState::Exited => {
                        log::warn!(
                            "[{}] Session RPC died before snapshot for {} session {}; sending ProcessExited",
                            log_prefix,
                            client_label,
                            session_uuid
                        );
                        let _ = worker
                            .send(ClientWorkerMessage::ControlFrame(
                                ClientControlFrame::ProcessExited {
                                    session_uuid: session_uuid.clone(),
                                    exit_code: None,
                                },
                            ))
                            .await;
                        let _ = worker
                            .send(ClientWorkerMessage::UnregisterSessionIoSender {
                                session_uuid: session_uuid.clone(),
                            })
                            .await;
                        return;
                    }
                    SnapshotAttachState::Reconnecting => {
                        log::info!(
                            "[{}] Session '{}' snapshot unavailable - reconnect pending",
                            log_prefix,
                            &session_uuid[..session_uuid.len().min(16)]
                        );
                        let _ = worker
                            .send(ClientWorkerMessage::ControlFrame(
                                ClientControlFrame::TerminalAttach {
                                    subscription_id: subscription_id.clone(),
                                    session_uuid: session_uuid.clone(),
                                    state: TerminalAttachState::Reconnecting,
                                },
                            ))
                            .await;
                    }
                }

                if attach_state != SnapshotAttachState::Reconnecting
                    && worker
                        .send(ClientWorkerMessage::ControlFrame(
                            ClientControlFrame::Scrollback {
                                session_uuid: session_uuid.clone(),
                                rows: snapshot_rows,
                                cols: snapshot_cols,
                                kitty_enabled,
                                data: snapshot,
                            },
                        ))
                        .await
                        .is_err()
                {
                    log::trace!(
                        "[{}] Worker channel closed before snapshot sent",
                        log_prefix
                    );
                    let _ = worker
                        .send(ClientWorkerMessage::UnregisterSessionIoSender {
                            session_uuid: session_uuid.clone(),
                        })
                        .await;
                    return;
                }

                pty_rx
            };

            loop {
                {
                    let active = active_flag
                        .lock()
                        .expect("Forwarder active_flag mutex poisoned");
                    if !*active {
                        log::debug!("[{}] PTY forwarder stopped by Lua", log_prefix);
                        break;
                    }
                }

                match pty_rx.recv().await {
                    Ok(PtyEvent::Output(data)) => {
                        let mut chunks = vec![data];
                        let mut stashed: Vec<PtyEvent> = Vec::new();
                        loop {
                            match pty_rx.try_recv() {
                                Ok(PtyEvent::Output(more)) => chunks.push(more),
                                Ok(other) => {
                                    let is_terminal =
                                        matches!(other, PtyEvent::ProcessExited { .. });
                                    stashed.push(other);
                                    if is_terminal {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }

                        let mut worker_closed = false;
                        for chunk in chunks {
                            let filtered =
                                filter.filter_chunk(&session_uuid, &mut query_filter_buffer, chunk);
                            if filtered.is_empty() {
                                continue;
                            }
                            if worker
                                .send(ClientWorkerMessage::TerminalBytes {
                                    session_uuid: session_uuid.clone(),
                                    data: filtered,
                                })
                                .await
                                .is_err()
                            {
                                log::trace!(
                                    "[{}] Worker channel closed, stopping forwarder",
                                    log_prefix
                                );
                                worker_closed = true;
                                break;
                            }
                        }
                        if worker_closed {
                            break;
                        }

                        if Self::forward_terminal_stream_events(
                            &worker,
                            &session_uuid,
                            &client_label,
                            log_prefix,
                            stashed,
                        )
                        .await
                        {
                            break;
                        }
                    }
                    Ok(event @ PtyEvent::ProcessExited { .. })
                    | Ok(event @ PtyEvent::KittyChanged(_))
                    | Ok(event @ PtyEvent::FocusReportingChanged(_)) => {
                        if Self::forward_terminal_stream_events(
                            &worker,
                            &session_uuid,
                            &client_label,
                            log_prefix,
                            vec![event],
                        )
                        .await
                        {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!(
                            "[{}] PTY forwarder lagged by {} events for {} session {}",
                            log_prefix,
                            n,
                            client_label,
                            session_uuid
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        log::info!(
                            "[{}] PTY channel closed for {} session {}",
                            log_prefix,
                            client_label,
                            session_uuid
                        );
                        break;
                    }
                }
            }

            *active_flag
                .lock()
                .expect("Forwarder active_flag mutex poisoned") = false;
            let _ = worker
                .send(ClientWorkerMessage::UnregisterSessionIoSender {
                    session_uuid: session_uuid.clone(),
                })
                .await;

            log::info!(
                "[{}] Stopped PTY forwarder for {} session {}",
                log_prefix,
                client_label,
                session_uuid
            );
        })
    }

    pub(super) async fn forward_terminal_stream_events(
        worker: &crate::worker::client::ClientWorkerHandle,
        session_uuid: &str,
        client_label: &str,
        log_prefix: &str,
        events: Vec<crate::agent::pty::PtyEvent>,
    ) -> bool {
        use crate::agent::pty::PtyEvent;
        use crate::worker::client::{ClientControlFrame, ClientWorkerMessage};

        for event in events {
            match event {
                PtyEvent::ProcessExited { exit_code } => {
                    log::info!(
                        "[{}] PTY process exited (code={:?}) for {} session {}",
                        log_prefix,
                        exit_code,
                        client_label,
                        session_uuid
                    );
                    let _ = worker
                        .send(ClientWorkerMessage::ControlFrame(
                            ClientControlFrame::ProcessExited {
                                session_uuid: session_uuid.to_string(),
                                exit_code,
                            },
                        ))
                        .await;
                    return true;
                }
                PtyEvent::KittyChanged(enabled) => {
                    let _ = worker
                        .send(ClientWorkerMessage::ControlFrame(
                            ClientControlFrame::KittyChanged {
                                session_uuid: session_uuid.to_string(),
                                enabled,
                            },
                        ))
                        .await;
                }
                PtyEvent::FocusReportingChanged(enabled) => {
                    let _ = worker
                        .send(ClientWorkerMessage::ControlFrame(
                            ClientControlFrame::FocusReportingChanged {
                                session_uuid: session_uuid.to_string(),
                                enabled,
                            },
                        ))
                        .await;
                }
                PtyEvent::Output(_) => unreachable!("output handled before stashing"),
                _ => {}
            }
        }
        false
    }

    pub(super) fn create_lua_tui_pty_forwarder(
        &mut self,
        req: crate::lua::primitives::CreateTuiForwarderRequest,
    ) {
        let forwarder_key = format!("tui:{}", req.session_uuid);

        if self.try_attach_tui_terminal_forwarder(&req) {
            return;
        }

        if let Some(output_tx) = self.tui_output_tx.clone() {
            let _guard = self.tokio_runtime.enter();
            let worker = self.spawn_tui_control_worker_adapter(output_tx);
            Self::send_worker_terminal_attach_state(
                &worker,
                &req.subscription_id,
                &req.session_uuid,
                "pending",
            );
            self.terminal_client_workers
                .insert(forwarder_key.clone(), worker);
        }
        self.replace_pending_terminal_attach(
            &forwarder_key,
            PendingTerminalAttachRequest::Tui(req),
        );
    }

    pub(super) fn try_attach_tui_terminal_forwarder(
        &mut self,
        req: &crate::lua::primitives::CreateTuiForwarderRequest,
    ) -> bool {
        let forwarder_key = format!("tui:{}", req.session_uuid);

        // Check if session exists
        let Some(session_handle) = self.handle_cache.get_session(&req.session_uuid) else {
            return false;
        };

        let pty_handle = session_handle.pty().clone();

        let Some(output_tx) = self.tui_output_tx.clone() else {
            return false;
        };

        // Abort any existing forwarder for this key
        if let Some(old_task) = self.pty_forwarders.remove(&forwarder_key) {
            old_task.abort();
            self.unregister_terminal_forwarder_peer(&forwarder_key, false);
            self.remove_terminal_client_worker(&forwarder_key, &req.session_uuid, "Lua-TUI");
            log::debug!(
                "[Lua-TUI] Aborted existing PTY forwarder for {}",
                forwarder_key
            );
        }

        let session_uuid = req.session_uuid.clone();
        let subscription_id = req.subscription_id.clone();
        let target_rows = req.rows;
        let target_cols = req.cols;
        let active_flag = Arc::clone(&req.active_flag);
        let _guard = self.tokio_runtime.enter();
        let worker = self.spawn_tui_client_worker_adapter(output_tx);
        Self::register_worker_session_io_sender(
            &worker,
            &req.session_uuid,
            pty_handle.clone(),
            "Lua-TUI",
        );
        self.terminal_client_workers
            .insert(forwarder_key.clone(), worker.clone());
        Self::send_worker_terminal_attach_state(
            &worker,
            &req.subscription_id,
            &req.session_uuid,
            "attached",
        );
        let snapshot_request_id = if pty_handle.is_session_backed() {
            let request_id = Self::next_session_io_request_id("terminal-snapshot");
            if !self.insert_pending_session_io_snapshot(
                request_id.clone(),
                crate::hub::PendingSessionIoSnapshot {
                    session_uuid: req.session_uuid.clone(),
                    started_at: Instant::now(),
                    target: crate::hub::PendingSessionIoSnapshotTarget::TerminalClientInitial {
                        worker: worker.clone(),
                        forwarder_key: forwarder_key.clone(),
                        subscription_id: req.subscription_id.clone(),
                        rows: target_rows,
                        cols: target_cols,
                        kitty_enabled: pty_handle.kitty_enabled(),
                        pty_handle: pty_handle.clone(),
                    },
                },
            ) {
                return false;
            }
            Some(request_id)
        } else {
            None
        };
        let hub_event_tx = self.hub_event_tx.clone();
        let task = Self::spawn_terminal_client_forwarder_runtime(
            pty_handle,
            worker,
            session_uuid,
            subscription_id,
            target_rows,
            target_cols,
            active_flag,
            snapshot_request_id,
            hub_event_tx,
            "Lua-TUI",
            "tui".to_string(),
            TerminalStreamFilter::None,
        );
        self.pty_forwarders.insert(forwarder_key, task);
        true
    }

    pub(super) fn create_lua_socket_pty_forwarder(
        &mut self,
        req: crate::lua::primitives::CreateSocketForwarderRequest,
    ) {
        let forwarder_key = format!("{}:{}", req.client_id, req.session_uuid);

        if self.try_attach_socket_terminal_forwarder(&req) {
            return;
        }

        if let Some(frame_tx) = self
            .socket_clients
            .get(&req.client_id)
            .map(crate::socket::client_conn::SocketClientConn::frame_sender)
        {
            let _guard = self.tokio_runtime.enter();
            let worker = self.spawn_socket_control_worker_adapter(req.client_id.clone(), frame_tx);
            Self::send_worker_terminal_attach_state(
                &worker,
                &req.subscription_id,
                &req.session_uuid,
                "pending",
            );
            self.terminal_client_workers
                .insert(forwarder_key.clone(), worker);
        }
        self.replace_pending_terminal_attach(
            &forwarder_key,
            PendingTerminalAttachRequest::Socket(req),
        );
    }

    pub(super) fn try_attach_socket_terminal_forwarder(
        &mut self,
        req: &crate::lua::primitives::CreateSocketForwarderRequest,
    ) -> bool {
        let forwarder_key = format!("{}:{}", req.client_id, req.session_uuid);

        let Some(session_handle) = self.handle_cache.get_session(&req.session_uuid) else {
            return false;
        };

        let pty_handle = session_handle.pty().clone();

        let Some(frame_tx) = self
            .socket_clients
            .get(&req.client_id)
            .map(crate::socket::client_conn::SocketClientConn::frame_sender)
        else {
            return false;
        };

        // Abort any existing forwarder for this key
        if let Some(old_task) = self.pty_forwarders.remove(&forwarder_key) {
            old_task.abort();
            self.unregister_terminal_forwarder_peer(&forwarder_key, false);
            self.remove_terminal_client_worker(&forwarder_key, &req.session_uuid, "Lua-Socket");
            log::debug!(
                "[Lua-Socket] Aborted existing PTY forwarder for {}",
                forwarder_key
            );
        }

        let active_terminal_peers = Arc::clone(&self.active_terminal_peers);

        let session_uuid = req.session_uuid.clone();
        let subscription_id = req.subscription_id.clone();
        let target_rows = req.rows;
        let target_cols = req.cols;
        let active_flag = Arc::clone(&req.active_flag);
        let client_id = req.client_id.clone();

        let _guard = self.tokio_runtime.enter();
        let worker = self.spawn_socket_client_worker_adapter(client_id.clone(), frame_tx.clone());
        Self::register_worker_session_io_sender(
            &worker,
            &req.session_uuid,
            pty_handle.clone(),
            "Lua-Socket",
        );
        self.terminal_client_workers
            .insert(forwarder_key.clone(), worker.clone());
        Self::send_worker_terminal_attach_state(
            &worker,
            &req.subscription_id,
            &req.session_uuid,
            "attached",
        );
        let snapshot_request_id = if pty_handle.is_session_backed() {
            let request_id = Self::next_session_io_request_id("terminal-snapshot");
            if !self.insert_pending_session_io_snapshot(
                request_id.clone(),
                crate::hub::PendingSessionIoSnapshot {
                    session_uuid: req.session_uuid.clone(),
                    started_at: Instant::now(),
                    target: crate::hub::PendingSessionIoSnapshotTarget::TerminalClientInitial {
                        worker: worker.clone(),
                        forwarder_key: forwarder_key.clone(),
                        subscription_id: req.subscription_id.clone(),
                        rows: target_rows,
                        cols: target_cols,
                        kitty_enabled: pty_handle.kitty_enabled(),
                        pty_handle: pty_handle.clone(),
                    },
                },
            ) {
                return false;
            }
            Some(request_id)
        } else {
            None
        };
        let hub_event_tx = self.hub_event_tx.clone();
        let task = Self::spawn_terminal_client_forwarder_runtime(
            pty_handle,
            worker,
            session_uuid,
            subscription_id,
            target_rows,
            target_cols,
            active_flag,
            snapshot_request_id,
            hub_event_tx,
            "Lua-Socket",
            client_id.clone(),
            TerminalStreamFilter::StripOscQueriesWhenInactive {
                active_terminal_peers,
                peer_id: client_id,
            },
        );
        self.pty_forwarders.insert(forwarder_key, task);
        true
    }

    pub(super) fn stop_lua_pty_forwarder(&mut self, forwarder_id: &str) {
        self.cleanup_pending_session_io_snapshots_for_forwarder(forwarder_id);
        if let Some(pending) = self.pending_terminal_attaches.remove(forwarder_id) {
            pending.request.deactivate();
        }
        if let Some(task) = self.pty_forwarders.remove(forwarder_id) {
            task.abort();
            self.unregister_terminal_forwarder_peer(forwarder_id, true);
            let session_uuid = forwarder_id
                .rsplit_once(':')
                .map_or(forwarder_id, |(_, session_uuid)| session_uuid)
                .to_string();
            if let Some((browser_identity, _)) = forwarder_id.rsplit_once(':') {
                if let Some(worker) = self.browser_client_workers.get(browser_identity) {
                    Self::unregister_worker_session_io_sender(worker, &session_uuid, "WebRTC");
                } else {
                    self.remove_terminal_client_worker(forwarder_id, &session_uuid, "Lua");
                }
            } else {
                self.remove_terminal_client_worker(forwarder_id, &session_uuid, "Lua");
            }
            log::debug!("[Lua] Stopped PTY forwarder {}", forwarder_id);
        }
    }

    #[cfg(test)]
    pub(super) fn poll_tui_requests(&mut self) {
        use crate::client::TuiRequest;

        let Some(ref mut rx) = self.tui_request_rx else {
            return;
        };

        // Drain into Vec to release the mutable borrow on self before
        // calling lua.call_tui_message().
        let requests: Vec<TuiRequest> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

        for request in requests {
            self.handle_tui_request(request);
        }
    }

    /// Handle one TUI request from the TuiRunner thread.
    pub fn handle_tui_request(&mut self, request: crate::client::TuiRequest) {
        use crate::client::TuiRequest;
        match request {
            TuiRequest::LuaMessage(msg) => {
                if self.handle_terminal_color_profile_message("tui", &msg) {
                    return;
                }
                if let Err(e) = self.lua.call_tui_message(msg) {
                    log::error!("[TUI] Lua message handling error: {}", e);
                }
            }
            TuiRequest::FocusChanged {
                session_uuid,
                focused,
            } => {
                self.set_active_terminal_peer(&session_uuid, "tui", focused);
                self.lua.set_pty_focused(&session_uuid, "tui", focused);
            }
            TuiRequest::PtyInput { session_uuid, data } => {
                self.lua.notify_pty_input(&session_uuid);
                let forwarder_key = format!("tui:{session_uuid}");
                if let Some(worker) = self.terminal_client_workers.get(&forwarder_key) {
                    let ingress = crate::worker::transport::TuiTransportAdapter::request_to_ingress(
                        TuiRequest::PtyInput {
                            session_uuid: session_uuid.clone(),
                            data,
                        },
                    );
                    let adapter = crate::worker::transport::TuiTransportAdapter::new();
                    let message = crate::worker::transport::TransportAdapter::ingress_to_client(
                        &adapter, ingress,
                    );
                    if let Err(e) = worker.try_send(message) {
                        log::warn!("[PTY-INPUT] Worker input queue rejected {forwarder_key}: {e}");
                    }
                } else {
                    log::warn!(
                        "[PTY-INPUT] No workerized terminal subscription for UUID {} (cache has {} agents)",
                        session_uuid,
                        self.handle_cache.len()
                    );
                }
            }
        }
    }
}
