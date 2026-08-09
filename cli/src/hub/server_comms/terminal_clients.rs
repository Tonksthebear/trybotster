use super::terminal_stream::{
    TerminalClientSubscription, TerminalInitialSnapshot, TerminalStreamFilter,
};
use super::*;

impl Hub {
    pub(super) fn create_tui_terminal_subscription(
        &mut self,
        req: crate::lua::primitives::TuiTerminalSubscriptionRequest,
    ) {
        let subscription_key = format!("tui:{}", req.session_uuid);

        if self.try_attach_tui_terminal_subscription(&req) {
            return;
        }

        // Pending is for missing sessions or SessionIo reader reconnect.
        if !self.should_queue_pending_terminal_attach(&req.session_uuid) {
            log::debug!(
                "[TUI] Not pending terminal attach for {} — session present but attach failed",
                subscription_key
            );
            return;
        }

        let attach_state = if self.is_session_reconnect_pending(&req.session_uuid) {
            "reconnecting"
        } else {
            "pending"
        };
        if let Some(output_tx) = self.tui_output_tx.clone() {
            let _guard = self.tokio_runtime.enter();
            let worker = self.spawn_tui_control_worker_adapter(output_tx);
            Self::send_worker_terminal_attach_state(
                &worker,
                &req.subscription_id,
                &req.session_uuid,
                attach_state,
            );
            self.terminal_client_workers
                .insert(subscription_key.clone(), worker);
        }
        self.replace_pending_terminal_attach(
            &subscription_key,
            PendingTerminalAttachRequest::Tui(req),
        );
    }

    pub(super) fn try_attach_tui_terminal_subscription(
        &mut self,
        req: &crate::lua::primitives::TuiTerminalSubscriptionRequest,
    ) -> bool {
        let subscription_key = format!("tui:{}", req.session_uuid);

        // Check if session exists
        let Some(session_handle) = self.handle_cache.get_session(&req.session_uuid) else {
            return false;
        };

        let pty_handle = session_handle.pty().clone();

        let Some(output_tx) = self.tui_output_tx.clone() else {
            return false;
        };

        // Stop any existing subscription for this key.
        if self
            .terminal_subscription_peers
            .contains_key(&subscription_key)
            || self.terminal_client_workers.contains_key(&subscription_key)
        {
            self.stop_terminal_subscription(&subscription_key);
            log::debug!(
                "[TUI] Replaced terminal subscription for {}",
                subscription_key
            );
        }

        let session_uuid = req.session_uuid.clone();
        let subscription_id = req.subscription_id.clone();
        let target_rows = req.rows;
        let target_cols = req.cols;
        let _guard = self.tokio_runtime.enter();
        let worker = self.spawn_tui_client_worker_adapter(output_tx);
        if let Ok(mut routes) = self.tui_session_input_routes.lock() {
            routes.insert(req.session_uuid.clone(), worker.clone());
        }
        self.terminal_client_workers
            .insert(subscription_key.clone(), worker.clone());

        self.start_terminal_client_subscription(TerminalClientSubscription {
            pty_handle: pty_handle.clone(),
            worker,
            session_uuid,
            subscription_id,
            rows: target_rows,
            cols: target_cols,
            log_prefix: "TUI",
            client_label: "tui".to_string(),
            output_prefix: Vec::new(),
            filter: TerminalStreamFilter::None,
            initial_snapshot: TerminalInitialSnapshot::Raw { subscription_key },
            confirm_subscription: false,
        })
    }

    pub(super) fn create_socket_terminal_subscription(
        &mut self,
        req: crate::lua::primitives::SocketTerminalSubscriptionRequest,
    ) {
        let subscription_key = format!("{}:{}", req.client_id, req.session_uuid);

        if self.try_attach_socket_terminal_subscription(&req) {
            return;
        }

        // Pending is for missing sessions or SessionIo reader reconnect.
        if !self.should_queue_pending_terminal_attach(&req.session_uuid) {
            log::debug!(
                "[Socket] Not pending terminal attach for {} — session present but attach failed",
                subscription_key
            );
            return;
        }

        let attach_state = if self.is_session_reconnect_pending(&req.session_uuid) {
            "reconnecting"
        } else {
            "pending"
        };
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
                attach_state,
            );
            self.terminal_client_workers
                .insert(subscription_key.clone(), worker);
        }
        self.replace_pending_terminal_attach(
            &subscription_key,
            PendingTerminalAttachRequest::Socket(req),
        );
    }

    pub(super) fn try_attach_socket_terminal_subscription(
        &mut self,
        req: &crate::lua::primitives::SocketTerminalSubscriptionRequest,
    ) -> bool {
        let subscription_key = format!("{}:{}", req.client_id, req.session_uuid);

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

        // Stop any existing subscription for this key.
        if self
            .terminal_subscription_peers
            .contains_key(&subscription_key)
            || self.terminal_client_workers.contains_key(&subscription_key)
        {
            self.stop_terminal_subscription(&subscription_key);
            log::debug!(
                "[Socket] Replaced terminal subscription for {}",
                subscription_key
            );
        }

        let active_terminal_peers = Arc::clone(&self.active_terminal_peers);

        let session_uuid = req.session_uuid.clone();
        let subscription_id = req.subscription_id.clone();
        let target_rows = req.rows;
        let target_cols = req.cols;
        let client_id = req.client_id.clone();

        let _guard = self.tokio_runtime.enter();
        let worker = self.spawn_socket_client_worker_adapter(client_id.clone(), frame_tx.clone());
        if let Some(conn) = self.socket_clients.get(&client_id) {
            conn.register_session_input_route(req.session_uuid.clone(), worker.clone());
        }
        self.terminal_client_workers
            .insert(subscription_key.clone(), worker.clone());

        self.start_terminal_client_subscription(TerminalClientSubscription {
            pty_handle: pty_handle.clone(),
            worker,
            session_uuid,
            subscription_id,
            rows: target_rows,
            cols: target_cols,
            log_prefix: "Socket",
            client_label: client_id.clone(),
            output_prefix: Vec::new(),
            filter: TerminalStreamFilter::StripOscQueriesWhenInactive {
                active_terminal_peers,
                peer_id: client_id,
            },
            initial_snapshot: TerminalInitialSnapshot::Raw { subscription_key },
            confirm_subscription: false,
        })
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
        }
    }
}
