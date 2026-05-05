use super::*;

impl Hub {
    pub(super) fn handle_tui_send_event(
        &mut self,
        send_req: crate::lua::primitives::TuiSendRequest,
    ) {
        use crate::client::TuiOutput;
        use crate::lua::primitives::TuiSendRequest;

        let Some(ref tx) = self.tui_output_tx else {
            return;
        };

        match send_req {
            TuiSendRequest::Json { data } => {
                let _ = tx.send(TuiOutput::Message(data));
            }
            TuiSendRequest::Binary { data } => {
                let _ = tx.send(TuiOutput::Binary(data));
            }
        }
        self.wake_tui();
    }

    pub(super) fn handle_socket_client_connected_event(
        &mut self,
        client_id: String,
        conn: crate::socket::client_conn::SocketClientConn,
    ) {
        log::info!("[Socket] Registering client: {}", client_id);
        self.socket_clients.insert(client_id.clone(), conn);
        if let Err(e) = self.lua.call_socket_client_connected(&client_id) {
            log::warn!("[Socket] Lua client_connected callback error: {e}");
        }
    }

    pub(super) fn handle_socket_client_disconnected_event(&mut self, client_id: String) {
        log::info!("[Socket] Unregistering client: {}", client_id);
        if let Some(conn) = self.socket_clients.remove(&client_id) {
            conn.disconnect();
        }
        self.unregister_terminal_client_peer(&client_id, true);
        let client_prefix = format!("{client_id}:");
        let worker_keys: Vec<String> = self
            .terminal_client_workers
            .keys()
            .filter(|key| key.starts_with(&client_prefix))
            .cloned()
            .collect();
        for key in worker_keys {
            if let Some(session_uuid) = key.strip_prefix(&client_prefix).map(str::to_owned) {
                self.remove_terminal_client_worker(&key, &session_uuid, "Socket");
            }
        }
        self.pty_forwarders.retain(|key, task| {
            if key.starts_with(&client_prefix) {
                task.abort();
                log::debug!("[Socket] Aborted PTY forwarder: {}", key);
                false
            } else {
                true
            }
        });
        self.pending_terminal_attaches.retain(|key, intent| {
            if key.starts_with(&client_prefix) {
                intent.request.deactivate();
                log::debug!("[Socket] Dropped pending terminal attach intent: {}", key);
                false
            } else {
                true
            }
        });
        if let Err(e) = self.lua.call_socket_client_disconnected(&client_id) {
            log::warn!("[Socket] Lua client_disconnected callback error: {e}");
        }
    }

    pub(super) fn handle_socket_message_event(
        &mut self,
        client_id: String,
        msg: serde_json::Value,
    ) {
        let bytes = serde_json::to_vec(&msg).map_or(0, |v| v.len());
        if msg.get("type").and_then(|v| v.as_str()) == Some("focus_changed") {
            let started = Instant::now();
            if let Some(session_uuid) = msg.get("session_uuid").and_then(|v| v.as_str()) {
                let focused = msg
                    .get("focused")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                self.set_active_terminal_peer(session_uuid, &client_id, focused);
                self.lua.set_pty_focused(session_uuid, &client_id, focused);
            }
            self.record_hot_span("socket_message.focus_changed", started, bytes, &client_id);
        } else {
            let started = Instant::now();
            if self.handle_terminal_color_profile_message(&client_id, &msg) {
                self.record_hot_span(
                    "socket_message.terminal_color_profile",
                    started,
                    bytes,
                    &client_id,
                );
            } else if let Err(e) = self.lua.call_socket_message(&client_id, msg) {
                self.hub_event_metrics
                    .record_counter("socket_message.error", 1);
                log::error!("[Socket] Lua message handling error for {}: {e}", client_id);
                self.record_hot_span("socket_message.lua", started, bytes, &client_id);
            } else {
                self.record_hot_span("socket_message.lua", started, bytes, &client_id);
            }
        }
    }

    pub(super) fn handle_socket_pty_input_event(
        &mut self,
        client_id: String,
        session_uuid: String,
        data: Vec<u8>,
    ) {
        if data == b"\x1b[I" {
            self.set_active_terminal_peer(&session_uuid, &client_id, true);
            self.lua.set_pty_focused(&session_uuid, &client_id, true);
        } else if data == b"\x1b[O" {
            self.set_active_terminal_peer(&session_uuid, &client_id, false);
            self.lua.set_pty_focused(&session_uuid, &client_id, false);
        }
        self.learn_terminal_probe_replies(&session_uuid, &client_id, &data);
        self.lua.notify_pty_input(&session_uuid);

        let forwarder_key = format!("{client_id}:{session_uuid}");
        if let Some(worker) = self.terminal_client_workers.get(&forwarder_key) {
            let ingress = crate::worker::transport::SocketFrameAdapter::frame_to_ingress(
                crate::socket::framing::Frame::PtyInput {
                    session_uuid: session_uuid.clone(),
                    data,
                },
            )
            .expect("PtyInput frame maps to worker ingress");
            let adapter = crate::worker::transport::SocketFrameAdapter::new(client_id);
            let message =
                crate::worker::transport::TransportAdapter::ingress_to_client(&adapter, ingress);
            if let Err(e) = worker.try_send(message) {
                log::warn!("[Socket] Worker input queue rejected {forwarder_key}: {e}");
            }
        } else {
            log::warn!("[Socket] No workerized terminal subscription for {forwarder_key}");
        }
    }

    pub(super) fn handle_socket_send_event(
        &mut self,
        send_req: crate::lua::primitives::SocketSendRequest,
    ) {
        use crate::lua::primitives::SocketSendRequest;
        use crate::socket::framing::Frame;

        match send_req {
            SocketSendRequest::Json { client_id, data } => {
                if let Some(conn) = self.socket_clients.get(&client_id) {
                    conn.send_frame(&Frame::Json(data));
                } else {
                    log::debug!("[Socket] Send to unknown client: {}", client_id);
                }
            }
            SocketSendRequest::Binary { client_id, data } => {
                if let Some(conn) = self.socket_clients.get(&client_id) {
                    conn.send_frame(&Frame::Binary(data));
                } else {
                    log::debug!("[Socket] Binary send to unknown client: {}", client_id);
                }
            }
        }
    }

    pub(super) fn handle_lua_pty_request_event(&mut self, request: crate::lua::PtyRequest) {
        use crate::lua::PtyRequest;

        match request {
            PtyRequest::CreateForwarder(req) => {
                self.create_lua_pty_forwarder(req);
            }
            PtyRequest::RefreshSnapshot(req) => {
                self.refresh_lua_terminal_snapshot(req);
            }
            PtyRequest::CreateTuiForwarder(req) => {
                self.create_lua_tui_pty_forwarder(req);
            }
            PtyRequest::CreateSocketForwarder(req) => {
                self.create_lua_socket_pty_forwarder(req);
            }
            PtyRequest::StopForwarder { forwarder_id } => {
                self.stop_lua_pty_forwarder(&forwarder_id);
            }
            PtyRequest::WritePty { session_uuid, data } => {
                if let Some(session_handle) = self.handle_cache.get_session(&session_uuid) {
                    if let Err(e) = session_handle.pty().enqueue_session_io_request(
                        crate::worker::session_io::SessionIoRequest::PtyInput { data },
                    ) {
                        log::error!("[PTY-WRITE] Session I/O enqueue failed: {e:?}");
                    }
                } else {
                    log::warn!("[PTY-WRITE] No session '{}'", session_uuid);
                }
            }
            PtyRequest::ResizePty {
                session_uuid,
                rows,
                cols,
            } => {
                if let Some(session_handle) = self.handle_cache.get_session(&session_uuid) {
                    if let Err(e) = session_handle.pty().enqueue_session_io_request(
                        crate::worker::session_io::SessionIoRequest::Resize { rows, cols },
                    ) {
                        log::error!("[PTY-RESIZE] Session I/O enqueue failed: {e:?}");
                    }
                } else {
                    log::debug!("[Lua] No session '{}'", session_uuid);
                }
            }
            PtyRequest::SpawnNotificationWatcher {
                watcher_key,
                session_uuid,
                session_name,
                observe_output,
                event_tx,
            } => {
                self.spawn_notification_watcher(
                    watcher_key,
                    session_uuid,
                    session_name,
                    observe_output,
                    event_tx,
                );
            }
        }
    }
}
