//! Per-connection state for socket clients (Hub-side).
//!
//! Each accepted socket connection gets a `SocketClientConn` that manages
//! the read/write tasks and translates between frames and `HubEvent`s.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::mpsc::{self, UnboundedSender, UnboundedReceiver};
use tokio::task::JoinHandle;

use crate::hub::events::HubEvent;
use super::framing::{Frame, FrameDecoder};

/// Hub-side connection state for a single socket client.
///
/// Owns read/write tasks that bridge between the Unix socket and the Hub event loop.
pub struct SocketClientConn {
    /// Unique identifier for this client.
    client_id: String,
    /// Sender for outgoing frames to this client.
    frame_tx: UnboundedSender<Vec<u8>>,
    /// Handle to the read task (for cleanup).
    read_handle: JoinHandle<()>,
    /// Handle to the write task (for cleanup).
    write_handle: JoinHandle<()>,
}

impl std::fmt::Debug for SocketClientConn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SocketClientConn")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

impl SocketClientConn {
    /// Create a new connection handler for an accepted socket.
    ///
    /// Spawns read and write tasks:
    /// - Read task: decodes frames from socket → sends `HubEvent` variants
    /// - Write task: receives encoded frames → writes to socket
    pub(crate) fn new(
        client_id: String,
        stream: UnixStream,
        hub_event_tx: UnboundedSender<HubEvent>,
    ) -> Self {
        let (read_half, write_half) = stream.into_split();
        let (frame_tx, frame_rx) = mpsc::unbounded_channel::<Vec<u8>>();

        let read_client_id = client_id.clone();
        let read_handle = tokio::spawn(Self::read_loop(
            read_client_id,
            read_half,
            hub_event_tx,
        ));

        let write_client_id = client_id.clone();
        let write_handle = tokio::spawn(Self::write_loop(
            write_client_id,
            write_half,
            frame_rx,
        ));

        Self {
            client_id,
            frame_tx,
            read_handle,
            write_handle,
        }
    }

    /// Send a frame to this client.
    ///
    /// The frame is encoded and queued for the write task.
    /// Returns `false` if the write channel is closed (client disconnected).
    pub fn send_frame(&self, frame: &Frame) -> bool {
        self.frame_tx.send(frame.encode()).is_ok()
    }

    /// Send pre-encoded bytes to this client.
    ///
    /// Useful when the frame is already encoded (e.g., from a PTY forwarder).
    pub fn send_raw(&self, encoded: Vec<u8>) -> bool {
        self.frame_tx.send(encoded).is_ok()
    }

    /// Client identifier.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Get a clone of the frame sender for direct use by forwarder tasks.
    ///
    /// The sender accepts pre-encoded frame bytes (from `Frame::encode()`).
    pub fn frame_sender(&self) -> UnboundedSender<Vec<u8>> {
        self.frame_tx.clone()
    }

    /// Disconnect this client, aborting read/write tasks.
    pub fn disconnect(self) {
        self.read_handle.abort();
        self.write_handle.abort();
    }

    /// Read loop — decodes frames from socket and sends HubEvents.
    async fn read_loop(
        client_id: String,
        mut reader: tokio::net::unix::OwnedReadHalf,
        hub_event_tx: UnboundedSender<HubEvent>,
    ) {
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 64 * 1024]; // 64KB read buffer

        loop {
            match reader.read(&mut buf).await {
                Ok(0) => {
                    // EOF — client disconnected
                    log::info!("[Socket] Client disconnected: {}", client_id);
                    let _ = hub_event_tx.send(HubEvent::SocketClientDisconnected {
                        client_id: client_id.clone(),
                    });
                    break;
                }
                Ok(n) => {
                    match decoder.feed(&buf[..n]) {
                        Ok(frames) => {
                            for frame in frames {
                                if !Self::dispatch_frame(&client_id, frame, &hub_event_tx) {
                                    return; // Hub channel closed
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("[Socket] Frame decode error for {}: {e}", client_id);
                            let _ = hub_event_tx.send(HubEvent::SocketClientDisconnected {
                                client_id: client_id.clone(),
                            });
                            break;
                        }
                    }
                }
                Err(e) => {
                    log::error!("[Socket] Read error for {}: {e}", client_id);
                    let _ = hub_event_tx.send(HubEvent::SocketClientDisconnected {
                        client_id: client_id.clone(),
                    });
                    break;
                }
            }
        }
    }

    /// Dispatch a decoded frame as a HubEvent.
    ///
    /// Returns `false` if the hub event channel is closed.
    fn dispatch_frame(
        client_id: &str,
        frame: Frame,
        hub_event_tx: &UnboundedSender<HubEvent>,
    ) -> bool {
        let event = match frame {
            Frame::Json(msg) => HubEvent::SocketMessage {
                client_id: client_id.to_string(),
                msg,
            },
            Frame::PtyInput { agent_index, pty_index, data } => HubEvent::SocketPtyInput {
                client_id: client_id.to_string(),
                agent_index: agent_index as usize,
                pty_index: pty_index as usize,
                data,
            },
            // Clients shouldn't send these frame types — ignore them
            Frame::PtyOutput { .. }
            | Frame::Scrollback { .. }
            | Frame::ProcessExited { .. }
            | Frame::Binary(_) => {
                log::warn!("[Socket] Client {} sent unexpected frame type", client_id);
                return true;
            }
        };

        hub_event_tx.send(event).is_ok()
    }

    /// Write loop — receives encoded frames and writes to socket.
    async fn write_loop(
        client_id: String,
        mut writer: tokio::net::unix::OwnedWriteHalf,
        mut frame_rx: UnboundedReceiver<Vec<u8>>,
    ) {
        while let Some(data) = frame_rx.recv().await {
            if let Err(e) = writer.write_all(&data).await {
                log::error!("[Socket] Write error for {}: {e}", client_id);
                break;
            }
        }
    }
}
