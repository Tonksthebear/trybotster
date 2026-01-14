//! Relay - Browser WebSocket adapter.
//!
//! This module provides the browser relay functionality, handling WebSocket
//! communication with connected browser clients via Action Cable. It manages:
//!
//! - E2E encrypted communication (Signal Protocol)
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
//! the Signal Protocol (X3DH + Double Ratchet), the same battle-tested
//! cryptography used by Signal, WhatsApp, and other secure messengers.
//! The Rails server only sees encrypted blobs and cannot read the terminal content.
//!
//! ## Protocol (Signal)
//!
//! 1. CLI generates identity keys and PreKeyBundle
//! 2. CLI displays QR code with PreKeyBundle
//! 3. Browser scans QR code and calls process_prekey_bundle()
//! 4. Browser sends PreKeySignalMessage to establish session
//! 5. CLI decrypts and creates Double Ratchet session
//! 6. Both sides can now encrypt/decrypt with forward secrecy
//!
//! ## Group Messaging (SenderKey)
//!
//! For CLI → multiple browsers broadcast:
//! 1. CLI creates SenderKeyDistributionMessage
//! 2. CLI sends distribution to each browser via individual session
//! 3. CLI uses group_encrypt for broadcasts (efficient)
//! 4. Browsers use group_decrypt to receive
//!
//! # Modules
//!
//! - [`connection`] - WebSocket transport and Signal encryption
//! - [`events`] - Browser event to Hub action conversion
//! - [`signal`] - Signal Protocol E2E encryption
//! - [`signal_stores`] - Signal Protocol store implementations
//! - [`persistence`] - Encrypted storage for Signal state
//! - [`state`] - Browser connection state management
//! - [`types`] - Protocol message types

pub mod browser;
pub mod connection;
pub mod events;
pub mod persistence;
pub mod signal;
pub mod signal_stores;
pub mod state;
pub mod types;

pub use events::{
    browser_event_to_client_action, check_browser_resize, command_to_event, BrowserEventContext,
    BrowserEventResult, BrowserResponse, ResizeAction,
};
pub use state::{
    build_agent_info, build_worktree_info, send_agent_created_to, send_agent_creating,
    send_agent_creating_to, send_agent_list, send_agent_list_to, send_agent_progress,
    send_agent_progress_to, send_agent_selected, send_agent_selected_to, send_scrollback,
    send_scrollback_to, send_worktree_list, send_worktree_list_to, BrowserSendContext,
    BrowserState, IdentifiedBrowserEvent,
};

pub use types::{
    AgentCreationStage, AgentInfo, BrowserCommand, BrowserEvent, BrowserResize,
    TerminalMessage, WorktreeInfo,
};

pub use connection::{TerminalOutputSender, TerminalRelay};

pub use signal::{PreKeyBundleData, SignalEnvelope, SignalProtocolManager, SIGNAL_PROTOCOL_VERSION, binary_format};

pub use persistence::{read_connection_url, write_connection_url, delete_connection_url};

pub use browser::{drain_and_route_pty_output, poll_events_headless};
