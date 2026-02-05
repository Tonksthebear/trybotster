//! TUI communication types.
//!
//! Defines the message types for TuiRunner <-> Hub communication:
//! - TuiRunner → Hub: `serde_json::Value` (JSON routed through Lua `client.lua`)
//! - Hub → TuiRunner: `TuiOutput` (PTY output, Lua events)
//!
//! ALL operations flow as JSON, sharing the same `client.lua` protocol as
//! browser clients. TuiRunner speaks the identical subscription protocol.
//!
//! # Message Format
//!
//! TuiRunner sends JSON messages using the Lua client protocol:
//! - Terminal I/O: `{subscriptionId: "tui_term", data: {type: "input", data: "..."}}`
//! - Resize: `{subscriptionId: "tui_term", data: {type: "resize", rows, cols}}`
//! - Agent lifecycle: `{subscriptionId: "tui_hub", data: {type: "create_agent", ...}}`
//! - Connection: `{subscriptionId: "tui_hub", data: {type: "get_connection_code"}}`
//! - Quit: `{subscriptionId: "tui_hub", data: {type: "quit"}}`

// Rust guideline compliant 2026-02

/// Output messages sent from Hub to TuiRunner.
///
/// TuiRunner receives these through the output channel and processes them
/// (feeding to vt100 parser, handling process exit, etc.).
#[derive(Debug, Clone)]
pub enum TuiOutput {
    /// Historical output from before connection.
    ///
    /// Sent once when connecting to a PTY, contains the scrollback buffer.
    Scrollback(Vec<u8>),

    /// Ongoing PTY output.
    ///
    /// Sent whenever the PTY produces new output.
    Output(Vec<u8>),

    /// PTY process exited.
    ///
    /// Sent when the PTY process terminates. TuiRunner should handle this
    /// appropriately (e.g., show exit status, disable input).
    ProcessExited {
        /// Exit code from the PTY process, if available.
        exit_code: Option<i32>,
    },

    /// JSON message from Lua event system.
    ///
    /// Carries agent lifecycle events and subscription data from Lua
    /// callbacks (`agent_created`, `agent_deleted`, `worktree_list`, etc.).
    /// These are broadcast by `broadcast_hub_event()` in Lua and forwarded
    /// by `process_lua_tui_sends()` in Hub.
    Message(serde_json::Value),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tui_output_debug() {
        let scrollback = TuiOutput::Scrollback(vec![1, 2, 3]);
        let output = TuiOutput::Output(vec![4, 5, 6]);
        let exited = TuiOutput::ProcessExited { exit_code: Some(0) };
        let message = TuiOutput::Message(serde_json::json!({"type": "agent_created"}));

        assert!(format!("{:?}", scrollback).contains("Scrollback"));
        assert!(format!("{:?}", output).contains("Output"));
        assert!(format!("{:?}", exited).contains("ProcessExited"));
        assert!(format!("{:?}", message).contains("Message"));
    }
}
