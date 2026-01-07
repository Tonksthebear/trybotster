//! Relay - Browser WebSocket adapter.
//!
//! This module provides the browser relay functionality, handling WebSocket
//! communication with connected browser clients via Action Cable. It manages:
//!
//! - E2E encrypted communication
//! - Browser event to Hub action conversion
//! - Terminal output streaming
//!
//! # Architecture
//!
//! ```text
//! Browser ◄──WebSocket──► Rails Action Cable ◄──WebSocket──► Relay
//!                                                              │
//!                              Hub ◄─── BrowserEvent → HubAction
//! ```
//!
//! # Encryption
//!
//! All communication between the CLI and browser is E2E encrypted using
//! crypto_box (TweetNaCl compatible). The Rails server only sees encrypted
//! blobs and cannot read the terminal content.
//!
//! # Modules
//!
//! - [`connection`] - WebSocket transport and encryption
//! - [`events`] - Browser event to Hub action conversion
//! - [`state`] - Browser connection state management
//! - [`types`] - Protocol message types

// Rust guideline compliant 2025-01

pub mod browser;
pub mod connection;
pub mod events;
pub mod state;
pub mod types;

pub use events::{
    browser_event_to_hub_action, check_browser_resize, command_to_event, BrowserEventContext,
    BrowserEventResult, BrowserResponse, ResizeAction,
};
pub use state::{
    build_agent_info, build_worktree_info, send_agent_list, send_agent_selected,
    send_worktree_list, BrowserSendContext, BrowserState,
};

// Re-export types for external use
pub use types::{
    AgentInfo, BrowserCommand, BrowserEvent, BrowserResize, EncryptedEnvelope, TerminalMessage,
    WorktreeInfo,
};

// Re-export connection types
pub use connection::{TerminalOutputSender, TerminalRelay};
