//! Compatibility types for browser integration.
//!
//! These types are used for browser terminal rendering and status display.
//! They will be used by the Action Cable terminal relay for sending
//! agent information to the browser.
//!
//! Rust guideline compliant 2025-01-05

use serde::{Deserialize, Serialize};

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

/// Agent info for browser display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebAgentInfo {
    pub id: String,
    pub session_key: String,
    pub repo: String,
    pub issue_number: Option<u64>,
    pub branch_name: Option<String>,
    pub worktree_path: String,
    pub status: String,
    pub selected: bool,
    pub hub_identifier: String,
    pub tunnel_status: String,
    pub tunnel_port: Option<u16>,
    pub last_invocation_url: Option<String>,
    pub server_running: bool,
    pub has_server_pty: bool,
    pub active_pty_view: String,
    pub scroll_offset: usize,
}

/// Worktree info for browser display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebWorktreeInfo {
    pub path: String,
    pub branch: String,
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
