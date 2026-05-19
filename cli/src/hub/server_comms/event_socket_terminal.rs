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
        let label = socket_message_label(&msg);
        if msg.get("type").and_then(|v| v.as_str()) == Some("debug_memory") {
            let mut data = self.debug_memory_diagnostics();
            if let Some(request_id) = msg.get("request_id").cloned() {
                data["request_id"] = request_id;
            }
            if let Some(conn) = self.socket_clients.get(&client_id) {
                conn.send_frame(&crate::socket::framing::Frame::Json(data));
            }
            self.record_hot_span("socket_message.debug_memory", Instant::now(), bytes, &label);
        } else if msg.get("type").and_then(|v| v.as_str()) == Some("focus_changed") {
            let started = Instant::now();
            if let Some(session_uuid) = msg.get("session_uuid").and_then(|v| v.as_str()) {
                let focused = msg
                    .get("focused")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                self.set_active_terminal_peer(session_uuid, &client_id, focused);
                self.lua.set_pty_focused(session_uuid, &client_id, focused);
            }
            self.record_hot_span("socket_message.focus_changed", started, bytes, &label);
        } else {
            let started = Instant::now();
            if self.handle_terminal_color_profile_message(&client_id, &msg) {
                self.record_hot_span(
                    "socket_message.terminal_color_profile",
                    started,
                    bytes,
                    &label,
                );
            } else if let Err(e) = self.lua.call_socket_message(&client_id, msg) {
                self.hub_event_metrics
                    .record_counter("socket_message.error", 1);
                log::error!("[Socket] Lua message handling error for {}: {e}", client_id);
                self.record_hot_span("socket_message.lua", started, bytes, &label);
            } else {
                self.record_hot_span("socket_message.lua", started, bytes, &label);
            }
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
            PtyRequest::SubscribeBrowserTerminal(req) => {
                self.create_browser_terminal_subscription(req);
            }
            PtyRequest::RefreshSnapshot(req) => {
                self.refresh_lua_terminal_snapshot(req);
            }
            PtyRequest::SubscribeTuiTerminal(req) => {
                self.create_tui_terminal_subscription(req);
            }
            PtyRequest::SubscribeSocketTerminal(req) => {
                self.create_socket_terminal_subscription(req);
            }
            PtyRequest::StopTerminalSubscription { subscription_key } => {
                self.stop_terminal_subscription(&subscription_key);
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

fn socket_message_label(msg: &serde_json::Value) -> String {
    let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("-");
    if msg.get("_mcp_rid").is_some() {
        return format!("rpc:{msg_type}");
    }
    if msg_type == "subscribe" {
        let channel = msg.get("channel").and_then(|v| v.as_str()).unwrap_or("-");
        return format!("sub:{channel}");
    }
    if let Some(data_type) = msg
        .get("data")
        .and_then(|v| v.get("type"))
        .and_then(|v| v.as_str())
    {
        return format!("{msg_type}:{data_type}");
    }
    msg_type.to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::socket_message_label;

    #[test]
    fn socket_message_label_identifies_rpc_and_subscription_routes() {
        assert_eq!(
            socket_message_label(&json!({"_mcp_rid": "rid-1", "type": "get_pty_snapshot"})),
            "rpc:get_pty_snapshot"
        );
        assert_eq!(
            socket_message_label(&json!({"type": "subscribe", "channel": "mcp"})),
            "sub:mcp"
        );
        assert_eq!(
            socket_message_label(
                &json!({"type": "message", "subscriptionId": "sub-1", "data": {"type": "tool_call"}})
            ),
            "message:tool_call"
        );
    }
}
