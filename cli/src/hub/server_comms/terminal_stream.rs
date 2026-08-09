use super::*;

pub(super) enum TerminalStreamFilter {
    None,
    StripOscQueriesWhenInactive {
        active_terminal_peers: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
        peer_id: String,
    },
}

pub(super) enum TerminalInitialSnapshot {
    Raw { subscription_key: String },
    PrefixedGzip { subscription_key: String },
}

pub(super) struct TerminalClientSubscription {
    pub(super) pty_handle: crate::hub::agent_handle::PtyHandle,
    pub(super) worker: crate::worker::client::ClientWorkerHandle,
    pub(super) session_uuid: String,
    pub(super) subscription_id: String,
    pub(super) rows: u16,
    pub(super) cols: u16,
    pub(super) log_prefix: &'static str,
    pub(super) client_label: String,
    pub(super) output_prefix: Vec<u8>,
    pub(super) filter: TerminalStreamFilter,
    pub(super) initial_snapshot: TerminalInitialSnapshot,
    pub(super) confirm_subscription: bool,
}

impl TerminalStreamFilter {
    fn to_session_io_filter(&self) -> crate::worker::session_io::TerminalOutputFilter {
        match self {
            Self::None => crate::worker::session_io::TerminalOutputFilter::None,
            Self::StripOscQueriesWhenInactive {
                active_terminal_peers,
                peer_id,
            } => crate::worker::session_io::TerminalOutputFilter::StripOscQueriesWhenInactive {
                active_terminal_peers: Arc::clone(active_terminal_peers),
                peer_id: peer_id.clone(),
            },
        }
    }
}

impl Hub {
    pub(super) fn start_terminal_client_subscription(
        &mut self,
        spec: TerminalClientSubscription,
    ) -> bool {
        let subscription_key = match &spec.initial_snapshot {
            TerminalInitialSnapshot::Raw { subscription_key }
            | TerminalInitialSnapshot::PrefixedGzip { subscription_key } => {
                subscription_key.clone()
            }
        };

        // Defer before registering a SessionIo sender so reconnect wait is a
        // clean no-op (create_* already emits reconnecting/pending).
        if self.is_session_reconnect_pending(&spec.session_uuid) {
            log::debug!(
                "[{}] Deferring terminal attach for {} session {} — SessionIo reader reconnect in progress",
                spec.log_prefix,
                spec.client_label,
                spec.session_uuid
            );
            return false;
        }

        Self::register_worker_session_io_sender(
            &spec.worker,
            &spec.session_uuid,
            spec.pty_handle.clone(),
            spec.log_prefix,
        );

        if !spec.pty_handle.is_session_backed() {
            log::warn!(
                "[{}] Refusing non-session-backed terminal subscription for {} session {}",
                spec.log_prefix,
                spec.client_label,
                spec.session_uuid
            );
            Self::emit_terminal_attach_failure(
                &spec.worker,
                &spec.subscription_id,
                &spec.session_uuid,
                TerminalAttachFailureKind::NotFound,
            );
            return false;
        }

        self.start_session_io_terminal_subscription(spec, subscription_key)
    }

    fn start_session_io_terminal_subscription(
        &mut self,
        spec: TerminalClientSubscription,
        subscription_key: String,
    ) -> bool {
        use crate::worker::client::ClientWorkerMessage;
        use crate::worker::session_io::{
            SessionIoRequest, TerminalInitialSnapshotDelivery, TerminalOutputSubscription,
            TerminalSnapshotPayloadMode,
        };

        let attach_requested_at = Instant::now();
        if let Err(e) = spec.worker.try_send(ClientWorkerMessage::SubscribeSession {
            session_uuid: spec.session_uuid.clone(),
            subscription_id: spec.subscription_id.clone(),
        }) {
            log::warn!(
                "[{}] Failed to queue client-worker subscription for {} session {}: {}",
                spec.log_prefix,
                spec.client_label,
                spec.session_uuid,
                e
            );
            return false;
        }
        let client_worker_subscribed_at = Instant::now();

        let request_id = Self::next_session_io_request_id("terminal-snapshot");
        let payload_mode = match &spec.initial_snapshot {
            TerminalInitialSnapshot::Raw { .. } => TerminalSnapshotPayloadMode::Raw,
            TerminalInitialSnapshot::PrefixedGzip { .. } => {
                TerminalSnapshotPayloadMode::PrefixedGzip
            }
        };

        log::info!(
            "[{}] Queue session I/O terminal attach for {} session {} subscription={} key={} resize={}x{} snapshot_request={}",
            spec.log_prefix,
            spec.client_label,
            spec.session_uuid,
            spec.subscription_id,
            subscription_key,
            spec.cols,
            spec.rows,
            request_id
        );

        let resize_result = spec
            .pty_handle
            .enqueue_session_io_request(SessionIoRequest::Resize {
                rows: spec.rows,
                cols: spec.cols,
            });
        let live_subscription = TerminalOutputSubscription {
            subscription_key: subscription_key.clone(),
            subscription_id: spec.subscription_id.clone(),
            worker: spec.worker.clone(),
            output_prefix: spec.output_prefix.clone(),
            filter: spec.filter.to_session_io_filter(),
        };
        let session_io_snapshot_queued_at = Instant::now();
        let snapshot_result =
            spec.pty_handle
                .enqueue_session_io_request(SessionIoRequest::GetInitialSnapshot {
                    delivery: TerminalInitialSnapshotDelivery {
                        request_id,
                        subscription_key: subscription_key.clone(),
                        session_uuid: spec.session_uuid.clone(),
                        subscription_id: spec.subscription_id.clone(),
                        worker: spec.worker.clone(),
                        rows: spec.rows,
                        cols: spec.cols,
                        kitty_enabled: spec.pty_handle.kitty_enabled(),
                        mode: spec
                            .pty_handle
                            .get_mode_flags()
                            .map(crate::session::protocol::mode_changed_from_flags),
                        payload_mode,
                        confirm_subscription: spec.confirm_subscription,
                        live_subscription: Some(live_subscription),
                        attach_requested_at: Some(attach_requested_at),
                        client_worker_subscribed_at: Some(client_worker_subscribed_at),
                        session_io_snapshot_queued_at: Some(session_io_snapshot_queued_at),
                        session_io_accepted_at: None,
                    },
                });

        if resize_result.is_err() || snapshot_result.is_err() {
            let process_live = crate::session::session_process_is_live(&spec.session_uuid);
            let reconnecting = self.is_session_reconnect_pending(&spec.session_uuid);
            let failure = Self::terminal_attach_failure_kind(
                process_live,
                reconnecting,
                resize_result.as_ref().err().copied(),
                snapshot_result.as_ref().err().copied(),
            );
            log::warn!(
                "[{}] Session I/O attach request failed for {} session {}: resize={:?} snapshot={:?} process_live={} reconnecting={} failure={failure:?}",
                spec.log_prefix,
                spec.client_label,
                spec.session_uuid,
                resize_result.err(),
                snapshot_result.err(),
                process_live,
                reconnecting,
            );
            Self::emit_terminal_attach_failure(
                &spec.worker,
                &spec.subscription_id,
                &spec.session_uuid,
                failure,
            );
            return false;
        }

        log::info!(
            "[{}] Started session I/O terminal subscription for {} session {} ({}x{})",
            spec.log_prefix,
            spec.client_label,
            spec.session_uuid,
            spec.cols,
            spec.rows
        );
        true
    }

    /// Control-plane failure for terminal attach when SessionIo cannot start.
    ///
    /// Dead sessions emit `ProcessExited` (not `not_ready`) so clients stop
    /// thrash loops. In-flight reader reconnect emits `Reconnecting`. Transient
    /// mailbox gaps keep `NotReady`.
    pub(super) fn emit_terminal_attach_failure(
        worker: &crate::worker::client::ClientWorkerHandle,
        subscription_id: &str,
        session_uuid: &str,
        kind: TerminalAttachFailureKind,
    ) {
        use crate::worker::client::{
            ClientControlFrame, ClientWorkerMessage, TerminalAttachState,
        };

        let control = match kind {
            TerminalAttachFailureKind::ProcessExited => ClientControlFrame::ProcessExited {
                session_uuid: session_uuid.to_string(),
                exit_code: None,
            },
            TerminalAttachFailureKind::NotReady => ClientControlFrame::TerminalAttach {
                subscription_id: subscription_id.to_string(),
                session_uuid: session_uuid.to_string(),
                state: TerminalAttachState::NotReady,
            },
            TerminalAttachFailureKind::Reconnecting => ClientControlFrame::TerminalAttach {
                subscription_id: subscription_id.to_string(),
                session_uuid: session_uuid.to_string(),
                state: TerminalAttachState::Reconnecting,
            },
            TerminalAttachFailureKind::NotFound => ClientControlFrame::TerminalAttach {
                subscription_id: subscription_id.to_string(),
                session_uuid: session_uuid.to_string(),
                state: TerminalAttachState::NotFound,
            },
        };
        let _ = worker.try_send(ClientWorkerMessage::ControlFrame(control));
        let _ = worker.try_send(ClientWorkerMessage::UnregisterSessionIoSender {
            session_uuid: session_uuid.to_string(),
        });
    }

    /// Classify attach failure: permanent death vs recoverable gap.
    ///
    /// Transient enqueue errors (`MailboxFull`, `ReaderMissing`) always stay
    /// `NotReady` so a brief race cannot permanently close the browser session.
    /// Active hub reader-reconnect is `Reconnecting` even when the process
    /// liveness probe races false. Connection/closed errors with a dead process
    /// and no reconnect intent are `ProcessExited`.
    #[must_use]
    pub(super) fn terminal_attach_failure_kind(
        process_live: bool,
        reconnecting: bool,
        resize_err: Option<crate::session::connection::SessionIoRequestEnqueueError>,
        snapshot_err: Option<crate::session::connection::SessionIoRequestEnqueueError>,
    ) -> TerminalAttachFailureKind {
        use crate::session::connection::SessionIoRequestEnqueueError as E;

        let is_transient = |err: E| matches!(err, E::MailboxFull | E::ReaderMissing);
        if resize_err.is_some_and(is_transient) || snapshot_err.is_some_and(is_transient) {
            return TerminalAttachFailureKind::NotReady;
        }
        if reconnecting {
            return TerminalAttachFailureKind::Reconnecting;
        }
        if process_live {
            TerminalAttachFailureKind::NotReady
        } else {
            TerminalAttachFailureKind::ProcessExited
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TerminalAttachFailureKind {
    ProcessExited,
    NotReady,
    Reconnecting,
    NotFound,
}
