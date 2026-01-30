//! HTTP Proxy for Preview Channel.
//!
//! This module provides HTTP proxying functionality for the preview feature.
//! It forwards encrypted HTTP requests from the browser to the agent's
//! local dev server and returns encrypted responses.
//!
//! # Architecture
//!
//! ```text
//! Browser ── PreviewChannel ──> HttpProxy ──> localhost:PORT ──> Dev Server
//!                                   │
//!                                   └── Compression (>4KB responses)
//! ```
//!
//! # Security
//!
//! - All traffic is E2E encrypted (Signal Protocol)
//! - Rails server cannot inspect HTTP content
//! - Only proxies to localhost (agent's own server)

// Rust guideline compliant 2025-01

use base64::{engine::general_purpose::STANDARD, Engine};
use flate2::write::GzEncoder;
use flate2::Compression;
use reqwest::Client;
use std::collections::HashMap;
use std::io::Write;
use std::time::Duration;

use super::preview_types::{HttpRequest, HttpResponse, ProxyConfig, ProxyResult};

/// HTTP Proxy for forwarding requests to agent's dev server.
#[derive(Debug)]
pub struct HttpProxy {
    /// HTTP client for making requests.
    client: Client,
    /// Proxy configuration.
    config: ProxyConfig,
}

impl HttpProxy {
    /// Create a new HTTP proxy with default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(ProxyConfig::default())
    }

    /// Create a new HTTP proxy with custom configuration.
    #[must_use]
    pub fn with_config(config: ProxyConfig) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .redirect(reqwest::redirect::Policy::none()) // Don't follow redirects
            .build()
            .expect("Failed to create HTTP client");

        Self { client, config }
    }

    /// Update the target port.
    pub fn set_port(&mut self, port: u16) {
        self.config.port = port;
    }

    /// Get the current target port.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.config.port
    }

    /// Proxy an HTTP request to the local dev server.
    ///
    /// # Arguments
    ///
    /// * `request` - The HTTP request from the browser
    ///
    /// # Returns
    ///
    /// A `ProxyResult` containing the response or error.
    pub async fn proxy(&self, request: &HttpRequest) -> ProxyResult {
        // Validate request
        if request.method.is_empty() {
            return ProxyResult::BadRequest("Missing HTTP method".to_string());
        }

        // Build target URL
        let url = format!(
            "http://{}:{}{}",
            self.config.host, self.config.port, request.url
        );

        log::debug!(
            "[HttpProxy] {} {} -> {}:{}",
            request.method,
            request.url,
            self.config.host,
            self.config.port
        );

        // Build the request
        let method = match request.method.to_uppercase().as_str() {
            "GET" => reqwest::Method::GET,
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            "DELETE" => reqwest::Method::DELETE,
            "PATCH" => reqwest::Method::PATCH,
            "HEAD" => reqwest::Method::HEAD,
            "OPTIONS" => reqwest::Method::OPTIONS,
            other => {
                return ProxyResult::BadRequest(format!("Unsupported HTTP method: {other}"));
            }
        };

        let mut req_builder = self.client.request(method, &url);

        // Add headers
        for (key, value) in &request.headers {
            // Skip hop-by-hop headers
            if !is_hop_by_hop_header(key) {
                req_builder = req_builder.header(key, value);
            }
        }

        // Add body if present
        if let Some(body_b64) = &request.body {
            match STANDARD.decode(body_b64) {
                Ok(body) => {
                    if body.len() > self.config.max_body_size {
                        return ProxyResult::PayloadTooLarge;
                    }
                    req_builder = req_builder.body(body);
                }
                Err(e) => {
                    return ProxyResult::BadRequest(format!("Invalid body encoding: {e}"));
                }
            }
        }

        // Execute the request
        match req_builder.send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                let status_text = response
                    .status()
                    .canonical_reason()
                    .unwrap_or("")
                    .to_string();

                // Collect headers
                let mut headers = HashMap::new();
                for (key, value) in response.headers() {
                    if let Ok(v) = value.to_str() {
                        // Skip hop-by-hop headers
                        if !is_hop_by_hop_header(key.as_str()) {
                            headers.insert(key.as_str().to_string(), v.to_string());
                        }
                    }
                }

                // Read body
                match response.bytes().await {
                    Ok(body) => {
                        let (body_b64, compressed) = self.encode_body(&body, &headers);

                        // If we compressed the body, add Content-Encoding header
                        // so the browser knows to decompress it
                        let mut response_headers = headers;
                        if compressed {
                            response_headers.insert(
                                "content-encoding".to_string(),
                                "gzip".to_string(),
                            );
                        }

                        log::debug!(
                            "[HttpProxy] Response: {} {} ({} bytes, compressed={})",
                            status,
                            status_text,
                            body.len(),
                            compressed
                        );

                        ProxyResult::Success(HttpResponse {
                            request_id: request.request_id,
                            status,
                            status_text,
                            headers: response_headers,
                            body: body_b64,
                            compressed,
                        })
                    }
                    Err(e) => ProxyResult::ConnectionError(format!("Failed to read body: {e}")),
                }
            }
            Err(e) => {
                if e.is_timeout() {
                    ProxyResult::Timeout
                } else if e.is_connect() {
                    ProxyResult::ConnectionError(format!(
                        "Connection refused (server not running on port {}?)",
                        self.config.port
                    ))
                } else {
                    ProxyResult::ConnectionError(e.to_string())
                }
            }
        }
    }

    /// Encode response body, compressing if beneficial.
    ///
    /// Returns (base64_body, is_compressed).
    fn encode_body(
        &self,
        body: &[u8],
        headers: &HashMap<String, String>,
    ) -> (Option<String>, bool) {
        if body.is_empty() {
            return (None, false);
        }

        // Check if already compressed
        let already_compressed = headers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("content-encoding")
                && (v.contains("gzip") || v.contains("br") || v.contains("deflate"))
        });

        // Only compress if above threshold and not already compressed
        if !already_compressed && body.len() >= self.config.compression_threshold {
            if let Ok(compressed) = gzip_compress(body) {
                // Only use if actually smaller
                if compressed.len() < body.len() {
                    return (Some(STANDARD.encode(&compressed)), true);
                }
            }
        }

        // Return uncompressed
        (Some(STANDARD.encode(body)), false)
    }
}

impl Default for HttpProxy {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if a header is hop-by-hop (should not be forwarded).
fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// Gzip compress data.
fn gzip_compress(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(data)?;
    encoder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proxy_creation() {
        let proxy = HttpProxy::new();
        assert_eq!(proxy.config.port, 3000);
        assert_eq!(proxy.config.host, "127.0.0.1");
    }

    #[test]
    fn test_proxy_set_port() {
        let mut proxy = HttpProxy::new();
        proxy.set_port(8080);
        assert_eq!(proxy.port(), 8080);
    }

    #[test]
    fn test_is_hop_by_hop_header() {
        assert!(is_hop_by_hop_header("Connection"));
        assert!(is_hop_by_hop_header("transfer-encoding"));
        assert!(is_hop_by_hop_header("KEEP-ALIVE"));
        assert!(!is_hop_by_hop_header("Content-Type"));
        assert!(!is_hop_by_hop_header("Content-Length"));
    }

    #[test]
    fn test_gzip_compress() {
        let data = b"Hello, World! This is some test data for compression.";
        let compressed = gzip_compress(data).unwrap();
        assert!(!compressed.is_empty());
        // Gzip adds overhead for small data, but the function should still work
    }

    #[test]
    fn test_encode_body_empty() {
        let proxy = HttpProxy::new();
        let (body, compressed) = proxy.encode_body(&[], &HashMap::new());
        assert!(body.is_none());
        assert!(!compressed);
    }

    #[test]
    fn test_encode_body_small() {
        let proxy = HttpProxy::new();
        let data = b"small";
        let (body, compressed) = proxy.encode_body(data, &HashMap::new());
        assert!(body.is_some());
        assert!(!compressed); // Below threshold
        assert_eq!(body.unwrap(), STANDARD.encode(data));
    }

    #[test]
    fn test_encode_body_large() {
        let proxy = HttpProxy::with_config(ProxyConfig {
            compression_threshold: 10,
            ..Default::default()
        });

        // Create compressible data (repeating pattern)
        let data: Vec<u8> = (0..1000).map(|i| (i % 26) as u8 + b'a').collect();
        let (body, compressed) = proxy.encode_body(&data, &HashMap::new());

        assert!(body.is_some());
        assert!(compressed); // Above threshold and compressible
    }

    #[test]
    fn test_encode_body_already_compressed() {
        let proxy = HttpProxy::with_config(ProxyConfig {
            compression_threshold: 10,
            ..Default::default()
        });

        let data: Vec<u8> = (0..1000).map(|i| (i % 26) as u8 + b'a').collect();
        let mut headers = HashMap::new();
        headers.insert("Content-Encoding".to_string(), "gzip".to_string());

        let (body, compressed) = proxy.encode_body(&data, &headers);
        assert!(body.is_some());
        assert!(!compressed); // Should not double-compress
    }

    #[tokio::test]
    async fn test_proxy_invalid_method() {
        let proxy = HttpProxy::new();
        let request = HttpRequest {
            request_id: 1,
            method: String::new(),
            url: "/".to_string(),
            headers: HashMap::new(),
            body: None,
        };

        let result = proxy.proxy(&request).await;
        assert!(matches!(result, ProxyResult::BadRequest(_)));
    }

    #[tokio::test]
    async fn test_proxy_unsupported_method() {
        let proxy = HttpProxy::new();
        let request = HttpRequest {
            request_id: 1,
            method: "INVALID".to_string(),
            url: "/".to_string(),
            headers: HashMap::new(),
            body: None,
        };

        let result = proxy.proxy(&request).await;
        assert!(matches!(result, ProxyResult::BadRequest(_)));
    }

    #[tokio::test]
    async fn test_proxy_connection_refused() {
        // Use a port that's unlikely to be in use
        let proxy = HttpProxy::with_config(ProxyConfig {
            port: 59999,
            timeout_secs: 1,
            ..Default::default()
        });

        let request = HttpRequest {
            request_id: 1,
            method: "GET".to_string(),
            url: "/".to_string(),
            headers: HashMap::new(),
            body: None,
        };

        let result = proxy.proxy(&request).await;
        assert!(matches!(result, ProxyResult::ConnectionError(_)));
    }
}
