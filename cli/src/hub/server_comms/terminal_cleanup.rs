use super::*;

impl Hub {
    pub(super) fn stop_terminal_subscription(&mut self, subscription_key: &str) {
        self.stop_terminal_subscription_with_worker_cleanup(subscription_key, true);
    }

    pub(super) fn replace_terminal_subscription(&mut self, subscription_key: &str) {
        self.stop_terminal_subscription_with_worker_cleanup(subscription_key, false);
    }

    fn stop_terminal_subscription_with_worker_cleanup(
        &mut self,
        subscription_key: &str,
        unregister_browser_worker: bool,
    ) {
        self.cleanup_pending_session_io_snapshots_for_subscription(subscription_key);
        if let Some(pending) = self.pending_terminal_attaches.remove(subscription_key) {
            pending.request.deactivate();
        }
        let tracked = self
            .terminal_subscription_peers
            .get(subscription_key)
            .cloned();
        if tracked.is_some() || self.terminal_client_workers.contains_key(subscription_key) {
            let session_uuid = tracked
                .as_ref()
                .map(|(session_uuid, _)| session_uuid.clone())
                .unwrap_or_else(|| {
                    subscription_key
                        .rsplit_once(':')
                        .map_or(subscription_key, |(_, session_uuid)| session_uuid)
                        .to_string()
                });
            if let Some(session_handle) = self.handle_cache.get_session(&session_uuid) {
                let _ = session_handle.pty().enqueue_session_io_request(
                    crate::worker::session_io::SessionIoRequest::UnsubscribeTerminal {
                        subscription_key: subscription_key.to_string(),
                    },
                );
            }
            self.unregister_terminal_subscription_peer(subscription_key, true);
            if let Some((browser_identity, _)) = subscription_key.rsplit_once(':') {
                if self.browser_client_workers.contains_key(browser_identity) {
                    if unregister_browser_worker {
                        if let Some(worker) = self.browser_client_workers.get(browser_identity) {
                            Self::unregister_worker_session_io_sender(
                                worker,
                                &session_uuid,
                                "WebRTC",
                            );
                        }
                    }
                } else {
                    self.remove_terminal_client_worker(subscription_key, &session_uuid, "Terminal");
                }
            } else {
                self.remove_terminal_client_worker(subscription_key, &session_uuid, "Terminal");
            }
            log::debug!(
                "[Terminal] Stopped terminal subscription {}",
                subscription_key
            );
        }
    }
}
