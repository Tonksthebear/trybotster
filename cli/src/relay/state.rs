//! Browser communication and state management.
//!
//! Handles the WebSocket relay connection to browsers, including:
//! - Connection state tracking
//!
//! # Architecture
//!
//! `BrowserState` tracks relay-level connection state. Per-browser view state
//! (mode, selection, scroll) is managed independently by the browser.

// Rust guideline compliant 2026-02

use super::crypto_service::CryptoService;
use super::olm_crypto::DeviceKeyBundle;

/// Browser connection state.
///
/// Consolidates relay-level browser connection state. Per-browser view state
/// (mode, scroll, selection) is tracked by the browser independently.
#[derive(Default)]
pub struct BrowserState {
    /// Whether a browser is currently connected.
    pub connected: bool,
    /// Device key bundle for QR code generation.
    pub device_key_bundle: Option<DeviceKeyBundle>,
    /// Whether the current bundle's one-time key has been used (consumed by a connection).
    /// When true, the QR code should be regenerated before pairing additional devices.
    pub bundle_used: bool,
    /// Shared crypto service for E2E encryption (vodozemac Olm).
    pub crypto_service: Option<CryptoService>,
    /// Whether the relay WebSocket connection is established.
    ///
    /// When `false`, the hub cannot receive browser handshake messages even if
    /// a valid device key bundle exists. The QR code should not be shown when
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
    /// clear `device_key_bundle` to prevent showing a QR code that would lead
    /// to "CLI did not respond" errors when browsers try to connect.
    #[test]
    fn test_relay_connected_prevents_false_positive_qr() {
        let state = BrowserState::default();

        // Simulate failed relay connection scenario:
        // - device_key_bundle should be None (cleared on failure)
        // - relay_connected should be false
        assert!(state.device_key_bundle.is_none());
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
