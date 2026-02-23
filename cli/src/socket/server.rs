//! Unix domain socket server for accepting client connections.
//!
//! Listens on a Unix socket and creates a [`SocketClientConn`] for each
//! accepted connection. Each connection is announced to the Hub via
//! `HubEvent::SocketClientConnected`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::net::UnixListener;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use crate::hub::events::HubEvent;
use super::client_conn::SocketClientConn;

/// Unix domain socket server for Hub IPC.
///
/// Binds a `UnixListener` and spawns an accept loop that creates
/// [`SocketClientConn`] instances for each connection.
#[derive(Debug)]
pub struct SocketServer {
    /// Path to the socket file (for cleanup).
    socket_path: PathBuf,
    /// Handle to the accept loop task.
    accept_handle: JoinHandle<()>,
}

impl SocketServer {
    /// Start the socket server at the given path.
    ///
    /// Removes any stale socket file, binds the listener, sets permissions
    /// to 0600, and spawns the accept loop.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket cannot be bound.
    pub(crate) fn start(
        socket_path: PathBuf,
        hub_event_tx: UnboundedSender<HubEvent>,
    ) -> Result<Self> {
        // Validate socket path length against OS limit (104 bytes on macOS, 108 on Linux)
        let path_len = socket_path.as_os_str().len();
        // sun_path is 104 on macOS, 108 on Linux; use conservative limit
        const MAX_SOCKET_PATH: usize = 104;
        if path_len >= MAX_SOCKET_PATH {
            anyhow::bail!(
                "Socket path too long ({path_len} bytes, max {}): {}\n\
                 Consider setting BOTSTER_HUB_ID to a shorter value.",
                MAX_SOCKET_PATH - 1,
                socket_path.display()
            );
        }

        // Remove stale socket file if it exists
        if socket_path.exists() {
            std::fs::remove_file(&socket_path)
                .with_context(|| format!("Failed to remove stale socket: {}", socket_path.display()))?;
        }

        // Ensure parent directory exists
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let listener = std::os::unix::net::UnixListener::bind(&socket_path)
            .with_context(|| format!("Failed to bind socket: {}", socket_path.display()))?;

        // Set socket permissions to owner-only (0600)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&socket_path, perms)?;
        }

        // Convert std listener to tokio async listener
        listener.set_nonblocking(true)?;
        let listener = UnixListener::from_std(listener)?;

        log::info!("Socket server listening on {}", socket_path.display());

        let path_clone = socket_path.clone();
        let accept_handle = tokio::spawn(Self::accept_loop(listener, hub_event_tx, path_clone));

        Ok(Self {
            socket_path,
            accept_handle,
        })
    }

    /// Accept loop — runs as a tokio task.
    async fn accept_loop(
        listener: UnixListener,
        hub_event_tx: UnboundedSender<HubEvent>,
        socket_path: PathBuf,
    ) {
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let client_id = generate_client_id();
                    log::info!("[Socket] Client connected: {}", client_id);

                    let conn = SocketClientConn::new(
                        client_id.clone(),
                        stream,
                        hub_event_tx.clone(),
                    );

                    if hub_event_tx.send(HubEvent::SocketClientConnected {
                        client_id,
                        conn,
                    }).is_err() {
                        log::warn!("[Socket] Hub event channel closed, stopping accept loop");
                        break;
                    }
                }
                Err(e) => {
                    // Check if the socket file still exists (server shutting down)
                    if !socket_path.exists() {
                        log::info!("[Socket] Socket file removed, stopping accept loop");
                        break;
                    }
                    log::error!("[Socket] Accept error: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    }

    /// Stop the socket server and clean up the socket file.
    pub fn shutdown(self) {
        self.accept_handle.abort();
        // Socket file cleanup is handled by daemon::cleanup_on_shutdown
    }

    /// Path to the socket file.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

/// Generate a unique client ID using a monotonic counter + random suffix.
fn generate_client_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let rand: u16 = rand::random();
    format!("socket:{seq:x}{rand:04x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::socket::framing::{Frame, FrameDecoder};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_server_accepts_connection_and_fires_event() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sock_path = tmp.path().join("test.sock");
        let (hub_tx, mut hub_rx) = mpsc::unbounded_channel::<HubEvent>();

        let server = SocketServer::start(sock_path.clone(), hub_tx).unwrap();

        let _stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();

        let event = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            hub_rx.recv(),
        )
        .await
        .expect("Timed out waiting for connect event")
        .expect("Channel closed");

        match event {
            HubEvent::SocketClientConnected { client_id, conn } => {
                assert!(
                    client_id.starts_with("socket:"),
                    "Expected 'socket:' prefix, got: {client_id}"
                );
                conn.disconnect();
            }
            other => panic!("Expected SocketClientConnected, got: {other:?}"),
        }

        server.shutdown();
    }

    #[tokio::test]
    async fn test_client_json_message_arrives_as_hub_event() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sock_path = tmp.path().join("test.sock");
        let (hub_tx, mut hub_rx) = mpsc::unbounded_channel::<HubEvent>();

        let _server = SocketServer::start(sock_path.clone(), hub_tx).unwrap();
        let mut stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();

        // Consume connect event, grab client_id
        let connected_id = match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            hub_rx.recv(),
        )
        .await
        .unwrap()
        .unwrap()
        {
            HubEvent::SocketClientConnected { client_id, .. } => client_id,
            other => panic!("Expected SocketClientConnected, got: {other:?}"),
        };

        // Send a JSON frame from the "client" side
        let frame = Frame::Json(serde_json::json!({
            "type": "subscribe",
            "channel": "hub",
            "subscriptionId": "test_sub_1"
        }));
        stream.write_all(&frame.encode()).await.unwrap();

        let event = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            hub_rx.recv(),
        )
        .await
        .expect("Timed out waiting for message event")
        .expect("Channel closed");

        match event {
            HubEvent::SocketMessage { client_id, msg } => {
                assert_eq!(client_id, connected_id);
                assert_eq!(msg["type"], "subscribe");
                assert_eq!(msg["channel"], "hub");
                assert_eq!(msg["subscriptionId"], "test_sub_1");
            }
            other => panic!("Expected SocketMessage, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_client_pty_input_arrives_as_hub_event() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sock_path = tmp.path().join("test.sock");
        let (hub_tx, mut hub_rx) = mpsc::unbounded_channel::<HubEvent>();

        let _server = SocketServer::start(sock_path.clone(), hub_tx).unwrap();
        let mut stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();

        // Consume connect event
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), hub_rx.recv())
            .await.unwrap().unwrap();

        let frame = Frame::PtyInput {
            agent_index: 2,
            pty_index: 1,
            data: b"ls -la\n".to_vec(),
        };
        stream.write_all(&frame.encode()).await.unwrap();

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), hub_rx.recv())
            .await.expect("Timed out").expect("Channel closed");

        match event {
            HubEvent::SocketPtyInput { agent_index, pty_index, data, .. } => {
                assert_eq!(agent_index, 2);
                assert_eq!(pty_index, 1);
                assert_eq!(data, b"ls -la\n");
            }
            other => panic!("Expected SocketPtyInput, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_server_sends_frame_to_client() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sock_path = tmp.path().join("test.sock");
        let (hub_tx, mut hub_rx) = mpsc::unbounded_channel::<HubEvent>();

        let _server = SocketServer::start(sock_path.clone(), hub_tx).unwrap();
        let mut stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();

        let conn = match tokio::time::timeout(std::time::Duration::from_secs(2), hub_rx.recv())
            .await.unwrap().unwrap()
        {
            HubEvent::SocketClientConnected { conn, .. } => conn,
            other => panic!("Expected SocketClientConnected, got: {other:?}"),
        };

        // Hub sends a JSON frame to the client
        assert!(conn.send_frame(&Frame::Json(serde_json::json!({
            "type": "agent_list",
            "agents": [{"name": "test-agent", "index": 0}]
        }))));

        // Read it from the client side
        let mut buf = [0u8; 4096];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), stream.read(&mut buf))
            .await.expect("Timed out").expect("Read failed");

        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&buf[..n]).unwrap();
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            Frame::Json(value) => {
                assert_eq!(value["type"], "agent_list");
                assert_eq!(value["agents"][0]["name"], "test-agent");
            }
            other => panic!("Expected Json frame, got: {other:?}"),
        }

        conn.disconnect();
    }

    #[tokio::test]
    async fn test_client_disconnect_fires_event() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sock_path = tmp.path().join("test.sock");
        let (hub_tx, mut hub_rx) = mpsc::unbounded_channel::<HubEvent>();

        let _server = SocketServer::start(sock_path.clone(), hub_tx).unwrap();
        let stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();

        let connected_id = match tokio::time::timeout(std::time::Duration::from_secs(2), hub_rx.recv())
            .await.unwrap().unwrap()
        {
            HubEvent::SocketClientConnected { client_id, .. } => client_id,
            other => panic!("Expected SocketClientConnected, got: {other:?}"),
        };

        // Drop stream to disconnect
        drop(stream);

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), hub_rx.recv())
            .await.expect("Timed out").expect("Channel closed");

        match event {
            HubEvent::SocketClientDisconnected { client_id } => {
                assert_eq!(client_id, connected_id);
            }
            other => panic!("Expected SocketClientDisconnected, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_multiple_clients_get_unique_ids() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sock_path = tmp.path().join("test.sock");
        let (hub_tx, mut hub_rx) = mpsc::unbounded_channel::<HubEvent>();

        let _server = SocketServer::start(sock_path.clone(), hub_tx).unwrap();

        let _s1 = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let _s2 = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let _s3 = tokio::net::UnixStream::connect(&sock_path).await.unwrap();

        let mut ids = Vec::new();
        for _ in 0..3 {
            let event = tokio::time::timeout(std::time::Duration::from_secs(2), hub_rx.recv())
                .await.expect("Timed out").expect("Channel closed");
            match event {
                HubEvent::SocketClientConnected { client_id, .. } => ids.push(client_id),
                other => panic!("Expected SocketClientConnected, got: {other:?}"),
            }
        }

        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), 3, "All client IDs should be unique, got: {ids:?}");
    }

    #[tokio::test]
    async fn test_unexpected_frame_type_from_client_ignored_not_fatal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sock_path = tmp.path().join("test.sock");
        let (hub_tx, mut hub_rx) = mpsc::unbounded_channel::<HubEvent>();

        let _server = SocketServer::start(sock_path.clone(), hub_tx).unwrap();
        let mut stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();

        // Consume connect event
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), hub_rx.recv())
            .await.unwrap().unwrap();

        // Send PtyOutput (hub→client only, unexpected from client)
        let bad = Frame::PtyOutput {
            agent_index: 0,
            pty_index: 0,
            data: b"bad".to_vec(),
        };
        stream.write_all(&bad.encode()).await.unwrap();

        // Then send a valid JSON frame
        let good = Frame::Json(serde_json::json!({"type": "ping"}));
        stream.write_all(&good.encode()).await.unwrap();

        // The valid frame should still arrive (bad one was ignored, not fatal)
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), hub_rx.recv())
            .await.expect("Timed out").expect("Channel closed");

        match event {
            HubEvent::SocketMessage { msg, .. } => assert_eq!(msg["type"], "ping"),
            other => panic!("Expected SocketMessage, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_server_sends_binary_frame_not_pty_output() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sock_path = tmp.path().join("test.sock");
        let (hub_tx, mut hub_rx) = mpsc::unbounded_channel::<HubEvent>();

        let _server = SocketServer::start(sock_path.clone(), hub_tx).unwrap();
        let mut stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();

        let conn = match tokio::time::timeout(std::time::Duration::from_secs(2), hub_rx.recv())
            .await.unwrap().unwrap()
        {
            HubEvent::SocketClientConnected { conn, .. } => conn,
            other => panic!("Expected SocketClientConnected, got: {other:?}"),
        };

        // Send Binary frame (not PtyOutput)
        conn.send_frame(&Frame::Binary(b"plugin payload".to_vec()));

        let mut buf = [0u8; 4096];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), stream.read(&mut buf))
            .await.expect("Timed out").expect("Read error");

        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&buf[..n]).unwrap();
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            Frame::Binary(data) => assert_eq!(data, b"plugin payload"),
            other => panic!("Expected Binary frame, got: {other:?}"),
        }

        conn.disconnect();
    }

    #[tokio::test]
    async fn test_socket_path_length_validation() {
        // Create a path that exceeds 104 bytes
        let tmp = tempfile::TempDir::new().unwrap();
        let long_name = "a".repeat(200);
        let sock_path = tmp.path().join(long_name).join("test.sock");

        let (hub_tx, _hub_rx) = mpsc::unbounded_channel::<HubEvent>();
        let result = SocketServer::start(sock_path, hub_tx);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("too long"), "Error should mention path too long: {err_msg}");
    }
}
