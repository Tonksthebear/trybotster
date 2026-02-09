//! TCP stream multiplexer for preview tunneling.
//!
//! Multiplexes multiple TCP streams over the encrypted WebRTC DataChannel.
//! Each browser-initiated stream maps to a `TcpStream::connect("127.0.0.1", port)`.
//!
//! # Frame Format
//!
//! After Olm decryption, CONTENT_STREAM frames have sub-framing:
//! ```text
//! [0x02][frame_type:1][stream_id:2 BE][payload...]
//! ```
//!
//! Frame types:
//! - `FRAME_OPEN` (0x00): Browser->CLI, payload = `[port:2 BE]`
//! - `FRAME_DATA` (0x01): Bidirectional, payload = raw bytes (<=16KB)
//! - `FRAME_CLOSE` (0x02): Bidirectional, empty payload
//! - `FRAME_OPENED` (0x03): CLI->Browser, empty payload (TCP connected)
//! - `FRAME_ERROR` (0x04): CLI->Browser, payload = UTF-8 error message

// Rust guideline compliant 2026-02

use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

/// Frame type: Browser->CLI, open a new TCP stream. Payload = `[port:2 BE]`.
pub const FRAME_OPEN: u8 = 0x00;

/// Frame type: Bidirectional, raw TCP data. Payload = bytes (<=16KB).
pub const FRAME_DATA: u8 = 0x01;

/// Frame type: Bidirectional, close a stream. Empty payload.
pub const FRAME_CLOSE: u8 = 0x02;

/// Frame type: CLI->Browser, TCP connection established. Empty payload.
pub const FRAME_OPENED: u8 = 0x03;

/// Frame type: CLI->Browser, error message. Payload = UTF-8 string.
pub const FRAME_ERROR: u8 = 0x04;

/// Maximum chunk size for TCP reads (matches cross-browser SCTP safe limit).
const MAX_CHUNK_SIZE: usize = 16384;

/// Bounded channel capacity for write backpressure from slow local servers.
const WRITE_CHANNEL_BOUND: usize = 64;

/// A wire frame for the stream multiplexer.
#[derive(Debug)]
pub struct StreamFrame {
    /// Frame type (OPEN, DATA, CLOSE, OPENED, ERROR).
    pub frame_type: u8,
    /// Stream identifier.
    pub stream_id: u16,
    /// Frame payload.
    pub payload: Vec<u8>,
}

/// Per-stream state holding the write channel and connection task handle.
struct StreamHandle {
    /// Sender for writing data to the TCP stream's write half.
    write_tx: mpsc::Sender<Vec<u8>>,
    /// Connection task handle (owns TCP connect, reader loop, and writer subtask).
    _task: tokio::task::JoinHandle<()>,
}

/// Per-browser-identity TCP stream multiplexer.
///
/// Manages multiple TCP streams over a single encrypted WebRTC DataChannel.
/// Outbound frames (OPENED, DATA, CLOSE, ERROR) are sent via `output_tx`
/// and drained by the Hub tick loop.
pub struct StreamMultiplexer {
    /// Active streams indexed by stream_id.
    streams: HashMap<u16, StreamHandle>,
    /// Sender for outbound frames (to browser via DataChannel).
    output_tx: mpsc::UnboundedSender<StreamFrame>,
    /// Receiver for outbound frames (drained by Hub).
    output_rx: mpsc::UnboundedReceiver<StreamFrame>,
}

impl std::fmt::Debug for StreamMultiplexer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamMultiplexer")
            .field("active_streams", &self.streams.len())
            .finish()
    }
}

impl StreamMultiplexer {
    /// Create a new stream multiplexer.
    pub fn new() -> Self {
        let (output_tx, output_rx) = mpsc::unbounded_channel();
        Self {
            streams: HashMap::new(),
            output_tx,
            output_rx,
        }
    }

    /// Handle an incoming frame from the browser.
    ///
    /// Dispatches OPEN, DATA, and CLOSE frames appropriately.
    pub fn handle_frame(&mut self, frame_type: u8, stream_id: u16, payload: Vec<u8>) {
        match frame_type {
            FRAME_OPEN => self.handle_open(stream_id, payload),
            FRAME_DATA => self.handle_data(stream_id, payload),
            FRAME_CLOSE => self.handle_close(stream_id),
            other => {
                log::warn!(
                    "[StreamMux] Unknown frame type 0x{:02x} for stream {}",
                    other,
                    stream_id
                );
            }
        }
    }

    /// Drain outbound frames for sending via WebRTC DataChannel.
    pub fn drain_output(&mut self) -> Vec<StreamFrame> {
        let mut frames = Vec::new();
        while let Ok(frame) = self.output_rx.try_recv() {
            frames.push(frame);
        }
        frames
    }

    /// Close all streams (cleanup on browser disconnect).
    pub fn close_all(&mut self) {
        let stream_ids: Vec<u16> = self.streams.keys().copied().collect();
        for stream_id in stream_ids {
            self.handle_close(stream_id);
        }
    }

    /// Handle FRAME_OPEN: connect to localhost:port and set up bidirectional forwarding.
    fn handle_open(&mut self, stream_id: u16, payload: Vec<u8>) {
        if payload.len() < 2 {
            log::warn!("[StreamMux] OPEN frame too short for stream {}", stream_id);
            self.send_error(stream_id, "OPEN payload too short");
            return;
        }

        let port = u16::from_be_bytes([payload[0], payload[1]]);

        // Remove any existing stream with this ID
        self.streams.remove(&stream_id);

        let output_tx = self.output_tx.clone();

        // Create bounded channel for writing data to TCP
        let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>(WRITE_CHANNEL_BOUND);

        // Spawn connection + read/write tasks
        let connect_output_tx = output_tx.clone();
        let reader_task = tokio::spawn(async move {
            // Connect to local server
            let tcp_stream = match TcpStream::connect(("127.0.0.1", port)).await {
                Ok(s) => {
                    log::info!(
                        "[StreamMux] Connected stream {} to 127.0.0.1:{}",
                        stream_id,
                        port
                    );
                    // Send OPENED
                    let _ = connect_output_tx.send(StreamFrame {
                        frame_type: FRAME_OPENED,
                        stream_id,
                        payload: Vec::new(),
                    });
                    s
                }
                Err(e) => {
                    log::warn!(
                        "[StreamMux] Failed to connect stream {} to port {}: {}",
                        stream_id,
                        port,
                        e
                    );
                    let _ = connect_output_tx.send(StreamFrame {
                        frame_type: FRAME_ERROR,
                        stream_id,
                        payload: format!("Connection refused: {}", e).into_bytes(),
                    });
                    return;
                }
            };

            let (mut read_half, mut write_half) = tcp_stream.into_split();

            // Spawn writer task (receives from write_rx, writes to TCP)
            let writer = tokio::spawn(async move {
                let mut write_rx = write_rx;
                while let Some(data) = write_rx.recv().await {
                    if let Err(e) = write_half.write_all(&data).await {
                        log::debug!(
                            "[StreamMux] Write error on stream {}: {}",
                            stream_id,
                            e
                        );
                        break;
                    }
                }
                // Channel closed or write error - shutdown write half
                let _ = write_half.shutdown().await;
            });

            // Read loop: reads from TCP, sends FRAME_DATA to output
            let mut buf = vec![0u8; MAX_CHUNK_SIZE];
            loop {
                match read_half.read(&mut buf).await {
                    Ok(0) => {
                        // TCP connection closed
                        log::debug!("[StreamMux] Stream {} read EOF", stream_id);
                        let _ = connect_output_tx.send(StreamFrame {
                            frame_type: FRAME_CLOSE,
                            stream_id,
                            payload: Vec::new(),
                        });
                        break;
                    }
                    Ok(n) => {
                        let _ = connect_output_tx.send(StreamFrame {
                            frame_type: FRAME_DATA,
                            stream_id,
                            payload: buf[..n].to_vec(),
                        });
                    }
                    Err(e) => {
                        log::debug!("[StreamMux] Stream {} read error: {}", stream_id, e);
                        let _ = connect_output_tx.send(StreamFrame {
                            frame_type: FRAME_CLOSE,
                            stream_id,
                            payload: Vec::new(),
                        });
                        break;
                    }
                }
            }

            writer.abort();
        });

        self.streams.insert(
            stream_id,
            StreamHandle {
                write_tx,
                _task: reader_task,
            },
        );
    }

    /// Handle FRAME_DATA: forward payload to the stream's TCP write half.
    fn handle_data(&mut self, stream_id: u16, payload: Vec<u8>) {
        if let Some(handle) = self.streams.get(&stream_id) {
            // Use try_send to avoid blocking the tick loop if the TCP write is slow
            if let Err(e) = handle.write_tx.try_send(payload) {
                match e {
                    mpsc::error::TrySendError::Full(_) => {
                        log::warn!(
                            "[StreamMux] Write channel full for stream {} (backpressure)",
                            stream_id
                        );
                    }
                    mpsc::error::TrySendError::Closed(_) => {
                        log::debug!(
                            "[StreamMux] Write channel closed for stream {}, removing",
                            stream_id
                        );
                        self.streams.remove(&stream_id);
                    }
                }
            }
        } else {
            log::debug!(
                "[StreamMux] DATA for unknown stream {}, ignoring",
                stream_id
            );
        }
    }

    /// Handle FRAME_CLOSE: remove and drop the stream handle.
    fn handle_close(&mut self, stream_id: u16) {
        if self.streams.remove(&stream_id).is_some() {
            log::debug!("[StreamMux] Closed stream {}", stream_id);
        }
        // Dropping StreamHandle closes write_tx -> writer task exits -> TCP closes
        // Reader task will see EOF or connection reset and exit naturally
    }

    /// Send an error frame to the browser.
    fn send_error(&self, stream_id: u16, message: &str) {
        let _ = self.output_tx.send(StreamFrame {
            frame_type: FRAME_ERROR,
            stream_id,
            payload: message.as_bytes().to_vec(),
        });
    }
}
