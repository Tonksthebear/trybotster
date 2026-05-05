use super::*;

impl Hub {
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
}
