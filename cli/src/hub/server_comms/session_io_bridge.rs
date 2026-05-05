use super::*;

impl Hub {
    /// Route browser PTY input through the workerized session I/O path.
    pub fn handle_pty_input(&mut self, input: crate::channel::webrtc::PtyInputIncoming) {
        if input.data == b"\x1b[I" {
            self.set_active_terminal_peer(&input.session_uuid, &input.browser_identity, true);
            self.lua
                .set_pty_focused(&input.session_uuid, &input.browser_identity, true);
            // Color profile is now sent by the browser as a JSON message
            // after snapshot load — no need to inject OSC probe bytes.
        } else if input.data == b"\x1b[O" {
            self.set_active_terminal_peer(&input.session_uuid, &input.browser_identity, false);
            self.lua
                .set_pty_focused(&input.session_uuid, &input.browser_identity, false);
        }

        self.learn_terminal_probe_replies(
            &input.session_uuid,
            &input.browser_identity,
            &input.data,
        );
        self.lua.notify_pty_input(&input.session_uuid);

        if let Some(worker) = self.browser_client_workers.get(&input.browser_identity) {
            let message = crate::worker::transport::ingress_to_client_message(
                crate::worker::transport::TransportIngress::TerminalInput {
                    session_uuid: input.session_uuid,
                    data: input.data,
                },
            );
            if let Err(e) = worker.try_send(message) {
                log::warn!(
                    "[WebRTC] Browser worker input queue rejected for {}: {e}",
                    &input.browser_identity[..input.browser_identity.len().min(8)]
                );
            }
        } else {
            log::warn!(
                "[WebRTC] No browser worker for {}",
                &input.browser_identity[..input.browser_identity.len().min(8)]
            );
        }
    }

    /// Route browser file paste input through the SessionIoWorker mailbox.
    pub fn handle_file_input(&mut self, file: crate::channel::webrtc::FileInputIncoming) {
        let Some(session_handle) = self.handle_cache.get_session(&file.session_uuid) else {
            log::warn!(
                "[FILE-INPUT] Dropping paste for missing session {}",
                file.session_uuid
            );
            return;
        };

        let request_id = Self::next_session_io_request_id("paste");
        let session_uuid = file.session_uuid.clone();
        if let Err(e) = session_handle.pty().enqueue_session_io_request(
            crate::worker::session_io::SessionIoRequest::PasteFile {
                request_id,
                filename: file.filename,
                data: file.data,
            },
        ) {
            log::error!(
                "[FILE-INPUT] Paste enqueue failed for session {} reason={e:?}",
                session_uuid
            );
        }
    }

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
