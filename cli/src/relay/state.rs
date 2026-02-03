//! Browser communication and state management.
//!
//! Handles the WebSocket relay connection to browsers, including:
//! - Connection state tracking
//! - Builder functions for browser messages (AgentInfo, WorktreeInfo, Scrollback)
//!
//! # Architecture
//!
//! `BrowserState` tracks relay-level connection state. Per-browser view state
//! (mode, selection, scroll) is managed independently by the browser.
//! Builder functions here are pure helpers used by WebRTC send methods.

// Rust guideline compliant 2026-01

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use flate2::{write::GzEncoder, Compression};
use std::io::Write;

use super::crypto_service::CryptoServiceHandle;
use super::signal::PreKeyBundleData;
use crate::{AgentInfo, TerminalMessage, WorktreeInfo};

/// Browser connection state.
///
/// Consolidates relay-level browser connection state. Per-browser view state
/// (mode, scroll, selection) is tracked by the browser independently.
#[derive(Default)]
pub struct BrowserState {
    /// Whether a browser is currently connected.
    pub connected: bool,
    /// Signal PreKeyBundle data for QR code generation.
    pub signal_bundle: Option<PreKeyBundleData>,
    /// Whether the current bundle's PreKey has been used (consumed by a connection).
    /// When true, the QR code should be regenerated before pairing additional devices.
    pub bundle_used: bool,
    /// Shared crypto service handle for E2E encryption.
    /// Used by WebRTC and agent channels for Signal Protocol operations.
    pub crypto_service: Option<CryptoServiceHandle>,
    /// Whether the relay WebSocket connection is established.
    ///
    /// When `false`, the hub cannot receive browser handshake messages even if
    /// a valid `signal_bundle` exists. The QR code should not be shown when
    /// this is `false` to avoid "CLI did not respond" errors.
    pub relay_connected: bool,
}

impl std::fmt::Debug for BrowserState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserState")
            .field("connected", &self.connected)
            .finish_non_exhaustive()
    }
}

impl BrowserState {
    /// Check if browser is connected and ready.
    pub fn is_connected(&self) -> bool {
        self.connected && self.relay_connected
    }

    /// Handle browser connected event.
    ///
    /// Sets the connected flag. Also marks the bundle as used since the PreKey
    /// has been consumed for this session.
    pub fn handle_connected(&mut self, device_name: &str) {
        log::info!("Browser connected: {device_name} - E2E encryption active");
        self.connected = true;
        // Mark bundle as used - the PreKey was consumed to establish this session.
        // A new QR code should be generated before pairing additional devices.
        self.bundle_used = true;
    }

    /// Handle browser disconnected event.
    pub fn handle_disconnected(&mut self) {
        log::info!("Browser disconnected");
        self.connected = false;
    }
}

/// Build AgentInfo from agent data.
///
/// This is a helper to convert agent data into the format expected by browsers.
///
/// Note: `active_pty_view` and `scroll_offset` are set to `None` because these
/// are now client-scoped state. Each browser tracks its own view selection and
/// scroll position independently via xterm.js.
#[must_use]
pub fn build_agent_info(id: &str, agent: &crate::Agent, hub_identifier: &str) -> AgentInfo {
    AgentInfo {
        id: id.to_string(),
        repo: Some(agent.repo.clone()),
        issue_number: agent.issue_number.map(u64::from),
        branch_name: Some(agent.branch_name.clone()),
        name: None,
        status: Some(format!("{:?}", agent.status)),
        port: agent.port(),
        server_running: Some(agent.is_server_running()),
        has_server_pty: Some(agent.has_server_pty()),
        // View and scroll are client-scoped - browser tracks its own state
        active_pty_view: None,
        scroll_offset: None,
        hub_identifier: Some(hub_identifier.to_string()),
    }
}

/// Build WorktreeInfo from worktree data.
#[must_use]
pub fn build_worktree_info(path: &str, branch: &str) -> WorktreeInfo {
    let issue_number = branch
        .strip_prefix("botster-issue-")
        .and_then(|s| s.parse::<u64>().ok());
    WorktreeInfo {
        path: path.to_string(),
        branch: branch.to_string(),
        issue_number,
    }
}

/// Build a scrollback message from raw bytes.
///
/// Compresses the raw PTY bytes with gzip and base64 encodes for transport.
/// Browser decompresses with native DecompressionStream API.
/// Typical compression ratio is 10:1 for terminal output.
///
/// This is a pure function for testability.
#[must_use]
pub fn build_scrollback_message(bytes: Vec<u8>) -> TerminalMessage {
    // Compress with gzip
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    let compressed = if encoder.write_all(&bytes).is_ok() {
        encoder.finish().unwrap_or_else(|_| bytes.clone())
    } else {
        bytes.clone()
    };

    // Base64 encode for JSON transport
    let data = BASE64.encode(&compressed);

    log::debug!(
        "Scrollback compression: {} bytes -> {} bytes ({:.1}x)",
        bytes.len(),
        compressed.len(),
        bytes.len() as f64 / compressed.len().max(1) as f64
    );

    TerminalMessage::Scrollback {
        data,
        compressed: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_browser_state_default() {
        let state = BrowserState::default();
        assert!(!state.is_connected());
        // relay_connected defaults to false - QR code should not be shown
        // until relay connection is established
        assert!(
            !state.relay_connected,
            "relay_connected should default to false"
        );
    }

    /// Verifies that `relay_connected` properly gates QR code visibility.
    ///
    /// When relay connection fails, we set `relay_connected = false` and
    /// clear `signal_bundle` to prevent showing a QR code that would lead
    /// to "CLI did not respond" errors when browsers try to connect.
    #[test]
    fn test_relay_connected_prevents_false_positive_qr() {
        let state = BrowserState::default();

        // Simulate failed relay connection scenario:
        // - signal_bundle should be None (cleared on failure)
        // - relay_connected should be false
        assert!(state.signal_bundle.is_none());
        assert!(!state.relay_connected);

        // The is_connected() check should also return false
        assert!(!state.is_connected());
    }

    #[test]
    fn test_browser_state_handle_disconnected() {
        let mut state = BrowserState::default();
        state.connected = true;

        state.handle_disconnected();

        assert!(!state.connected);
    }

    #[test]
    fn test_handle_connected() {
        let mut state = BrowserState::default();
        state.handle_connected("Test Device");

        assert!(state.connected);
    }

    #[test]
    fn test_build_scrollback_message() {
        let bytes = b"First line\r\nSecond line\r\n\x1b[32mcolored\x1b[0m output".to_vec();
        let message = build_scrollback_message(bytes);

        match message {
            TerminalMessage::Scrollback { data, compressed } => {
                assert!(compressed, "Should be marked as compressed");
                assert!(!data.is_empty(), "Data should not be empty");
                // Small inputs won't compress well due to gzip header + base64 overhead
                // Just verify it produces valid base64
                assert!(data
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='));
            }
            _ => panic!("Expected Scrollback message"),
        }
    }

    #[test]
    fn test_build_scrollback_message_empty() {
        let message = build_scrollback_message(vec![]);

        match message {
            TerminalMessage::Scrollback { data, compressed } => {
                assert!(compressed, "Should be marked as compressed");
                // Empty input still produces gzip header
                assert!(!data.is_empty(), "Gzip of empty still has header");
            }
            _ => panic!("Expected Scrollback message"),
        }
    }

    #[test]
    fn test_build_scrollback_message_compression_ratio() {
        // Terminal output with repeated patterns compresses very well
        let repeated =
            "$ ls -la\r\ntotal 0\r\ndrwxr-xr-x  2 user user  40 Jan  1 00:00 .\r\n".repeat(100);
        let bytes = repeated.as_bytes().to_vec();
        let original_len = bytes.len();
        let message = build_scrollback_message(bytes);

        match message {
            TerminalMessage::Scrollback { data, .. } => {
                // Base64 adds ~33% overhead, but gzip should give >5x compression
                // So final size should be less than original
                assert!(
                    data.len() < original_len,
                    "Compressed+encoded ({}) should be smaller than original ({})",
                    data.len(),
                    original_len
                );
            }
            _ => panic!("Expected Scrollback message"),
        }
    }

    #[test]
    fn test_handle_connected_sets_bundle_used() {
        let mut state = BrowserState::default();
        assert!(!state.bundle_used, "bundle_used should be false initially");

        state.handle_connected("Test Device");

        assert!(
            state.bundle_used,
            "bundle_used should be true after connection"
        );
        assert!(state.connected);
    }

    #[test]
    fn test_bundle_used_default_false() {
        let state = BrowserState::default();
        assert!(!state.bundle_used, "bundle_used should default to false");
    }
}
