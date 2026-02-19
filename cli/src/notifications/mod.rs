//! Web push notification infrastructure.
//!
//! Manages VAPID keys and browser push subscriptions for delivering
//! agent alert notifications directly from CLI to browser push services.
//! Rails is never in the notification path.
//!
//! # Architecture
//!
//! ```text
//! Agent alert fires in PTY
//!     ↓
//! CLI sends web push (RFC 8030) to browser push service
//!     ↓
//! Push service delivers to service worker
//!     ↓
//! Service worker writes to IndexedDB + shows browser notification
//! ```
//!
//! # VAPID Keys
//!
//! The device generates a P-256 ECDSA keypair (VAPID, RFC 8292) shared
//! across all hubs. The private key is stored in device-level encrypted
//! persistence. The public key is sent to browsers via DataChannel so
//! they can subscribe. Multi-device setups copy keys via the browser.
//!
//! # Push Subscriptions
//!
//! Browsers send their push subscription (endpoint + keys) back to CLI
//! via DataChannel. CLI stores these per browser identity and uses them
//! to send web push messages when notifications fire.

// Rust guideline compliant 2026-02

pub mod vapid;
pub mod push;
