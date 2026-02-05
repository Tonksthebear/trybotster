//! Data types for the preview HTTP tunnel protocol.
//!
//! This module defines the message types used for E2E encrypted HTTP
//! proxying between the browser and agent's dev server.
//!
//! # Message Flow
//!
//! ```text
//! Browser                    CLI                        Dev Server
//!    |                        |                              |
//!    |-- HttpRequest -------->|                              |
//!    |                        |-- localhost:PORT ----------->|
//!    |                        |<-- HTTP Response ------------|
//!    |<-- HttpResponse -------|                              |
//! ```
//!
//! # Encryption
//!
//! All messages are encrypted using Signal Protocol (same session as terminal).
//! The Rails server cannot inspect HTTP content.
//!
//! # Compression
//!
//! Response bodies >4KB are gzip compressed before encryption.

// Rust guideline compliant 2026-02

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Message types for preview channel (CLI -> Browser).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PreviewMessage {
    /// CLI is ready to receive HTTP requests.
    ///
    /// Sent after the WebRTC preview channel is established. The browser
    /// should wait for this before sending requests to avoid message loss.
    #[serde(rename = "preview_ready")]
    Ready,
    /// HTTP response from agent's dev server.
    #[serde(rename = "http_response")]
    HttpResponse(HttpResponse),
    /// Error occurred while processing request.
    #[serde(rename = "preview_error")]
    Error {
        /// Request ID this error relates to.
        request_id: u64,
        /// Error message.
        error: String,
    },
    /// Preview server status update.
    #[serde(rename = "preview_status")]
    Status {
        /// Whether the dev server is running.
        server_running: bool,
        /// Port the dev server is listening on.
        port: Option<u16>,
    },
}

/// HTTP response message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpResponse {
    /// Request ID for correlation.
    pub request_id: u64,
    /// HTTP status code.
    pub status: u16,
    /// HTTP status text.
    #[serde(default)]
    pub status_text: String,
    /// Response headers.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Response body (base64 encoded, possibly gzip compressed).
    #[serde(default)]
    pub body: Option<String>,
    /// Whether body is gzip compressed.
    #[serde(default)]
    pub compressed: bool,
}

/// Browser command types for preview channel (Browser -> CLI).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PreviewCommand {
    /// HTTP request to proxy to dev server.
    #[serde(rename = "http_request")]
    HttpRequest(HttpRequest),
    /// Request server status.
    #[serde(rename = "get_status")]
    GetStatus,
}

/// HTTP request message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequest {
    /// Unique request ID for response correlation.
    pub request_id: u64,
    /// HTTP method (GET, POST, etc.).
    pub method: String,
    /// Request URL (path + query string).
    pub url: String,
    /// Request headers.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Request body (base64 encoded).
    #[serde(default)]
    pub body: Option<String>,
}

/// Events received from the browser via the preview channel.
///
/// These are parsed from [`PreviewCommand`]s for consumption by the Hub.
#[derive(Debug, Clone)]
pub enum PreviewEvent {
    /// HTTP request to proxy.
    HttpRequest {
        /// Browser's identity key (for routing response).
        browser_identity: String,
        /// The HTTP request.
        request: HttpRequest,
    },
    /// Status request.
    GetStatus {
        /// Browser's identity key.
        browser_identity: String,
    },
}

/// Result of proxying an HTTP request.
#[derive(Debug)]
pub enum ProxyResult {
    /// Successfully proxied, got response.
    Success(HttpResponse),
    /// Connection error (server not running, refused, etc.).
    ConnectionError(String),
    /// Timeout waiting for response.
    Timeout,
    /// Request was too large.
    PayloadTooLarge,
    /// Invalid request format.
    BadRequest(String),
}

impl ProxyResult {
    /// Convert to a PreviewMessage for sending to browser.
    #[must_use]
    pub fn into_message(self, request_id: u64) -> PreviewMessage {
        match self {
            Self::Success(response) => PreviewMessage::HttpResponse(response),
            Self::ConnectionError(msg) => PreviewMessage::Error {
                request_id,
                error: format!("Connection error: {msg}"),
            },
            Self::Timeout => PreviewMessage::Error {
                request_id,
                error: "Request timeout".to_string(),
            },
            Self::PayloadTooLarge => PreviewMessage::Error {
                request_id,
                error: "Payload too large".to_string(),
            },
            Self::BadRequest(msg) => PreviewMessage::Error {
                request_id,
                error: format!("Bad request: {msg}"),
            },
        }
    }
}

/// Configuration for the HTTP proxy.
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// Target host (usually localhost).
    pub host: String,
    /// Target port (agent's dev server port).
    pub port: u16,
    /// Request timeout in seconds.
    pub timeout_secs: u64,
    /// Maximum request body size in bytes.
    pub max_body_size: usize,
    /// Compression threshold in bytes.
    pub compression_threshold: usize,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 3000,
            timeout_secs: 30,
            max_body_size: 10 * 1024 * 1024, // 10MB
            compression_threshold: 4096,     // 4KB
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_request_serialization() {
        let req = HttpRequest {
            request_id: 42,
            method: "GET".to_string(),
            url: "/api/status".to_string(),
            headers: HashMap::from([("Accept".to_string(), "application/json".to_string())]),
            body: None,
        };

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""request_id":42"#));
        assert!(json.contains(r#""method":"GET""#));
        assert!(json.contains(r#""url":"/api/status""#));
    }

    #[test]
    fn test_http_response_serialization() {
        let resp = HttpResponse {
            request_id: 42,
            status: 200,
            status_text: "OK".to_string(),
            headers: HashMap::from([("Content-Type".to_string(), "application/json".to_string())]),
            body: Some("eyJzdGF0dXMiOiJvayJ9".to_string()), // base64 for {"status":"ok"}
            compressed: false,
        };

        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""request_id":42"#));
        assert!(json.contains(r#""status":200"#));
        assert!(json.contains(r#""status_text":"OK""#));
    }

    #[test]
    fn test_preview_command_http_request_parsing() {
        let json = r#"{
            "type": "http_request",
            "request_id": 1,
            "method": "POST",
            "url": "/users",
            "headers": {"Content-Type": "application/json"},
            "body": "eyJuYW1lIjoiSm9obiJ9"
        }"#;

        let cmd: PreviewCommand = serde_json::from_str(json).unwrap();
        match cmd {
            PreviewCommand::HttpRequest(req) => {
                assert_eq!(req.request_id, 1);
                assert_eq!(req.method, "POST");
                assert_eq!(req.url, "/users");
                assert!(req.body.is_some());
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_preview_message_http_response_serialization() {
        let msg = PreviewMessage::HttpResponse(HttpResponse {
            request_id: 1,
            status: 201,
            status_text: "Created".to_string(),
            headers: HashMap::new(),
            body: None,
            compressed: false,
        });

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"http_response""#));
        assert!(json.contains(r#""status":201"#));
    }

    #[test]
    fn test_preview_message_error_serialization() {
        let msg = PreviewMessage::Error {
            request_id: 42,
            error: "Server not running".to_string(),
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"preview_error""#));
        assert!(json.contains(r#""request_id":42"#));
        assert!(json.contains(r#""error":"Server not running""#));
    }

    #[test]
    fn test_preview_message_ready_serialization() {
        let msg = PreviewMessage::Ready;
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"preview_ready""#));
    }

    #[test]
    fn test_proxy_result_into_message() {
        let success = ProxyResult::Success(HttpResponse {
            request_id: 1,
            status: 200,
            status_text: "OK".to_string(),
            headers: HashMap::new(),
            body: None,
            compressed: false,
        });

        let msg = success.into_message(1);
        assert!(matches!(msg, PreviewMessage::HttpResponse(_)));

        let error = ProxyResult::ConnectionError("refused".to_string());
        let msg = error.into_message(2);
        match msg {
            PreviewMessage::Error { request_id, error } => {
                assert_eq!(request_id, 2);
                assert!(error.contains("refused"));
            }
            _ => panic!("Wrong variant"),
        }
    }
}
