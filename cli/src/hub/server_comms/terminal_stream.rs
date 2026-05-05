use super::terminal_snapshot::SnapshotAttachState;
use super::*;

pub(super) enum TerminalStreamFilter {
    None,
    StripOscQueriesWhenInactive {
        active_terminal_peers: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
        peer_id: String,
    },
}

impl TerminalStreamFilter {
    fn filter_chunk(
        &self,
        session_uuid: &str,
        query_filter_buffer: &mut Vec<u8>,
        data: Vec<u8>,
    ) -> Vec<u8> {
        match self {
            Self::None => data,
            Self::StripOscQueriesWhenInactive {
                active_terminal_peers,
                peer_id,
            } => {
                if active_terminal_peers
                    .lock()
                    .ok()
                    .and_then(|active| active.get(session_uuid).cloned())
                    .is_some_and(|active_peer| active_peer != peer_id.as_str())
                {
                    crate::hub::terminal_profile::strip_osc_queries_from_output(
                        query_filter_buffer,
                        &data,
                    )
                } else {
                    query_filter_buffer.clear();
                    data
                }
            }
        }
    }
}

impl Hub {
    pub(super) fn spawn_terminal_client_forwarder_runtime(
        pty_handle: crate::hub::agent_handle::PtyHandle,
        worker: crate::worker::client::ClientWorkerHandle,
        session_uuid: String,
        subscription_id: String,
        target_rows: u16,
        target_cols: u16,
        active_flag: Arc<std::sync::Mutex<bool>>,
        snapshot_request_id: Option<String>,
        hub_event_tx: crate::hub::events::HubEventTx,
        log_prefix: &'static str,
        client_label: String,
        filter: TerminalStreamFilter,
    ) -> tokio::task::JoinHandle<()> {
        let pty_for_snapshot = pty_handle.clone();

        tokio::spawn(async move {
            use crate::agent::pty::PtyEvent;
            use crate::worker::client::{
                ClientControlFrame, ClientWorkerMessage, TerminalAttachState,
            };

            log::info!(
                "[{}] Started PTY forwarder for {} session {}",
                log_prefix,
                client_label,
                session_uuid
            );
            let _ = worker
                .send(ClientWorkerMessage::SubscribeSession {
                    session_uuid: session_uuid.clone(),
                    subscription_id: subscription_id.clone(),
                })
                .await;
            let mut query_filter_buffer = Vec::new();

            let mut pty_rx = if let Some(request_id) = snapshot_request_id {
                let pty_rx = pty_handle.subscribe();
                let resize_result = pty_handle.enqueue_session_io_request(
                    crate::worker::session_io::SessionIoRequest::Resize {
                        rows: target_rows,
                        cols: target_cols,
                    },
                );
                let snapshot_result = pty_handle.enqueue_session_io_request(
                    crate::worker::session_io::SessionIoRequest::GetSnapshot {
                        request_id: request_id.clone(),
                    },
                );

                if resize_result.is_err() || snapshot_result.is_err() {
                    log::warn!(
                        "[{}] Session I/O snapshot request failed for {} session {}: resize={:?} snapshot={:?}",
                        log_prefix,
                        client_label,
                        session_uuid,
                        resize_result.err(),
                        snapshot_result.err()
                    );
                    let _ = hub_event_tx.send(
                        crate::hub::events::HubEvent::DropPendingSessionIoSnapshot { request_id },
                    );
                    let _ = worker
                        .send(ClientWorkerMessage::ControlFrame(
                            ClientControlFrame::TerminalAttach {
                                subscription_id: subscription_id.clone(),
                                session_uuid: session_uuid.clone(),
                                state: TerminalAttachState::NotReady,
                            },
                        ))
                        .await;
                    let _ = worker
                        .send(ClientWorkerMessage::UnregisterSessionIoSender {
                            session_uuid: session_uuid.clone(),
                        })
                        .await;
                    return;
                }

                pty_rx
            } else {
                let (snapshot, kitty_enabled, snapshot_rows, snapshot_cols, pty_rx) =
                    match tokio::task::spawn_blocking(move || {
                        pty_for_snapshot.snapshot_and_subscribe()
                    })
                    .await
                    {
                        Ok(result) => result,
                        Err(e) => {
                            log::warn!(
                                "[{}] Snapshot fetch task failed for {} session {}: {}",
                                log_prefix,
                                client_label,
                                session_uuid,
                                e
                            );
                            (
                                Vec::new(),
                                false,
                                target_rows,
                                target_cols,
                                pty_handle.subscribe(),
                            )
                        }
                    };

                log::debug!(
                    "[{}] Snapshot bytes for {} session {}: {}",
                    log_prefix,
                    client_label,
                    session_uuid,
                    snapshot.len()
                );

                let attach_state =
                    Self::classify_snapshot_attach_state(&pty_handle, &session_uuid, &snapshot);
                match attach_state {
                    SnapshotAttachState::Ready => {}
                    SnapshotAttachState::Exited => {
                        log::warn!(
                            "[{}] Session RPC died before snapshot for {} session {}; sending ProcessExited",
                            log_prefix,
                            client_label,
                            session_uuid
                        );
                        let _ = worker
                            .send(ClientWorkerMessage::ControlFrame(
                                ClientControlFrame::ProcessExited {
                                    session_uuid: session_uuid.clone(),
                                    exit_code: None,
                                },
                            ))
                            .await;
                        let _ = worker
                            .send(ClientWorkerMessage::UnregisterSessionIoSender {
                                session_uuid: session_uuid.clone(),
                            })
                            .await;
                        return;
                    }
                    SnapshotAttachState::Reconnecting => {
                        log::info!(
                            "[{}] Session '{}' snapshot unavailable - reconnect pending",
                            log_prefix,
                            &session_uuid[..session_uuid.len().min(16)]
                        );
                        let _ = worker
                            .send(ClientWorkerMessage::ControlFrame(
                                ClientControlFrame::TerminalAttach {
                                    subscription_id: subscription_id.clone(),
                                    session_uuid: session_uuid.clone(),
                                    state: TerminalAttachState::Reconnecting,
                                },
                            ))
                            .await;
                    }
                }

                if attach_state != SnapshotAttachState::Reconnecting
                    && worker
                        .send(ClientWorkerMessage::ControlFrame(
                            ClientControlFrame::Scrollback {
                                session_uuid: session_uuid.clone(),
                                rows: snapshot_rows,
                                cols: snapshot_cols,
                                kitty_enabled,
                                data: snapshot,
                            },
                        ))
                        .await
                        .is_err()
                {
                    log::trace!(
                        "[{}] Worker channel closed before snapshot sent",
                        log_prefix
                    );
                    let _ = worker
                        .send(ClientWorkerMessage::UnregisterSessionIoSender {
                            session_uuid: session_uuid.clone(),
                        })
                        .await;
                    return;
                }

                pty_rx
            };

            loop {
                {
                    let active = active_flag
                        .lock()
                        .expect("Forwarder active_flag mutex poisoned");
                    if !*active {
                        log::debug!("[{}] PTY forwarder stopped by Lua", log_prefix);
                        break;
                    }
                }

                match pty_rx.recv().await {
                    Ok(PtyEvent::Output(data)) => {
                        let mut chunks = vec![data];
                        let mut stashed: Vec<PtyEvent> = Vec::new();
                        loop {
                            match pty_rx.try_recv() {
                                Ok(PtyEvent::Output(more)) => chunks.push(more),
                                Ok(other) => {
                                    let is_terminal =
                                        matches!(other, PtyEvent::ProcessExited { .. });
                                    stashed.push(other);
                                    if is_terminal {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }

                        let mut worker_closed = false;
                        for chunk in chunks {
                            let filtered =
                                filter.filter_chunk(&session_uuid, &mut query_filter_buffer, chunk);
                            if filtered.is_empty() {
                                continue;
                            }
                            if worker
                                .send(ClientWorkerMessage::TerminalBytes {
                                    session_uuid: session_uuid.clone(),
                                    data: filtered,
                                })
                                .await
                                .is_err()
                            {
                                log::trace!(
                                    "[{}] Worker channel closed, stopping forwarder",
                                    log_prefix
                                );
                                worker_closed = true;
                                break;
                            }
                        }
                        if worker_closed {
                            break;
                        }

                        if Self::forward_terminal_stream_events(
                            &worker,
                            &session_uuid,
                            &client_label,
                            log_prefix,
                            stashed,
                        )
                        .await
                        {
                            break;
                        }
                    }
                    Ok(event @ PtyEvent::ProcessExited { .. })
                    | Ok(event @ PtyEvent::KittyChanged(_))
                    | Ok(event @ PtyEvent::FocusReportingChanged(_)) => {
                        if Self::forward_terminal_stream_events(
                            &worker,
                            &session_uuid,
                            &client_label,
                            log_prefix,
                            vec![event],
                        )
                        .await
                        {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!(
                            "[{}] PTY forwarder lagged by {} events for {} session {}",
                            log_prefix,
                            n,
                            client_label,
                            session_uuid
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        log::info!(
                            "[{}] PTY channel closed for {} session {}",
                            log_prefix,
                            client_label,
                            session_uuid
                        );
                        break;
                    }
                }
            }

            *active_flag
                .lock()
                .expect("Forwarder active_flag mutex poisoned") = false;
            let _ = worker
                .send(ClientWorkerMessage::UnregisterSessionIoSender {
                    session_uuid: session_uuid.clone(),
                })
                .await;

            log::info!(
                "[{}] Stopped PTY forwarder for {} session {}",
                log_prefix,
                client_label,
                session_uuid
            );
        })
    }

    pub(super) async fn forward_terminal_stream_events(
        worker: &crate::worker::client::ClientWorkerHandle,
        session_uuid: &str,
        client_label: &str,
        log_prefix: &str,
        events: Vec<crate::agent::pty::PtyEvent>,
    ) -> bool {
        use crate::agent::pty::PtyEvent;
        use crate::worker::client::{ClientControlFrame, ClientWorkerMessage};

        for event in events {
            match event {
                PtyEvent::ProcessExited { exit_code } => {
                    log::info!(
                        "[{}] PTY process exited (code={:?}) for {} session {}",
                        log_prefix,
                        exit_code,
                        client_label,
                        session_uuid
                    );
                    let _ = worker
                        .send(ClientWorkerMessage::ControlFrame(
                            ClientControlFrame::ProcessExited {
                                session_uuid: session_uuid.to_string(),
                                exit_code,
                            },
                        ))
                        .await;
                    return true;
                }
                PtyEvent::KittyChanged(enabled) => {
                    let _ = worker
                        .send(ClientWorkerMessage::ControlFrame(
                            ClientControlFrame::KittyChanged {
                                session_uuid: session_uuid.to_string(),
                                enabled,
                            },
                        ))
                        .await;
                }
                PtyEvent::FocusReportingChanged(enabled) => {
                    let _ = worker
                        .send(ClientWorkerMessage::ControlFrame(
                            ClientControlFrame::FocusReportingChanged {
                                session_uuid: session_uuid.to_string(),
                                enabled,
                            },
                        ))
                        .await;
                }
                PtyEvent::Output(_) => unreachable!("output handled before stashing"),
                _ => {}
            }
        }
        false
    }
}
