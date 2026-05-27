use super::terminal_stream::{
    TerminalClientSubscription, TerminalInitialSnapshot, TerminalStreamFilter,
};
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
    pub(super) fn try_attach_browser_terminal_subscription(
        &mut self,
        req: &crate::lua::BrowserTerminalSubscriptionRequest,
    ) -> bool {
        let subscription_key = format!("{}:{}", req.peer_id, req.session_uuid);

        let Some(session_handle) = self.handle_cache.get_session(&req.session_uuid) else {
            return false;
        };

        let pty_handle = session_handle.pty().clone();

        let Some(worker) = self.browser_client_workers.get(&req.peer_id).cloned() else {
            log::warn!(
                "[WebRTC] Cannot attach terminal for peer {} without browser worker",
                &req.peer_id[..req.peer_id.len().min(8)]
            );
            return false;
        };

        if self
            .terminal_subscription_peers
            .contains_key(&subscription_key)
        {
            if self.terminal_subscription_id(&subscription_key)
                != Some(req.subscription_id.as_str())
            {
                self.replace_terminal_subscription(&subscription_key);
            } else {
                let _ = pty_handle.enqueue_session_io_request(
                    crate::worker::session_io::SessionIoRequest::Resize {
                        rows: req.rows,
                        cols: req.cols,
                    },
                );
                let _ = worker.try_send(crate::worker::client::ClientWorkerMessage::ControlFrame(
                    crate::worker::client::ClientControlFrame::BoundaryJson(serde_json::json!({
                        "type": "subscribed",
                        "subscriptionId": req.subscription_id.clone(),
                    })),
                ));

                // Item 3: reattach mode replay (on top of Item 2 producer)
                // On reused-subscription reattach, emit current mode state (full sparse
                // ModeChanged from live ModeFlags) *before* "attached" so the client
                // receives mode state as part of the attach barrier.
                if let Some(flags) = pty_handle.get_mode_flags() {
                    // Convert live ModeFlags into full sparse ModeChanged for reattach replay.
                    // Every current field is Some(...) so the client receives complete state.
                    let mode = crate::session::protocol::ModeChanged {
                        kitty_enabled: Some(flags.kitty_enabled),
                        cursor_visible: Some(flags.cursor_visible),
                        bracketed_paste: Some(flags.bracketed_paste),
                        mouse_mode: Some(flags.mouse_mode),
                        alt_screen: Some(flags.alt_screen),
                        focus_reporting: Some(flags.focus_reporting),
                        application_cursor: Some(flags.application_cursor),
                    };
                    let _ = worker.try_send(crate::worker::client::ClientWorkerMessage::ControlFrame(
                        crate::worker::client::ClientControlFrame::ModeChanged {
                            session_uuid: req.session_uuid.clone(),
                            mode,
                        },
                    ));
                }

                Self::send_worker_terminal_attach_state(
                    &worker,
                    &req.subscription_id,
                    &req.session_uuid,
                    "attached",
                );

                log::debug!(
                    "[WebRTC] Reused active terminal subscription for {} resize={}x{}",
                    subscription_key,
                    req.cols,
                    req.rows
                );
                return true;
            }
        }

        // Stop any existing subscription for this key.
        if self
            .terminal_subscription_peers
            .contains_key(&subscription_key)
        {
            self.replace_terminal_subscription(&subscription_key);
            log::debug!(
                "[WebRTC] Replaced terminal subscription for {}",
                subscription_key
            );
        }

        let peer_id = req.peer_id.clone();
        let session_uuid = req.session_uuid.clone();
        let target_rows = req.rows;
        let target_cols = req.cols;
        let prefix = req.prefix.clone().unwrap_or_else(|| vec![0x01]);
        let active_terminal_peers = Arc::clone(&self.active_terminal_peers);
        let subscription_id = req.subscription_id.clone();

        let _guard = self.tokio_runtime.enter();
        let attached = self.start_terminal_client_subscription(TerminalClientSubscription {
            pty_handle: pty_handle.clone(),
            worker,
            session_uuid,
            subscription_id,
            rows: target_rows,
            cols: target_cols,
            log_prefix: "WebRTC",
            client_label: peer_id.clone(),
            output_prefix: prefix,
            filter: TerminalStreamFilter::StripOscQueriesWhenInactive {
                active_terminal_peers,
                peer_id: peer_id.clone(),
            },
            initial_snapshot: TerminalInitialSnapshot::PrefixedGzip {
                subscription_key: subscription_key.clone(),
            },
            confirm_subscription: true,
        });

        if attached {
            self.register_terminal_subscription_peer(
                &subscription_key,
                &req.session_uuid,
                &req.peer_id,
            );
            self.register_terminal_subscription_id(&subscription_key, &req.subscription_id);
        }
        attached
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
            PendingTerminalAttachRequest::WebRtc(req) => {
                self.try_attach_browser_terminal_subscription(req)
            }
            PendingTerminalAttachRequest::Tui(req) => {
                self.try_attach_tui_terminal_subscription(req)
            }
            PendingTerminalAttachRequest::Socket(req) => {
                self.try_attach_socket_terminal_subscription(req)
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
                let subscription_key = format!("tui:{}", req.session_uuid);
                if let Some(worker) = self.terminal_client_workers.get(&subscription_key) {
                    Self::send_worker_terminal_attach_state(
                        worker,
                        &req.subscription_id,
                        &req.session_uuid,
                        state,
                    );
                }
            }
            PendingTerminalAttachRequest::Socket(req) => {
                let subscription_key = format!("{}:{}", req.client_id, req.session_uuid);
                if let Some(worker) = self.terminal_client_workers.get(&subscription_key) {
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
        subscription_key: &str,
        request: PendingTerminalAttachRequest,
    ) {
        if let Some(prev) = self.pending_terminal_attaches.remove(subscription_key) {
            prev.request.deactivate();
        }

        self.pending_terminal_attaches.insert(
            subscription_key.to_string(),
            PendingTerminalAttach {
                request,
                requested_at: Instant::now(),
            },
        );
    }

    pub(super) fn create_browser_terminal_subscription(
        &mut self,
        req: crate::lua::BrowserTerminalSubscriptionRequest,
    ) {
        let subscription_key = format!("{}:{}", req.peer_id, req.session_uuid);

        if self.try_attach_browser_terminal_subscription(&req) {
            return;
        }

        self.replace_pending_terminal_attach(
            &subscription_key,
            PendingTerminalAttachRequest::WebRtc(req.clone()),
        );
        self.send_terminal_attach_state(
            &req.peer_id,
            &req.subscription_id,
            &req.session_uuid,
            "pending",
        );
    }
}
