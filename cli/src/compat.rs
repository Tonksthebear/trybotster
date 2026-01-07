//! Compatibility types for browser integration.
//!
//! These types are used for browser terminal rendering and status display.
//!
//! Rust guideline compliant 2025-01

/// Browser terminal dimensions
#[derive(Debug, Clone, Default)]
pub struct BrowserDimensions {
    /// Terminal width in columns.
    pub cols: u16,
    /// Terminal height in rows.
    pub rows: u16,
    /// Current display mode (GUI or TUI).
    pub mode: BrowserMode,
}

/// Browser operating mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BrowserMode {
    /// Full graphical mode with direct terminal rendering.
    #[default]
    Gui,
    /// Text-based mode with TUI-style layout.
    Tui,
}

/// VPN connection status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VpnStatus {
    /// Not connected to VPN.
    #[default]
    Disconnected,
    /// Establishing VPN connection.
    Connecting,
    /// VPN connection active.
    Connected,
    /// VPN connection error.
    Error,
}
