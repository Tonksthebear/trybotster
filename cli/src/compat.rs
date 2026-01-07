//! Compatibility types for browser integration.
//!
//! These types are used for browser terminal rendering and status display.
//!
//! Rust guideline compliant 2025-01

/// Browser terminal dimensions
#[derive(Debug, Clone, Default)]
pub struct BrowserDimensions {
    pub cols: u16,
    pub rows: u16,
    pub mode: BrowserMode,
}

/// Browser operating mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BrowserMode {
    #[default]
    Gui,
    Tui,
}

/// VPN connection status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VpnStatus {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Error,
}
