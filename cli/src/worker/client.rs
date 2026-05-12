//! Transport-neutral client worker contract.
//!
//! Client workers own per-client session subscriptions and terminal stream
//! delivery, but they do not know whether the client is a browser, TUI, socket,
//! or future transport. Transport-specific encoding and send mechanics belong
//! behind `TransportAdapter`.

use std::collections::HashMap;

use crate::client::ClientId;

use super::hub_control::{HubControlMessage, HubControlOrigin, WorkerBackpressure};
use super::session_io::SessionIoRequest;
use super::transport::TransportEgress;
use super::{BoundedQueueConfig, RequestId, SessionUuid, SubscriptionId};

/// Default bounded mailbox config for client-worker input.
pub const CLIENT_WORKER_QUEUE: BoundedQueueConfig = BoundedQueueConfig::new("worker.client", 1024);
const CLIENT_SESSION_IO_MISSING_SOURCE: &str = "worker.client.session_io_missing";
const CLIENT_SESSION_RESIZE_UNSUBSCRIBED_SOURCE: &str = "worker.client.session_resize_unsubscribed";
const CLIENT_REQUEST_SNAPSHOT_UNSUBSCRIBED_SOURCE: &str =
    "worker.client.request_snapshot_unsubscribed";

/// Health reported for a connected client transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientConnectionHealth {
    /// Transport is ready to send and receive work.
    Ready,
    /// Transport is alive but currently unable to keep up.
    Backpressured {
        /// Queue/source label that reported pressure.
        source: &'static str,
    },
    /// Transport is reconnecting.
    Reconnecting {
        /// Monotonic generation used to ignore stale reconnect completions.
        generation: u64,
    },
    /// Transport disconnected.
    Disconnected {
        /// Human-readable disconnect reason for diagnostics.
        reason: String,
    },
}

/// Known terminal attach states sent to clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalAttachState {
    /// Terminal attach is queued until the session data plane becomes available.
    Pending,
    /// Terminal is attached or the snapshot/stream will follow.
    Attached,
    /// Terminal exists but its backing process is reconnecting.
    Reconnecting,
    /// Terminal subscription exists but the session data plane is not ready.
    NotReady,
    /// Terminal could not be found.
    NotFound,
}

impl TerminalAttachState {
    /// Return the wire value used by existing clients.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Attached => "attached",
            Self::Reconnecting => "reconnecting",
            Self::NotReady => "not_ready",
            Self::NotFound => "not_found",
        }
    }
}

impl TryFrom<&str> for TerminalAttachState {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "pending" => Ok(Self::Pending),
            "attached" => Ok(Self::Attached),
            "reconnecting" => Ok(Self::Reconnecting),
            "not_ready" => Ok(Self::NotReady),
            "not_found" => Ok(Self::NotFound),
            _ => Err(()),
        }
    }
}

/// Control frame emitted by the session/hub side toward a client worker.
#[derive(Debug, Clone)]
pub enum ClientControlFrame {
    /// Request/response correlation pong.
    Pong {
        /// Request identifier from the ping.
        request_id: RequestId,
    },
    /// Terminal attach state for a subscription.
    TerminalAttach {
        /// Transport-local subscription identifier.
        subscription_id: SubscriptionId,
        /// Session whose attach state changed.
        session_uuid: SessionUuid,
        /// Known attach state.
        state: TerminalAttachState,
    },
    /// Kitty keyboard mode changed.
    KittyChanged {
        /// Session whose mode changed.
        session_uuid: SessionUuid,
        /// Whether kitty keyboard mode is enabled.
        enabled: bool,
    },
    /// Focus reporting mode changed.
    FocusReportingChanged {
        /// Session whose mode changed.
        session_uuid: SessionUuid,
        /// Whether focus reporting is enabled.
        enabled: bool,
    },
    /// Client focus state changed.
    FocusChanged {
        /// Session whose focus changed.
        session_uuid: SessionUuid,
        /// Whether the client is focused.
        focused: bool,
    },
    /// WebRTC data-channel pong heartbeat.
    DcPong,
    /// WebRTC data-channel pong heartbeat was received.
    DcPongReceived,
    /// Initial or recovery snapshot for a session.
    Snapshot {
        /// Session the snapshot belongs to.
        session_uuid: SessionUuid,
        /// Opaque snapshot bytes owned by terminal/session code.
        payload: Vec<u8>,
    },
    /// Initial or recovery scrollback snapshot with terminal metadata.
    Scrollback {
        /// Session the snapshot belongs to.
        session_uuid: SessionUuid,
        /// Authoritative row count used to produce the snapshot.
        rows: u16,
        /// Authoritative column count used to produce the snapshot.
        cols: u16,
        /// Whether kitty keyboard mode is enabled in the inner PTY.
        kitty_enabled: bool,
        /// Opaque snapshot bytes owned by terminal/session code.
        data: Vec<u8>,
    },
    /// Terminal mode flags changed.
    ModeChanged {
        /// Session whose mode changed.
        session_uuid: SessionUuid,
        /// Sparse mode update.
        mode: crate::session::protocol::ModeChanged,
    },
    /// Session process exited.
    ProcessExited {
        /// Session that exited.
        session_uuid: SessionUuid,
        /// Exit code when available.
        exit_code: Option<i32>,
    },
    /// Boundary JSON whose shape is owned by Lua/plugin/relay code.
    BoundaryJson(serde_json::Value),
    /// Plugin-level binary payload outside PTY routing.
    Binary(Vec<u8>),
}

/// Messages accepted by a client worker.
#[derive(Debug, Clone)]
pub enum ClientWorkerMessage {
    /// Register or replace the session-I/O request sender for a session.
    RegisterSessionIoSender {
        /// Session whose input should route through this sender.
        session_uuid: SessionUuid,
        /// Session-I/O request mailbox.
        tx: tokio::sync::mpsc::Sender<SessionIoRequest>,
    },
    /// Remove the session-I/O sender for a session and detach any active subscription.
    UnregisterSessionIoSender {
        /// Session whose input sender is no longer valid.
        session_uuid: SessionUuid,
    },
    /// Subscribe this client to a session.
    SubscribeSession {
        /// Session to subscribe to.
        session_uuid: SessionUuid,
        /// Transport-local subscription identifier.
        subscription_id: SubscriptionId,
    },
    /// Unsubscribe this client from a session.
    UnsubscribeSession {
        /// Session to unsubscribe from.
        session_uuid: SessionUuid,
        /// Transport-local subscription identifier.
        subscription_id: SubscriptionId,
    },
    /// Raw terminal bytes for a subscribed session.
    TerminalBytes {
        /// Session that produced the bytes.
        session_uuid: SessionUuid,
        /// Raw terminal bytes.
        data: Vec<u8>,
    },
    /// Raw terminal input from the client to a subscribed session.
    SessionInput {
        /// Session that should receive the bytes.
        session_uuid: SessionUuid,
        /// Raw PTY input bytes.
        data: Vec<u8>,
    },
    /// Browser file/drop payload from the client to a subscribed session.
    PasteFile {
        /// Session that should receive the file path injection.
        session_uuid: SessionUuid,
        /// Original client filename.
        filename: String,
        /// Raw file bytes.
        data: Vec<u8>,
    },
    /// Resize a subscribed session through the SessionIoWorker mailbox.
    SessionResize {
        /// Session that should receive the resize.
        session_uuid: SessionUuid,
        /// Requested terminal rows.
        rows: u16,
        /// Requested terminal columns.
        cols: u16,
    },
    /// Request a fresh snapshot for a subscribed session.
    RequestSnapshot {
        /// Session to snapshot.
        session_uuid: SessionUuid,
        /// Requested terminal rows.
        rows: u16,
        /// Requested terminal columns.
        cols: u16,
    },
    /// Control frame for a subscribed session or client.
    ControlFrame(ClientControlFrame),
    /// Client transport health changed.
    Health(ClientConnectionHealth),
    /// Worker ingress queue hit backpressure.
    Backpressure {
        /// Queue/source label.
        source: &'static str,
        /// Queue capacity.
        capacity: usize,
    },
    /// Request/response correlation ping.
    Ping {
        /// Request identifier.
        request_id: RequestId,
    },
    /// Wrap a message with the reconnect generation it was observed under.
    WithGeneration {
        /// Client reconnect generation associated with this delivery.
        generation: u64,
        /// Message to process when the generation is current.
        message: Box<ClientWorkerMessage>,
    },
    /// Shut down this client worker.
    Shutdown {
        /// Human-readable reason for diagnostics.
        reason: String,
    },
}

/// Handle shape for future code that owns a bounded client-worker mailbox.
#[derive(Debug, Clone)]
pub struct ClientWorkerHandle {
    /// Client identity represented by this worker.
    pub client_id: ClientId,
    /// Bounded mailbox sender.
    pub tx: tokio::sync::mpsc::Sender<ClientWorkerMessage>,
}

impl ClientWorkerHandle {
    /// Enqueue a message for this client worker.
    pub async fn send(
        &self,
        message: ClientWorkerMessage,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<ClientWorkerMessage>> {
        self.tx.send(message).await
    }

    /// Try to enqueue a message without waiting on a full mailbox.
    pub fn try_send(
        &self,
        message: ClientWorkerMessage,
    ) -> Result<(), tokio::sync::mpsc::error::TrySendError<ClientWorkerMessage>> {
        self.tx.try_send(message)
    }
}

/// Runtime dependencies for a transport-neutral client worker.
#[derive(Debug)]
pub struct ClientWorkerConfig {
    /// Client identity represented by the worker.
    pub client_id: ClientId,
    /// Hub-control mailbox for orchestration requests.
    pub hub_control_tx: tokio::sync::mpsc::Sender<HubControlMessage>,
    /// Transport egress mailbox owned by the adapter layer.
    pub outbound_tx: tokio::sync::mpsc::Sender<TransportEgress>,
    /// Session-I/O request mailboxes keyed by session UUID.
    pub session_io_txs: HashMap<SessionUuid, tokio::sync::mpsc::Sender<SessionIoRequest>>,
    /// Bounded mailbox config for this worker.
    pub mailbox: BoundedQueueConfig,
    /// Diagnostics label and capacity for the outbound transport queue.
    pub outbound: BoundedQueueConfig,
}

impl ClientWorkerConfig {
    /// Construct a config with the default client-worker mailbox and transport queue metadata.
    #[must_use]
    pub fn new(
        client_id: ClientId,
        hub_control_tx: tokio::sync::mpsc::Sender<HubControlMessage>,
        outbound_tx: tokio::sync::mpsc::Sender<TransportEgress>,
        session_io_txs: HashMap<SessionUuid, tokio::sync::mpsc::Sender<SessionIoRequest>>,
    ) -> Self {
        Self {
            client_id,
            hub_control_tx,
            outbound_tx,
            session_io_txs,
            mailbox: CLIENT_WORKER_QUEUE,
            outbound: BoundedQueueConfig::new("worker.client.outbound", 512),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryKind {
    Terminal,
    Control,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClientSubscription {
    subscription_id: SubscriptionId,
}

/// Client-worker delivery counters for slow-client policy diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClientWorkerStats {
    /// Terminal frames accepted by the transport egress queue.
    pub terminal_frames_sent: u64,
    /// Terminal frames dropped because the transport egress queue was full.
    pub terminal_frames_dropped: u64,
    /// Backpressure notices emitted to hub control.
    pub backpressure_events: u64,
}

/// Transport-neutral client worker runtime.
#[derive(Debug)]
pub struct ClientWorker {
    client_id: ClientId,
    hub_control_tx: tokio::sync::mpsc::Sender<HubControlMessage>,
    outbound_tx: tokio::sync::mpsc::Sender<TransportEgress>,
    session_io_txs: HashMap<SessionUuid, tokio::sync::mpsc::Sender<SessionIoRequest>>,
    subscriptions: HashMap<SessionUuid, ClientSubscription>,
    health: ClientConnectionHealth,
    generation: u64,
    next_request_id: u64,
    outbound: BoundedQueueConfig,
    stats: ClientWorkerStats,
}

impl ClientWorker {
    /// Spawn a client worker and return the mailbox handle future hub code can own.
    #[must_use]
    pub fn start(config: ClientWorkerConfig) -> ClientWorkerHandle {
        let capacity = config.mailbox.capacity;
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        let client_id = config.client_id.clone();

        tokio::spawn(async move {
            let worker = Self::new(config);
            worker.run(rx).await;
        });

        ClientWorkerHandle { client_id, tx }
    }

    fn new(config: ClientWorkerConfig) -> Self {
        Self {
            client_id: config.client_id,
            hub_control_tx: config.hub_control_tx,
            outbound_tx: config.outbound_tx,
            session_io_txs: config.session_io_txs,
            subscriptions: HashMap::new(),
            health: ClientConnectionHealth::Ready,
            generation: 0,
            next_request_id: 0,
            outbound: config.outbound,
            stats: ClientWorkerStats::default(),
        }
    }

    async fn run(mut self, mut rx: tokio::sync::mpsc::Receiver<ClientWorkerMessage>) {
        while let Some(message) = rx.recv().await {
            let should_stop = self.handle_message(message).await;
            if should_stop {
                break;
            }
        }
    }

    async fn handle_message(&mut self, message: ClientWorkerMessage) -> bool {
        let message = match message {
            ClientWorkerMessage::WithGeneration {
                generation,
                message,
            } => {
                if generation < self.generation {
                    return false;
                }

                *message
            }
            message => message,
        };

        match message {
            ClientWorkerMessage::SubscribeSession {
                session_uuid,
                subscription_id,
            } => {
                self.subscribe(session_uuid, subscription_id).await;
                false
            }
            ClientWorkerMessage::RegisterSessionIoSender { session_uuid, tx } => {
                self.register_session_io_sender(session_uuid, tx);
                false
            }
            ClientWorkerMessage::UnregisterSessionIoSender { session_uuid } => {
                self.unregister_session_io_sender(session_uuid).await;
                false
            }
            ClientWorkerMessage::UnsubscribeSession {
                session_uuid,
                subscription_id,
            } => {
                self.unsubscribe(session_uuid, subscription_id).await;
                false
            }
            ClientWorkerMessage::TerminalBytes { session_uuid, data } => {
                self.deliver_terminal_bytes(session_uuid, data).await;
                false
            }
            ClientWorkerMessage::SessionInput { session_uuid, data } => {
                self.route_session_input(session_uuid, data).await;
                false
            }
            ClientWorkerMessage::PasteFile {
                session_uuid,
                filename,
                data,
            } => {
                self.route_session_file(session_uuid, filename, data).await;
                false
            }
            ClientWorkerMessage::SessionResize {
                session_uuid,
                rows,
                cols,
            } => {
                self.route_session_resize(session_uuid, rows, cols).await;
                false
            }
            ClientWorkerMessage::RequestSnapshot {
                session_uuid,
                rows,
                cols,
            } => {
                self.request_session_snapshot(session_uuid, rows, cols)
                    .await;
                false
            }
            ClientWorkerMessage::ControlFrame(frame) => {
                self.deliver_control_frame(frame).await;
                false
            }
            ClientWorkerMessage::Health(health) => {
                self.update_health(health).await;
                false
            }
            ClientWorkerMessage::Backpressure { source, capacity } => {
                self.report_backpressure(source, capacity, None).await;
                false
            }
            ClientWorkerMessage::Ping { request_id } => {
                if !matches!(self.health, ClientConnectionHealth::Ready) {
                    return false;
                }
                self.try_deliver_egress(
                    TransportEgress::Pong { request_id },
                    None,
                    DeliveryKind::Control,
                )
                .await;
                false
            }
            ClientWorkerMessage::WithGeneration { .. } => {
                unreachable!("generation wrapper handled before dispatch")
            }
            ClientWorkerMessage::Shutdown { reason } => {
                let _ = self
                    .outbound_tx
                    .send(TransportEgress::Close {
                        reason: reason.clone(),
                    })
                    .await;
                self.send_hub_control(HubControlMessage::Shutdown {
                    origin: HubControlOrigin::Client(self.client_id.clone()),
                    reason,
                })
                .await;
                true
            }
        }
    }

    async fn subscribe(&mut self, session_uuid: SessionUuid, subscription_id: SubscriptionId) {
        if let Some(existing) = self.subscriptions.get(&session_uuid) {
            if existing.subscription_id == subscription_id {
                return;
            }
        }

        self.subscriptions.insert(
            session_uuid.clone(),
            ClientSubscription {
                subscription_id: subscription_id.clone(),
            },
        );

        self.send_hub_control(HubControlMessage::AttachClient {
            client_id: self.client_id.clone(),
            session_uuid,
            subscription_id,
        })
        .await;
    }

    fn register_session_io_sender(
        &mut self,
        session_uuid: SessionUuid,
        tx: tokio::sync::mpsc::Sender<SessionIoRequest>,
    ) {
        self.session_io_txs.insert(session_uuid, tx);
    }

    async fn unregister_session_io_sender(&mut self, session_uuid: SessionUuid) {
        self.session_io_txs.remove(&session_uuid);
        let Some(subscription) = self.subscriptions.remove(&session_uuid) else {
            return;
        };

        self.send_hub_control(HubControlMessage::DetachClient {
            client_id: self.client_id.clone(),
            session_uuid,
            subscription_id: subscription.subscription_id,
        })
        .await;
    }

    async fn unsubscribe(&mut self, session_uuid: SessionUuid, subscription_id: SubscriptionId) {
        let should_detach = self
            .subscriptions
            .get(&session_uuid)
            .is_some_and(|subscription| subscription.subscription_id == subscription_id);

        if !should_detach {
            return;
        }

        self.subscriptions.remove(&session_uuid);
        self.send_hub_control(HubControlMessage::DetachClient {
            client_id: self.client_id.clone(),
            session_uuid,
            subscription_id,
        })
        .await;
    }

    async fn deliver_terminal_bytes(&mut self, session_uuid: SessionUuid, data: Vec<u8>) {
        let Some(subscription) = self.subscriptions.get(&session_uuid) else {
            return;
        };

        if !matches!(self.health, ClientConnectionHealth::Ready) {
            return;
        }

        let egress = TransportEgress::TerminalBytes {
            subscription_id: subscription.subscription_id.clone(),
            session_uuid: session_uuid.clone(),
            data,
        };
        self.try_deliver_egress(egress, Some(session_uuid), DeliveryKind::Terminal)
            .await;
    }

    async fn route_session_input(&mut self, session_uuid: SessionUuid, data: Vec<u8>) {
        let Some(subscription_id) = self
            .subscriptions
            .get(&session_uuid)
            .map(|subscription| subscription.subscription_id.clone())
        else {
            return;
        };

        let Some(tx) = self.session_io_txs.get(&session_uuid).cloned() else {
            log::warn!(
                "[ClientWorker] PTY input for subscribed session {} dropped because session I/O sender is not ready",
                session_uuid
            );
            self.try_deliver_egress(
                TransportEgress::TerminalAttach {
                    subscription_id,
                    session_uuid: session_uuid.clone(),
                    state: TerminalAttachState::NotReady,
                },
                Some(session_uuid.clone()),
                DeliveryKind::Control,
            )
            .await;
            self.report_backpressure(CLIENT_SESSION_IO_MISSING_SOURCE, 0, Some(session_uuid))
                .await;
            return;
        };

        match tx.try_send(SessionIoRequest::PtyInput { data }) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                self.report_backpressure(
                    super::session_io::SESSION_IO_WORKER_QUEUE.name,
                    super::session_io::SESSION_IO_WORKER_QUEUE.capacity,
                    Some(session_uuid),
                )
                .await;
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                self.session_io_txs.remove(&session_uuid);
            }
        }
    }

    async fn route_session_file(
        &mut self,
        session_uuid: SessionUuid,
        filename: String,
        data: Vec<u8>,
    ) {
        let Some(subscription_id) = self
            .subscriptions
            .get(&session_uuid)
            .map(|subscription| subscription.subscription_id.clone())
        else {
            return;
        };

        let Some(tx) = self.session_io_txs.get(&session_uuid).cloned() else {
            self.try_deliver_egress(
                TransportEgress::TerminalAttach {
                    subscription_id,
                    session_uuid: session_uuid.clone(),
                    state: TerminalAttachState::NotReady,
                },
                Some(session_uuid.clone()),
                DeliveryKind::Control,
            )
            .await;
            self.report_backpressure(CLIENT_SESSION_IO_MISSING_SOURCE, 0, Some(session_uuid))
                .await;
            return;
        };

        self.next_request_id = self.next_request_id.wrapping_add(1);
        let request_id = format!("client-file-{}-{}", self.client_id, self.next_request_id);
        match tx.try_send(SessionIoRequest::PasteFile {
            request_id,
            filename,
            data,
        }) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                self.report_backpressure(
                    super::session_io::SESSION_IO_WORKER_QUEUE.name,
                    super::session_io::SESSION_IO_WORKER_QUEUE.capacity,
                    Some(session_uuid),
                )
                .await;
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                self.session_io_txs.remove(&session_uuid);
            }
        }
    }

    async fn route_session_resize(&mut self, session_uuid: SessionUuid, rows: u16, cols: u16) {
        let Some(subscription_id) = self
            .subscriptions
            .get(&session_uuid)
            .map(|subscription| subscription.subscription_id.clone())
        else {
            self.report_backpressure(
                CLIENT_SESSION_RESIZE_UNSUBSCRIBED_SOURCE,
                0,
                Some(session_uuid),
            )
            .await;
            return;
        };

        let Some(tx) = self.session_io_txs.get(&session_uuid).cloned() else {
            self.try_deliver_egress(
                TransportEgress::TerminalAttach {
                    subscription_id,
                    session_uuid: session_uuid.clone(),
                    state: TerminalAttachState::NotReady,
                },
                Some(session_uuid.clone()),
                DeliveryKind::Control,
            )
            .await;
            self.report_backpressure(CLIENT_SESSION_IO_MISSING_SOURCE, 0, Some(session_uuid))
                .await;
            return;
        };

        match tx.try_send(SessionIoRequest::Resize { rows, cols }) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                self.report_backpressure(
                    super::session_io::SESSION_IO_WORKER_QUEUE.name,
                    super::session_io::SESSION_IO_WORKER_QUEUE.capacity,
                    Some(session_uuid),
                )
                .await;
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                self.session_io_txs.remove(&session_uuid);
            }
        }
    }

    async fn request_session_snapshot(&mut self, session_uuid: SessionUuid, rows: u16, cols: u16) {
        let Some(subscription_id) = self
            .subscriptions
            .get(&session_uuid)
            .map(|subscription| subscription.subscription_id.clone())
        else {
            self.report_backpressure(
                CLIENT_REQUEST_SNAPSHOT_UNSUBSCRIBED_SOURCE,
                0,
                Some(session_uuid),
            )
            .await;
            return;
        };

        self.send_hub_control(HubControlMessage::RequestSnapshot {
            client_id: self.client_id.clone(),
            session_uuid,
            subscription_id,
            rows,
            cols,
        })
        .await;
    }

    async fn deliver_control_frame(&mut self, frame: ClientControlFrame) {
        if !matches!(self.health, ClientConnectionHealth::Ready) {
            return;
        }

        match frame {
            ClientControlFrame::Pong { request_id } => {
                self.try_deliver_egress(
                    TransportEgress::Pong { request_id },
                    None,
                    DeliveryKind::Control,
                )
                .await;
            }
            ClientControlFrame::TerminalAttach {
                subscription_id,
                session_uuid,
                state,
            } => {
                self.try_deliver_egress(
                    TransportEgress::TerminalAttach {
                        subscription_id,
                        session_uuid: session_uuid.clone(),
                        state,
                    },
                    Some(session_uuid),
                    DeliveryKind::Control,
                )
                .await;
            }
            ClientControlFrame::KittyChanged {
                session_uuid,
                enabled,
            } => {
                if !self.subscriptions.contains_key(&session_uuid) {
                    return;
                }
                self.try_deliver_egress(
                    TransportEgress::KittyChanged {
                        session_uuid: session_uuid.clone(),
                        enabled,
                    },
                    Some(session_uuid),
                    DeliveryKind::Control,
                )
                .await;
            }
            ClientControlFrame::FocusReportingChanged {
                session_uuid,
                enabled,
            } => {
                if !self.subscriptions.contains_key(&session_uuid) {
                    return;
                }
                self.try_deliver_egress(
                    TransportEgress::FocusReportingChanged {
                        session_uuid: session_uuid.clone(),
                        enabled,
                    },
                    Some(session_uuid),
                    DeliveryKind::Control,
                )
                .await;
            }
            ClientControlFrame::FocusChanged {
                session_uuid,
                focused,
            } => {
                if !self.subscriptions.contains_key(&session_uuid) {
                    return;
                }
                self.try_deliver_egress(
                    TransportEgress::FocusChanged {
                        session_uuid: session_uuid.clone(),
                        focused,
                    },
                    Some(session_uuid),
                    DeliveryKind::Control,
                )
                .await;
            }
            ClientControlFrame::DcPong => {
                self.try_deliver_egress(TransportEgress::DcPong, None, DeliveryKind::Control)
                    .await;
            }
            ClientControlFrame::DcPongReceived => {}
            ClientControlFrame::Snapshot {
                session_uuid,
                payload,
            } => {
                if !self.subscriptions.contains_key(&session_uuid) {
                    return;
                }
                self.try_deliver_egress(
                    TransportEgress::Snapshot {
                        session_uuid: session_uuid.clone(),
                        payload,
                    },
                    Some(session_uuid),
                    DeliveryKind::Control,
                )
                .await;
            }
            ClientControlFrame::ModeChanged { session_uuid, mode } => {
                if !self.subscriptions.contains_key(&session_uuid) {
                    return;
                }
                self.try_deliver_egress(
                    TransportEgress::ModeChanged {
                        session_uuid: session_uuid.clone(),
                        mode,
                    },
                    Some(session_uuid),
                    DeliveryKind::Control,
                )
                .await;
            }
            ClientControlFrame::Scrollback {
                session_uuid,
                rows,
                cols,
                kitty_enabled,
                data,
            } => {
                let Some(subscription) = self.subscriptions.get(&session_uuid) else {
                    return;
                };
                let egress = TransportEgress::Scrollback {
                    subscription_id: subscription.subscription_id.clone(),
                    session_uuid: session_uuid.clone(),
                    rows,
                    cols,
                    kitty_enabled,
                    data,
                };
                self.try_deliver_egress(egress, Some(session_uuid), DeliveryKind::Control)
                    .await;
            }
            ClientControlFrame::ProcessExited {
                session_uuid,
                exit_code,
            } => {
                let Some(subscription) = self.subscriptions.get(&session_uuid) else {
                    return;
                };
                let egress = TransportEgress::ProcessExited {
                    subscription_id: subscription.subscription_id.clone(),
                    session_uuid: session_uuid.clone(),
                    exit_code,
                };
                self.try_deliver_egress(egress, Some(session_uuid), DeliveryKind::Control)
                    .await;
            }
            ClientControlFrame::BoundaryJson(value) => {
                self.try_deliver_egress(
                    TransportEgress::BoundaryJson(value),
                    None,
                    DeliveryKind::Control,
                )
                .await;
            }
            ClientControlFrame::Binary(data) => {
                self.try_deliver_egress(TransportEgress::Binary(data), None, DeliveryKind::Control)
                    .await;
            }
        }
    }

    async fn try_deliver_egress(
        &mut self,
        egress: TransportEgress,
        session_uuid: Option<SessionUuid>,
        kind: DeliveryKind,
    ) {
        match self.outbound_tx.try_send(egress) {
            Ok(()) => {
                if matches!(kind, DeliveryKind::Terminal) {
                    self.stats.terminal_frames_sent += 1;
                }
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                if matches!(kind, DeliveryKind::Terminal) {
                    self.stats.terminal_frames_dropped += 1;
                }
                let source = match kind {
                    DeliveryKind::Terminal => self.outbound.name,
                    DeliveryKind::Control => "worker.client.outbound.control",
                };
                self.report_backpressure(source, self.outbound.capacity, session_uuid)
                    .await;
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                self.update_health(ClientConnectionHealth::Disconnected {
                    reason: "transport egress closed".to_string(),
                })
                .await;
            }
        }
    }

    async fn update_health(&mut self, health: ClientConnectionHealth) {
        match &health {
            ClientConnectionHealth::Reconnecting { generation } => {
                if *generation > self.generation {
                    self.generation = *generation;
                }
                self.send_hub_control(HubControlMessage::Reconnect {
                    origin: HubControlOrigin::Client(self.client_id.clone()),
                    session_uuid: None,
                    generation: self.generation,
                })
                .await;
            }
            ClientConnectionHealth::Backpressured { source } => {
                self.report_backpressure(source, self.outbound.capacity, None)
                    .await;
            }
            ClientConnectionHealth::Ready | ClientConnectionHealth::Disconnected { .. } => {}
        }

        self.health = health;
    }

    async fn report_backpressure(
        &mut self,
        source: &'static str,
        capacity: usize,
        session_uuid: Option<SessionUuid>,
    ) {
        self.stats.backpressure_events += 1;
        self.send_hub_control(HubControlMessage::Backpressure(WorkerBackpressure {
            source,
            capacity,
            session_uuid,
            client_id: Some(self.client_id.clone()),
        }))
        .await;
    }

    async fn send_hub_control(&self, message: HubControlMessage) {
        let _ = self.hub_control_tx.send(message).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type WorkerHarness = (
        ClientWorkerHandle,
        tokio::sync::mpsc::Receiver<HubControlMessage>,
        tokio::sync::mpsc::Receiver<TransportEgress>,
        tokio::sync::mpsc::Receiver<SessionIoRequest>,
    );

    fn spawn_worker(client_id: ClientId, outbound_capacity: usize) -> WorkerHarness {
        let (hub_control_tx, hub_control_rx) = tokio::sync::mpsc::channel(16);
        let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(outbound_capacity);
        let (session_io_tx, session_io_rx) = tokio::sync::mpsc::channel(1);
        let mut session_io_txs = HashMap::new();
        session_io_txs.insert("sess-1".to_string(), session_io_tx);

        let mut config =
            ClientWorkerConfig::new(client_id, hub_control_tx, outbound_tx, session_io_txs);
        config.outbound = BoundedQueueConfig::new("test.outbound", outbound_capacity);

        (
            ClientWorker::start(config),
            hub_control_rx,
            outbound_rx,
            session_io_rx,
        )
    }

    async fn recv_hub(
        rx: &mut tokio::sync::mpsc::Receiver<HubControlMessage>,
    ) -> HubControlMessage {
        tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for hub control")
            .expect("hub control closed")
    }

    async fn recv_egress(rx: &mut tokio::sync::mpsc::Receiver<TransportEgress>) -> TransportEgress {
        tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for egress")
            .expect("egress closed")
    }

    async fn assert_no_hub(rx: &mut tokio::sync::mpsc::Receiver<HubControlMessage>) {
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv())
                .await
                .is_err(),
            "unexpected hub control message"
        );
    }

    async fn subscribe(handle: &ClientWorkerHandle) {
        handle
            .send(ClientWorkerMessage::SubscribeSession {
                session_uuid: "sess-1".to_string(),
                subscription_id: "sub-1".to_string(),
            })
            .await
            .expect("send subscribe");
    }

    #[tokio::test]
    async fn subscribe_unsubscribe_emit_hub_control_and_gate_terminal_delivery() {
        let (handle, mut hub_rx, mut outbound_rx, _session_rx) = spawn_worker(ClientId::Tui, 8);

        handle
            .send(ClientWorkerMessage::TerminalBytes {
                session_uuid: "sess-1".to_string(),
                data: b"before".to_vec(),
            })
            .await
            .expect("send terminal before subscribe");
        assert!(outbound_rx.try_recv().is_err());

        subscribe(&handle).await;
        assert!(matches!(
            recv_hub(&mut hub_rx).await,
            HubControlMessage::AttachClient {
                client_id: ClientId::Tui,
                session_uuid,
                subscription_id,
            } if session_uuid == "sess-1" && subscription_id == "sub-1"
        ));

        handle
            .send(ClientWorkerMessage::TerminalBytes {
                session_uuid: "sess-1".to_string(),
                data: b"after".to_vec(),
            })
            .await
            .expect("send terminal after subscribe");
        assert!(matches!(
            recv_egress(&mut outbound_rx).await,
            TransportEgress::TerminalBytes {
                subscription_id,
                data,
                ..
            } if subscription_id == "sub-1" && data == b"after"
        ));

        handle
            .send(ClientWorkerMessage::UnsubscribeSession {
                session_uuid: "sess-1".to_string(),
                subscription_id: "sub-1".to_string(),
            })
            .await
            .expect("send unsubscribe");
        assert!(matches!(
            recv_hub(&mut hub_rx).await,
            HubControlMessage::DetachClient {
                client_id: ClientId::Tui,
                session_uuid,
                subscription_id,
            } if session_uuid == "sess-1" && subscription_id == "sub-1"
        ));
    }

    #[tokio::test]
    async fn session_input_routes_to_attached_session_io_worker_only_when_subscribed() {
        let (handle, mut hub_rx, _outbound_rx, mut session_rx) =
            spawn_worker(ClientId::browser("browser-identity"), 8);

        handle
            .send(ClientWorkerMessage::SessionInput {
                session_uuid: "sess-1".to_string(),
                data: b"ignored".to_vec(),
            })
            .await
            .expect("send input before subscribe");
        assert!(session_rx.try_recv().is_err());

        subscribe(&handle).await;
        let _ = recv_hub(&mut hub_rx).await;

        handle
            .send(ClientWorkerMessage::SessionInput {
                session_uuid: "sess-1".to_string(),
                data: b"ls\n".to_vec(),
            })
            .await
            .expect("send subscribed input");

        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(1), session_rx.recv())
                .await
                .expect("timed out waiting for session input")
                .expect("session io closed"),
            SessionIoRequest::PtyInput { data } if data == b"ls\n"
        ));
    }

    #[tokio::test]
    async fn dynamic_session_io_sender_registration_routes_input() {
        let (handle, mut hub_rx, _outbound_rx, mut seeded_session_rx) =
            spawn_worker(ClientId::browser("browser-identity"), 8);
        let (dynamic_tx, mut dynamic_rx) = tokio::sync::mpsc::channel(1);

        handle
            .send(ClientWorkerMessage::RegisterSessionIoSender {
                session_uuid: "sess-2".to_string(),
                tx: dynamic_tx,
            })
            .await
            .expect("register dynamic session sender");
        handle
            .send(ClientWorkerMessage::SubscribeSession {
                session_uuid: "sess-2".to_string(),
                subscription_id: "sub-2".to_string(),
            })
            .await
            .expect("subscribe dynamic session");
        assert!(matches!(
            recv_hub(&mut hub_rx).await,
            HubControlMessage::AttachClient {
                session_uuid,
                subscription_id,
                ..
            } if session_uuid == "sess-2" && subscription_id == "sub-2"
        ));

        handle
            .send(ClientWorkerMessage::SessionInput {
                session_uuid: "sess-2".to_string(),
                data: b"dynamic\n".to_vec(),
            })
            .await
            .expect("send dynamic input");

        assert!(seeded_session_rx.try_recv().is_err());
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(1), dynamic_rx.recv())
                .await
                .expect("timed out waiting for dynamic input")
                .expect("dynamic session io closed"),
            SessionIoRequest::PtyInput { data } if data == b"dynamic\n"
        ));
    }

    #[tokio::test]
    async fn subscribed_input_without_session_io_sender_emits_not_ready() {
        let (handle, mut hub_rx, mut outbound_rx, mut seeded_session_rx) =
            spawn_worker(ClientId::browser("browser-identity"), 8);

        handle
            .send(ClientWorkerMessage::SubscribeSession {
                session_uuid: "sess-missing-sender".to_string(),
                subscription_id: "sub-missing-sender".to_string(),
            })
            .await
            .expect("subscribe missing sender");
        assert!(matches!(
            recv_hub(&mut hub_rx).await,
            HubControlMessage::AttachClient {
                session_uuid,
                subscription_id,
                ..
            } if session_uuid == "sess-missing-sender"
                && subscription_id == "sub-missing-sender"
        ));

        handle
            .send(ClientWorkerMessage::SessionInput {
                session_uuid: "sess-missing-sender".to_string(),
                data: b"typed-before-registration".to_vec(),
            })
            .await
            .expect("send input without sender");

        assert!(seeded_session_rx.try_recv().is_err());
        assert!(matches!(
            recv_egress(&mut outbound_rx).await,
            TransportEgress::TerminalAttach {
                subscription_id,
                session_uuid,
                state: TerminalAttachState::NotReady,
            } if subscription_id == "sub-missing-sender" && session_uuid == "sess-missing-sender"
        ));
        assert!(matches!(
            recv_hub(&mut hub_rx).await,
            HubControlMessage::Backpressure(WorkerBackpressure {
                source: CLIENT_SESSION_IO_MISSING_SOURCE,
                capacity: 0,
                session_uuid: Some(session_uuid),
                client_id: Some(ClientId::Browser(browser_identity)),
            }) if session_uuid == "sess-missing-sender" && browser_identity == "browser-identity"
        ));
    }

    #[tokio::test]
    async fn subscribed_resize_without_session_io_sender_emits_not_ready() {
        let (handle, mut hub_rx, mut outbound_rx, mut seeded_session_rx) =
            spawn_worker(ClientId::browser("browser-identity"), 8);

        handle
            .send(ClientWorkerMessage::SubscribeSession {
                session_uuid: "sess-missing-resize-sender".to_string(),
                subscription_id: "sub-missing-resize-sender".to_string(),
            })
            .await
            .expect("subscribe missing resize sender");
        assert!(matches!(
            recv_hub(&mut hub_rx).await,
            HubControlMessage::AttachClient {
                session_uuid,
                subscription_id,
                ..
            } if session_uuid == "sess-missing-resize-sender"
                && subscription_id == "sub-missing-resize-sender"
        ));

        handle
            .send(ClientWorkerMessage::SessionResize {
                session_uuid: "sess-missing-resize-sender".to_string(),
                rows: 42,
                cols: 150,
            })
            .await
            .expect("send resize without sender");

        assert!(seeded_session_rx.try_recv().is_err());
        assert!(matches!(
            recv_egress(&mut outbound_rx).await,
            TransportEgress::TerminalAttach {
                subscription_id,
                session_uuid,
                state: TerminalAttachState::NotReady,
            } if subscription_id == "sub-missing-resize-sender"
                && session_uuid == "sess-missing-resize-sender"
        ));
        assert!(matches!(
            recv_hub(&mut hub_rx).await,
            HubControlMessage::Backpressure(WorkerBackpressure {
                source: CLIENT_SESSION_IO_MISSING_SOURCE,
                capacity: 0,
                session_uuid: Some(session_uuid),
                client_id: Some(ClientId::Browser(browser_identity)),
            }) if session_uuid == "sess-missing-resize-sender"
                && browser_identity == "browser-identity"
        ));
    }

    #[tokio::test]
    async fn unregister_session_io_sender_detaches_active_subscription() {
        let (handle, mut hub_rx, _outbound_rx, mut session_rx) = spawn_worker(ClientId::Tui, 8);

        subscribe(&handle).await;
        let _ = recv_hub(&mut hub_rx).await;

        handle
            .send(ClientWorkerMessage::UnregisterSessionIoSender {
                session_uuid: "sess-1".to_string(),
            })
            .await
            .expect("unregister session sender");
        assert!(matches!(
            recv_hub(&mut hub_rx).await,
            HubControlMessage::DetachClient {
                session_uuid,
                subscription_id,
                ..
            } if session_uuid == "sess-1" && subscription_id == "sub-1"
        ));

        handle
            .send(ClientWorkerMessage::SessionInput {
                session_uuid: "sess-1".to_string(),
                data: b"ignored\n".to_vec(),
            })
            .await
            .expect("send input after unregister");
        assert!(session_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn closed_session_io_sender_is_removed_then_next_input_is_not_ready() {
        let (handle, mut hub_rx, mut outbound_rx, _session_rx) = spawn_worker(ClientId::Tui, 8);
        let (closed_tx, closed_rx) = tokio::sync::mpsc::channel(1);
        drop(closed_rx);

        handle
            .send(ClientWorkerMessage::RegisterSessionIoSender {
                session_uuid: "sess-closed".to_string(),
                tx: closed_tx,
            })
            .await
            .expect("register closed sender");
        handle
            .send(ClientWorkerMessage::SubscribeSession {
                session_uuid: "sess-closed".to_string(),
                subscription_id: "sub-closed".to_string(),
            })
            .await
            .expect("subscribe closed sender");
        let _ = recv_hub(&mut hub_rx).await;

        handle
            .send(ClientWorkerMessage::SessionInput {
                session_uuid: "sess-closed".to_string(),
                data: b"first".to_vec(),
            })
            .await
            .expect("route to closed sender");
        assert_no_hub(&mut hub_rx).await;

        handle
            .send(ClientWorkerMessage::SessionInput {
                session_uuid: "sess-closed".to_string(),
                data: b"second".to_vec(),
            })
            .await
            .expect("route after stale sender removal");
        assert!(matches!(
            recv_egress(&mut outbound_rx).await,
            TransportEgress::TerminalAttach {
                subscription_id,
                session_uuid,
                state: TerminalAttachState::NotReady,
            } if subscription_id == "sub-closed" && session_uuid == "sess-closed"
        ));
        assert!(matches!(
            recv_hub(&mut hub_rx).await,
            HubControlMessage::Backpressure(WorkerBackpressure {
                source: CLIENT_SESSION_IO_MISSING_SOURCE,
                capacity: 0,
                session_uuid: Some(session_uuid),
                client_id: Some(ClientId::Tui),
            }) if session_uuid == "sess-closed"
        ));
    }

    #[tokio::test]
    async fn dynamic_session_io_sender_backpressure_keeps_session_context() {
        let (handle, mut hub_rx, _outbound_rx, _session_rx) = spawn_worker(ClientId::Tui, 8);
        let (blocked_tx, _blocked_rx) = tokio::sync::mpsc::channel(1);
        blocked_tx
            .try_send(SessionIoRequest::PtyInput {
                data: b"held".to_vec(),
            })
            .expect("fill dynamic session queue");

        handle
            .send(ClientWorkerMessage::RegisterSessionIoSender {
                session_uuid: "sess-dynamic".to_string(),
                tx: blocked_tx,
            })
            .await
            .expect("register blocked sender");
        handle
            .send(ClientWorkerMessage::SubscribeSession {
                session_uuid: "sess-dynamic".to_string(),
                subscription_id: "sub-dynamic".to_string(),
            })
            .await
            .expect("subscribe dynamic session");
        let _ = recv_hub(&mut hub_rx).await;

        handle
            .send(ClientWorkerMessage::SessionInput {
                session_uuid: "sess-dynamic".to_string(),
                data: b"blocked".to_vec(),
            })
            .await
            .expect("send input to full dynamic sender");
        assert!(matches!(
            recv_hub(&mut hub_rx).await,
            HubControlMessage::Backpressure(WorkerBackpressure {
                source,
                capacity,
                session_uuid: Some(session_uuid),
                client_id: Some(ClientId::Tui),
            }) if source == super::super::session_io::SESSION_IO_WORKER_QUEUE.name
                && capacity == super::super::session_io::SESSION_IO_WORKER_QUEUE.capacity
                && session_uuid == "sess-dynamic"
        ));
    }

    #[tokio::test]
    async fn dynamic_session_resize_backpressure_keeps_session_context() {
        let (handle, mut hub_rx, _outbound_rx, _session_rx) =
            spawn_worker(ClientId::browser("browser-identity"), 8);
        let (blocked_tx, _blocked_rx) = tokio::sync::mpsc::channel(1);
        blocked_tx
            .try_send(SessionIoRequest::PtyInput {
                data: b"held".to_vec(),
            })
            .expect("fill dynamic session queue");

        handle
            .send(ClientWorkerMessage::RegisterSessionIoSender {
                session_uuid: "sess-resize-full".to_string(),
                tx: blocked_tx,
            })
            .await
            .expect("register blocked sender");
        handle
            .send(ClientWorkerMessage::SubscribeSession {
                session_uuid: "sess-resize-full".to_string(),
                subscription_id: "sub-resize-full".to_string(),
            })
            .await
            .expect("subscribe dynamic session");
        let _ = recv_hub(&mut hub_rx).await;

        handle
            .send(ClientWorkerMessage::SessionResize {
                session_uuid: "sess-resize-full".to_string(),
                rows: 43,
                cols: 151,
            })
            .await
            .expect("send resize to full sender");
        assert!(matches!(
            recv_hub(&mut hub_rx).await,
            HubControlMessage::Backpressure(WorkerBackpressure {
                source,
                capacity,
                session_uuid: Some(session_uuid),
                client_id: Some(ClientId::Browser(browser_identity)),
            }) if source == super::super::session_io::SESSION_IO_WORKER_QUEUE.name
                && capacity == super::super::session_io::SESSION_IO_WORKER_QUEUE.capacity
                && session_uuid == "sess-resize-full"
                && browser_identity == "browser-identity"
        ));
    }

    #[tokio::test]
    async fn request_snapshot_without_subscription_records_observable_drop() {
        let (handle, mut hub_rx, _outbound_rx, _session_rx) =
            spawn_worker(ClientId::browser("browser-identity"), 8);

        handle
            .send(ClientWorkerMessage::RequestSnapshot {
                session_uuid: "sess-unsubscribed-snapshot".to_string(),
                rows: 24,
                cols: 80,
            })
            .await
            .expect("send request_snapshot before subscribe");

        assert!(matches!(
            recv_hub(&mut hub_rx).await,
            HubControlMessage::Backpressure(WorkerBackpressure {
                source: CLIENT_REQUEST_SNAPSHOT_UNSUBSCRIBED_SOURCE,
                capacity: 0,
                session_uuid: Some(session_uuid),
                client_id: Some(ClientId::Browser(browser_identity)),
            }) if session_uuid == "sess-unsubscribed-snapshot"
                && browser_identity == "browser-identity"
        ));
    }

    #[tokio::test]
    async fn same_session_resubscribe_replaces_subscription_id() {
        let (handle, mut hub_rx, _outbound_rx, _session_rx) = spawn_worker(ClientId::Tui, 8);

        subscribe(&handle).await;
        let _ = recv_hub(&mut hub_rx).await;

        handle
            .send(ClientWorkerMessage::SubscribeSession {
                session_uuid: "sess-1".to_string(),
                subscription_id: "sub-1".to_string(),
            })
            .await
            .expect("same subscription");
        assert_no_hub(&mut hub_rx).await;

        handle
            .send(ClientWorkerMessage::SubscribeSession {
                session_uuid: "sess-1".to_string(),
                subscription_id: "sub-2".to_string(),
            })
            .await
            .expect("replacement subscription");
        assert!(matches!(
            recv_hub(&mut hub_rx).await,
            HubControlMessage::AttachClient {
                session_uuid,
                subscription_id,
                ..
            } if session_uuid == "sess-1" && subscription_id == "sub-2"
        ));
        assert_no_hub(&mut hub_rx).await;
    }

    #[tokio::test]
    async fn slow_outbound_terminal_delivery_reports_backpressure_with_routing_context() {
        let (handle, mut hub_rx, _outbound_rx, _session_rx) = spawn_worker(ClientId::Tui, 1);

        subscribe(&handle).await;
        let _ = recv_hub(&mut hub_rx).await;

        handle
            .send(ClientWorkerMessage::TerminalBytes {
                session_uuid: "sess-1".to_string(),
                data: b"first".to_vec(),
            })
            .await
            .expect("send first terminal frame");
        handle
            .send(ClientWorkerMessage::TerminalBytes {
                session_uuid: "sess-1".to_string(),
                data: b"second".to_vec(),
            })
            .await
            .expect("send second terminal frame");

        assert!(matches!(
            recv_hub(&mut hub_rx).await,
            HubControlMessage::Backpressure(WorkerBackpressure {
                source: "test.outbound",
                capacity: 1,
                session_uuid: Some(session_uuid),
                client_id: Some(ClientId::Tui),
            }) if session_uuid == "sess-1"
        ));
    }

    #[tokio::test]
    async fn reconnect_generation_drops_stale_wrapped_deliveries() {
        let (handle, mut hub_rx, mut outbound_rx, _session_rx) = spawn_worker(ClientId::Tui, 8);

        subscribe(&handle).await;
        let _ = recv_hub(&mut hub_rx).await;

        handle
            .send(ClientWorkerMessage::Health(
                ClientConnectionHealth::Reconnecting { generation: 2 },
            ))
            .await
            .expect("send reconnect health");
        assert!(matches!(
            recv_hub(&mut hub_rx).await,
            HubControlMessage::Reconnect {
                origin: HubControlOrigin::Client(ClientId::Tui),
                generation: 2,
                ..
            }
        ));

        handle
            .send(ClientWorkerMessage::Health(ClientConnectionHealth::Ready))
            .await
            .expect("send ready health");
        handle
            .send(ClientWorkerMessage::WithGeneration {
                generation: 1,
                message: Box::new(ClientWorkerMessage::TerminalBytes {
                    session_uuid: "sess-1".to_string(),
                    data: b"stale".to_vec(),
                }),
            })
            .await
            .expect("send stale frame");
        assert!(outbound_rx.try_recv().is_err());

        handle
            .send(ClientWorkerMessage::WithGeneration {
                generation: 2,
                message: Box::new(ClientWorkerMessage::TerminalBytes {
                    session_uuid: "sess-1".to_string(),
                    data: b"fresh".to_vec(),
                }),
            })
            .await
            .expect("send fresh frame");
        assert!(matches!(
            recv_egress(&mut outbound_rx).await,
            TransportEgress::TerminalBytes { data, .. } if data == b"fresh"
        ));
    }

    #[tokio::test]
    async fn ping_replies_with_transport_neutral_pong_control_frame() {
        let (handle, _hub_rx, mut outbound_rx, _session_rx) =
            spawn_worker(ClientId::Socket("socket-1".to_string()), 8);

        handle
            .send(ClientWorkerMessage::Ping {
                request_id: "req-1".to_string(),
            })
            .await
            .expect("send ping");

        assert!(matches!(
            recv_egress(&mut outbound_rx).await,
            TransportEgress::Pong { request_id } if request_id == "req-1"
        ));
    }

    #[tokio::test]
    async fn control_frames_are_gated_when_transport_is_not_ready() {
        let (handle, mut hub_rx, mut outbound_rx, _session_rx) =
            spawn_worker(ClientId::Socket("socket-1".to_string()), 8);

        subscribe(&handle).await;
        let _ = recv_hub(&mut hub_rx).await;

        handle
            .send(ClientWorkerMessage::Health(
                ClientConnectionHealth::Reconnecting { generation: 1 },
            ))
            .await
            .expect("send reconnecting health");
        let _ = recv_hub(&mut hub_rx).await;

        handle
            .send(ClientWorkerMessage::ControlFrame(
                ClientControlFrame::KittyChanged {
                    session_uuid: "sess-1".to_string(),
                    enabled: true,
                },
            ))
            .await
            .expect("send typed control");

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), outbound_rx.recv())
                .await
                .is_err(),
            "control egress should stay gated while reconnecting"
        );
    }

    #[tokio::test]
    async fn typed_scrollback_process_exit_and_binary_egress_are_lossless() {
        let (handle, mut hub_rx, mut outbound_rx, _session_rx) =
            spawn_worker(ClientId::Socket("socket-1".to_string()), 8);

        subscribe(&handle).await;
        let _ = recv_hub(&mut hub_rx).await;

        handle
            .send(ClientWorkerMessage::ControlFrame(
                ClientControlFrame::Scrollback {
                    session_uuid: "sess-1".to_string(),
                    rows: 33,
                    cols: 101,
                    kitty_enabled: true,
                    data: vec![0, 1, 2, 255],
                },
            ))
            .await
            .expect("send scrollback");
        assert!(matches!(
            recv_egress(&mut outbound_rx).await,
            TransportEgress::Scrollback {
                subscription_id,
                session_uuid,
                rows: 33,
                cols: 101,
                kitty_enabled: true,
                data,
            } if subscription_id == "sub-1"
                && session_uuid == "sess-1"
                && data == vec![0, 1, 2, 255]
        ));

        handle
            .send(ClientWorkerMessage::ControlFrame(
                ClientControlFrame::ProcessExited {
                    session_uuid: "sess-1".to_string(),
                    exit_code: Some(7),
                },
            ))
            .await
            .expect("send process exit");
        assert!(matches!(
            recv_egress(&mut outbound_rx).await,
            TransportEgress::ProcessExited {
                subscription_id,
                session_uuid,
                exit_code: Some(7),
            } if subscription_id == "sub-1" && session_uuid == "sess-1"
        ));

        handle
            .send(ClientWorkerMessage::ControlFrame(
                ClientControlFrame::Binary(vec![9, 8, 7]),
            ))
            .await
            .expect("send binary");
        assert!(matches!(
            recv_egress(&mut outbound_rx).await,
            TransportEgress::Binary(data) if data == vec![9, 8, 7]
        ));
    }

    #[tokio::test]
    async fn browser_and_tui_clients_use_the_same_worker_path() {
        for client_id in [
            ClientId::Tui,
            ClientId::Socket("socket-identity".to_string()),
            ClientId::browser("browser-identity"),
        ] {
            let (handle, mut hub_rx, mut outbound_rx, _session_rx) =
                spawn_worker(client_id.clone(), 8);

            subscribe(&handle).await;
            assert!(matches!(
                recv_hub(&mut hub_rx).await,
                HubControlMessage::AttachClient {
                    client_id: attached_client_id,
                    ..
                } if attached_client_id == client_id
            ));

            handle
                .send(ClientWorkerMessage::TerminalBytes {
                    session_uuid: "sess-1".to_string(),
                    data: b"x".to_vec(),
                })
                .await
                .expect("send terminal bytes");
            assert!(matches!(
                recv_egress(&mut outbound_rx).await,
                TransportEgress::TerminalBytes {
                    subscription_id,
                    data,
                    ..
                } if subscription_id == "sub-1" && data == b"x"
            ));
        }
    }
}
