use super::*;

impl Hub {
    /// Remove paste files tracked for a closed session.
    pub fn cleanup_paste_files(&mut self, session_uuid: &str) {
        if let Some(files) = self.paste_files.remove(session_uuid) {
            for path in &files {
                if let Err(e) = std::fs::remove_file(path) {
                    log::warn!(
                        "[FILE-INPUT] Failed to clean up paste file {}: {e}",
                        path.display()
                    );
                }
            }
            if !files.is_empty() {
                log::info!(
                    "[FILE-INPUT] Cleaned up {} paste file(s) for {session_uuid}",
                    files.len()
                );
            }
        }
    }

    pub(super) fn handle_session_io_event(
        &mut self,
        event: crate::worker::session_io::SessionIoEvent,
    ) {
        use crate::worker::session_io::SessionIoEvent;

        match event {
            SessionIoEvent::PasteFileWritten {
                session_uuid,
                path,
                bytes,
                ..
            } => {
                log::info!(
                    "[FILE-INPUT] Wrote {} bytes to {} (session={})",
                    bytes,
                    path.display(),
                    session_uuid,
                );
                self.paste_files.entry(session_uuid).or_default().push(path);
            }
            SessionIoEvent::PasteFileFailed {
                session_uuid,
                reason,
                detail,
                ..
            } => {
                log::error!(
                    "[FILE-INPUT] Paste failed for session {} reason={reason:?}: {detail}",
                    session_uuid
                );
            }
            SessionIoEvent::PreparedSnapshot {
                request_id,
                session_uuid,
                uncompressed_len,
                payload,
                recovery,
            } => {
                self.route_prepared_session_io_snapshot(
                    request_id,
                    session_uuid,
                    uncompressed_len,
                    payload,
                    recovery,
                );
            }
            SessionIoEvent::Snapshot {
                request_id,
                session_uuid,
                payload,
            } => {
                self.route_terminal_client_initial_snapshot(request_id, session_uuid, payload);
            }
            _ => {}
        }
    }
}
