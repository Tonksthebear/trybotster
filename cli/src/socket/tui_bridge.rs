//! TUI bridge adapter for `botster attach`.
//!
//! Translates between the TuiRunner's in-process channels and the socket
//! wire protocol. TuiRunner doesn't change at all — the bridge provides
//! the same `mpsc` channel types and wake pipe semantics.
//!
//! ```text
//! TuiRunner <--mpsc + wake pipe--> TuiBridge <--frames--> Unix Socket <--> Hub
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use super::framing::{Frame, FrameDecoder};
use crate::client::{TuiOutput, TuiRequest};

/// How many times attach-mode retries reconnecting after hub restart.
const RECONNECT_RETRIES: u32 = 10;

/// Fixed delay between reconnect attempts.
const RECONNECT_RETRY_MS: u64 = 1_000;
/// Current socket protocol version spoken by attach-mode TUI bridge.
const SOCKET_PROTOCOL_VERSION: u32 = 2;
/// Oldest socket protocol version still accepted by this bridge.
const SOCKET_PROTOCOL_MIN_SUPPORTED: u32 = 1;

/// Why a bridge session ended.
enum SessionExit {
    Shutdown,
    HubDisconnected,
}

/// Connected bridge between TuiRunner and a Hub socket.
#[derive(Debug)]
pub struct TuiBridge {
    /// Sender for TuiOutput to TuiRunner (bridge → TUI direction).
    output_tx: mpsc::UnboundedSender<TuiOutput>,
    /// Receiver for TuiRequest from TuiRunner (TUI → bridge direction).
    request_rx: mpsc::UnboundedReceiver<TuiRequest>,
    /// Read half of the Unix socket.
    socket_reader: tokio::net::unix::OwnedReadHalf,
    /// Write half of the Unix socket.
    socket_writer: tokio::net::unix::OwnedWriteHalf,
    /// Write end of the wake pipe (wakes TuiRunner's `libc::poll()`).
    wake_write_fd: Option<std::os::unix::io::RawFd>,
    /// Shared shutdown flag.
    shutdown: Arc<AtomicBool>,
    /// Socket path used for reconnecting after hub restart.
    reconnect_path: Option<std::path::PathBuf>,
}

/// Result of setting up a TUI bridge connection.
#[derive(Debug)]
pub struct BridgeChannels {
    /// Sender for TuiRunner → bridge (TuiRequest).
    pub request_tx: mpsc::UnboundedSender<TuiRequest>,
    /// Receiver for bridge → TuiRunner (TuiOutput).
    pub output_rx: mpsc::UnboundedReceiver<TuiOutput>,
}

impl TuiBridge {
    /// Connect to a Hub socket and create the bridge channels.
    ///
    /// Returns the bridge (to be run) and the channels for TuiRunner.
    pub fn connect(
        stream: UnixStream,
        wake_write_fd: Option<std::os::unix::io::RawFd>,
        shutdown: Arc<AtomicBool>,
    ) -> (Self, BridgeChannels) {
        Self::connect_with_reconnect(stream, None, wake_write_fd, shutdown)
    }

    /// Connect to a Hub socket and enable automatic reconnect on EOF.
    pub fn connect_with_reconnect(
        stream: UnixStream,
        reconnect_path: Option<std::path::PathBuf>,
        wake_write_fd: Option<std::os::unix::io::RawFd>,
        shutdown: Arc<AtomicBool>,
    ) -> (Self, BridgeChannels) {
        let (reader, writer) = stream.into_split();
        let (output_tx, output_rx) = mpsc::unbounded_channel::<TuiOutput>();
        let (request_tx, request_rx) = mpsc::unbounded_channel::<TuiRequest>();

        let bridge = Self {
            output_tx,
            request_rx,
            socket_reader: reader,
            socket_writer: writer,
            wake_write_fd,
            shutdown,
            reconnect_path,
        };

        let channels = BridgeChannels {
            request_tx,
            output_rx,
        };

        (bridge, channels)
    }

    /// Run the bridge, forwarding between TuiRunner channels and socket frames.
    ///
    /// This method runs until the socket closes or shutdown is signaled.
    pub async fn run(mut self) {
        loop {
            let exit = self.run_session().await;
            if matches!(exit, SessionExit::Shutdown) {
                self.shutdown.store(true, Ordering::SeqCst);
                return;
            }

            let Some(path) = self.reconnect_path.clone() else {
                self.shutdown.store(true, Ordering::SeqCst);
                return;
            };

            match reconnect_to_hub(&path, RECONNECT_RETRIES, RECONNECT_RETRY_MS, &self.shutdown)
                .await
            {
                Ok(stream) => {
                    log::info!("[TuiBridge] Reconnected to hub");
                    let (reader, writer) = stream.into_split();
                    self.socket_reader = reader;
                    self.socket_writer = writer;
                    let dropped = self.drain_stale_requests();
                    if dropped > 0 {
                        log::warn!(
                            "[TuiBridge] Dropped {dropped} stale outbound request(s) before reattach"
                        );
                    }
                    let _ = self.output_tx.send(TuiOutput::Message(serde_json::json!({
                        "type": "bridge_reconnected",
                    })));
                    if let Some(fd) = self.wake_write_fd {
                        unsafe {
                            libc::write(fd, [1u8].as_ptr() as *const libc::c_void, 1);
                        }
                    }
                }
                Err(e) => {
                    log::error!("[TuiBridge] Failed to reconnect to hub: {e}");
                    self.shutdown.store(true, Ordering::SeqCst);
                    return;
                }
            }
        }
    }

    async fn run_session(&mut self) -> SessionExit {
        let hello = Frame::Json(serde_json::json!({
            "type": "hello",
            "protocol_version": SOCKET_PROTOCOL_VERSION,
            "min_supported_version": SOCKET_PROTOCOL_MIN_SUPPORTED,
            "client": "tui_bridge",
        }));
        if let Err(e) = self.socket_writer.write_all(&hello.encode()).await {
            log::error!("[TuiBridge] Failed to send hello: {e}");
            return SessionExit::HubDisconnected;
        }

        // Subscribe to hub channel on every fresh socket connection.
        let subscribe = Frame::Json(serde_json::json!({
            "type": "subscribe",
            "channel": "hub",
            "subscriptionId": "tui_hub"
        }));
        if let Err(e) = self.socket_writer.write_all(&subscribe.encode()).await {
            log::error!("[TuiBridge] Failed to send subscribe: {e}");
            return SessionExit::HubDisconnected;
        }

        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 64 * 1024];

        loop {
            tokio::select! {
                msg = self.request_rx.recv() => {
                    let Some(request) = msg else {
                        return SessionExit::Shutdown;
                    };

                    // Intercept quit: detach instead of forwarding to hub.
                    if let TuiRequest::LuaMessage(ref json) = request {
                        if json.pointer("/data/type").and_then(|v| v.as_str()) == Some("quit") {
                            log::info!("[TuiBridge] Quit intercepted — detaching from hub");
                            self.shutdown.store(true, Ordering::SeqCst);
                            return SessionExit::Shutdown;
                        }
                    }

                    let encoded = tui_request_to_frame(&request).encode();
                    if let Err(e) = self.socket_writer.write_all(&encoded).await {
                        log::error!("[TuiBridge] Socket write error: {e}");
                        return SessionExit::HubDisconnected;
                    }
                }
                read = self.socket_reader.read(&mut buf) => {
                    match read {
                        Ok(0) => {
                            log::info!("[TuiBridge] Socket EOF, hub disconnected");
                            return SessionExit::HubDisconnected;
                        }
                        Ok(n) => {
                            match decoder.feed(&buf[..n]) {
                                Ok(frames) => {
                                    for frame in frames {
                                        match frame {
                                            Frame::Json(value)
                                                if value.get("type").and_then(|v| v.as_str())
                                                    == Some("hello_ack") =>
                                            {
                                                let peer = value
                                                    .get("protocol_version")
                                                    .and_then(|v| v.as_u64())
                                                    .unwrap_or(0);
                                                let peer_min = value
                                                    .get("min_supported_version")
                                                    .and_then(|v| v.as_u64())
                                                    .unwrap_or(0);
                                                log::debug!(
                                                    "[TuiBridge] Negotiated socket protocol: peer={} min={}",
                                                    peer,
                                                    peer_min
                                                );
                                            }
                                            other => {
                                                if let Some(output) = frame_to_tui_output(other) {
                                                    if self.output_tx.send(output).is_err() {
                                                        log::info!("[TuiBridge] TuiRunner channel closed");
                                                        return SessionExit::Shutdown;
                                                    }
                                                    // Wake TuiRunner from libc::poll()
                                                    if let Some(fd) = self.wake_write_fd {
                                                        unsafe {
                                                            libc::write(
                                                                fd,
                                                                [1u8].as_ptr() as *const libc::c_void,
                                                                1,
                                                            );
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    log::error!("[TuiBridge] Frame decode error: {e}");
                                    return SessionExit::HubDisconnected;
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("[TuiBridge] Socket read error: {e}");
                            return SessionExit::HubDisconnected;
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if self.shutdown.load(Ordering::Relaxed) {
                        return SessionExit::Shutdown;
                    }
                }
            }
        }
    }

    /// Drop queued TUI->hub requests accumulated while the socket was down.
    ///
    /// After reconnect we rebuild subscriptions from fresh runner state;
    /// replaying queued requests from the dead socket can create ordering
    /// races (duplicate subscribe/replay before UI reset).
    fn drain_stale_requests(&mut self) -> usize {
        let mut dropped = 0usize;
        loop {
            match self.request_rx.try_recv() {
                Ok(_) => dropped += 1,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                | Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
            }
        }
        dropped
    }
}

async fn reconnect_to_hub(
    socket_path: &std::path::Path,
    retries: u32,
    delay_ms: u64,
    shutdown: &Arc<AtomicBool>,
) -> std::io::Result<UnixStream> {
    let mut last_err: Option<std::io::Error> = None;

    for attempt in 0..retries {
        if shutdown.load(Ordering::Relaxed) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "shutdown requested",
            ));
        }

        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }

        match UnixStream::connect(socket_path).await {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                last_err = Some(e);
            }
        }
    }

    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "hub socket unavailable")
    }))
}

/// Convert a socket frame to a TuiOutput message.
///
/// Returns `None` for frame types that don't map to TuiOutput.
fn frame_to_tui_output(frame: Frame) -> Option<TuiOutput> {
    match frame {
        Frame::Json(value) => Some(TuiOutput::Message(value)),
        Frame::PtyOutput { session_uuid, data } => Some(TuiOutput::Output { session_uuid, data }),
        Frame::Scrollback {
            session_uuid,
            rows,
            cols,
            kitty_enabled,
            data,
        } => Some(TuiOutput::Scrollback {
            session_uuid,
            rows,
            cols,
            data,
            kitty_enabled,
        }),
        Frame::ProcessExited {
            session_uuid,
            exit_code,
        } => Some(TuiOutput::ProcessExited {
            session_uuid,
            exit_code,
        }),
        Frame::PtyInput { .. } => None, // Client-to-hub only
        Frame::Binary(data) => Some(TuiOutput::Binary(data)),
    }
}

/// Convert a TuiRequest to a socket frame.
fn tui_request_to_frame(request: &TuiRequest) -> Frame {
    match request {
        TuiRequest::LuaMessage(json) => Frame::Json(json.clone()),
        TuiRequest::PtyInput { session_uuid, data } => Frame::PtyInput {
            session_uuid: session_uuid.clone(),
            data: data.clone(),
        },
        TuiRequest::FocusChanged {
            session_uuid,
            focused,
        } => Frame::Json(serde_json::json!({
            "type": "focus_changed",
            "session_uuid": session_uuid,
            "focused": focused,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a connected TuiBridge over a Unix socket pair.
    async fn setup_bridge(
        tmp: &tempfile::TempDir,
        name: &str,
    ) -> (
        tokio::task::JoinHandle<()>,
        BridgeChannels,
        tokio::net::unix::OwnedReadHalf,
        tokio::net::unix::OwnedWriteHalf,
        Arc<AtomicBool>,
    ) {
        let sock_path = tmp.path().join(name);
        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();
        let client_stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let (server_stream, _) = listener.accept().await.unwrap();
        let (server_read, server_write) = server_stream.into_split();

        let shutdown = Arc::new(AtomicBool::new(false));
        let (bridge, channels) = TuiBridge::connect(client_stream, None, shutdown.clone());
        let handle = tokio::spawn(bridge.run());

        (handle, channels, server_read, server_write, shutdown)
    }

    /// Helper: clean shutdown of bridge.
    async fn teardown(
        handle: tokio::task::JoinHandle<()>,
        channels: BridgeChannels,
        shutdown: Arc<AtomicBool>,
        server_read: tokio::net::unix::OwnedReadHalf,
        server_write: tokio::net::unix::OwnedWriteHalf,
    ) {
        shutdown.store(true, Ordering::SeqCst);
        drop(channels.request_tx);
        drop(server_read);
        drop(server_write);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await;
    }

    #[tokio::test]
    async fn test_bridge_json_hub_to_tui() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (handle, mut channels, server_read, mut server_write, shutdown) =
            setup_bridge(&tmp, "json_h2t.sock").await;

        let msg = Frame::Json(serde_json::json!({"type": "agent_list", "count": 2}));
        server_write.write_all(&msg.encode()).await.unwrap();

        let output =
            tokio::time::timeout(std::time::Duration::from_secs(2), channels.output_rx.recv())
                .await
                .expect("Timed out")
                .expect("Channel closed");

        match output {
            TuiOutput::Message(value) => {
                assert_eq!(value["type"], "agent_list");
                assert_eq!(value["count"], 2);
            }
            other => panic!("Expected TuiOutput::Message, got: {other:?}"),
        }

        teardown(handle, channels, shutdown, server_read, server_write).await;
    }

    #[tokio::test]
    async fn test_bridge_json_tui_to_hub() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (handle, channels, mut server_read, server_write, shutdown) =
            setup_bridge(&tmp, "json_t2h.sock").await;

        // Send a test message from TUI side
        channels
            .request_tx
            .send(TuiRequest::LuaMessage(serde_json::json!({"type": "ping"})))
            .unwrap();

        // Read all frames — bridge sends auto-subscribe first, then our test message
        let mut decoder = FrameDecoder::new();
        let mut all_frames = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while all_frames
            .iter()
            .filter(|f| matches!(f, Frame::Json(v) if v["type"] == "ping"))
            .count()
            == 0
        {
            let mut buf = [0u8; 4096];
            let n = tokio::time::timeout_at(deadline, server_read.read(&mut buf))
                .await
                .expect("Timed out waiting for ping frame")
                .expect("Read error");
            all_frames.extend(decoder.feed(&buf[..n]).unwrap());
        }

        // Verify auto-subscribe was sent
        let has_subscribe = all_frames
            .iter()
            .any(|f| matches!(f, Frame::Json(v) if v["type"] == "subscribe"));
        assert!(
            has_subscribe,
            "Expected auto-subscribe frame, got: {all_frames:?}"
        );

        // Verify our test message arrived
        let has_ping = all_frames
            .iter()
            .any(|f| matches!(f, Frame::Json(v) if v["type"] == "ping"));
        assert!(has_ping, "Expected ping frame, got: {all_frames:?}");

        teardown(handle, channels, shutdown, server_read, server_write).await;
    }

    #[tokio::test]
    async fn test_bridge_pty_output_hub_to_tui() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (handle, mut channels, server_read, mut server_write, shutdown) =
            setup_bridge(&tmp, "pty_out.sock").await;

        let frame = Frame::PtyOutput {
            session_uuid: "test-session".to_string(),
            data: b"$ echo hello\r\nhello\r\n".to_vec(),
        };
        server_write.write_all(&frame.encode()).await.unwrap();

        let output =
            tokio::time::timeout(std::time::Duration::from_secs(2), channels.output_rx.recv())
                .await
                .expect("Timed out")
                .expect("Channel closed");

        match output {
            TuiOutput::Output { session_uuid, data } => {
                assert_eq!(session_uuid, "test-session");
                assert_eq!(data, b"$ echo hello\r\nhello\r\n");
            }
            other => panic!("Expected TuiOutput::Output, got: {other:?}"),
        }

        teardown(handle, channels, shutdown, server_read, server_write).await;
    }

    #[tokio::test]
    async fn test_bridge_pty_input_tui_to_hub() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (handle, channels, mut server_read, server_write, shutdown) =
            setup_bridge(&tmp, "pty_in.sock").await;

        channels
            .request_tx
            .send(TuiRequest::PtyInput {
                session_uuid: "test-session".to_string(),
                data: b"hello".to_vec(),
            })
            .unwrap();

        // Read frames — bridge sends auto-subscribe first, then our PtyInput
        let mut decoder = FrameDecoder::new();
        let mut all_frames = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while !all_frames
            .iter()
            .any(|f| matches!(f, Frame::PtyInput { .. }))
        {
            let mut buf = [0u8; 4096];
            let n = tokio::time::timeout_at(deadline, server_read.read(&mut buf))
                .await
                .expect("Timed out waiting for PtyInput frame")
                .expect("Read error");
            all_frames.extend(decoder.feed(&buf[..n]).unwrap());
        }

        let pty_frame = all_frames
            .iter()
            .find(|f| matches!(f, Frame::PtyInput { .. }))
            .unwrap();
        match pty_frame {
            Frame::PtyInput { session_uuid, data } => {
                assert_eq!(session_uuid, "test-session");
                assert_eq!(data, b"hello");
            }
            _ => unreachable!(),
        }

        teardown(handle, channels, shutdown, server_read, server_write).await;
    }

    #[tokio::test]
    async fn test_bridge_scrollback_and_process_exited() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (handle, mut channels, server_read, mut server_write, shutdown) =
            setup_bridge(&tmp, "lifecycle.sock").await;

        // Scrollback
        let sb = Frame::Scrollback {
            session_uuid: "test-session".to_string(),
            rows: 24,
            cols: 80,
            kitty_enabled: true,
            data: b"scrollback".to_vec(),
        };
        server_write.write_all(&sb.encode()).await.unwrap();

        let output =
            tokio::time::timeout(std::time::Duration::from_secs(2), channels.output_rx.recv())
                .await
                .unwrap()
                .unwrap();

        match output {
            TuiOutput::Scrollback {
                session_uuid,
                rows,
                cols,
                kitty_enabled,
                data,
            } => {
                assert_eq!(session_uuid, "test-session");
                assert_eq!(rows, 24);
                assert_eq!(cols, 80);
                assert!(kitty_enabled);
                assert_eq!(data, b"scrollback");
            }
            other => panic!("Expected Scrollback, got: {other:?}"),
        }

        // Process exited
        let ex = Frame::ProcessExited {
            session_uuid: "test-session".to_string(),
            exit_code: Some(42),
        };
        server_write.write_all(&ex.encode()).await.unwrap();

        let output =
            tokio::time::timeout(std::time::Duration::from_secs(2), channels.output_rx.recv())
                .await
                .unwrap()
                .unwrap();

        match output {
            TuiOutput::ProcessExited {
                session_uuid,
                exit_code,
            } => {
                assert_eq!(session_uuid, "test-session");
                assert_eq!(exit_code, Some(42));
            }
            other => panic!("Expected ProcessExited, got: {other:?}"),
        }

        teardown(handle, channels, shutdown, server_read, server_write).await;
    }

    #[tokio::test]
    async fn test_bridge_empty_scrollback_forwards_to_tui() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (handle, mut channels, server_read, mut server_write, shutdown) =
            setup_bridge(&tmp, "empty_scrollback.sock").await;

        let sb = Frame::Scrollback {
            session_uuid: "test-session".to_string(),
            rows: 24,
            cols: 80,
            kitty_enabled: false,
            data: Vec::new(),
        };
        server_write.write_all(&sb.encode()).await.unwrap();

        let output =
            tokio::time::timeout(std::time::Duration::from_secs(2), channels.output_rx.recv())
                .await
                .unwrap()
                .unwrap();

        match output {
            TuiOutput::Scrollback {
                session_uuid,
                rows,
                cols,
                kitty_enabled,
                data,
            } => {
                assert_eq!(session_uuid, "test-session");
                assert_eq!(rows, 24);
                assert_eq!(cols, 80);
                assert!(!kitty_enabled);
                assert!(data.is_empty());
            }
            other => panic!("Expected Scrollback, got: {other:?}"),
        }

        teardown(handle, channels, shutdown, server_read, server_write).await;
    }

    #[tokio::test]
    async fn test_bridge_hub_disconnect_signals_shutdown() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (handle, _channels, server_read, server_write, shutdown) =
            setup_bridge(&tmp, "dc.sock").await;

        // Drop server side — simulates hub going away
        drop(server_read);
        drop(server_write);

        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("Bridge should exit after hub disconnects");

        assert!(
            shutdown.load(Ordering::SeqCst),
            "Shutdown flag should be set after hub disconnects"
        );
    }

    #[tokio::test]
    async fn test_bridge_quit_detaches_without_forwarding() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (handle, channels, server_read, _server_write, shutdown) =
            setup_bridge(&tmp, "quit.sock").await;

        // Send a quit message (same as Ctrl+Q in TuiRunner)
        channels
            .request_tx
            .send(TuiRequest::LuaMessage(serde_json::json!({
                "subscriptionId": "tui_hub",
                "data": { "type": "quit" }
            })))
            .unwrap();

        // Bridge should exit (quit intercepted → shutdown flag set)
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("Bridge should exit after quit");

        assert!(
            shutdown.load(Ordering::SeqCst),
            "Shutdown flag should be set after quit"
        );

        // Read everything the server received — should only have subscribe, no quit
        let mut decoder = FrameDecoder::new();
        let mut all_frames = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match server_read.try_read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(frames) = decoder.feed(&buf[..n]) {
                        all_frames.extend(frames);
                    }
                }
            }
        }

        let has_quit = all_frames.iter().any(|f| {
            matches!(f, Frame::Json(v) if v.pointer("/data/type").and_then(|v| v.as_str()) == Some("quit"))
        });
        assert!(
            !has_quit,
            "Quit message should NOT be forwarded to hub, got: {all_frames:?}"
        );
    }

    #[tokio::test]
    async fn test_bridge_reconnect_resubscribes_and_drops_stale_requests() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sock_path = tmp.path().join("reconnect.sock");

        let listener_a = tokio::net::UnixListener::bind(&sock_path).unwrap();
        let client_stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let (server_stream_a, _) = listener_a.accept().await.unwrap();
        let (mut server_read_a, server_write_a) = server_stream_a.into_split();

        let shutdown = Arc::new(AtomicBool::new(false));
        let (bridge, mut channels) = TuiBridge::connect_with_reconnect(
            client_stream,
            Some(sock_path.clone()),
            None,
            shutdown.clone(),
        );
        let handle = tokio::spawn(bridge.run());

        // Initial connection sends hello + hub subscribe.
        let mut decoder_a = FrameDecoder::new();
        let mut buf = [0u8; 4096];
        let mut frames = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while !frames.iter().any(
            |f| matches!(f, Frame::Json(v) if v["type"] == "subscribe" && v["channel"] == "hub"),
        ) {
            let n = tokio::time::timeout_at(deadline, server_read_a.read(&mut buf))
                .await
                .expect("timed out waiting for initial subscribe")
                .expect("read error on initial socket");
            if n == 0 {
                break;
            }
            frames.extend(decoder_a.feed(&buf[..n]).expect("decode initial frame"));
        }
        assert!(
            frames
                .iter()
                .any(|f| matches!(f, Frame::Json(v) if v["type"] == "hello")),
            "expected hello frame, got: {frames:?}"
        );
        assert!(
            frames.iter().any(
                |f| matches!(f, Frame::Json(v) if v["type"] == "subscribe" && v["channel"] == "hub")
            ),
            "expected initial hub subscribe frame, got: {frames:?}"
        );

        // Force disconnect and queue a stale request while socket is down.
        drop(server_write_a);
        drop(server_read_a);
        drop(listener_a);
        let _ = std::fs::remove_file(&sock_path);

        channels
            .request_tx
            .send(TuiRequest::LuaMessage(serde_json::json!({
                "type": "stale_before_reconnect",
                "subscriptionId": "tui:stale",
            })))
            .unwrap();

        // Bring up replacement hub socket for reconnect.
        let listener_b = tokio::net::UnixListener::bind(&sock_path).unwrap();
        let (server_stream_b, _) =
            tokio::time::timeout(std::time::Duration::from_secs(5), listener_b.accept())
                .await
                .expect("timed out waiting for reconnect accept")
                .expect("reconnect accept failed");
        let (mut server_read_b, server_write_b) = server_stream_b.into_split();

        // Runner gets bridge_reconnected event on successful reconnect.
        let reconnect_msg =
            tokio::time::timeout(std::time::Duration::from_secs(2), channels.output_rx.recv())
                .await
                .expect("timed out waiting for bridge_reconnected")
                .expect("output channel closed");
        match reconnect_msg {
            TuiOutput::Message(value) => {
                assert_eq!(value["type"], "bridge_reconnected");
            }
            other => panic!("expected bridge_reconnected message, got: {other:?}"),
        }

        // New socket session should begin with fresh hello + hub subscribe.
        let mut decoder_b = FrameDecoder::new();
        let mut frames = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while !frames.iter().any(
            |f| matches!(f, Frame::Json(v) if v["type"] == "subscribe" && v["channel"] == "hub"),
        ) {
            let n = tokio::time::timeout_at(deadline, server_read_b.read(&mut buf))
                .await
                .expect("timed out waiting for reconnect subscribe")
                .expect("read error on reconnect socket");
            if n == 0 {
                break;
            }
            frames.extend(decoder_b.feed(&buf[..n]).expect("decode reconnect frame"));
        }
        assert!(
            frames
                .iter()
                .any(|f| matches!(f, Frame::Json(v) if v["type"] == "hello")),
            "expected reconnect hello frame, got: {frames:?}"
        );
        assert!(
            frames.iter().any(
                |f| matches!(f, Frame::Json(v) if v["type"] == "subscribe" && v["channel"] == "hub")
            ),
            "expected reconnect hub subscribe frame, got: {frames:?}"
        );
        assert!(
            !frames
                .iter()
                .any(|f| matches!(f, Frame::Json(v) if v["type"] == "stale_before_reconnect")),
            "stale request should be dropped on reconnect, got: {frames:?}"
        );

        // Fresh post-reconnect requests must still flow to hub.
        channels
            .request_tx
            .send(TuiRequest::LuaMessage(serde_json::json!({
                "type": "fresh_after_reconnect",
            })))
            .unwrap();

        let mut saw_fresh = false;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline && !saw_fresh {
            let n = tokio::time::timeout_at(deadline, server_read_b.read(&mut buf))
                .await
                .expect("timed out waiting for fresh request")
                .expect("read error while waiting for fresh request");
            if n == 0 {
                break;
            }
            let frames = decoder_b.feed(&buf[..n]).expect("decode frame");
            saw_fresh = frames
                .iter()
                .any(|f| matches!(f, Frame::Json(v) if v["type"] == "fresh_after_reconnect"));
        }
        assert!(saw_fresh, "expected fresh request to flow after reconnect");

        drop(listener_b);
        teardown(handle, channels, shutdown, server_read_b, server_write_b).await;
    }
}
