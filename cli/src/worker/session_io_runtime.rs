//! Concrete session I/O worker runtime.
//!
//! This worker owns the blocking read side of a single per-session Unix socket.
//! It preserves the session-process wire protocol while coalescing hot-path
//! output before crossing back into the hub event loop.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use tokio::sync::broadcast;

use crate::agent::notification::AgentNotification;
use crate::agent::pty::{PromptMark, PtyEvent};
use crate::hub::events::{HubEvent, HubEventTx};
use crate::session::protocol::{
    encode_empty, encode_frame, encode_json, Frame, FrameDecoder, ModeChanged, NotificationPayload,
    PromptMarkPayload, FRAME_BELL, FRAME_CWD_CHANGED, FRAME_GET_MODE_FLAGS, FRAME_GET_SCREEN,
    FRAME_GET_SNAPSHOT, FRAME_MODE_CHANGED, FRAME_NOTIFICATION, FRAME_PROCESS_EXITED,
    FRAME_PROMPT_MARK, FRAME_PTY_INPUT, FRAME_PTY_OUTPUT, FRAME_RESIZE, FRAME_SET_COLOR_PROFILE,
    FRAME_SHUTDOWN, FRAME_TITLE_CHANGED,
};

use super::session_io::{
    prepare_snapshot_payload, write_paste_file, SessionIoBatch, SessionIoEvent, SessionIoRequest,
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
    pub response_tx: std::sync::mpsc::Sender<Frame>,
    pub hub_event_tx: HubEventTx,
    pub reader_alive: Arc<AtomicBool>,
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
}

fn run_request_processor(config: SessionIoRequestProcessorConfig) {
    let mut processor = SessionIoRequestProcessor {
        stream: config.stream,
        request_rx: config.request_rx,
        session_uuid: config.session_uuid,
        hub_event_tx: config.hub_event_tx,
        reader_alive: config.reader_alive,
    };
    processor.run();
}

struct SessionIoRequestProcessor {
    stream: UnixStream,
    request_rx: tokio::sync::mpsc::Receiver<SessionIoRequest>,
    session_uuid: String,
    hub_event_tx: HubEventTx,
    reader_alive: Arc<AtomicBool>,
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
                if let Err(e) = self.write_frame(encode_frame(FRAME_PTY_INPUT, &data)) {
                    log::warn!("[session-io] PTY input request failed: {e}");
                }
            }
            SessionIoRequest::Resize { rows, cols } => {
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
            SessionIoRequest::GetSnapshot { .. } => {
                if let Err(e) = self.write_frame(encode_empty(FRAME_GET_SNAPSHOT)) {
                    log::warn!("[session-io] snapshot RPC request failed: {e}");
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
                self.coalescer.flush_metadata(
                    &self.session_uuid,
                    &self.event_tx,
                    &self.kitty_enabled,
                    &self.cursor_visible,
                );
                self.resize_pending.store(false, Ordering::Release);
                self.last_output_at.store(now_millis(), Ordering::Relaxed);
                self.coalescer.push_output(frame.payload);
            }
            FRAME_TITLE_CHANGED => {
                self.flush_output();
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
                self.flush_output();
                if let Ok(mode) = frame.json::<ModeChanged>() {
                    self.coalescer.merge_mode(mode);
                }
            }
            FRAME_CWD_CHANGED => {
                self.flush_output();
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
                let _ = self.hub_event_tx.send(HubEvent::SessionProcessExited {
                    session_uuid: self.session_uuid.clone(),
                    exit_code,
                });
                log::info!("[session-io] process exited (code={exit_code:?})");
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

    fn flush_if_ready(&mut self) {
        if self.coalescer.should_flush_output() || self.coalescer.metadata_age_expired() {
            self.flush_pending();
        }
    }

    fn flush_output(&mut self) {
        self.coalescer
            .flush_output(&self.session_uuid, &self.event_tx, &self.hub_event_tx);
    }

    fn flush_pending(&mut self) {
        self.coalescer.flush_all(
            &self.session_uuid,
            &self.event_tx,
            &self.hub_event_tx,
            &self.kitty_enabled,
            &self.cursor_visible,
        );
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

    fn flush_all(
        &mut self,
        session_uuid: &str,
        event_tx: &broadcast::Sender<PtyEvent>,
        hub_event_tx: &HubEventTx,
        kitty_enabled: &AtomicBool,
        cursor_visible: &AtomicBool,
    ) {
        self.flush_output(session_uuid, event_tx, hub_event_tx);
        self.flush_metadata(session_uuid, event_tx, kitty_enabled, cursor_visible);
    }

    fn flush_output(
        &mut self,
        session_uuid: &str,
        event_tx: &broadcast::Sender<PtyEvent>,
        hub_event_tx: &HubEventTx,
    ) {
        if self.output.is_empty() {
            return;
        }

        let output = std::mem::take(&mut self.output);
        self.output_frames = 0;
        self.output_started_at = None;
        let _ = event_tx.send(PtyEvent::output(output.clone()));
        let _ = hub_event_tx.send(HubEvent::SessionIoBatch(SessionIoBatch {
            session_uuid: session_uuid.to_string(),
            output: Some(output),
        }));
    }

    fn flush_metadata(
        &mut self,
        _session_uuid: &str,
        event_tx: &broadcast::Sender<PtyEvent>,
        kitty_enabled: &AtomicBool,
        cursor_visible: &AtomicBool,
    ) {
        if let Some(mode) = self.mode.take() {
            if let Some(kitty) = mode.kitty_enabled {
                let old = kitty_enabled.load(Ordering::Relaxed);
                if kitty != old {
                    kitty_enabled.store(kitty, Ordering::Relaxed);
                    let _ = event_tx.send(PtyEvent::kitty_changed(kitty));
                }
            }
            if let Some(vis) = mode.cursor_visible {
                cursor_visible.store(vis, Ordering::Relaxed);
                let _ = event_tx.send(PtyEvent::cursor_visibility_changed(vis));
            }
            if let Some(focus) = mode.focus_reporting {
                let _ = event_tx.send(PtyEvent::focus_reporting_changed(focus));
            }
        }

        if let Some(title) = self.title.take() {
            let _ = event_tx.send(PtyEvent::title_changed(title));
        }
        if let Some(cwd) = self.cwd.take() {
            let _ = event_tx.send(PtyEvent::cwd_changed(cwd));
        }
        if !self.has_metadata() {
            self.metadata_started_at = None;
        }
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
    use std::net::Shutdown;
    use std::sync::mpsc;
    use tokio::sync::mpsc as tokio_mpsc;

    use super::*;
    use crate::session::protocol::{FRAME_MODE_FLAGS, FRAME_SCREEN};

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn spawn_test_worker(
        reader: UnixStream,
    ) -> (
        broadcast::Receiver<PtyEvent>,
        mpsc::Receiver<Frame>,
        tokio_mpsc::UnboundedReceiver<HubEvent>,
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
        tokio_mpsc::UnboundedReceiver<HubEvent>,
        Arc<AtomicBool>,
    ) {
        let (event_tx, event_rx) = broadcast::channel(32);
        let (response_tx, response_rx) = mpsc::channel();
        let (hub_tx, hub_rx) = tokio_mpsc::unbounded_channel();
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
            response_tx,
            hub_event_tx: HubEventTx::from(hub_tx),
            reader_alive: Arc::clone(&alive),
        })
        .expect("spawn worker");

        (request_tx, event_rx, response_rx, hub_rx, alive)
    }

    fn recv_output(rx: &mut broadcast::Receiver<PtyEvent>) -> Vec<u8> {
        match rx
            .blocking_recv()
            .expect("receive output event from worker")
        {
            PtyEvent::Output(data) => data,
            other => panic!("expected output, got {other:?}"),
        }
    }

    #[test]
    fn coalesces_synthetic_output_burst_before_hub_delivery() {
        let (mut writer, reader) = UnixStream::pair().expect("unix pair");
        let (mut event_rx, _response_rx, mut hub_rx, _alive) = spawn_test_worker(reader);

        let mut payload = Vec::new();
        for chunk in [b"one".as_slice(), b"two".as_slice(), b"three".as_slice()] {
            payload.extend_from_slice(&encode_frame(FRAME_PTY_OUTPUT, chunk));
        }
        writer.write_all(&payload).expect("write frames");
        writer.shutdown(Shutdown::Both).expect("shutdown writer");

        assert_eq!(recv_output(&mut event_rx), b"onetwothree");

        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
        let mut batches = Vec::new();
        while batches.is_empty() && std::time::Instant::now() < deadline {
            while let Ok(event) = hub_rx.try_recv() {
                if let HubEvent::SessionIoBatch(batch) = event {
                    batches.push(batch.output.expect("output batch"));
                }
            }
            if batches.is_empty() {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }

        assert_eq!(batches, vec![b"onetwothree".to_vec()]);
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
        let (_event_rx, _response_rx, mut hub_rx, alive) = spawn_test_worker(reader);

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

        let mut batches = Vec::new();
        let mut exits = 0;
        while let Ok(event) = hub_rx.try_recv() {
            match event {
                HubEvent::SessionIoBatch(batch) => {
                    batches.push(batch.output.expect("output batch"));
                }
                HubEvent::SessionProcessExited { exit_code, .. } => {
                    assert_eq!(exit_code, None);
                    exits += 1;
                }
                other => panic!("unexpected hub event: {other:?}"),
            }
        }

        let replayed = batches.concat();
        assert_eq!(replayed, expected);
        assert!(
            batches.len() <= 64,
            "1001 observed-log-shaped frames should cross the hub in bounded batches, got {}",
            batches.len()
        );
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

        match event_rx.blocking_recv().expect("cursor event") {
            PtyEvent::CursorVisibilityChanged(false) => {}
            other => panic!("expected cursor hidden, got {other:?}"),
        }
        assert_eq!(recv_output(&mut event_rx), b"after");
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
