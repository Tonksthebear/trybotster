//! Relay - Browser communication adapter.
//!
//! This module provides the browser relay functionality, handling WebRTC
//! DataChannel communication with connected browser clients. It manages:
//!
//! - E2E encrypted communication (vodozemac Olm)
//! - Terminal output streaming
//!
//! # Architecture
//!
//! ```text
//! Browser ◄──WebRTC DataChannel (E2E encrypted)──► CLI
//!                                                    │
//!                              Hub ◄─── HubCommandChannel (signaling via Rails)
//!                              │
//!                WebRtcSubscriptions (virtual channel routing)
//!                              │
//!                    TerminalRelayChannel (PTY I/O subscription type)
//! ```
//!
//! # Encryption
//!
//! All communication between the CLI and browser is E2E encrypted using
//! vodozemac Olm (direct, no matrix-sdk-crypto wrapper).
//!
//! ## Vodozemac Crypto (v6)
//!
//! Uses Olm (1:1 Double Ratchet) from vodozemac directly.
//! Key bundles are 161 bytes (fixed size), fitting easily in QR codes.
//!
//! 1. CLI creates vodozemac Account
//! 2. CLI displays QR code with DeviceKeyBundle
//! 3. Browser scans QR, creates outbound Olm session
//! 4. Browser sends PreKey message via DataChannel
//! 5. CLI creates inbound session, both sides encrypted
//!
//! # Modules
//!
//! - [`crypto_service`] - Thread-safe crypto wrapper (`Arc<Mutex<VodozemacCrypto>>`)
//! - [`olm_crypto`] - Vodozemac Olm E2E encryption
//! - [`persistence`] - Encrypted storage for crypto state
//! - [`state`] - Browser connection state management
//! - [`types`] - Protocol message types
//! - [`stream_mux`] - TCP stream multiplexer for preview tunneling

pub mod crypto_service;
pub mod olm_crypto;
pub mod persistence;
pub mod state;
pub mod stream_mux;
pub mod types;

pub use state::{build_agent_info, build_scrollback_message, build_worktree_info, BrowserState};

pub use types::{AgentInfo, BrowserCommand, BrowserResize, SessionInfo, TerminalMessage, WorktreeInfo};

pub use olm_crypto::{
    binary_format, DeviceKeyBundle, OlmEnvelope, VodozemacCrypto, VodozemacCryptoState,
    MSG_TYPE_NORMAL, MSG_TYPE_PREKEY, PROTOCOL_VERSION,
};

pub use persistence::{delete_connection_url, read_connection_url, write_connection_url};

pub use crypto_service::{create_crypto_service, CryptoService};
