//! Shared WebSocket transport.
//!
//! Thin wrapper around `tokio-tungstenite` providing type-isolated
//! reader/writer halves. All WebSocket consumers in the crate should
//! use this module rather than `tokio-tungstenite` directly.
//!
//! # Architecture
//!
//! A single [`connect`] function handles URL→request building, header
//! insertion, and TLS negotiation. It returns a ([`WsWriter`], [`WsReader`])
//! pair ready for use in `tokio::select!` loops.
//!
//! By centralizing the connection logic, future enhancements (TLS config,
//! proxy support, metrics, timeouts) automatically apply to all consumers.

// Rust guideline compliant 2026-02

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite;

/// Concrete WebSocket stream type (avoids repeating the 6-line generic everywhere).
type WsStream = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

/// Received WebSocket message.
#[derive(Debug)]
pub enum WsMessage {
    /// UTF-8 text frame.
    Text(String),
    /// Binary frame.
    Binary(Vec<u8>),
    /// Ping frame with payload.
    Ping(Vec<u8>),
    /// Pong frame with payload.
    Pong(Vec<u8>),
    /// Close frame with status code and reason.
    Close {
        /// WebSocket close code (1000 = normal, 1005 = no code).
        code: u16,
        /// Human-readable close reason.
        reason: String,
    },
}

/// Write half of a WebSocket connection.
#[derive(Debug)]
pub struct WsWriter {
    sink: futures_util::stream::SplitSink<WsStream, tungstenite::Message>,
}

impl WsWriter {
    /// Send a UTF-8 text frame.
    ///
    /// # Errors
    ///
    /// Returns an error if the send fails (connection closed, I/O error).
    pub async fn send_text(&mut self, text: &str) -> Result<()> {
        self.sink
            .send(tungstenite::Message::Text(text.to_string()))
            .await
            .context("WebSocket send_text failed")
    }

    /// Send a pong frame in response to a ping.
    ///
    /// # Errors
    ///
    /// Returns an error if the send fails.
    pub async fn send_pong(&mut self, data: Vec<u8>) -> Result<()> {
        self.sink
            .send(tungstenite::Message::Pong(data))
            .await
            .context("WebSocket send_pong failed")
    }

    /// Send a close frame.
    ///
    /// # Errors
    ///
    /// Returns an error if the send fails.
    pub async fn send_close(&mut self) -> Result<()> {
        self.sink
            .send(tungstenite::Message::Close(None))
            .await
            .context("WebSocket send_close failed")
    }

    /// Flush pending writes and close the sink.
    ///
    /// # Errors
    ///
    /// Returns an error if closing fails.
    pub async fn close(&mut self) -> Result<()> {
        self.sink.close().await.context("WebSocket close failed")
    }
}

/// Read half of a WebSocket connection.
#[derive(Debug)]
pub struct WsReader {
    stream: futures_util::stream::SplitStream<WsStream>,
}

impl WsReader {
    /// Receive the next message, returning `None` when the stream ends.
    ///
    /// Raw `Frame` variants are skipped internally.
    pub async fn recv(&mut self) -> Option<Result<WsMessage>> {
        loop {
            match self.stream.next().await {
                Some(Ok(tungstenite::Message::Text(text))) => {
                    return Some(Ok(WsMessage::Text(text.to_string())));
                }
                Some(Ok(tungstenite::Message::Binary(data))) => {
                    return Some(Ok(WsMessage::Binary(data.to_vec())));
                }
                Some(Ok(tungstenite::Message::Ping(data))) => {
                    return Some(Ok(WsMessage::Ping(data.to_vec())));
                }
                Some(Ok(tungstenite::Message::Pong(data))) => {
                    return Some(Ok(WsMessage::Pong(data.to_vec())));
                }
                Some(Ok(tungstenite::Message::Close(close_frame))) => {
                    let (code, reason) = close_frame
                        .map(|cf| (cf.code.into(), cf.reason.to_string()))
                        .unwrap_or((1005, String::new()));
                    return Some(Ok(WsMessage::Close { code, reason }));
                }
                Some(Ok(tungstenite::Message::Frame(_))) => {
                    // Raw frames — skip
                    continue;
                }
                Some(Err(e)) => {
                    return Some(Err(anyhow::anyhow!("WebSocket read error: {e}")));
                }
                None => return None,
            }
        }
    }
}

/// Connect to a WebSocket URL with optional headers.
///
/// Builds an HTTP request from `url`, inserts each `(name, value)` header,
/// then performs the WebSocket handshake. Returns split (writer, reader)
/// halves for independent use in `tokio::select!` loops.
///
/// # Errors
///
/// Returns an error if the URL is invalid, header values are malformed,
/// or the WebSocket handshake fails.
pub async fn connect(url: &str, headers: &[(&str, &str)]) -> Result<(WsWriter, WsReader)> {
    use tungstenite::client::IntoClientRequest;

    let mut request = url
        .into_client_request()
        .with_context(|| format!("invalid WebSocket URL: {url}"))?;

    for &(name, value) in headers {
        let header_name = tungstenite::http::HeaderName::from_bytes(name.as_bytes())
            .with_context(|| format!("invalid header name: {name}"))?;
        let header_value = tungstenite::http::HeaderValue::from_str(value)
            .with_context(|| format!("invalid header value for {name}"))?;
        request.headers_mut().insert(header_name, header_value);
    }

    let (ws_stream, _response) = tokio_tungstenite::connect_async(request)
        .await
        .context("WebSocket connect failed")?;

    let (sink, stream) = ws_stream.split();

    Ok((WsWriter { sink }, WsReader { stream }))
}

/// Convert an HTTP(S) URL to WS(S) scheme.
///
/// Passes `ws://` and `wss://` through unchanged.
#[must_use]
pub fn http_to_ws_scheme(url: &str) -> String {
    if url.starts_with("wss://") || url.starts_with("ws://") {
        url.to_string()
    } else {
        url.replace("https://", "wss://")
            .replace("http://", "ws://")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_to_ws_scheme_https() {
        assert_eq!(
            http_to_ws_scheme("https://example.com"),
            "wss://example.com"
        );
    }

    #[test]
    fn test_http_to_ws_scheme_http() {
        assert_eq!(
            http_to_ws_scheme("http://localhost:3000"),
            "ws://localhost:3000"
        );
    }

    #[test]
    fn test_http_to_ws_scheme_wss_passthrough() {
        assert_eq!(
            http_to_ws_scheme("wss://example.com/cable"),
            "wss://example.com/cable"
        );
    }

    #[test]
    fn test_http_to_ws_scheme_ws_passthrough() {
        assert_eq!(
            http_to_ws_scheme("ws://localhost:3000/cable"),
            "ws://localhost:3000/cable"
        );
    }

    #[test]
    fn test_http_to_ws_scheme_with_path() {
        assert_eq!(
            http_to_ws_scheme("https://example.com/api/v1"),
            "wss://example.com/api/v1"
        );
    }

    #[tokio::test]
    async fn test_connect_invalid_url_returns_error() {
        let result = connect("not-a-url", &[]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_connect_unreachable_host_returns_error() {
        let result = connect("wss://127.0.0.1:1/invalid", &[]).await;
        assert!(result.is_err());
    }
}
