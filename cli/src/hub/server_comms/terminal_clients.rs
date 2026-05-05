use super::terminal_stream::TerminalStreamFilter;
use super::*;

impl Hub {
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
