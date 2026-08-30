//! Relay - Browser communication adapter.
//!
//! This module provides the browser relay functionality, handling WebRTC
//! DataChannel communication with connected browser clients. It manages:
//!
//! - Key-authenticated browser admission
//! - Terminal output streaming
//!
//! # Architecture
//!
//! ```text
//! Browser ◄──WebRTC DataChannel──► CLI
//!                                                    │
//!                              Hub ◄─── HubCommandChannel (signaling via Rails)
//!                              │
//!                WebRtcSubscriptions (virtual channel routing)
//!                              │
//!                    TerminalRelayChannel (PTY I/O subscription type)
//! ```
//!
//! WebRTC DataChannels provide transport encryption. The hub admits each
//! browser with a one-time pairing secret or a stored browser public key.
//!
//! # Modules
//!
//! - [`persistence`] - Encrypted storage for relay-side secrets and metadata
//! - [`state`] - Browser connection state management
//! - [`types`] - Protocol message types
//! - [`stream_mux`] - TCP stream multiplexer for preview tunneling

pub mod pairing;
pub mod persistence;
pub mod state;
pub mod stream_mux;
pub mod types;

pub use state::BrowserState;

pub use types::{AgentInfo, BrowserCommand, BrowserResize, TerminalMessage};

pub use persistence::{delete_connection_url, read_connection_url, write_connection_url};
pub use pairing::{
    AuthAccepted, AuthChallenge, BrowserAdmission, BrowserAuthRequest, BrowserCredential,
    DtlsBinding, PairingAuthority, PairingBundle, PAIRING_PROTOCOL_VERSION,
};
