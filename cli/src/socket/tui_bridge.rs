//! TUI bridge adapter for `botster attach`.
//!
//! Translates between the TuiRunner's in-process channels and the socket
//! wire protocol. TuiRunner doesn't change at all — the bridge provides
//! the same `mpsc` channel types and wake pipe semantics.
//!
//! ```text
//! TuiRunner <--mpsc + wake pipe--> TuiBridge <--frames--> Unix Socket <--> Hub
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::client::{TuiOutput, TuiRequest};
use super::framing::{Frame, FrameDecoder};

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
    pub async fn run(self) {
        let Self {
            output_tx,
            mut request_rx,
            mut socket_reader,
            mut socket_writer,
            wake_write_fd,
            shutdown,
        } = self;

        // Subscribe to hub channel — mirrors what handlers/tui.lua does on connect.
        // Without this, the hub's Lua Client won't send agent state or events.
        let subscribe = Frame::Json(serde_json::json!({
            "type": "subscribe",
            "channel": "hub",
            "subscriptionId": "tui_hub"
        }));
        if let Err(e) = socket_writer.write_all(&subscribe.encode()).await {
            log::error!("[TuiBridge] Failed to send subscribe: {e}");
            shutdown.store(true, Ordering::SeqCst);
            return;
        }

        let shutdown_read = Arc::clone(&shutdown);

        // Inbound: socket frames → TuiOutput → TuiRunner
        let inbound = tokio::spawn(async move {
            let mut decoder = FrameDecoder::new();
            let mut buf = [0u8; 64 * 1024];

            loop {
                if shutdown_read.load(Ordering::Relaxed) {
                    break;
                }

                match socket_reader.read(&mut buf).await {
                    Ok(0) => {
                        log::info!("[TuiBridge] Socket EOF, hub disconnected");
                        break;
                    }
                    Ok(n) => {
                        match decoder.feed(&buf[..n]) {
                            Ok(frames) => {
                                for frame in frames {
                                    if let Some(output) = frame_to_tui_output(frame) {
                                        if output_tx.send(output).is_err() {
                                            log::info!("[TuiBridge] TuiRunner channel closed");
                                            return;
                                        }
                                        // Wake TuiRunner from libc::poll()
                                        if let Some(fd) = wake_write_fd {
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
                            Err(e) => {
                                log::error!("[TuiBridge] Frame decode error: {e}");
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        log::error!("[TuiBridge] Socket read error: {e}");
                        break;
                    }
                }
            }
        });

        // Outbound: TuiRequest from TuiRunner → socket frames
        //
        // Intercepts "quit" messages — in attach mode, Ctrl+Q should detach
        // the TUI (drop the socket), NOT tell the hub to shutdown.
        let shutdown_outbound = Arc::clone(&shutdown);
        let outbound = tokio::spawn(async move {
            while let Some(request) = request_rx.recv().await {
                // Intercept quit: detach instead of forwarding to hub.
                if let TuiRequest::LuaMessage(ref json) = request {
                    if json.pointer("/data/type").and_then(|v| v.as_str()) == Some("quit") {
                        log::info!("[TuiBridge] Quit intercepted — detaching from hub");
                        shutdown_outbound.store(true, Ordering::SeqCst);
                        break;
                    }
                }

                let encoded = tui_request_to_frame(&request).encode();
                if let Err(e) = socket_writer.write_all(&encoded).await {
                    log::error!("[TuiBridge] Socket write error: {e}");
                    break;
                }
            }
        });

        // Wait for either direction to finish
        tokio::select! {
            _ = inbound => {}
            _ = outbound => {}
        }

        shutdown.store(true, Ordering::SeqCst);
    }
}

/// Convert a socket frame to a TuiOutput message.
///
/// Returns `None` for frame types that don't map to TuiOutput.
fn frame_to_tui_output(frame: Frame) -> Option<TuiOutput> {
    match frame {
        Frame::Json(value) => Some(TuiOutput::Message(value)),
        Frame::PtyOutput { agent_index, pty_index, data } => Some(TuiOutput::Output {
            agent_index: Some(agent_index as usize),
            pty_index: Some(pty_index as usize),
            data,
        }),
        Frame::Scrollback { agent_index, pty_index, kitty_enabled, data } => {
            Some(TuiOutput::Scrollback {
                agent_index: Some(agent_index as usize),
                pty_index: Some(pty_index as usize),
                data,
                kitty_enabled,
            })
        }
        Frame::ProcessExited { agent_index, pty_index, exit_code } => {
            Some(TuiOutput::ProcessExited {
                agent_index: Some(agent_index as usize),
                pty_index: Some(pty_index as usize),
                exit_code,
            })
        }
        Frame::PtyInput { .. } => None, // Client-to-hub only
        Frame::Binary(_) => None, // Raw binary not used by TUI
    }
}

/// Convert a TuiRequest to a socket frame.
fn tui_request_to_frame(request: &TuiRequest) -> Frame {
    match request {
        TuiRequest::LuaMessage(json) => Frame::Json(json.clone()),
        TuiRequest::PtyInput { agent_index, pty_index, data } => Frame::PtyInput {
            agent_index: *agent_index as u16,
            pty_index: *pty_index as u16,
            data: data.clone(),
        },
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

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            channels.output_rx.recv(),
        )
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
            .send(TuiRequest::LuaMessage(
                serde_json::json!({"type": "ping"}),
            ))
            .unwrap();

        // Read all frames — bridge sends auto-subscribe first, then our test message
        let mut decoder = FrameDecoder::new();
        let mut all_frames = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while all_frames.iter().filter(|f| matches!(f, Frame::Json(v) if v["type"] == "ping")).count() == 0 {
            let mut buf = [0u8; 4096];
            let n = tokio::time::timeout_at(deadline, server_read.read(&mut buf))
                .await
                .expect("Timed out waiting for ping frame")
                .expect("Read error");
            all_frames.extend(decoder.feed(&buf[..n]).unwrap());
        }

        // Verify auto-subscribe was sent
        let has_subscribe = all_frames.iter().any(|f| matches!(f, Frame::Json(v) if v["type"] == "subscribe"));
        assert!(has_subscribe, "Expected auto-subscribe frame, got: {all_frames:?}");

        // Verify our test message arrived
        let has_ping = all_frames.iter().any(|f| matches!(f, Frame::Json(v) if v["type"] == "ping"));
        assert!(has_ping, "Expected ping frame, got: {all_frames:?}");

        teardown(handle, channels, shutdown, server_read, server_write).await;
    }

    #[tokio::test]
    async fn test_bridge_pty_output_hub_to_tui() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (handle, mut channels, server_read, mut server_write, shutdown) =
            setup_bridge(&tmp, "pty_out.sock").await;

        let frame = Frame::PtyOutput {
            agent_index: 0,
            pty_index: 0,
            data: b"$ echo hello\r\nhello\r\n".to_vec(),
        };
        server_write.write_all(&frame.encode()).await.unwrap();

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            channels.output_rx.recv(),
        )
        .await
        .expect("Timed out")
        .expect("Channel closed");

        match output {
            TuiOutput::Output { agent_index, pty_index, data } => {
                assert_eq!(agent_index, Some(0));
                assert_eq!(pty_index, Some(0));
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
                agent_index: 1,
                pty_index: 0,
                data: b"hello".to_vec(),
            })
            .unwrap();

        // Read frames — bridge sends auto-subscribe first, then our PtyInput
        let mut decoder = FrameDecoder::new();
        let mut all_frames = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while !all_frames.iter().any(|f| matches!(f, Frame::PtyInput { .. })) {
            let mut buf = [0u8; 4096];
            let n = tokio::time::timeout_at(deadline, server_read.read(&mut buf))
                .await
                .expect("Timed out waiting for PtyInput frame")
                .expect("Read error");
            all_frames.extend(decoder.feed(&buf[..n]).unwrap());
        }

        let pty_frame = all_frames.iter().find(|f| matches!(f, Frame::PtyInput { .. })).unwrap();
        match pty_frame {
            Frame::PtyInput { agent_index, pty_index, data } => {
                assert_eq!(*agent_index, 1);
                assert_eq!(*pty_index, 0);
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
            agent_index: 0,
            pty_index: 0,
            kitty_enabled: true,
            data: b"scrollback".to_vec(),
        };
        server_write.write_all(&sb.encode()).await.unwrap();

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            channels.output_rx.recv(),
        )
        .await
        .unwrap()
        .unwrap();

        match output {
            TuiOutput::Scrollback { agent_index, pty_index, kitty_enabled, data } => {
                assert_eq!(agent_index, Some(0));
                assert_eq!(pty_index, Some(0));
                assert!(kitty_enabled);
                assert_eq!(data, b"scrollback");
            }
            other => panic!("Expected Scrollback, got: {other:?}"),
        }

        // Process exited
        let ex = Frame::ProcessExited {
            agent_index: 0,
            pty_index: 0,
            exit_code: Some(42),
        };
        server_write.write_all(&ex.encode()).await.unwrap();

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            channels.output_rx.recv(),
        )
        .await
        .unwrap()
        .unwrap();

        match output {
            TuiOutput::ProcessExited { agent_index, pty_index, exit_code } => {
                assert_eq!(agent_index, Some(0));
                assert_eq!(pty_index, Some(0));
                assert_eq!(exit_code, Some(42));
            }
            other => panic!("Expected ProcessExited, got: {other:?}"),
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
        let (handle, channels, mut server_read, server_write, shutdown) =
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
        assert!(!has_quit, "Quit message should NOT be forwarded to hub, got: {all_frames:?}");
    }
}
