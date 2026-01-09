//! Relay - Browser connectivity via Tailscale mesh.
//!
//! This module provides browser relay functionality via Tailscale/Headscale
//! mesh networking. Browser connects to CLI via Tailscale SSH.
//!
//! # Architecture
//!
//! ```text
//! Browser ◄──Tailscale SSH──► CLI (same tailnet, direct P2P when possible)
//!                               │
//!           Hub ◄─── BrowserEvent → HubAction
//! ```
//!
//! # Security
//!
//! - WireGuard E2E encryption (Tailscale)
//! - Per-user tailnet isolation at Headscale infrastructure level
//! - Pre-auth key in URL fragment (server never sees it)
//!
//! # Modules
//!
//! - [`browser`] - Browser event handling
//! - [`events`] - Browser event to Hub action conversion
//! - [`state`] - Browser connection state management
//! - [`types`] - Protocol message types

pub mod browser;
pub mod events;
pub mod state;
pub mod types;

pub use events::{
    browser_event_to_hub_action, check_browser_resize, command_to_event, BrowserEventContext,
    BrowserEventResult, BrowserResponse, ResizeAction,
};
pub use state::{
    build_agent_info, build_worktree_info, send_agent_list, send_agent_selected,
    send_scrollback, send_worktree_list, BrowserSendContext, BrowserState,
    TerminalOutputSender,
};

pub use types::{
    AgentInfo, BrowserCommand, BrowserEvent, BrowserResize, TerminalMessage,
    WorktreeInfo,
};

// Re-export Tailscale types from browser_connect module
pub use crate::browser_connect::{BrowserConnectionInfo, BrowserConnector};
pub use crate::tailscale::TailscaleClient;
