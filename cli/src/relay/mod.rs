//! Relay - Browser communication adapter.
//!
//! This module provides the browser relay functionality, handling WebRTC
//! DataChannel communication with connected browser clients. It manages:
//!
//! - E2E encrypted communication (Matrix Olm/Megolm)
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
//! Matrix Olm/Megolm protocols via `matrix-sdk-crypto`.
//!
//! ## Matrix Crypto (v5)
//!
//! Uses Olm (1:1 Double Ratchet) and Megolm (group ratchet) from matrix-sdk-crypto.
//! Key bundles are ~165 bytes, fitting easily in QR codes.
//!
//! 1. CLI creates OlmMachine with synthetic Matrix IDs
//! 2. CLI displays QR code with DeviceKeyBundle
//! 3. Browser scans QR, creates own OlmMachine
//! 4. Browser establishes Olm session via key claim
//! 5. Both sides can encrypt/decrypt with forward secrecy
//!
//! # Modules
//!
//! - [`crypto_service`] - Thread-safe crypto operations
//! - [`matrix_crypto`] - Matrix Olm/Megolm E2E encryption
//! - [`persistence`] - Encrypted storage for crypto state
//! - [`state`] - Browser connection state management
//! - [`types`] - Protocol message types
//! - [`http_proxy`] - HTTP proxy for preview tunneling
//! - [`preview_types`] - Preview proxy message types

pub mod crypto_service;
pub mod http_proxy;
pub mod matrix_crypto;
pub mod persistence;
pub mod preview_types;
pub mod state;
pub mod types;

pub use state::{build_agent_info, build_scrollback_message, build_worktree_info, BrowserState};

pub use types::{AgentInfo, BrowserCommand, BrowserResize, TerminalMessage, WorktreeInfo};

pub use matrix_crypto::{
    binary_format as matrix_binary_format, CryptoEnvelope, DeviceKeyBundle, MatrixCryptoManager,
    MatrixCryptoState, MATRIX_PROTOCOL_VERSION, MSG_TYPE_MEGOLM, MSG_TYPE_OLM, MSG_TYPE_OLM_PREKEY,
};

pub use persistence::{delete_connection_url, read_connection_url, write_connection_url};

pub use preview_types::{
    HttpRequest, HttpResponse, PreviewCommand, PreviewEvent, PreviewMessage, ProxyConfig,
    ProxyResult,
};

pub use http_proxy::HttpProxy;

pub use crypto_service::{CryptoRequest, CryptoService, CryptoServiceHandle};
