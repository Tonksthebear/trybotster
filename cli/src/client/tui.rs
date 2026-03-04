//! TUI communication types.
//!
//! Defines the message types for TuiRunner <-> Hub communication:
//! - TuiRunner → Hub: [`TuiRequest`] (JSON for Lua protocol, raw bytes for PTY input)
//! - Hub → TuiRunner: [`TuiOutput`] (PTY output, Lua events)
//!
//! # Message Format
//!
//! Control messages (resize, agent lifecycle, etc.) flow as JSON through
//! the Lua `client.lua` protocol, shared with browser clients.
//!
//! PTY keyboard input bypasses Lua entirely — raw bytes go directly from
//! TuiRunner to the PTY writer via [`TuiRequest::PtyInput`].
//!
//! JSON message types:
//! - Resize: `{subscriptionId: "tui:{session_uuid}", data: {type: "resize", rows, cols}}`
//! - Agent lifecycle: `{subscriptionId: "tui_hub", data: {type: "create_agent", ...}}`
//! - Connection: `{subscriptionId: "tui_hub", data: {type: "get_connection_code"}}`
//! - Quit: `{subscriptionId: "tui_hub", data: {type: "quit"}}`

// Rust guideline compliant 2026-02

/// Request messages sent from TuiRunner to Hub.
///
/// Two variants separate control messages (routed through Lua) from
/// PTY keyboard input (written directly to the PTY, bypassing Lua).
/// This mirrors [`crate::lua::primitives::TuiSendRequest`] which has
/// the same `Json`/`Binary` split for the Hub → TUI direction.
#[derive(Debug, Clone)]
pub enum TuiRequest {
    /// JSON message routed through Lua `client.lua` protocol.
    ///
    /// Used for resize, agent lifecycle, subscriptions, and all other
    /// control operations that need Lua processing.
    LuaMessage(serde_json::Value),

    /// Raw PTY input bytes — bypasses Lua entirely.
    ///
    /// Keyboard input goes directly to the PTY writer. No JSON
    /// serialization, no `from_utf8_lossy`, no Lua round-trip.
    PtyInput {
        /// Session UUID identifying the target PTY.
        session_uuid: String,
        /// Raw input bytes to write to the PTY.
        data: Vec<u8>,
    },
}

/// Output messages sent from Hub to TuiRunner.
///
/// TuiRunner receives these through the output channel and processes them
/// (feeding to AlacrittyParser, handling process exit, etc.).
///
/// PTY-related variants carry a `session_uuid` field to identify which
/// parser should receive the data.
#[derive(Debug, Clone)]
pub enum TuiOutput {
    /// Historical output from before connection.
    ///
    /// Sent once when connecting to a PTY, contains the scrollback buffer.
    Scrollback {
        /// Session UUID for parser routing.
        session_uuid: String,
        /// Raw scrollback data.
        data: Vec<u8>,
        /// Whether the inner PTY has kitty keyboard protocol active.
        ///
        /// Carried alongside scrollback because the snapshot bytes are
        /// ANSI output — the TUI needs to know the kitty state explicitly
        /// to push the protocol to the outer terminal on agent switch.
        kitty_enabled: bool,
    },

    /// Ongoing PTY output.
    ///
    /// Sent whenever the PTY produces new output.
    Output {
        /// Session UUID for parser routing.
        session_uuid: String,
        /// Raw PTY output data.
        data: Vec<u8>,
    },

    /// Batched PTY output — multiple chunks coalesced by the forwarder.
    ///
    /// Reduces wake pipe writes from one-per-4KB-chunk to one-per-batch.
    OutputBatch {
        /// Session UUID for parser routing.
        session_uuid: String,
        /// Coalesced output chunks (processed sequentially to preserve CSI 3J detection).
        chunks: Vec<Vec<u8>>,
    },

    /// PTY process exited.
    ///
    /// Sent when the PTY process terminates. TuiRunner should handle this
    /// appropriately (e.g., show exit status, disable input).
    ProcessExited {
        /// Session UUID for identifying which PTY exited.
        session_uuid: String,
        /// Exit code from the PTY process, if available.
        exit_code: Option<i32>,
    },

    /// JSON message from Lua event system.
    ///
    /// Carries agent lifecycle events and subscription data from Lua
    /// callbacks (`agent_created`, `agent_deleted`, `worktree_list`, etc.).
    /// These are broadcast by `broadcast_hub_event()` in Lua and delivered
    /// via `HubEvent::TuiSend` to the Hub event loop.
    Message(serde_json::Value),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tui_output_debug() {
        let scrollback = TuiOutput::Scrollback { session_uuid: "sess-0".into(), data: vec![1, 2, 3], kitty_enabled: false };
        let output = TuiOutput::Output { session_uuid: "sess-0".into(), data: vec![4, 5, 6] };
        let exited = TuiOutput::ProcessExited { session_uuid: "sess-0".into(), exit_code: Some(0) };
        let message = TuiOutput::Message(serde_json::json!({"type": "agent_created"}));

        assert!(format!("{:?}", scrollback).contains("Scrollback"));
        assert!(format!("{:?}", output).contains("Output"));
        assert!(format!("{:?}", exited).contains("ProcessExited"));
        assert!(format!("{:?}", message).contains("Message"));
    }

    #[test]
    fn test_tui_request_debug() {
        let lua_msg = TuiRequest::LuaMessage(serde_json::json!({"type": "resize"}));
        let pty_input = TuiRequest::PtyInput {
            session_uuid: "sess-0".into(),
            data: vec![b'h', b'i'],
        };

        assert!(format!("{:?}", lua_msg).contains("LuaMessage"));
        assert!(format!("{:?}", pty_input).contains("PtyInput"));
    }
}
