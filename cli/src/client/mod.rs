//! Client types for TUI and browser communication.
//!
//! This module provides:
//! - `ClientId` — unique identifier for client sessions (TUI, browser, internal)
//! - `TuiRequest` — messages from TuiRunner to Hub (JSON control + raw PTY input)
//! - `TuiOutput` — messages from Hub to TuiRunner (PTY output, Lua events)
//! - `CreateAgentRequest` / `DeleteAgentRequest` — client-layer agent operation types
//!
//! # Architecture
//!
//! ```text
//! TuiRunner (rendering, keyboard)
//!   │
//!   ├── TuiRequest::LuaMessage ──► Hub ──► lua.call_tui_message() ──► client.lua
//!   ├── TuiRequest::PtyInput   ──► Hub ──► pty.write_input_direct() (raw bytes)
//!   ◄── TuiOutput::Message     ◄── Lua tui.send() (events, subscriptions)
//!   ◄── TuiOutput::Output      ◄── Lua PTY forwarder tasks
//! ```
//!
//! Control operations (resize, subscriptions, agent lifecycle) flow as JSON
//! through `client.lua`. PTY keyboard input bypasses Lua as raw bytes.

// Rust guideline compliant 2026-02

mod tui;
mod types;

pub use tui::{TuiOutput, TuiRequest};
pub use types::{CreateAgentRequest, DeleteAgentRequest};

/// Unique identifier for a client session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ClientId {
    /// The local TUI client.
    Tui,
    /// A browser client, identified by crypto identity key.
    Browser(String),
    /// Internal operations (Lua scripts, background tasks).
    ///
    /// Used for operations that don't have a specific client identity,
    /// like Lua-initiated PTY resizes.
    Internal,
}

impl ClientId {
    /// Create a browser client ID from a crypto identity key.
    pub fn browser(identity: impl Into<String>) -> Self {
        ClientId::Browser(identity.into())
    }

    /// Check if this is the TUI client.
    pub fn is_tui(&self) -> bool {
        matches!(self, ClientId::Tui)
    }

    /// Check if this is a browser client.
    pub fn is_browser(&self) -> bool {
        matches!(self, ClientId::Browser(_))
    }

    /// Get the browser identity if this is a browser client.
    pub fn browser_identity(&self) -> Option<&str> {
        match self {
            ClientId::Browser(id) => Some(id),
            ClientId::Tui | ClientId::Internal => None,
        }
    }
}

impl std::fmt::Display for ClientId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientId::Tui => write!(f, "tui"),
            ClientId::Browser(id) => write!(f, "browser:{}", &id[..8.min(id.len())]),
            ClientId::Internal => write!(f, "internal"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_id_display() {
        assert_eq!(format!("{}", ClientId::Tui), "tui");
        assert_eq!(
            format!("{}", ClientId::Browser("abcd1234efgh5678".to_string())),
            "browser:abcd1234"
        );
        // Short identity
        assert_eq!(
            format!("{}", ClientId::Browser("abc".to_string())),
            "browser:abc"
        );
    }

    #[test]
    fn test_client_id_equality() {
        assert_eq!(ClientId::Tui, ClientId::Tui);
        assert_eq!(
            ClientId::Browser("abc".to_string()),
            ClientId::Browser("abc".to_string())
        );
        assert_ne!(ClientId::Tui, ClientId::Browser("abc".to_string()));
    }

    #[test]
    fn test_client_id_browser_constructor() {
        let id = ClientId::browser("test-identity");
        assert!(id.is_browser());
        assert!(!id.is_tui());
    }

    #[test]
    fn test_client_id_browser_identity() {
        let id = ClientId::browser("test-identity");
        assert_eq!(id.browser_identity(), Some("test-identity"));
        assert_eq!(ClientId::Tui.browser_identity(), None);
    }
}
