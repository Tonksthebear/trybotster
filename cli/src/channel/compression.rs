//! Gzip compression for channel payloads.
//!
//! This module provides compression and decompression utilities with a
//! simple wire format using marker bytes:
//!
//! - `0x00` prefix: uncompressed data follows
//! - `0x1f` prefix: gzip-compressed data follows (0x1f is gzip magic byte)
//!
//! # Usage
//!
//! ```ignore
//! // Compress if over threshold
//! let compressed = maybe_compress(data, Some(4096))?;
//!
//! // Decompress (auto-detects format)
//! let decompressed = maybe_decompress(&compressed)?;
//! ```
//!
//! Rust guideline compliant 2025-01

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::io::{Read, Write};

use super::ChannelError;

/// Marker byte for uncompressed data.
const MARKER_UNCOMPRESSED: u8 = 0x00;

/// Marker byte for gzip-compressed data (also gzip magic byte).
const MARKER_GZIP: u8 = 0x1f;

/// Compress data if it exceeds the threshold.
///
/// Returns data with a marker byte prefix indicating compression status.
///
/// # Arguments
///
/// * `data` - The data to potentially compress
/// * `threshold` - Minimum size to trigger compression. `None` disables compression.
///
/// # Returns
///
/// Data prefixed with marker byte:
/// - `[0x00, ...data]` if uncompressed
/// - `[0x1f, ...gzip_data]` if compressed
///
/// If compression doesn't reduce size, returns uncompressed with `0x00` marker.
///
/// # Errors
///
/// Returns `ChannelError::CompressionError` if gzip encoding fails.
pub fn maybe_compress(data: &[u8], threshold: Option<usize>) -> Result<Vec<u8>, ChannelError> {
    let threshold = match threshold {
        Some(t) => t,
        None => {
            // Compression disabled - just add marker
            let mut result = Vec::with_capacity(1 + data.len());
            result.push(MARKER_UNCOMPRESSED);
            result.extend_from_slice(data);
            return Ok(result);
        }
    };

    if data.len() < threshold {
        // Below threshold - don't compress
        let mut result = Vec::with_capacity(1 + data.len());
        result.push(MARKER_UNCOMPRESSED);
        result.extend_from_slice(data);
        return Ok(result);
    }

    // Try to compress
    let mut compressed = Vec::with_capacity(data.len());
    compressed.push(MARKER_GZIP);

    {
        let mut encoder = GzEncoder::new(&mut compressed, Compression::fast());
        encoder
            .write_all(data)
            .map_err(|e| ChannelError::CompressionError(format!("gzip write failed: {e}")))?;
        encoder
            .finish()
            .map_err(|e| ChannelError::CompressionError(format!("gzip finish failed: {e}")))?;
    }

    // Only use compressed if actually smaller
    if compressed.len() < data.len() + 1 {
        Ok(compressed)
    } else {
        // Compression didn't help - return uncompressed
        let mut result = Vec::with_capacity(1 + data.len());
        result.push(MARKER_UNCOMPRESSED);
        result.extend_from_slice(data);
        Ok(result)
    }
}

/// Decompress data based on marker byte.
///
/// # Arguments
///
/// * `data` - Data with marker byte prefix
///
/// # Returns
///
/// Decompressed data (without marker byte).
///
/// # Errors
///
/// Returns `ChannelError::CompressionError` if:
/// - Data is empty
/// - Unknown marker byte
/// - Gzip decompression fails
pub fn maybe_decompress(data: &[u8]) -> Result<Vec<u8>, ChannelError> {
    if data.is_empty() {
        return Ok(vec![]);
    }

    match data[0] {
        MARKER_UNCOMPRESSED => {
            // Uncompressed - just strip marker
            Ok(data[1..].to_vec())
        }
        MARKER_GZIP => {
            // Gzip compressed - decompress
            let mut decoder = GzDecoder::new(&data[1..]);
            let mut decompressed = Vec::new();
            decoder.read_to_end(&mut decompressed).map_err(|e| {
                ChannelError::CompressionError(format!("gzip decompress failed: {e}"))
            })?;
            Ok(decompressed)
        }
        _ => {
            // No recognized marker - treat as raw uncompressed data (e.g., JSON from browser).
            // This provides backwards compatibility with clients that don't add markers.
            Ok(data.to_vec())
        }
    }
}

/// Check if an HTTP response should be compressed.
///
/// Returns `false` if:
/// - Body is smaller than threshold
/// - Body is already compressed (has Content-Encoding header)
///
/// # Arguments
///
/// * `body` - Response body bytes
/// * `headers` - Response headers as (name, value) pairs
/// * `threshold` - Minimum body size to consider compression
pub fn should_compress_response(body: &[u8], headers: &[(String, String)], threshold: usize) -> bool {
    if body.len() < threshold {
        return false;
    }

    // Check if already compressed
    let already_compressed = headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("content-encoding")
            && (value.contains("gzip")
                || value.contains("br")
                || value.contains("deflate")
                || value.contains("zstd"))
    });

    !already_compressed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip_uncompressed() {
        let data = b"hello world";
        let compressed = maybe_compress(data, Some(1000)).expect("compress");

        // Should be uncompressed (below threshold)
        assert_eq!(compressed[0], MARKER_UNCOMPRESSED);

        let decompressed = maybe_decompress(&compressed).expect("decompress");
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_roundtrip_compressed() {
        // Create data that compresses well (repetitive)
        let data: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();
        let compressed = maybe_compress(&data, Some(100)).expect("compress");

        // Should be compressed (above threshold)
        assert_eq!(compressed[0], MARKER_GZIP);
        assert!(compressed.len() < data.len());

        let decompressed = maybe_decompress(&compressed).expect("decompress");
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_compression_disabled() {
        let data = b"hello world this is a longer string";
        let compressed = maybe_compress(data, None).expect("compress");

        // Should be uncompressed (disabled)
        assert_eq!(compressed[0], MARKER_UNCOMPRESSED);
        assert_eq!(&compressed[1..], data.as_slice());
    }

    #[test]
    fn test_incompressible_data() {
        // Random data doesn't compress well
        let data: Vec<u8> = (0..1000).map(|_| rand::random::<u8>()).collect();
        let compressed = maybe_compress(&data, Some(100)).expect("compress");

        // May or may not compress - but should roundtrip
        let decompressed = maybe_decompress(&compressed).expect("decompress");
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_empty_data() {
        let decompressed = maybe_decompress(&[]).expect("decompress");
        assert!(decompressed.is_empty());
    }

    #[test]
    fn test_unknown_marker_passthrough() {
        // Unknown markers should be treated as raw data (pass-through)
        let data = vec![0x42, 0x01, 0x02, 0x03];
        let result = maybe_decompress(&data).expect("should pass through");
        assert_eq!(result, data); // Raw data returned as-is
    }

    #[test]
    fn test_json_passthrough() {
        // JSON data (starting with '{') should pass through unchanged
        let json_data = br#"{"type":"connected","device_name":"Chrome"}"#;
        let result = maybe_decompress(json_data).expect("should pass through");
        assert_eq!(result, json_data);
    }

    #[test]
    fn test_should_compress_response_small() {
        let body = b"small";
        let headers = vec![];
        assert!(!should_compress_response(body, &headers, 1000));
    }

    #[test]
    fn test_should_compress_response_large() {
        let body = vec![0u8; 5000];
        let headers = vec![];
        assert!(should_compress_response(&body, &headers, 1000));
    }

    #[test]
    fn test_should_compress_response_already_gzipped() {
        let body = vec![0u8; 5000];
        let headers = vec![("Content-Encoding".to_string(), "gzip".to_string())];
        assert!(!should_compress_response(&body, &headers, 1000));
    }

    #[test]
    fn test_should_compress_response_already_brotli() {
        let body = vec![0u8; 5000];
        let headers = vec![("content-encoding".to_string(), "br".to_string())];
        assert!(!should_compress_response(&body, &headers, 1000));
    }
}
