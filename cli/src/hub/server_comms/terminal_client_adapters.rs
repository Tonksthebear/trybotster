use super::*;

impl Hub {
    pub(super) fn spawn_tui_client_worker_adapter(
        &self,
        output_tx: tokio::sync::mpsc::UnboundedSender<crate::client::TuiOutput>,
    ) -> crate::worker::client::ClientWorkerHandle {
        use crate::worker::client::{ClientWorker, ClientWorkerConfig};
        use crate::worker::hub_control::HUB_CONTROL_QUEUE;
        use crate::worker::transport::{TransportEgress, TuiTransportAdapter};

        let (hub_control_tx, mut hub_control_rx) =
            tokio::sync::mpsc::channel(HUB_CONTROL_QUEUE.capacity);
        let (outbound_tx, mut outbound_rx) = tokio::sync::mpsc::channel::<TransportEgress>(4096);
        let wake_fd = self.tui_wake_fd;
        let hub_event_tx = self.hub_event_tx.clone();

        tokio::spawn(async move {
            while let Some(message) = hub_control_rx.recv().await {
                let _ =
                    hub_event_tx.send(crate::hub::events::HubEvent::ClientWorkerControl(message));
            }
        });

        tokio::spawn(async move {
            while let Some(egress) = outbound_rx.recv().await {
                let Some(output) = TuiTransportAdapter::egress_to_output(egress) else {
                    continue;
                };
                if output_tx.send(output).is_err() {
                    break;
                }
                if let Some(fd) = wake_fd {
                    crate::hub::wake_tui_pipe(fd);
                }
            }
        });

        let mut config = ClientWorkerConfig::new(
            crate::client::ClientId::Tui,
            hub_control_tx,
            outbound_tx,
            std::collections::HashMap::new(),
        );
        config.outbound =
            crate::worker::BoundedQueueConfig::new("worker.client.tui.outbound", 4096);
        ClientWorker::start(config)
    }

    pub(super) fn spawn_tui_control_worker_adapter(
        &self,
        output_tx: tokio::sync::mpsc::UnboundedSender<crate::client::TuiOutput>,
    ) -> crate::worker::client::ClientWorkerHandle {
        use crate::worker::client::{ClientWorker, ClientWorkerConfig};
        use crate::worker::hub_control::HUB_CONTROL_QUEUE;
        use crate::worker::transport::{TransportEgress, TuiTransportAdapter};

        let (hub_control_tx, mut hub_control_rx) =
            tokio::sync::mpsc::channel(HUB_CONTROL_QUEUE.capacity);
        let (outbound_tx, mut outbound_rx) = tokio::sync::mpsc::channel::<TransportEgress>(4096);
        let hub_event_tx = self.hub_event_tx.clone();
        let wake_fd = self.tui_wake_fd;

        tokio::spawn(async move {
            while let Some(message) = hub_control_rx.recv().await {
                let _ =
                    hub_event_tx.send(crate::hub::events::HubEvent::ClientWorkerControl(message));
            }
        });

        tokio::spawn(async move {
            while let Some(egress) = outbound_rx.recv().await {
                let Some(output) = TuiTransportAdapter::egress_to_output(egress) else {
                    continue;
                };
                if output_tx.send(output).is_err() {
                    break;
                }
                if let Some(fd) = wake_fd {
                    crate::hub::wake_tui_pipe(fd);
                }
            }
        });

        let mut config = ClientWorkerConfig::new(
            crate::client::ClientId::Tui,
            hub_control_tx,
            outbound_tx,
            std::collections::HashMap::new(),
        );
        config.outbound =
            crate::worker::BoundedQueueConfig::new("worker.client.tui.outbound", 4096);
        ClientWorker::start(config)
    }

    pub(super) fn spawn_socket_client_worker_adapter(
        &self,
        client_id: String,
        frame_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    ) -> crate::worker::client::ClientWorkerHandle {
        use crate::worker::client::{ClientWorker, ClientWorkerConfig};
        use crate::worker::hub_control::HUB_CONTROL_QUEUE;
        use crate::worker::transport::{SocketFrameAdapter, TransportEgress};

        let (hub_control_tx, mut hub_control_rx) =
            tokio::sync::mpsc::channel(HUB_CONTROL_QUEUE.capacity);
        let (outbound_tx, mut outbound_rx) = tokio::sync::mpsc::channel::<TransportEgress>(512);
        let hub_event_tx = self.hub_event_tx.clone();
        let hub_control_event_tx = self.hub_event_tx.clone();
        let disconnect_client_id = client_id.clone();

        tokio::spawn(async move {
            while let Some(message) = hub_control_rx.recv().await {
                let _ = hub_control_event_tx
                    .send(crate::hub::events::HubEvent::ClientWorkerControl(message));
            }
        });

        tokio::spawn(async move {
            while let Some(egress) = outbound_rx.recv().await {
                let Some(frame) = SocketFrameAdapter::egress_to_frame(egress) else {
                    continue;
                };
                match frame_tx.try_send(frame.encode()) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        log::warn!(
                            "[Lua-Socket] Adapter writer queue full for {}, forcing reconnect",
                            disconnect_client_id
                        );
                        let _ = hub_event_tx.send(
                            crate::hub::events::HubEvent::SocketClientDisconnected {
                                client_id: disconnect_client_id.clone(),
                            },
                        );
                        break;
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
                }
            }
        });

        ClientWorker::start(ClientWorkerConfig::new(
            crate::client::ClientId::Socket(client_id),
            hub_control_tx,
            outbound_tx,
            std::collections::HashMap::new(),
        ))
    }

    pub(super) fn spawn_socket_control_worker_adapter(
        &self,
        client_id: String,
        frame_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    ) -> crate::worker::client::ClientWorkerHandle {
        use crate::worker::client::{ClientWorker, ClientWorkerConfig};
        use crate::worker::hub_control::HUB_CONTROL_QUEUE;
        use crate::worker::transport::{SocketFrameAdapter, TransportEgress};

        let (hub_control_tx, mut hub_control_rx) =
            tokio::sync::mpsc::channel(HUB_CONTROL_QUEUE.capacity);
        let (outbound_tx, mut outbound_rx) = tokio::sync::mpsc::channel::<TransportEgress>(512);
        let hub_event_tx = self.hub_event_tx.clone();
        let hub_control_event_tx = self.hub_event_tx.clone();
        let disconnect_client_id = client_id.clone();

        tokio::spawn(async move {
            while let Some(message) = hub_control_rx.recv().await {
                let _ = hub_control_event_tx
                    .send(crate::hub::events::HubEvent::ClientWorkerControl(message));
            }
        });

        tokio::spawn(async move {
            while let Some(egress) = outbound_rx.recv().await {
                let Some(frame) = SocketFrameAdapter::egress_to_frame(egress) else {
                    continue;
                };
                match frame_tx.try_send(frame.encode()) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        let _ = hub_event_tx.send(
                            crate::hub::events::HubEvent::SocketClientDisconnected {
                                client_id: disconnect_client_id.clone(),
                            },
                        );
                        break;
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
                }
            }
        });

        ClientWorker::start(ClientWorkerConfig::new(
            crate::client::ClientId::Socket(client_id),
            hub_control_tx,
            outbound_tx,
            std::collections::HashMap::new(),
        ))
    }

    pub(super) fn spawn_webrtc_client_worker_adapter(
        &self,
        browser_identity: String,
    ) -> crate::worker::client::ClientWorkerHandle {
        use crate::worker::client::{ClientWorker, ClientWorkerConfig};
        use crate::worker::hub_control::HUB_CONTROL_QUEUE;
        use crate::worker::transport::TransportEgress;

        let (hub_control_tx, mut hub_control_rx) =
            tokio::sync::mpsc::channel(HUB_CONTROL_QUEUE.capacity);
        let (outbound_tx, mut outbound_rx) = tokio::sync::mpsc::channel::<TransportEgress>(4096);
        let hub_control_event_tx = self.hub_event_tx.clone();
        let hub_event_tx = self.hub_event_tx.clone();
        let peer_id = browser_identity.clone();

        tokio::spawn(async move {
            while let Some(message) = hub_control_rx.recv().await {
                let _ = hub_control_event_tx
                    .send(crate::hub::events::HubEvent::ClientWorkerControl(message));
            }
        });

        tokio::spawn(async move {
            while let Some(egress) = outbound_rx.recv().await {
                if hub_event_tx
                    .send(crate::hub::events::HubEvent::WebRtcClientWorkerEgress {
                        browser_identity: peer_id.clone(),
                        egress,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        let mut config = ClientWorkerConfig::new(
            crate::client::ClientId::browser(browser_identity),
            hub_control_tx,
            outbound_tx,
            std::collections::HashMap::new(),
        );
        config.outbound =
            crate::worker::BoundedQueueConfig::new("worker.client.webrtc.outbound", 4096);
        ClientWorker::start(config)
    }

    pub(super) fn register_worker_session_io_sender(
        worker: &crate::worker::client::ClientWorkerHandle,
        session_uuid: &str,
        pty_handle: crate::hub::agent_handle::PtyHandle,
        label: &'static str,
    ) {
        use crate::worker::client::ClientWorkerMessage;

        let session_io_tx = match pty_handle.session_io_sender() {
            Ok(tx) => tx,
            Err(e) => {
                #[cfg(test)]
                {
                    let _ = &e;
                    Self::register_test_worker_direct_pty_sender(
                        worker,
                        session_uuid,
                        pty_handle,
                        label,
                    );
                    return;
                }
                #[cfg(not(test))]
                {
                    log::warn!(
                        "[{label}] Session I/O worker mailbox is not ready for {session_uuid}: {e:?}"
                    );
                    return;
                }
            }
        };

        if let Err(e) = worker.try_send(ClientWorkerMessage::RegisterSessionIoSender {
            session_uuid: session_uuid.to_string(),
            tx: session_io_tx,
        }) {
            log::warn!("[{label}] Failed to register worker session I/O sender: {e}");
        }
    }

    #[cfg(test)]
    fn register_test_worker_direct_pty_sender(
        worker: &crate::worker::client::ClientWorkerHandle,
        session_uuid: &str,
        pty_handle: crate::hub::agent_handle::PtyHandle,
        label: &'static str,
    ) {
        use crate::worker::client::ClientWorkerMessage;
        use crate::worker::session_io::SessionIoRequest;

        let (session_io_tx, mut session_io_rx) =
            tokio::sync::mpsc::channel(crate::worker::session_io::SESSION_IO_WORKER_QUEUE.capacity);
        let session_uuid_for_task = session_uuid.to_string();
        tokio::spawn(async move {
            while let Some(request) = session_io_rx.recv().await {
                if let SessionIoRequest::PtyInput { data } = request {
                    if let Err(e) = pty_handle.write_input_direct(&data) {
                        log::error!("[{label}] Test PTY write failed: {e}");
                    }
                }
            }
            log::trace!("[{label}] Test session I/O sender closed for {session_uuid_for_task}");
        });

        if let Err(e) = worker.try_send(ClientWorkerMessage::RegisterSessionIoSender {
            session_uuid: session_uuid.to_string(),
            tx: session_io_tx,
        }) {
            log::warn!("[{label}] Failed to register test PTY sender: {e}");
        }
    }

    pub(super) fn unregister_worker_session_io_sender(
        worker: &crate::worker::client::ClientWorkerHandle,
        session_uuid: &str,
        label: &'static str,
    ) {
        if let Err(e) = worker.try_send(
            crate::worker::client::ClientWorkerMessage::UnregisterSessionIoSender {
                session_uuid: session_uuid.to_string(),
            },
        ) {
            log::debug!("[{label}] Failed to unregister worker session I/O sender: {e}");
        }
    }

    pub(super) fn remove_terminal_client_worker(
        &mut self,
        forwarder_key: &str,
        session_uuid: &str,
        label: &'static str,
    ) {
        if let Some(worker) = self.terminal_client_workers.remove(forwarder_key) {
            Self::unregister_worker_session_io_sender(&worker, session_uuid, label);
        }
    }

    pub(super) fn handle_transport_control_message(
        &mut self,
        message: crate::worker::hub_control::HubControlMessage,
    ) {
        use crate::worker::hub_control::{HubControlMessage, TransportSignal};

        match message {
            HubControlMessage::TransportSignalReady { signal, .. } => match signal {
                TransportSignal::Ice {
                    browser_identity,
                    envelope,
                } => {
                    self.emit_outgoing_signal(&browser_identity, envelope, "ICE candidate");
                }
                TransportSignal::Answer {
                    browser_identity,
                    envelope,
                } => {
                    if self.emit_outgoing_signal(&browser_identity, envelope, "answer") {
                        log::info!("[WebRTC] Encrypted answer sent via Lua relay (async)");
                    }
                }
            },
            HubControlMessage::TransportPeerStateChanged {
                browser_identity,
                state,
                ..
            } => {
                log::debug!(
                    "[WebRTC] Transport peer state for {}: {:?}",
                    &browser_identity[..browser_identity.len().min(8)],
                    state
                );
            }
            HubControlMessage::TransportRatchetRestartRequested {
                browser_identity, ..
            } => self.send_ratchet_bundle_refresh(&browser_identity),
            HubControlMessage::TransportBackpressure { pressure, .. } => {
                self.hub_event_metrics
                    .record_counter("webrtc_transport.backpressure", 1);
                log::debug!("[WebRTC] Transport backpressure: {:?}", pressure);
            }
            _ => {}
        }
    }

    pub(super) fn handle_client_worker_control(
        &mut self,
        message: crate::worker::hub_control::HubControlMessage,
    ) {
        use crate::client::ClientId;
        use crate::worker::hub_control::HubControlMessage;

        match message {
            HubControlMessage::AttachClient {
                client_id,
                session_uuid,
                subscription_id,
            } => {
                let forwarder_key = match &client_id {
                    ClientId::Tui => format!("tui:{session_uuid}"),
                    ClientId::Socket(client_id) => format!("{client_id}:{session_uuid}"),
                    ClientId::Browser(browser_identity) => {
                        let forwarder_key = format!("{browser_identity}:{session_uuid}");
                        let (rows, cols) = self
                            .browser_terminal_attach_sizes
                            .remove(&forwarder_key)
                            .unwrap_or((24, 80));
                        let req = crate::lua::CreateForwarderRequest {
                            peer_id: browser_identity.clone(),
                            session_uuid: session_uuid.clone(),
                            prefix: Some(vec![0x01]),
                            subscription_id,
                            rows,
                            cols,
                            active_flag: Arc::new(std::sync::Mutex::new(true)),
                        };
                        self.create_lua_pty_forwarder(req);
                        return;
                    }
                    ClientId::Internal => return,
                };
                self.register_terminal_forwarder_peer(
                    &forwarder_key,
                    &session_uuid,
                    &client_id.to_string(),
                );
            }
            HubControlMessage::DetachClient {
                client_id,
                session_uuid,
                ..
            } => {
                let forwarder_key = match client_id {
                    ClientId::Tui => format!("tui:{session_uuid}"),
                    ClientId::Socket(client_id) => format!("{client_id}:{session_uuid}"),
                    ClientId::Browser(browser_identity) => {
                        format!("{browser_identity}:{session_uuid}")
                    }
                    ClientId::Internal => return,
                };
                self.stop_lua_pty_forwarder(&forwarder_key);
            }
            HubControlMessage::Backpressure(backpressure) => {
                self.hub_event_metrics
                    .record_counter("client_worker.backpressure", 1);
                if backpressure.source == "worker.client.session_io_missing" {
                    self.hub_event_metrics
                        .record_counter("client_worker.session_io_missing", 1);
                }
                log::warn!("[ClientWorker] Backpressure: {backpressure:?}");
            }
            HubControlMessage::Reconnect { .. }
            | HubControlMessage::SessionLifecycle { .. }
            | HubControlMessage::Shutdown { .. } => {
                log::trace!("[ClientWorker] Hub-control request: {message:?}");
            }
            HubControlMessage::TransportBackpressure { .. }
            | HubControlMessage::TransportPeerStateChanged { .. }
            | HubControlMessage::TransportSignalReady { .. }
            | HubControlMessage::TransportRatchetRestartRequested { .. } => {
                self.handle_transport_control_message(message);
            }
        }
    }
}
