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
//! - Terminal I/O: `{subscriptionId: "tui:{agent}:{pty}", data: {type: "input", data: "..."}}`
//! - Resize: `{subscriptionId: "tui:{agent}:{pty}", data: {type: "resize", rows, cols}}`
//! - Agent lifecycle: `{subscriptionId: "tui_hub", data: {type: "create_agent", ...}}`
//! - Connection: `{subscriptionId: "tui_hub", data: {type: "get_connection_code"}}`
//! - Quit: `{subscriptionId: "tui_hub", data: {type: "quit"}}`

// Rust guideline compliant 2026-02

/// Output messages sent from Hub to TuiRunner.
///
/// TuiRunner receives these through the output channel and processes them
/// (feeding to vt100 parser, handling process exit, etc.).
///
/// PTY-related variants carry optional `agent_index` and `pty_index` fields
/// to identify which parser should receive the data. When `None`, data is
/// routed to the currently active parser (backward compat).
#[derive(Debug, Clone)]
pub enum TuiOutput {
    /// Historical output from before connection.
    ///
    /// Sent once when connecting to a PTY, contains the scrollback buffer.
    Scrollback {
        /// Agent index for parser routing (`None` = active parser).
        agent_index: Option<usize>,
        /// PTY index for parser routing (`None` = active PTY).
        pty_index: Option<usize>,
        /// Raw scrollback data.
        data: Vec<u8>,
    },

    /// Ongoing PTY output.
    ///
    /// Sent whenever the PTY produces new output.
    Output {
        /// Agent index for parser routing (`None` = active parser).
        agent_index: Option<usize>,
        /// PTY index for parser routing (`None` = active PTY).
        pty_index: Option<usize>,
        /// Raw PTY output data.
        data: Vec<u8>,
    },

    /// PTY process exited.
    ///
    /// Sent when the PTY process terminates. TuiRunner should handle this
    /// appropriately (e.g., show exit status, disable input).
    ProcessExited {
        /// Agent index for identifying which PTY exited.
        agent_index: Option<usize>,
        /// PTY index for identifying which PTY exited.
        pty_index: Option<usize>,
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
        let scrollback = TuiOutput::Scrollback { agent_index: Some(0), pty_index: Some(0), data: vec![1, 2, 3] };
        let output = TuiOutput::Output { agent_index: Some(0), pty_index: Some(1), data: vec![4, 5, 6] };
        let exited = TuiOutput::ProcessExited { agent_index: Some(0), pty_index: Some(0), exit_code: Some(0) };
        let message = TuiOutput::Message(serde_json::json!({"type": "agent_created"}));

        assert!(format!("{:?}", scrollback).contains("Scrollback"));
        assert!(format!("{:?}", output).contains("Output"));
        assert!(format!("{:?}", exited).contains("ProcessExited"));
        assert!(format!("{:?}", message).contains("Message"));
    }
}
