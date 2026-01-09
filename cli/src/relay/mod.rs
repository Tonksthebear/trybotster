//! Relay - Browser WebSocket adapter.
//!
//! This module provides the browser relay functionality, handling WebSocket
//! communication with connected browser clients via Action Cable. It manages:
//!
//! - E2E encrypted communication (vodozemac Olm)
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
//! vodozemac's Olm implementation - the same NCC-audited cryptography used by Matrix.
//! The Rails server only sees encrypted blobs and cannot read the terminal content.
//!
//! ## Protocol (Olm)
//!
//! 1. CLI generates Olm account and displays QR code with session keys
//! 2. Browser scans QR code (ed25519, curve25519, one_time_key)
//! 3. Browser creates outbound Olm session using CLI's keys
//! 4. Browser sends PreKey message to establish session
//! 5. CLI creates inbound session from PreKey message
//! 6. Both sides can now encrypt/decrypt with the session
//!
//! # Modules
//!
//! - [`connection`] - WebSocket transport and Olm encryption
//! - [`events`] - Browser event to Hub action conversion
//! - [`olm`] - Olm E2E encryption (vodozemac wrapper)
//! - [`state`] - Browser connection state management
//! - [`types`] - Protocol message types

pub mod browser;
pub mod connection;
pub mod events;
pub mod olm;
pub mod persistence;
pub mod state;
pub mod types;

pub use events::{
    browser_event_to_hub_action, check_browser_resize, command_to_event, BrowserEventContext,
    BrowserEventResult, BrowserResponse, ResizeAction,
};
pub use state::{
    build_agent_info, build_worktree_info, send_agent_list, send_agent_selected,
    send_scrollback, send_worktree_list, BrowserSendContext, BrowserState,
};

pub use types::{
    AgentInfo, BrowserCommand, BrowserEvent, BrowserResize, EncryptedEnvelope, TerminalMessage,
    WorktreeInfo,
};

pub use connection::{TerminalOutputSender, TerminalRelay};

pub use olm::{OlmAccount, OlmEnvelope, OlmSession, SessionEstablishmentKeys};
