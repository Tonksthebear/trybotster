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
//! - [`events`] - Browser event to Hub action conversion

// Rust guideline compliant 2025-01

pub mod connection;
pub mod events;

pub use events::{browser_event_to_hub_action, command_to_event, BrowserEventContext};

// Re-export connection types (formerly terminal_relay)
pub use connection::{
    AgentInfo, BrowserCommand, BrowserEvent, BrowserResize, EncryptedEnvelope, TerminalMessage,
    TerminalOutputSender, TerminalRelay, WorktreeInfo,
};
