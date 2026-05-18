//! Concrete session I/O worker runtime.
//!
//! This worker owns the blocking read side of a single per-session Unix socket.
//! It preserves the session-process wire protocol while coalescing hot-path
//! output before publishing to the terminal event broadcast.

use std::collections::{HashMap, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use tokio::sync::broadcast;

use crate::agent::notification::AgentNotification;
use crate::agent::pty::{PromptMark, PtyEvent};
use crate::hub::events::{HubEvent, HubEventTx};
use crate::session::protocol::{
    FRAME_BELL, FRAME_CWD_CHANGED, FRAME_GET_MODE_FLAGS, FRAME_GET_SCREEN, FRAME_GET_SNAPSHOT,
    FRAME_MODE_CHANGED, FRAME_NOTIFICATION, FRAME_PROCESS_EXITED, FRAME_PROMPT_MARK,
    FRAME_PTY_INPUT, FRAME_PTY_OUTPUT, FRAME_RESIZE, FRAME_SET_COLOR_PROFILE, FRAME_SHUTDOWN,
    FRAME_SNAPSHOT, FRAME_TITLE_CHANGED, Frame, FrameDecoder, ModeChanged, NotificationPayload,
    PromptMarkPayload, encode_empty, encode_frame, encode_json,
};

use super::session_io::{
    SessionIoEvent, SessionIoRequest, TerminalInitialSnapshotDelivery, TerminalOutputSubscription,
    prepare_snapshot_payload, write_paste_file,
};

const MAX_OUTPUT_BYTES: usize = 32 * 1024;
const MAX_OUTPUT_FRAMES: usize = 16;
const MAX_BATCH_AGE: Duration = Duration::from_millis(4);

pub(crate) struct SessionIoWorkerConfig {
    pub stream: UnixStream,
    pub write_stream: UnixStream,
    pub request_rx: Option<tokio::sync::mpsc::Receiver<SessionIoRequest>>,
    pub session_uuid: String,
    pub event_tx: broadcast::Sender<PtyEvent>,
    pub kitty_enabled: Arc<AtomicBool>,
    pub cursor_visible: Arc<AtomicBool>,
    pub resize_pending: Arc<AtomicBool>,
    pub last_output_at: Arc<AtomicU64>,
    pub last_human_input_ms: Arc<AtomicI64>,
    pub response_tx: std::sync::mpsc::Sender<Frame>,
    pub hub_event_tx: HubEventTx,
    pub reader_alive: Arc<AtomicBool>,
    pub pending_snapshot_requests: Arc<Mutex<VecDeque<PendingSnapshotRequest>>>,
    pub terminal_subscriptions: Arc<Mutex<HashMap<String, TerminalOutputSubscription>>>,
}

#[derive(Debug, Clone)]
pub(crate) enum PendingSnapshotRequest {
    Hub {
        request_id: String,
    },
    Initial {
        delivery: TerminalInitialSnapshotDelivery,
    },
}

pub(crate) struct SessionIoWorkerHandle {
    _join: std::thread::JoinHandle<()>,
    _request_join: std::thread::JoinHandle<()>,
}

pub(crate) struct SessionIoWorker;

impl SessionIoWorker {
    pub(crate) fn spawn(mut config: SessionIoWorkerConfig) -> Result<SessionIoWorkerHandle> {
        let thread_name = format!(
            "session-io-{}",
            &config.session_uuid[..config.session_uuid.len().min(16)]
        );
        let request_config = SessionIoRequestProcessorConfig {
            stream: config
                .write_stream
                .try_clone()
                .context("dup session socket for request processor thread")?,
            request_rx: config
                .request_rx
                .take()
                .context("session I/O request receiver missing")?,
            session_uuid: config.session_uuid.clone(),
            hub_event_tx: config.hub_event_tx.clone(),
            reader_alive: Arc::clone(&config.reader_alive),
            last_human_input_ms: Arc::clone(&config.last_human_input_ms),
            pending_snapshot_requests: Arc::clone(&config.pending_snapshot_requests),
            terminal_subscriptions: Arc::clone(&config.terminal_subscriptions),
        };
        let request_thread_name = format!(
            "session-io-requests-{}",
            &config.session_uuid[..config.session_uuid.len().min(16)]
        );
        let request_join = std::thread::Builder::new()
            .name(request_thread_name)
            .spawn(move || run_request_processor(request_config))
            .context("spawn session I/O request processor thread")?;

        let join = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || run_worker(config))
            .context("spawn session I/O worker thread")?;

        Ok(SessionIoWorkerHandle {
            _join: join,
            _request_join: request_join,
        })
    }
}

fn run_worker(config: SessionIoWorkerConfig) {
    let mut runtime = SessionIoRuntime::new(config);
    runtime.run();
}

struct SessionIoRequestProcessorConfig {
    stream: UnixStream,
    request_rx: tokio::sync::mpsc::Receiver<SessionIoRequest>,
    session_uuid: String,
    hub_event_tx: HubEventTx,
    reader_alive: Arc<AtomicBool>,
    last_human_input_ms: Arc<AtomicI64>,
    pending_snapshot_requests: Arc<Mutex<VecDeque<PendingSnapshotRequest>>>,
    terminal_subscriptions: Arc<Mutex<HashMap<String, TerminalOutputSubscription>>>,
}

fn run_request_processor(config: SessionIoRequestProcessorConfig) {
    let mut processor = SessionIoRequestProcessor {
        stream: config.stream,
        request_rx: config.request_rx,
        session_uuid: config.session_uuid,
        hub_event_tx: config.hub_event_tx,
        reader_alive: config.reader_alive,
        last_human_input_ms: config.last_human_input_ms,
        pending_snapshot_requests: config.pending_snapshot_requests,
        terminal_subscriptions: config.terminal_subscriptions,
    };
    processor.run();
}

struct SessionIoRequestProcessor {
    stream: UnixStream,
    request_rx: tokio::sync::mpsc::Receiver<SessionIoRequest>,
    session_uuid: String,
    hub_event_tx: HubEventTx,
    reader_alive: Arc<AtomicBool>,
    last_human_input_ms: Arc<AtomicI64>,
    pending_snapshot_requests: Arc<Mutex<VecDeque<PendingSnapshotRequest>>>,
    terminal_subscriptions: Arc<Mutex<HashMap<String, TerminalOutputSubscription>>>,
}

impl SessionIoRequestProcessor {
    fn run(&mut self) {
        while let Some(request) = self.request_rx.blocking_recv() {
            if !self.reader_alive.load(Ordering::Acquire) {
                break;
            }
            self.handle_request(request);
        }
    }

    fn handle_request(&mut self, request: SessionIoRequest) {
        match request {
            SessionIoRequest::PtyInput { data } => {
                Self::stamp_human_input(&self.last_human_input_ms, &data);
                if let Err(e) = self.write_frame(encode_frame(FRAME_PTY_INPUT, &data)) {
                    log::warn!("[session-io] PTY input request failed: {e}");
                }
            }
            SessionIoRequest::Resize { rows, cols } => {
                log::info!(
                    "[session-io] queue resize for {} to {}x{}",
                    self.session_uuid,
                    cols,
                    rows
                );
                match encode_json(
                    FRAME_RESIZE,
                    &serde_json::json!({ "rows": rows, "cols": cols }),
                )
                .and_then(|frame| self.write_frame(frame).map_err(anyhow::Error::from))
                {
                    Ok(()) => {}
                    Err(e) => log::warn!("[session-io] resize request failed: {e}"),
                }
            }
            SessionIoRequest::GetSnapshot { request_id } => {
                if let Ok(mut pending) = self.pending_snapshot_requests.lock() {
                    pending.push_back(PendingSnapshotRequest::Hub {
                        request_id: request_id.clone(),
                    });
                }
                if let Err(e) = self.write_frame(encode_empty(FRAME_GET_SNAPSHOT)) {
                    if let Ok(mut pending) = self.pending_snapshot_requests.lock() {
                        pending.retain(|pending| {
                            !matches!(
                                pending,
                                PendingSnapshotRequest::Hub { request_id: pending_id }
                                    if pending_id == &request_id
                            )
                        });
                    }
                    log::warn!("[session-io] snapshot RPC request failed: {e}");
                    let _ = self
                        .hub_event_tx
                        .send(HubEvent::SessionIo(SessionIoEvent::Snapshot {
                            request_id,
                            session_uuid: self.session_uuid.clone(),
                            payload: Vec::new(),
                        }));
                }
            }
            SessionIoRequest::GetInitialSnapshot { mut delivery } => {
                log::info!(
                    "[session-io] request initial snapshot for {} subscription={} key={} target={}x{} request={}",
                    self.session_uuid,
                    delivery.subscription_id,
                    delivery.subscription_key,
                    delivery.cols,
                    delivery.rows,
                    delivery.request_id
                );
                delivery.session_io_accepted_at = Some(Instant::now());
                if let Ok(mut pending) = self.pending_snapshot_requests.lock() {
                    pending.push_back(PendingSnapshotRequest::Initial {
                        delivery: delivery.clone(),
                    });
                }
                if let Err(e) = self.write_frame(encode_empty(FRAME_GET_SNAPSHOT)) {
                    if let Ok(mut pending) = self.pending_snapshot_requests.lock() {
                        pending.retain(|pending| {
                            !matches!(
                                pending,
                                PendingSnapshotRequest::Initial { delivery: pending_delivery }
                                    if pending_delivery.request_id == delivery.request_id
                            )
                        });
                    }
                    log::warn!("[session-io] initial snapshot RPC request failed: {e}");
                    Self::deliver_initial_attach_state(
                        &delivery,
                        crate::worker::client::TerminalAttachState::NotReady,
                    );
                    Self::unregister_initial_snapshot(&delivery, &self.terminal_subscriptions);
                }
            }
            SessionIoRequest::SubscribeTerminal { subscription } => {
                log::info!(
                    "[session-io] register terminal subscription for {} subscription={} key={}",
                    self.session_uuid,
                    subscription.subscription_id,
                    subscription.subscription_key
                );
                if let Ok(mut subscriptions) = self.terminal_subscriptions.lock() {
                    subscriptions.insert(subscription.subscription_key.clone(), subscription);
                }
            }
            SessionIoRequest::UnsubscribeTerminal { subscription_key } => {
                log::info!(
                    "[session-io] unregister terminal subscription for {} key={}",
                    self.session_uuid,
                    subscription_key
                );
                if let Ok(mut subscriptions) = self.terminal_subscriptions.lock() {
                    subscriptions.remove(&subscription_key);
                }
            }
            SessionIoRequest::PasteFile {
                request_id,
                filename,
                data,
            } => {
                let session_uuid = self.session_uuid.clone();
                match write_paste_file(&session_uuid, &filename, &data, |input| {
                    self.write_frame(encode_frame(FRAME_PTY_INPUT, input))
                        .map_err(|e| e.to_string())
                }) {
                    Ok(write) => {
                        let _ = self.hub_event_tx.send(HubEvent::SessionIo(
                            SessionIoEvent::PasteFileWritten {
                                request_id,
                                session_uuid: self.session_uuid.clone(),
                                path: write.path,
                                bytes: write.bytes,
                            },
                        ));
                    }
                    Err((reason, detail)) => {
                        let _ = self.hub_event_tx.send(HubEvent::SessionIo(
                            SessionIoEvent::PasteFileFailed {
                                request_id,
                                session_uuid: self.session_uuid.clone(),
                                reason,
                                detail,
                            },
                        ));
                    }
                }
            }
            SessionIoRequest::PrepareSnapshot {
                request_id,
                snapshot,
                recovery,
            } => {
                let event = if let Some(prepared) = prepare_snapshot_payload(&snapshot) {
                    SessionIoEvent::PreparedSnapshot {
                        request_id,
                        session_uuid: self.session_uuid.clone(),
                        uncompressed_len: prepared.uncompressed_len,
                        payload: prepared.payload,
                        recovery,
                    }
                } else {
                    SessionIoEvent::PreparedSnapshot {
                        request_id,
                        session_uuid: self.session_uuid.clone(),
                        uncompressed_len: 0,
                        payload: Vec::new(),
                        recovery,
                    }
                };
                let _ = self.hub_event_tx.send(HubEvent::SessionIo(event));
            }
            SessionIoRequest::GetModeFlags { .. } => {
                if let Err(e) = self.write_frame(encode_empty(FRAME_GET_MODE_FLAGS)) {
                    log::warn!("[session-io] mode-flags RPC request failed: {e}");
                }
            }
            SessionIoRequest::GetScreen { .. } => {
                if let Err(e) = self.write_frame(encode_empty(FRAME_GET_SCREEN)) {
                    log::warn!("[session-io] screen RPC request failed: {e}");
                }
            }
            SessionIoRequest::SetColorProfile(profile) => {
                match encode_json(FRAME_SET_COLOR_PROFILE, &profile)
                    .and_then(|frame| self.write_frame(frame).map_err(anyhow::Error::from))
                {
                    Ok(()) => {}
                    Err(e) => log::warn!("[session-io] color-profile request failed: {e}"),
                }
            }
            SessionIoRequest::Shutdown { .. } => {
                if let Err(e) = self.write_frame(encode_empty(FRAME_SHUTDOWN)) {
                    log::warn!("[session-io] shutdown request failed: {e}");
                }
            }
        }
    }

    fn deliver_initial_attach_state(
        delivery: &TerminalInitialSnapshotDelivery,
        state: crate::worker::client::TerminalAttachState,
    ) {
        let _ = delivery
            .worker
            .try_send(crate::worker::client::ClientWorkerMessage::ControlFrame(
                crate::worker::client::ClientControlFrame::TerminalAttach {
                    subscription_id: delivery.subscription_id.clone(),
                    session_uuid: delivery.session_uuid.clone(),
                    state,
                },
            ));
    }

    fn unregister_initial_snapshot(
        delivery: &TerminalInitialSnapshotDelivery,
        subscriptions: &Arc<Mutex<HashMap<String, TerminalOutputSubscription>>>,
    ) {
        if let Ok(mut subscriptions) = subscriptions.lock() {
            subscriptions.remove(&delivery.subscription_key);
        }
        let _ = delivery.worker.try_send(
            crate::worker::client::ClientWorkerMessage::UnregisterSessionIoSender {
                session_uuid: delivery.session_uuid.clone(),
            },
        );
    }

    fn stamp_human_input(last_human_input_ms: &AtomicI64, data: &[u8]) {
        if data == b"\x1b[I" || data == b"\x1b[O" {
            return;
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        last_human_input_ms.store(now, Ordering::Relaxed);
    }

    fn write_frame(&mut self, frame: Vec<u8>) -> std::io::Result<()> {
        self.stream.write_all(&frame)?;
        self.stream.flush()
    }
}

struct SessionIoRuntime {
    stream: UnixStream,
    decoder: FrameDecoder,
    session_uuid: String,
    event_tx: broadcast::Sender<PtyEvent>,
    kitty_enabled: Arc<AtomicBool>,
    cursor_visible: Arc<AtomicBool>,
    resize_pending: Arc<AtomicBool>,
    last_output_at: Arc<AtomicU64>,
    response_tx: std::sync::mpsc::Sender<Frame>,
    hub_event_tx: HubEventTx,
    reader_alive: Arc<AtomicBool>,
    pending_snapshot_requests: Arc<Mutex<VecDeque<PendingSnapshotRequest>>>,
    terminal_subscriptions: Arc<Mutex<HashMap<String, TerminalOutputSubscription>>>,
    terminal_filter_buffers: HashMap<String, Vec<u8>>,
    coalescer: SessionIoCoalescer,
    saw_process_exit: bool,
}

impl SessionIoRuntime {
    fn new(config: SessionIoWorkerConfig) -> Self {
        Self {
            stream: config.stream,
            decoder: FrameDecoder::new(),
            session_uuid: config.session_uuid,
            event_tx: config.event_tx,
            kitty_enabled: config.kitty_enabled,
            cursor_visible: config.cursor_visible,
            resize_pending: config.resize_pending,
            last_output_at: config.last_output_at,
            response_tx: config.response_tx,
            hub_event_tx: config.hub_event_tx,
            reader_alive: config.reader_alive,
            pending_snapshot_requests: config.pending_snapshot_requests,
            terminal_subscriptions: config.terminal_subscriptions,
            terminal_filter_buffers: HashMap::new(),
            coalescer: SessionIoCoalescer::default(),
            saw_process_exit: false,
        }
    }

    fn run(&mut self) {
        log::info!(
            "[session-io] started for {}",
            &self.session_uuid[..self.session_uuid.len().min(16)]
        );

        let mut buf = [0u8; 8192];
        loop {
            let timeout = self.coalescer.pending_timeout();
            let _ = self.stream.set_read_timeout(timeout);

            let n = match self.stream.read(&mut buf) {
                Ok(0) => {
                    log::info!("[session-io] session socket EOF");
                    break;
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                    ) =>
                {
                    self.flush_pending();
                    continue;
                }
                Err(e) => {
                    log::warn!("[session-io] read error: {e}");
                    break;
                }
                Ok(n) => n,
            };

            let frames = self.decoder.feed(&buf[..n]);
            for frame in frames {
                if !self.handle_frame(frame) {
                    self.reader_alive.store(false, Ordering::Release);
                    return;
                }
            }
            if self.decoder.is_desynced() {
                self.flush_pending();
                log::warn!("[session-io] protocol desync detected; treating as connection death");
                break;
            }
            self.flush_if_ready();
        }

        self.flush_pending();
        if !self.saw_process_exit {
            let _ = self.hub_event_tx.send(HubEvent::SessionProcessExited {
                session_uuid: self.session_uuid.clone(),
                exit_code: None,
            });
        }
        self.reader_alive.store(false, Ordering::Release);
    }

    fn handle_frame(&mut self, frame: Frame) -> bool {
        match frame.frame_type {
            FRAME_PTY_OUTPUT => {
                self.resize_pending.store(false, Ordering::Release);
                self.last_output_at.store(now_millis(), Ordering::Relaxed);
                self.coalescer.push_output(frame.payload);
            }
            FRAME_TITLE_CHANGED => {
                self.coalescer
                    .set_title(String::from_utf8_lossy(&frame.payload).into_owned());
            }
            FRAME_BELL => {
                self.flush_pending();
                let _ = self
                    .event_tx
                    .send(PtyEvent::notification(AgentNotification::Bell));
            }
            FRAME_MODE_CHANGED => {
                if let Ok(mode) = frame.json::<ModeChanged>() {
                    self.coalescer.merge_mode(mode);
                }
            }
            FRAME_CWD_CHANGED => {
                self.coalescer
                    .set_cwd(String::from_utf8_lossy(&frame.payload).into_owned());
            }
            FRAME_PROMPT_MARK => {
                self.flush_pending();
                if let Ok(payload) = frame.json::<PromptMarkPayload>() {
                    if let Some(mark) = PromptMark::from_name(payload.mark.as_str()) {
                        let _ = self.event_tx.send(PtyEvent::prompt_mark(mark));
                    }
                }
            }
            FRAME_NOTIFICATION => {
                self.flush_pending();
                if let Ok(payload) = frame.json::<NotificationPayload>() {
                    let notif = if payload.title.is_empty() {
                        AgentNotification::Osc9((!payload.body.is_empty()).then_some(payload.body))
                    } else {
                        AgentNotification::Osc777 {
                            title: payload.title,
                            body: payload.body,
                        }
                    };
                    let _ = self.event_tx.send(PtyEvent::notification(notif));
                }
            }
            FRAME_PROCESS_EXITED => {
                self.flush_pending();
                let exit_code = frame
                    .json::<serde_json::Value>()
                    .ok()
                    .and_then(|v| v["exit_code"].as_i64())
                    .map(|c| c as i32);
                self.saw_process_exit = true;
                self.deliver_terminal_control(
                    crate::worker::client::ClientControlFrame::ProcessExited {
                        session_uuid: self.session_uuid.clone(),
                        exit_code,
                    },
                );
                let _ = self.hub_event_tx.send(HubEvent::SessionProcessExited {
                    session_uuid: self.session_uuid.clone(),
                    exit_code,
                });
                log::info!("[session-io] process exited (code={exit_code:?})");
            }
            FRAME_SNAPSHOT => {
                self.flush_pending();
                let pending_request = self
                    .pending_snapshot_requests
                    .lock()
                    .ok()
                    .and_then(|mut pending| pending.pop_front());
                match pending_request {
                    Some(PendingSnapshotRequest::Hub { request_id }) => {
                        let _ =
                            self.hub_event_tx
                                .send(HubEvent::SessionIo(SessionIoEvent::Snapshot {
                                    request_id,
                                    session_uuid: self.session_uuid.clone(),
                                    payload: frame.payload,
                                }));
                    }
                    Some(PendingSnapshotRequest::Initial { delivery }) => {
                        self.deliver_initial_snapshot(delivery, frame.payload);
                    }
                    None if self.response_tx.send(frame).is_err() => {
                        log::debug!("[session-io] response channel closed");
                        return false;
                    }
                    None => {}
                }
            }
            _ => {
                self.flush_pending();
                if self.response_tx.send(frame).is_err() {
                    log::debug!("[session-io] response channel closed");
                    return false;
                }
            }
        }
        true
    }

    fn deliver_initial_snapshot(
        &mut self,
        delivery: TerminalInitialSnapshotDelivery,
        snapshot: Vec<u8>,
    ) {
        let snapshot_ready_at = Instant::now();
        log::info!(
            "[session-io] deliver initial snapshot for {} subscription={} key={} target={}x{} snapshot_bytes={}",
            self.session_uuid,
            delivery.subscription_id,
            delivery.subscription_key,
            delivery.cols,
            delivery.rows,
            snapshot.len()
        );
        if snapshot.is_empty() {
            if crate::session::session_process_is_live(&self.session_uuid) {
                let mut outbound_accepted = true;
                if delivery.confirm_subscription {
                    outbound_accepted &= delivery
                        .worker
                        .try_send(crate::worker::client::ClientWorkerMessage::ControlFrame(
                            crate::worker::client::ClientControlFrame::BoundaryJson(
                                serde_json::json!({
                                    "type": "subscribed",
                                    "subscriptionId": delivery.subscription_id.clone(),
                                }),
                            ),
                        ))
                        .is_ok();
                }
                outbound_accepted &= delivery
                    .worker
                    .try_send(crate::worker::client::ClientWorkerMessage::ControlFrame(
                        crate::worker::client::ClientControlFrame::TerminalAttach {
                            subscription_id: delivery.subscription_id.clone(),
                            session_uuid: self.session_uuid.clone(),
                            state: crate::worker::client::TerminalAttachState::Reconnecting,
                        },
                    ))
                    .is_ok();
                if outbound_accepted {
                    self.emit_initial_attach_timing(&delivery, snapshot.len(), snapshot_ready_at);
                }
                self.activate_live_subscription_after_snapshot(&delivery);
            } else {
                let _ = delivery.worker.try_send(
                    crate::worker::client::ClientWorkerMessage::ControlFrame(
                        crate::worker::client::ClientControlFrame::ProcessExited {
                            session_uuid: self.session_uuid.clone(),
                            exit_code: None,
                        },
                    ),
                );
                let _ = delivery.worker.try_send(
                    crate::worker::client::ClientWorkerMessage::UnregisterSessionIoSender {
                        session_uuid: self.session_uuid.clone(),
                    },
                );
                if let Ok(mut subscriptions) = self.terminal_subscriptions.lock() {
                    subscriptions.remove(&delivery.subscription_key);
                }
            }
            return;
        }

        let snapshot_bytes = snapshot.len();
        let data = match delivery.payload_mode {
            crate::worker::session_io::TerminalSnapshotPayloadMode::Raw => snapshot,
            crate::worker::session_io::TerminalSnapshotPayloadMode::PrefixedGzip => {
                prepare_snapshot_payload(&snapshot)
                    .map(|prepared| prepared.payload)
                    .unwrap_or_default()
            }
        };

        if data.is_empty() {
            return;
        }

        match delivery
            .worker
            .try_send(crate::worker::client::ClientWorkerMessage::ControlFrame(
                crate::worker::client::ClientControlFrame::Scrollback {
                    session_uuid: self.session_uuid.clone(),
                    rows: delivery.rows,
                    cols: delivery.cols,
                    kitty_enabled: delivery.kitty_enabled,
                    data,
                },
            )) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                self.report_initial_snapshot_backpressure(&delivery);
                return;
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                if let Ok(mut subscriptions) = self.terminal_subscriptions.lock() {
                    subscriptions.remove(&delivery.subscription_key);
                }
                return;
            }
        }

        let mut outbound_accepted = true;
        if delivery.confirm_subscription {
            outbound_accepted &= delivery
                .worker
                .try_send(crate::worker::client::ClientWorkerMessage::ControlFrame(
                    crate::worker::client::ClientControlFrame::BoundaryJson(serde_json::json!({
                        "type": "subscribed",
                        "subscriptionId": delivery.subscription_id.clone(),
                    })),
                ))
                .is_ok();
        }

        outbound_accepted &= delivery
            .worker
            .try_send(crate::worker::client::ClientWorkerMessage::ControlFrame(
                crate::worker::client::ClientControlFrame::TerminalAttach {
                    subscription_id: delivery.subscription_id.clone(),
                    session_uuid: self.session_uuid.clone(),
                    state: crate::worker::client::TerminalAttachState::Attached,
                },
            ))
            .is_ok();
        if outbound_accepted {
            self.emit_initial_attach_timing(&delivery, snapshot_bytes, snapshot_ready_at);
        }
        self.activate_live_subscription_after_snapshot(&delivery);
    }

    fn emit_initial_attach_timing(
        &self,
        delivery: &TerminalInitialSnapshotDelivery,
        snapshot_bytes: usize,
        snapshot_ready_at: Instant,
    ) {
        if delivery.attach_requested_at.is_none() {
            return;
        }
        let outbound_accepted_at = Instant::now();
        let timing = crate::worker::session_io::TerminalAttachTiming {
            request_id: delivery.request_id.clone(),
            subscription_key: delivery.subscription_key.clone(),
            session_uuid: delivery.session_uuid.clone(),
            subscription_id: delivery.subscription_id.clone(),
            snapshot_bytes,
            attach_to_client_worker_subscribed: delivery.attach_requested_at.and_then(|started| {
                delivery
                    .client_worker_subscribed_at
                    .map(|ended| ended - started)
            }),
            attach_to_session_io_queued: delivery.attach_requested_at.and_then(|started| {
                delivery
                    .session_io_snapshot_queued_at
                    .map(|ended| ended - started)
            }),
            attach_to_session_io_accepted: delivery
                .attach_requested_at
                .and_then(|started| delivery.session_io_accepted_at.map(|ended| ended - started)),
            attach_to_snapshot_ready: delivery
                .attach_requested_at
                .map(|started| snapshot_ready_at - started),
            snapshot_ready_to_client_worker_accepted: outbound_accepted_at - snapshot_ready_at,
            attach_to_client_worker_accepted: delivery
                .attach_requested_at
                .map(|started| outbound_accepted_at - started),
        };
        let _ = self
            .hub_event_tx
            .send(HubEvent::SessionIo(SessionIoEvent::TerminalAttachTiming(
                timing,
            )));
    }

    fn activate_live_subscription_after_snapshot(
        &mut self,
        delivery: &TerminalInitialSnapshotDelivery,
    ) {
        let Some(subscription) = delivery.live_subscription.clone() else {
            return;
        };
        log::info!(
            "[session-io] register terminal subscription after initial snapshot for {} subscription={} key={}",
            self.session_uuid,
            subscription.subscription_id,
            subscription.subscription_key
        );
        if let Ok(mut subscriptions) = self.terminal_subscriptions.lock() {
            subscriptions.insert(subscription.subscription_key.clone(), subscription);
        }
    }

    fn report_initial_snapshot_backpressure(&self, delivery: &TerminalInitialSnapshotDelivery) {
        let _ = self.hub_event_tx.send(HubEvent::ClientWorkerControl(
            crate::worker::hub_control::HubControlMessage::Backpressure(
                crate::worker::hub_control::WorkerBackpressure {
                    source: "worker.session_io.initial_snapshot_delivery",
                    capacity: crate::worker::client::CLIENT_WORKER_QUEUE.capacity,
                    session_uuid: Some(self.session_uuid.clone()),
                    client_id: Some(delivery.worker.client_id.clone()),
                },
            ),
        ));
    }

    fn flush_if_ready(&mut self) {
        if self.coalescer.should_flush_output() || self.coalescer.metadata_age_expired() {
            self.flush_pending();
        }
    }

    fn flush_output(&mut self) {
        if let Some(output) = self.coalescer.take_output() {
            self.deliver_terminal_output(&output);
            let _ = self.event_tx.send(PtyEvent::output(output));
        }
    }

    fn flush_pending(&mut self) {
        self.flush_output();
        self.flush_metadata();
    }

    fn flush_metadata(&mut self) {
        for event in self.coalescer.take_metadata(
            &self.session_uuid,
            &self.kitty_enabled,
            &self.cursor_visible,
        ) {
            match &event {
                PtyEvent::KittyChanged(enabled) => {
                    self.deliver_terminal_control(
                        crate::worker::client::ClientControlFrame::KittyChanged {
                            session_uuid: self.session_uuid.clone(),
                            enabled: *enabled,
                        },
                    );
                }
                PtyEvent::FocusReportingChanged(enabled) => {
                    self.deliver_terminal_control(
                        crate::worker::client::ClientControlFrame::FocusReportingChanged {
                            session_uuid: self.session_uuid.clone(),
                            enabled: *enabled,
                        },
                    );
                }
                _ => {}
            }
            let _ = self.event_tx.send(event);
        }
    }

    fn current_terminal_subscriptions(&mut self) -> Vec<TerminalOutputSubscription> {
        let subscriptions = self
            .terminal_subscriptions
            .lock()
            .map(|subscriptions| subscriptions.clone())
            .unwrap_or_default();
        self.terminal_filter_buffers
            .retain(|subscription_key, _| subscriptions.contains_key(subscription_key));
        subscriptions.into_values().collect()
    }

    fn deliver_terminal_output(&mut self, output: &[u8]) {
        for subscription in self.current_terminal_subscriptions() {
            let buffer = self
                .terminal_filter_buffers
                .entry(subscription.subscription_key.clone())
                .or_default();
            let filtered = subscription
                .filter
                .filter_chunk(&self.session_uuid, buffer, output);
            if filtered.is_empty() {
                continue;
            }
            let data = if subscription.output_prefix.is_empty() {
                filtered
            } else {
                let mut prefixed =
                    Vec::with_capacity(subscription.output_prefix.len() + filtered.len());
                prefixed.extend(&subscription.output_prefix);
                prefixed.extend(filtered);
                prefixed
            };
            match subscription.worker.try_send(
                crate::worker::client::ClientWorkerMessage::TerminalBytes {
                    session_uuid: self.session_uuid.clone(),
                    data,
                },
            ) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    self.report_terminal_delivery_backpressure(&subscription);
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    if let Ok(mut subscriptions) = self.terminal_subscriptions.lock() {
                        subscriptions.remove(&subscription.subscription_key);
                    }
                }
            }
        }
    }

    fn deliver_terminal_control(&mut self, frame: crate::worker::client::ClientControlFrame) {
        for subscription in self.current_terminal_subscriptions() {
            match subscription.worker.try_send(
                crate::worker::client::ClientWorkerMessage::ControlFrame(frame.clone()),
            ) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    self.report_terminal_delivery_backpressure(&subscription);
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    if let Ok(mut subscriptions) = self.terminal_subscriptions.lock() {
                        subscriptions.remove(&subscription.subscription_key);
                    }
                }
            }
        }
    }

    fn report_terminal_delivery_backpressure(&self, subscription: &TerminalOutputSubscription) {
        let _ = self.hub_event_tx.send(HubEvent::ClientWorkerControl(
            crate::worker::hub_control::HubControlMessage::Backpressure(
                crate::worker::hub_control::WorkerBackpressure {
                    source: "worker.session_io.terminal_delivery",
                    capacity: crate::worker::client::CLIENT_WORKER_QUEUE.capacity,
                    session_uuid: Some(self.session_uuid.clone()),
                    client_id: Some(subscription.worker.client_id.clone()),
                },
            ),
        ));
    }
}

#[derive(Default)]
struct SessionIoCoalescer {
    output: Vec<u8>,
    output_frames: usize,
    output_started_at: Option<Instant>,
    mode: Option<ModeChanged>,
    title: Option<String>,
    cwd: Option<String>,
    metadata_started_at: Option<Instant>,
}

impl SessionIoCoalescer {
    fn push_output(&mut self, data: Vec<u8>) {
        if data.is_empty() {
            return;
        }
        if self.output_started_at.is_none() {
            self.output_started_at = Some(Instant::now());
        }
        self.output.extend_from_slice(&data);
        self.output_frames += 1;
    }

    fn set_title(&mut self, title: String) {
        self.ensure_metadata_started();
        self.title = Some(title);
    }

    fn set_cwd(&mut self, cwd: String) {
        self.ensure_metadata_started();
        self.cwd = Some(cwd);
    }

    fn merge_mode(&mut self, mode: ModeChanged) {
        self.ensure_metadata_started();
        let merged = self.mode.get_or_insert_with(ModeChanged::default);
        if mode.kitty_enabled.is_some() {
            merged.kitty_enabled = mode.kitty_enabled;
        }
        if mode.cursor_visible.is_some() {
            merged.cursor_visible = mode.cursor_visible;
        }
        if mode.alt_screen.is_some() {
            merged.alt_screen = mode.alt_screen;
        }
        if mode.mouse_mode.is_some() {
            merged.mouse_mode = mode.mouse_mode;
        }
        if mode.bracketed_paste.is_some() {
            merged.bracketed_paste = mode.bracketed_paste;
        }
        if mode.focus_reporting.is_some() {
            merged.focus_reporting = mode.focus_reporting;
        }
        if mode.application_cursor.is_some() {
            merged.application_cursor = mode.application_cursor;
        }
    }

    fn pending_timeout(&self) -> Option<Duration> {
        let started = self.output_started_at.or(self.metadata_started_at)?;
        let elapsed = started.elapsed();
        Some(MAX_BATCH_AGE.saturating_sub(elapsed))
    }

    fn should_flush_output(&self) -> bool {
        !self.output.is_empty()
            && (self.output.len() >= MAX_OUTPUT_BYTES
                || self.output_frames >= MAX_OUTPUT_FRAMES
                || self
                    .output_started_at
                    .is_some_and(|started| started.elapsed() >= MAX_BATCH_AGE))
    }

    fn metadata_age_expired(&self) -> bool {
        self.has_metadata()
            && self
                .metadata_started_at
                .is_some_and(|started| started.elapsed() >= MAX_BATCH_AGE)
    }

    fn take_output(&mut self) -> Option<Vec<u8>> {
        if self.output.is_empty() {
            return None;
        }

        let output = std::mem::take(&mut self.output);
        self.output_frames = 0;
        self.output_started_at = None;
        Some(output)
    }

    fn take_metadata(
        &mut self,
        _session_uuid: &str,
        kitty_enabled: &AtomicBool,
        cursor_visible: &AtomicBool,
    ) -> Vec<PtyEvent> {
        let mut events = Vec::new();
        if let Some(mode) = self.mode.take() {
            if let Some(kitty) = mode.kitty_enabled {
                let old = kitty_enabled.load(Ordering::Relaxed);
                if kitty != old {
                    kitty_enabled.store(kitty, Ordering::Relaxed);
                    events.push(PtyEvent::kitty_changed(kitty));
                }
            }
            if let Some(vis) = mode.cursor_visible {
                cursor_visible.store(vis, Ordering::Relaxed);
                events.push(PtyEvent::cursor_visibility_changed(vis));
            }
            if let Some(focus) = mode.focus_reporting {
                events.push(PtyEvent::focus_reporting_changed(focus));
            }
        }

        if let Some(title) = self.title.take() {
            events.push(PtyEvent::title_changed(title));
        }
        if let Some(cwd) = self.cwd.take() {
            events.push(PtyEvent::cwd_changed(cwd));
        }
        if !self.has_metadata() {
            self.metadata_started_at = None;
        }
        events
    }

    fn ensure_metadata_started(&mut self) {
        if self.metadata_started_at.is_none() {
            self.metadata_started_at = Some(Instant::now());
        }
    }

    fn has_metadata(&self) -> bool {
        self.mode.is_some() || self.title.is_some() || self.cwd.is_some()
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::net::Shutdown;
    use std::sync::mpsc;
    use tokio::sync::mpsc as tokio_mpsc;

    use super::*;
    use crate::session::protocol::{FRAME_MODE_FLAGS, FRAME_SCREEN};

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    const TEST_EVENT_TIMEOUT: Duration = Duration::from_millis(500);

    fn block_on_with_timeout<F, T>(future: F) -> T
    where
        F: Future<Output = T>,
    {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime")
            .block_on(async {
                tokio::time::timeout(TEST_EVENT_TIMEOUT, future)
                    .await
                    .expect("timed out waiting for worker event")
            })
    }

    fn spawn_test_worker(
        reader: UnixStream,
    ) -> (
        broadcast::Receiver<PtyEvent>,
        mpsc::Receiver<Frame>,
        tokio_mpsc::Receiver<HubEvent>,
        Arc<AtomicBool>,
    ) {
        let (_request_tx, event_rx, response_rx, hub_rx, alive) =
            spawn_test_worker_with_requests(reader);
        (event_rx, response_rx, hub_rx, alive)
    }

    fn spawn_test_worker_with_requests(
        reader: UnixStream,
    ) -> (
        tokio_mpsc::Sender<SessionIoRequest>,
        broadcast::Receiver<PtyEvent>,
        mpsc::Receiver<Frame>,
        tokio_mpsc::Receiver<HubEvent>,
        Arc<AtomicBool>,
    ) {
        let (event_tx, event_rx) = broadcast::channel(32);
        let (response_tx, response_rx) = mpsc::channel();
        let (hub_tx, hub_rx) = tokio_mpsc::channel(64);
        let (request_tx, request_rx) = tokio_mpsc::channel(32);
        let alive = Arc::new(AtomicBool::new(true));
        let write_stream = reader.try_clone().expect("clone request stream");

        SessionIoWorker::spawn(SessionIoWorkerConfig {
            stream: reader,
            write_stream,
            request_rx: Some(request_rx),
            session_uuid: "sess-test-io".to_string(),
            event_tx,
            kitty_enabled: Arc::new(AtomicBool::new(false)),
            cursor_visible: Arc::new(AtomicBool::new(true)),
            resize_pending: Arc::new(AtomicBool::new(false)),
            last_output_at: Arc::new(AtomicU64::new(0)),
            last_human_input_ms: Arc::new(AtomicI64::new(0)),
            response_tx,
            hub_event_tx: HubEventTx::from(hub_tx),
            reader_alive: Arc::clone(&alive),
            pending_snapshot_requests: Arc::new(Mutex::new(VecDeque::new())),
            terminal_subscriptions: Arc::new(Mutex::new(HashMap::new())),
        })
        .expect("spawn worker");

        (request_tx, event_rx, response_rx, hub_rx, alive)
    }

    fn recv_pty_event(rx: &mut broadcast::Receiver<PtyEvent>) -> PtyEvent {
        block_on_with_timeout(async { rx.recv().await.expect("receive pty event from worker") })
    }

    fn recv_output(rx: &mut broadcast::Receiver<PtyEvent>) -> Vec<u8> {
        match recv_pty_event(rx) {
            PtyEvent::Output(data) => data,
            other => panic!("expected output, got {other:?}"),
        }
    }

    fn recv_hub_event(rx: &mut tokio_mpsc::Receiver<HubEvent>) -> HubEvent {
        block_on_with_timeout(async { rx.recv().await.expect("receive hub event") })
    }

    fn assert_no_hub_event(rx: &mut tokio_mpsc::Receiver<HubEvent>) {
        assert!(
            rx.try_recv().is_err(),
            "unexpected extra hub event after ordered boundary"
        );
    }

    #[test]
    fn terminal_output_is_delivered_directly_to_client_worker_subscription() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (request_tx, mut event_rx, _response_rx, mut hub_rx, _alive) =
            spawn_test_worker_with_requests(reader);
        let (client_tx, mut client_rx) =
            tokio_mpsc::channel::<crate::worker::client::ClientWorkerMessage>(8);

        block_on_with_timeout(async {
            request_tx
                .send(SessionIoRequest::SubscribeTerminal {
                    subscription: TerminalOutputSubscription {
                        subscription_key: "client:sess-test-io".to_string(),
                        subscription_id: "terminal_sess-test-io".to_string(),
                        worker: crate::worker::client::ClientWorkerHandle {
                            client_id: crate::client::ClientId::Socket("client".to_string()),
                            tx: client_tx,
                        },
                        output_prefix: Vec::new(),
                        filter: crate::worker::session_io::TerminalOutputFilter::None,
                    },
                })
                .await
                .expect("subscribe terminal");
        });
        std::thread::sleep(Duration::from_millis(10));

        writer
            .write_all(&encode_frame(FRAME_PTY_OUTPUT, b"direct-output"))
            .expect("write output frame");

        assert_eq!(recv_output(&mut event_rx), b"direct-output");
        let message = block_on_with_timeout(async {
            client_rx
                .recv()
                .await
                .expect("client worker terminal bytes")
        });
        match message {
            crate::worker::client::ClientWorkerMessage::TerminalBytes { session_uuid, data } => {
                assert_eq!(session_uuid, "sess-test-io");
                assert_eq!(data, b"direct-output");
            }
            other => panic!("expected TerminalBytes, got {other:?}"),
        }
        assert_no_hub_event(&mut hub_rx);
    }

    #[test]
    fn initial_snapshot_is_delivered_directly_to_client_worker() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (request_tx, _event_rx, _response_rx, mut hub_rx, _alive) =
            spawn_test_worker_with_requests(reader);
        let (client_tx, mut client_rx) =
            tokio_mpsc::channel::<crate::worker::client::ClientWorkerMessage>(8);
        let worker = crate::worker::client::ClientWorkerHandle {
            client_id: crate::client::ClientId::Socket("client".to_string()),
            tx: client_tx,
        };

        block_on_with_timeout(async {
            request_tx
                .send(SessionIoRequest::GetInitialSnapshot {
                    delivery: crate::worker::session_io::TerminalInitialSnapshotDelivery {
                        request_id: "initial-direct".to_string(),
                        subscription_key: "client:sess-test-io".to_string(),
                        session_uuid: "sess-test-io".to_string(),
                        subscription_id: "terminal_sess-test-io".to_string(),
                        worker,
                        rows: 24,
                        cols: 80,
                        kitty_enabled: false,
                        payload_mode: crate::worker::session_io::TerminalSnapshotPayloadMode::Raw,
                        confirm_subscription: false,
                        live_subscription: None,
                        attach_requested_at: None,
                        client_worker_subscribed_at: None,
                        session_io_snapshot_queued_at: None,
                        session_io_accepted_at: None,
                    },
                })
                .await
                .expect("request initial snapshot");
        });
        std::thread::sleep(Duration::from_millis(10));

        writer
            .write_all(&encode_frame(FRAME_SNAPSHOT, b"initial-direct-snapshot"))
            .expect("write snapshot frame");

        let message = block_on_with_timeout(async {
            client_rx
                .recv()
                .await
                .expect("client worker initial snapshot")
        });
        match message {
            crate::worker::client::ClientWorkerMessage::ControlFrame(
                crate::worker::client::ClientControlFrame::Scrollback {
                    session_uuid,
                    rows,
                    cols,
                    kitty_enabled,
                    data,
                },
            ) => {
                assert_eq!(session_uuid, "sess-test-io");
                assert_eq!(rows, 24);
                assert_eq!(cols, 80);
                assert!(!kitty_enabled);
                assert_eq!(data, b"initial-direct-snapshot");
            }
            other => panic!("expected Scrollback control frame, got {other:?}"),
        }
        assert_no_hub_event(&mut hub_rx);
    }

    #[test]
    fn initial_snapshot_with_attach_timestamps_emits_terminal_attach_timing() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (request_tx, _event_rx, _response_rx, mut hub_rx, _alive) =
            spawn_test_worker_with_requests(reader);
        let (client_tx, mut client_rx) =
            tokio_mpsc::channel::<crate::worker::client::ClientWorkerMessage>(8);
        let worker = crate::worker::client::ClientWorkerHandle {
            client_id: crate::client::ClientId::Socket("client".to_string()),
            tx: client_tx,
        };
        let attach_requested_at = Instant::now();
        let client_worker_subscribed_at = attach_requested_at + Duration::from_millis(1);
        let session_io_snapshot_queued_at = attach_requested_at + Duration::from_millis(2);

        block_on_with_timeout(async {
            request_tx
                .send(SessionIoRequest::GetInitialSnapshot {
                    delivery: crate::worker::session_io::TerminalInitialSnapshotDelivery {
                        request_id: "initial-timing".to_string(),
                        subscription_key: "client:sess-test-io".to_string(),
                        session_uuid: "sess-test-io".to_string(),
                        subscription_id: "terminal_sess-test-io".to_string(),
                        worker,
                        rows: 24,
                        cols: 80,
                        kitty_enabled: false,
                        payload_mode: crate::worker::session_io::TerminalSnapshotPayloadMode::Raw,
                        confirm_subscription: true,
                        live_subscription: None,
                        attach_requested_at: Some(attach_requested_at),
                        client_worker_subscribed_at: Some(client_worker_subscribed_at),
                        session_io_snapshot_queued_at: Some(session_io_snapshot_queued_at),
                        session_io_accepted_at: None,
                    },
                })
                .await
                .expect("request initial snapshot timing");
        });
        std::thread::sleep(Duration::from_millis(10));

        writer
            .write_all(&encode_frame(FRAME_SNAPSHOT, b"initial-timing-snapshot"))
            .expect("write snapshot frame");

        for expected in ["Scrollback", "BoundaryJson", "TerminalAttach"] {
            let message = block_on_with_timeout(async {
                client_rx
                    .recv()
                    .await
                    .expect("client worker attach message")
            });
            match (expected, message) {
                (
                    "Scrollback",
                    crate::worker::client::ClientWorkerMessage::ControlFrame(
                        crate::worker::client::ClientControlFrame::Scrollback { data, .. },
                    ),
                ) => assert_eq!(data, b"initial-timing-snapshot"),
                (
                    "BoundaryJson",
                    crate::worker::client::ClientWorkerMessage::ControlFrame(
                        crate::worker::client::ClientControlFrame::BoundaryJson(value),
                    ),
                ) => assert_eq!(value["type"], "subscribed"),
                (
                    "TerminalAttach",
                    crate::worker::client::ClientWorkerMessage::ControlFrame(
                        crate::worker::client::ClientControlFrame::TerminalAttach { state, .. },
                    ),
                ) => assert_eq!(state, crate::worker::client::TerminalAttachState::Attached),
                (expected, other) => panic!("expected {expected}, got {other:?}"),
            }
        }

        match recv_hub_event(&mut hub_rx) {
            HubEvent::SessionIo(SessionIoEvent::TerminalAttachTiming(timing)) => {
                assert_eq!(timing.request_id, "initial-timing");
                assert_eq!(timing.subscription_key, "client:sess-test-io");
                assert_eq!(timing.session_uuid, "sess-test-io");
                assert_eq!(timing.subscription_id, "terminal_sess-test-io");
                assert_eq!(timing.snapshot_bytes, b"initial-timing-snapshot".len());
                assert!(timing.attach_to_client_worker_subscribed.is_some());
                assert!(timing.attach_to_session_io_queued.is_some());
                assert!(timing.attach_to_session_io_accepted.is_some());
                assert!(timing.attach_to_snapshot_ready.is_some());
                assert!(timing.attach_to_client_worker_accepted.is_some());
            }
            other => panic!("expected TerminalAttachTiming, got {other:?}"),
        }
    }

    #[test]
    fn initial_snapshot_barrier_delivers_snapshot_before_following_live_output() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (request_tx, mut event_rx, _response_rx, _hub_rx, _alive) =
            spawn_test_worker_with_requests(reader);
        let (client_tx, mut client_rx) =
            tokio_mpsc::channel::<crate::worker::client::ClientWorkerMessage>(8);
        let worker = crate::worker::client::ClientWorkerHandle {
            client_id: crate::client::ClientId::Socket("client".to_string()),
            tx: client_tx,
        };
        let live_subscription = TerminalOutputSubscription {
            subscription_key: "client:sess-test-io".to_string(),
            subscription_id: "terminal_sess-test-io".to_string(),
            worker: worker.clone(),
            output_prefix: Vec::new(),
            filter: crate::worker::session_io::TerminalOutputFilter::None,
        };

        block_on_with_timeout(async {
            request_tx
                .send(SessionIoRequest::GetInitialSnapshot {
                    delivery: crate::worker::session_io::TerminalInitialSnapshotDelivery {
                        request_id: "initial-ordered".to_string(),
                        subscription_key: "client:sess-test-io".to_string(),
                        session_uuid: "sess-test-io".to_string(),
                        subscription_id: "terminal_sess-test-io".to_string(),
                        worker,
                        rows: 24,
                        cols: 80,
                        kitty_enabled: false,
                        payload_mode: crate::worker::session_io::TerminalSnapshotPayloadMode::Raw,
                        confirm_subscription: false,
                        live_subscription: Some(live_subscription),
                        attach_requested_at: None,
                        client_worker_subscribed_at: None,
                        session_io_snapshot_queued_at: None,
                        session_io_accepted_at: None,
                    },
                })
                .await
                .expect("request ordered initial snapshot");
        });
        std::thread::sleep(Duration::from_millis(10));

        writer
            .write_all(&encode_frame(FRAME_PTY_OUTPUT, b"live-before-snapshot"))
            .expect("write pre-snapshot output frame");
        assert_eq!(recv_output(&mut event_rx), b"live-before-snapshot");
        assert!(
            client_rx.try_recv().is_err(),
            "live output must not reach client before initial snapshot"
        );

        writer
            .write_all(&encode_frame(FRAME_SNAPSHOT, b"ordered-snapshot"))
            .expect("write snapshot frame");
        let message = block_on_with_timeout(async {
            client_rx
                .recv()
                .await
                .expect("client worker initial snapshot")
        });
        assert!(
            matches!(
                message,
                crate::worker::client::ClientWorkerMessage::ControlFrame(
                    crate::worker::client::ClientControlFrame::Scrollback { .. }
                )
            ),
            "initial snapshot must be delivered before any live terminal bytes"
        );

        writer
            .write_all(&encode_frame(FRAME_PTY_OUTPUT, b"live-after-snapshot"))
            .expect("write post-snapshot output frame");
        assert_eq!(recv_output(&mut event_rx), b"live-after-snapshot");
        let message = block_on_with_timeout(async {
            client_rx
                .recv()
                .await
                .expect("client worker terminal attach")
        });
        assert!(
            matches!(
                message,
                crate::worker::client::ClientWorkerMessage::ControlFrame(
                    crate::worker::client::ClientControlFrame::TerminalAttach { .. }
                )
            ),
            "terminal attach control should remain ordered after the snapshot"
        );
        let message = block_on_with_timeout(async {
            client_rx
                .recv()
                .await
                .expect("client worker live terminal bytes")
        });
        match message {
            crate::worker::client::ClientWorkerMessage::TerminalBytes { data, .. } => {
                assert_eq!(data, b"live-after-snapshot");
            }
            other => panic!("expected live terminal bytes after snapshot, got {other:?}"),
        }
    }

    #[test]
    fn empty_live_initial_snapshot_confirms_subscription_before_reconnecting_attach() {
        let session_uuid = "sess-test-io".to_string();
        let socket_path =
            crate::session::session_socket_path(&session_uuid).expect("session socket path");
        std::fs::write(&socket_path, b"").expect("create live session socket");
        crate::session::write_session_pid_file(&session_uuid, std::process::id())
            .expect("write live session pid");

        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (request_tx, _event_rx, _response_rx, _hub_rx, _alive) =
            spawn_test_worker_with_requests(reader);
        let (client_tx, mut client_rx) =
            tokio_mpsc::channel::<crate::worker::client::ClientWorkerMessage>(8);
        let worker = crate::worker::client::ClientWorkerHandle {
            client_id: crate::client::ClientId::Socket("client".to_string()),
            tx: client_tx,
        };

        block_on_with_timeout(async {
            request_tx
                .send(SessionIoRequest::GetInitialSnapshot {
                    delivery: crate::worker::session_io::TerminalInitialSnapshotDelivery {
                        request_id: "initial-empty-live".to_string(),
                        subscription_key: format!("client:{session_uuid}"),
                        session_uuid: session_uuid.clone(),
                        subscription_id: "terminal_empty_live".to_string(),
                        worker,
                        rows: 24,
                        cols: 80,
                        kitty_enabled: false,
                        payload_mode: crate::worker::session_io::TerminalSnapshotPayloadMode::Raw,
                        confirm_subscription: true,
                        live_subscription: None,
                        attach_requested_at: None,
                        client_worker_subscribed_at: None,
                        session_io_snapshot_queued_at: None,
                        session_io_accepted_at: None,
                    },
                })
                .await
                .expect("request empty initial snapshot");
        });
        std::thread::sleep(Duration::from_millis(10));

        writer
            .write_all(&encode_frame(FRAME_SNAPSHOT, b""))
            .expect("write empty snapshot frame");

        let first = block_on_with_timeout(async {
            client_rx.recv().await.expect("subscription confirmation")
        });
        match first {
            crate::worker::client::ClientWorkerMessage::ControlFrame(
                crate::worker::client::ClientControlFrame::BoundaryJson(value),
            ) => {
                assert_eq!(
                    value.get("type").and_then(|value| value.as_str()),
                    Some("subscribed")
                );
                assert_eq!(
                    value.get("subscriptionId").and_then(|value| value.as_str()),
                    Some("terminal_empty_live")
                );
            }
            other => panic!("expected subscribed boundary, got {other:?}"),
        }

        let second =
            block_on_with_timeout(async { client_rx.recv().await.expect("reconnecting attach") });
        match second {
            crate::worker::client::ClientWorkerMessage::ControlFrame(
                crate::worker::client::ClientControlFrame::TerminalAttach { state, .. },
            ) => {
                assert_eq!(
                    state,
                    crate::worker::client::TerminalAttachState::Reconnecting
                );
            }
            other => panic!("expected reconnecting terminal attach, got {other:?}"),
        }

        if let Ok(path) = crate::session::session_socket_path(&session_uuid) {
            let _ = std::fs::remove_file(path);
        }
        if let Ok(path) = crate::session::session_pid_path(&session_uuid) {
            let _ = std::fs::remove_file(path);
        }
    }

    fn wait_for_reader_stop(alive: &AtomicBool) {
        let deadline = Instant::now() + TEST_EVENT_TIMEOUT;
        while alive.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(
            !alive.load(Ordering::Acquire),
            "reader did not stop within {TEST_EVENT_TIMEOUT:?}"
        );
    }

    #[test]
    fn coalesces_synthetic_output_burst_before_hub_delivery() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (mut event_rx, _response_rx, _hub_rx, _alive) = spawn_test_worker(reader);

        let mut payload = Vec::new();
        for chunk in [b"one".as_slice(), b"two".as_slice(), b"three".as_slice()] {
            payload.extend_from_slice(&encode_frame(FRAME_PTY_OUTPUT, chunk));
        }
        writer.write_all(&payload).expect("write frames");
        writer.shutdown(Shutdown::Both).expect("shutdown writer");

        assert_eq!(recv_output(&mut event_rx), b"onetwothree");
    }

    #[test]
    fn output_age_flushes_under_four_ms_rule_without_eof_or_size_threshold() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (mut event_rx, _response_rx, _hub_rx, _alive) = spawn_test_worker(reader);

        let mut payload = Vec::new();
        for chunk in [b"age-".as_slice(), b"flush".as_slice()] {
            payload.extend_from_slice(&encode_frame(FRAME_PTY_OUTPUT, chunk));
        }
        writer.write_all(&payload).expect("write frames");

        assert_eq!(recv_output(&mut event_rx), b"age-flush");
    }

    #[test]
    fn output_thresholds_flush_without_waiting_for_eof() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (mut event_rx, _response_rx, _hub_rx, _alive) = spawn_test_worker(reader);

        let mut frame_threshold_payload = Vec::new();
        for _ in 0..MAX_OUTPUT_FRAMES {
            frame_threshold_payload.extend_from_slice(&encode_frame(FRAME_PTY_OUTPUT, b"f"));
        }
        writer
            .write_all(&frame_threshold_payload)
            .expect("write frame-threshold burst");
        assert_eq!(recv_output(&mut event_rx), vec![b'f'; MAX_OUTPUT_FRAMES]);

        let byte_threshold_payload = vec![b'x'; MAX_OUTPUT_BYTES];
        writer
            .write_all(&encode_frame(FRAME_PTY_OUTPUT, &byte_threshold_payload))
            .expect("write byte-threshold frame");
        assert_eq!(recv_output(&mut event_rx), byte_threshold_payload);
    }

    #[test]
    fn request_processor_emits_prepared_snapshot_event() {
        let (_writer, reader) = UnixStream::pair().expect("unix pair");
        let (request_tx, _event_rx, _response_rx, mut hub_rx, _alive) =
            spawn_test_worker_with_requests(reader);

        request_tx
            .blocking_send(SessionIoRequest::PrepareSnapshot {
                request_id: "snapshot-req".to_string(),
                snapshot: vec![b'x'; 4096],
                recovery: true,
            })
            .expect("send prepare snapshot");

        match hub_rx.blocking_recv().expect("prepared snapshot event") {
            HubEvent::SessionIo(SessionIoEvent::PreparedSnapshot {
                request_id,
                session_uuid,
                uncompressed_len,
                payload,
                recovery,
            }) => {
                assert_eq!(request_id, "snapshot-req");
                assert_eq!(session_uuid, "sess-test-io");
                assert_eq!(uncompressed_len, 4096);
                assert!(payload.starts_with(&[0x1f, 0x8b]));
                assert!(recovery);
            }
            other => panic!("expected prepared snapshot event, got {other:?}"),
        }
    }

    #[test]
    fn get_snapshot_request_emits_correlated_snapshot_event() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (request_tx, _event_rx, _response_rx, mut hub_rx, _alive) =
            spawn_test_worker_with_requests(reader);

        request_tx
            .blocking_send(SessionIoRequest::GetSnapshot {
                request_id: "snapshot-rpc".to_string(),
            })
            .expect("send get snapshot");

        let mut request = [0u8; 5];
        writer
            .read_exact(&mut request)
            .expect("read get snapshot frame");
        assert_eq!(request[4], FRAME_GET_SNAPSHOT);

        writer
            .write_all(&encode_frame(FRAME_SNAPSHOT, b"snapshot-bytes"))
            .expect("write snapshot response");

        match hub_rx.blocking_recv().expect("snapshot event") {
            HubEvent::SessionIo(SessionIoEvent::Snapshot {
                request_id,
                session_uuid,
                payload,
            }) => {
                assert_eq!(request_id, "snapshot-rpc");
                assert_eq!(session_uuid, "sess-test-io");
                assert_eq!(payload, b"snapshot-bytes");
            }
            other => panic!("expected snapshot event, got {other:?}"),
        }
    }

    #[test]
    fn request_processor_emits_paste_file_written_event() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let temp = tempfile::tempdir().expect("tempdir");
        let worktree = temp.path().join("worktree");
        let manifest_dir = temp
            .path()
            .join("workspaces")
            .join("ws")
            .join("sessions")
            .join("sess-test-io");
        std::fs::create_dir_all(&manifest_dir).expect("manifest dir");
        std::fs::create_dir_all(&worktree).expect("worktree");
        std::fs::write(
            manifest_dir.join("manifest.json"),
            serde_json::json!({ "worktree_path": worktree })
                .to_string()
                .as_bytes(),
        )
        .expect("manifest");
        std::env::set_var("BOTSTER_CONFIG_DIR", temp.path());

        let (_writer, reader) = UnixStream::pair().expect("unix pair");
        let (request_tx, _event_rx, _response_rx, mut hub_rx, _alive) =
            spawn_test_worker_with_requests(reader);

        request_tx
            .blocking_send(SessionIoRequest::PasteFile {
                request_id: "paste-req".to_string(),
                filename: "screen.PNG".to_string(),
                data: b"image".to_vec(),
            })
            .expect("send paste request");

        match hub_rx.blocking_recv().expect("paste event") {
            HubEvent::SessionIo(SessionIoEvent::PasteFileWritten {
                request_id,
                session_uuid,
                path,
                bytes,
            }) => {
                assert_eq!(request_id, "paste-req");
                assert_eq!(session_uuid, "sess-test-io");
                assert_eq!(bytes, 5);
                assert!(path.to_string_lossy().contains("sess-test-io"));
                assert_eq!(std::fs::read(&path).expect("paste file"), b"image");
                let _ = std::fs::remove_file(path);
            }
            other => panic!("expected paste written event, got {other:?}"),
        }
        std::env::remove_var("BOTSTER_CONFIG_DIR");
    }

    #[test]
    fn noisy_log_replay_preserves_order_and_bounds_hub_batches() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (mut event_rx, _response_rx, mut hub_rx, alive) = spawn_test_worker(reader);

        let mut payload = Vec::new();
        let mut expected = Vec::new();
        for i in 0..=1000 {
            let chunk = format!("\x1b]2;botster replay {i}\x07frame-{i:04}\r\n");
            expected.extend_from_slice(chunk.as_bytes());
            payload.extend_from_slice(&encode_frame(FRAME_PTY_OUTPUT, chunk.as_bytes()));
        }

        writer.write_all(&payload).expect("write noisy replay");
        writer.shutdown(Shutdown::Both).expect("shutdown writer");

        for _ in 0..100 {
            if !alive.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let mut output = Vec::new();
        while output.len() < expected.len() {
            output.extend_from_slice(&recv_output(&mut event_rx));
        }

        let mut exits = 0;
        while let Ok(event) = hub_rx.try_recv() {
            match event {
                HubEvent::SessionProcessExited { exit_code, .. } => {
                    assert_eq!(exit_code, None);
                    exits += 1;
                }
                other => panic!("unexpected hub event: {other:?}"),
            }
        }

        assert_eq!(output, expected);
        assert_eq!(exits, 1);
    }

    #[test]
    fn coalesces_output_until_sixteen_frames_or_32k() {
        let mut by_frame = SessionIoCoalescer::default();
        for _ in 0..15 {
            by_frame.push_output(vec![b'x']);
        }
        assert!(!by_frame.should_flush_output());
        by_frame.push_output(vec![b'x']);
        assert!(by_frame.should_flush_output());

        let mut by_bytes = SessionIoCoalescer::default();
        by_bytes.push_output(vec![b'x'; MAX_OUTPUT_BYTES - 1]);
        assert!(!by_bytes.should_flush_output());
        by_bytes.push_output(vec![b'y']);
        assert!(by_bytes.should_flush_output());
    }

    #[test]
    fn worker_routes_control_responses_while_output_arrives() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (mut event_rx, response_rx, _hub_rx, _alive) = spawn_test_worker(reader);

        let mut payload = encode_frame(FRAME_PTY_OUTPUT, b"abc");
        payload.extend_from_slice(&encode_frame(FRAME_SCREEN, b"plain screen"));
        writer.write_all(&payload).expect("write frames");

        assert_eq!(recv_output(&mut event_rx), b"abc");
        let frame = response_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("control response");
        assert_eq!(frame.frame_type, FRAME_SCREEN);
        assert_eq!(frame.payload, b"plain screen");
    }

    #[test]
    fn output_flushes_before_prompt_mark() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (mut event_rx, _response_rx, _hub_rx, _alive) = spawn_test_worker(reader);

        let mut payload = encode_frame(FRAME_PTY_OUTPUT, b"before");
        payload.extend_from_slice(
            &encode_json(
                FRAME_PROMPT_MARK,
                &PromptMarkPayload {
                    mark: "prompt_start".to_string(),
                },
            )
            .expect("prompt frame"),
        );
        writer.write_all(&payload).expect("write frames");

        assert_eq!(recv_output(&mut event_rx), b"before");
        match event_rx.blocking_recv().expect("prompt mark event") {
            PtyEvent::PromptMark(PromptMark::PromptStart) => {}
            other => panic!("expected prompt-start, got {other:?}"),
        }
    }

    #[test]
    fn output_flushes_before_bell_notification() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (mut event_rx, _response_rx, _hub_rx, _alive) = spawn_test_worker(reader);

        let mut payload = encode_frame(FRAME_PTY_OUTPUT, b"before-bell");
        payload.extend_from_slice(&encode_frame(FRAME_BELL, b""));
        writer.write_all(&payload).expect("write frames");

        assert_eq!(recv_output(&mut event_rx), b"before-bell");
        match recv_pty_event(&mut event_rx) {
            PtyEvent::Notification(AgentNotification::Bell) => {}
            other => panic!("expected bell notification, got {other:?}"),
        }
    }

    #[test]
    fn output_flushes_before_osc_notification() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (mut event_rx, _response_rx, _hub_rx, _alive) = spawn_test_worker(reader);

        let mut payload = encode_frame(FRAME_PTY_OUTPUT, b"before-notification");
        payload.extend_from_slice(
            &encode_json(
                FRAME_NOTIFICATION,
                &NotificationPayload {
                    title: "Build".to_string(),
                    body: "done".to_string(),
                },
            )
            .expect("notification frame"),
        );
        writer.write_all(&payload).expect("write frames");

        assert_eq!(recv_output(&mut event_rx), b"before-notification");
        match recv_pty_event(&mut event_rx) {
            PtyEvent::Notification(AgentNotification::Osc777 { title, body }) => {
                assert_eq!(title, "Build");
                assert_eq!(body, "done");
            }
            other => panic!("expected OSC notification, got {other:?}"),
        }
    }

    #[test]
    fn mode_changed_coalesces_sparse_fields_last_value_wins() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (mut event_rx, _response_rx, _hub_rx, _alive) = spawn_test_worker(reader);

        let mut payload = encode_json(
            FRAME_MODE_CHANGED,
            &ModeChanged {
                kitty_enabled: Some(true),
                ..ModeChanged::default()
            },
        )
        .expect("mode frame");
        payload.extend_from_slice(
            &encode_json(
                FRAME_MODE_CHANGED,
                &ModeChanged {
                    cursor_visible: Some(false),
                    kitty_enabled: Some(false),
                    ..ModeChanged::default()
                },
            )
            .expect("mode frame"),
        );
        payload.extend_from_slice(&encode_frame(FRAME_PTY_OUTPUT, b"after"));
        writer.write_all(&payload).expect("write frames");

        assert_eq!(recv_output(&mut event_rx), b"after");
        match event_rx.blocking_recv().expect("cursor event") {
            PtyEvent::CursorVisibilityChanged(false) => {}
            other => panic!("expected cursor hidden, got {other:?}"),
        }
    }

    #[test]
    fn title_metadata_storm_flushes_latest_value_after_output_window() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (mut event_rx, _response_rx, _hub_rx, _alive) = spawn_test_worker(reader);

        let mut payload = Vec::new();
        for title in ["step-1", "step-2", "step-3"] {
            payload.extend_from_slice(&encode_frame(FRAME_TITLE_CHANGED, title.as_bytes()));
            payload.extend_from_slice(&encode_frame(FRAME_PTY_OUTPUT, title.as_bytes()));
        }
        writer.write_all(&payload).expect("write metadata storm");
        writer.shutdown(Shutdown::Both).expect("shutdown writer");

        assert_eq!(recv_output(&mut event_rx), b"step-1step-2step-3");
        match event_rx.blocking_recv().expect("title event") {
            PtyEvent::TitleChanged(title) => assert_eq!(title, "step-3"),
            other => panic!("expected latest title, got {other:?}"),
        }
    }

    #[test]
    fn cwd_metadata_storm_flushes_latest_value_after_output_window() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (mut event_rx, _response_rx, _hub_rx, _alive) = spawn_test_worker(reader);

        let mut payload = Vec::new();
        for cwd in ["/tmp/one", "/tmp/two", "/tmp/three"] {
            payload.extend_from_slice(&encode_frame(FRAME_CWD_CHANGED, cwd.as_bytes()));
            payload.extend_from_slice(&encode_frame(FRAME_PTY_OUTPUT, cwd.as_bytes()));
        }
        writer.write_all(&payload).expect("write metadata storm");
        writer.shutdown(Shutdown::Both).expect("shutdown writer");

        assert_eq!(recv_output(&mut event_rx), b"/tmp/one/tmp/two/tmp/three");
        match event_rx.blocking_recv().expect("cwd event") {
            PtyEvent::CwdChanged(cwd) => assert_eq!(cwd, "/tmp/three"),
            other => panic!("expected latest cwd, got {other:?}"),
        }
    }

    #[test]
    fn process_exited_then_eof_emits_one_exit() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (_event_rx, _response_rx, mut hub_rx, alive) = spawn_test_worker(reader);

        let frame = encode_json(FRAME_PROCESS_EXITED, &serde_json::json!({ "exit_code": 0 }))
            .expect("encode exit frame");
        writer.write_all(&frame).expect("write exit frame");
        writer.shutdown(Shutdown::Both).expect("shutdown writer");

        for _ in 0..50 {
            if !alive.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let mut exits = Vec::new();
        while let Ok(event) = hub_rx.try_recv() {
            if let HubEvent::SessionProcessExited { exit_code, .. } = event {
                exits.push(exit_code);
            }
        }

        assert_eq!(exits, vec![Some(0)]);
    }

    #[test]
    fn output_flushes_before_process_exit_and_eof_does_not_duplicate_exit() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (mut event_rx, _response_rx, mut hub_rx, alive) = spawn_test_worker(reader);

        let mut payload = encode_frame(FRAME_PTY_OUTPUT, b"before-exit");
        payload.extend_from_slice(
            &encode_json(FRAME_PROCESS_EXITED, &serde_json::json!({ "exit_code": 7 }))
                .expect("encode exit frame"),
        );
        writer.write_all(&payload).expect("write frames");
        writer.shutdown(Shutdown::Both).expect("shutdown writer");

        assert_eq!(recv_output(&mut event_rx), b"before-exit");
        match recv_hub_event(&mut hub_rx) {
            HubEvent::SessionProcessExited { exit_code, .. } => assert_eq!(exit_code, Some(7)),
            other => panic!("expected process exit, got {other:?}"),
        }
        wait_for_reader_stop(&alive);
        assert_no_hub_event(&mut hub_rx);
    }

    #[test]
    fn eof_without_process_exit_emits_disconnect_exit() {
        let (writer, reader) = UnixStream::pair().expect("unix pair");
        let (_event_rx, _response_rx, mut hub_rx, alive) = spawn_test_worker(reader);
        drop(writer);

        for _ in 0..50 {
            if !alive.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let event = hub_rx.try_recv().expect("disconnect exit");
        match event {
            HubEvent::SessionProcessExited { exit_code, .. } => assert_eq!(exit_code, None),
            other => panic!("expected disconnect exit, got {other:?}"),
        }
    }

    #[test]
    fn protocol_desync_flushes_output_then_emits_disconnect_exit() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (mut event_rx, _response_rx, mut hub_rx, alive) = spawn_test_worker(reader);

        let mut payload = encode_frame(FRAME_PTY_OUTPUT, b"before-desync");
        for _ in 0..100 {
            payload.extend_from_slice(&0u32.to_le_bytes());
        }
        writer.write_all(&payload).expect("write corrupt stream");

        for _ in 0..50 {
            if !alive.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(recv_output(&mut event_rx), b"before-desync");
        let mut exits = Vec::new();
        while let Ok(event) = hub_rx.try_recv() {
            if let HubEvent::SessionProcessExited { exit_code, .. } = event {
                exits.push(exit_code);
            }
        }
        assert_eq!(exits, vec![None]);
    }

    #[test]
    fn routes_mode_flags_response_as_control_frame() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (_event_rx, response_rx, _hub_rx, _alive) = spawn_test_worker(reader);

        writer
            .write_all(&encode_frame(FRAME_MODE_FLAGS, b"{}"))
            .expect("write mode flags");

        let frame = response_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("mode flags response");
        assert_eq!(frame.frame_type, FRAME_MODE_FLAGS);
    }

    #[test]
    fn unknown_control_frame_is_routed_to_response_channel() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (_event_rx, response_rx, _hub_rx, _alive) = spawn_test_worker(reader);

        writer
            .write_all(&encode_frame(FRAME_GET_SCREEN, b"request-like-frame"))
            .expect("write frame");

        let frame = response_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("response frame");
        assert_eq!(frame.frame_type, FRAME_GET_SCREEN);
    }
}
