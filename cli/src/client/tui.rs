//! TUI communication types.
//!
//! Defines the message types for TuiRunner <-> Hub communication:
//! - `TuiRequest`: Messages from TuiRunner to Hub (user actions)
//! - `TuiOutput`: Messages from Hub to TuiRunner (PTY output, events)
//! - `TuiAgentMetadata`: Agent info returned after selection
//!
//! Hub processes `TuiRequest` messages directly in its tick loop
//! (`poll_tui_requests` -> `handle_tui_request`) without an intermediary
//! async task. Output flows back to TuiRunner via `TuiOutput` channel.

// Rust guideline compliant 2026-02

/// Requests from TuiRunner to Hub.
///
/// Every variant maps to a Hub operation processed synchronously in
/// `handle_tui_request()`. TuiRunner holds the sender end of an
/// unbounded channel and sends these messages on user interaction.
///
/// # Design Principle
///
/// TuiRunner should NOT know about Hub internals. All communication
/// goes through TuiRequest. Hub processes each variant directly via
/// HandleCache — no async Client trait indirection.
#[derive(Debug)]
pub enum TuiRequest {
    // === PTY I/O Operations ===
    /// Send keyboard input to a specific PTY.
    ///
    /// Carries explicit agent and PTY indices from TuiRunner,
    /// which tracks the current selection via `current_agent_index`
    /// and `current_pty_index`.
    SendInput {
        /// Index of the agent in Hub's agent list.
        agent_index: usize,
        /// Index of the PTY within the agent (0=CLI, 1=server).
        pty_index: usize,
        /// Raw bytes to send (keyboard input).
        data: Vec<u8>,
    },

    /// Update terminal dimensions and resize a specific PTY.
    ///
    /// Carries explicit agent and PTY indices from TuiRunner,
    /// which tracks the current selection via `current_agent_index`
    /// and `current_pty_index`.
    SetDims {
        /// Index of the agent in Hub's agent list.
        agent_index: usize,
        /// Index of the PTY within the agent (0=CLI, 1=server).
        pty_index: usize,
        /// Terminal width in columns.
        cols: u16,
        /// Terminal height in rows.
        rows: u16,
    },

    /// Select an agent by index and connect to its CLI PTY.
    ///
    /// Hub handles the agent selection, including:
    /// - Looking up the agent by index
    /// - Connecting to its CLI PTY
    /// - Notifying Hub of the selection (SelectAgentForClient)
    /// - Returning metadata via the response channel
    SelectAgent {
        /// Zero-based agent index in Hub's agent list.
        index: usize,
        /// Channel to receive the selected agent's metadata (or None if not found).
        response_tx: tokio::sync::oneshot::Sender<Option<TuiAgentMetadata>>,
    },

    /// Connect to a specific PTY on the current agent.
    ConnectToPty {
        /// Index of the agent in Hub's agent list.
        agent_index: usize,
        /// Index of the PTY within the agent (0=CLI, 1=server).
        pty_index: usize,
    },

    /// Disconnect from a specific PTY.
    ///
    /// Carries explicit agent and PTY indices from TuiRunner,
    /// which tracks the current selection via `current_agent_index`
    /// and `current_pty_index`.
    DisconnectFromPty {
        /// Index of the agent in Hub's agent list.
        agent_index: usize,
        /// Index of the PTY within the agent (0=CLI, 1=server).
        pty_index: usize,
    },

    // === Hub Operations ===
    /// Request Hub shutdown.
    Quit,

    /// List available worktrees for agent creation.
    ListWorktrees {
        /// Channel to receive list of (name, path) worktree pairs.
        response_tx: tokio::sync::oneshot::Sender<Vec<(String, String)>>,
    },

    /// Get the current connection code with QR PNG for display.
    GetConnectionCodeWithQr {
        /// Channel to receive the connection data (URL + QR PNG) or error message.
        response_tx: tokio::sync::oneshot::Sender<Result<crate::tui::ConnectionCodeData, String>>,
    },

    /// Create a new agent.
    CreateAgent {
        /// Agent creation parameters (worktree, issue, etc.).
        request: super::types::CreateAgentRequest,
    },

    /// Delete an existing agent.
    DeleteAgent {
        /// Agent deletion parameters (identifies which agent to remove).
        request: super::types::DeleteAgentRequest,
    },

    /// Regenerate the connection code (Signal bundle).
    RegenerateConnectionCode,

    /// Copy connection URL to clipboard.
    CopyConnectionUrl,
}

/// Metadata returned when selecting an agent for TUI.
///
/// Contains the essential information TuiRunner needs after selecting an agent,
/// without exposing Hub internals. This maintains the principle that TuiRunner
/// only interfaces through TuiRequest, not Hub types directly.
#[derive(Debug, Clone)]
pub struct TuiAgentMetadata {
    /// The agent's unique identifier (session key).
    pub agent_id: String,
    /// The agent's index in the Hub's ordered list.
    pub agent_index: usize,
    /// Whether this agent has a server PTY (index 1).
    pub has_server_pty: bool,
}

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

    /// Hub event forwarded to TuiRunner.
    ///
    /// Hub receives events via broadcast channel and forwards them to TuiRunner
    /// through the output channel. This keeps TuiRunner decoupled from the Hub's
    /// broadcast mechanism — all communication flows through the TuiOutput channel.
    HubEvent(crate::hub::HubEvent),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tui_output_debug() {
        let scrollback = TuiOutput::Scrollback(vec![1, 2, 3]);
        let output = TuiOutput::Output(vec![4, 5, 6]);
        let exited = TuiOutput::ProcessExited { exit_code: Some(0) };

        assert!(format!("{:?}", scrollback).contains("Scrollback"));
        assert!(format!("{:?}", output).contains("Output"));
        assert!(format!("{:?}", exited).contains("ProcessExited"));
    }

    #[test]
    fn test_tui_request_debug() {
        let send_input = TuiRequest::SendInput { agent_index: 0, pty_index: 0, data: vec![1, 2, 3] };
        let set_dims = TuiRequest::SetDims { agent_index: 0, pty_index: 0, cols: 80, rows: 24 };
        let (response_tx, _rx) = tokio::sync::oneshot::channel();
        let select_agent = TuiRequest::SelectAgent { index: 0, response_tx };
        let connect = TuiRequest::ConnectToPty { agent_index: 0, pty_index: 0 };
        let disconnect = TuiRequest::DisconnectFromPty { agent_index: 0, pty_index: 0 };

        assert!(format!("{:?}", send_input).contains("SendInput"));
        assert!(format!("{:?}", set_dims).contains("SetDims"));
        assert!(format!("{:?}", select_agent).contains("SelectAgent"));
        assert!(format!("{:?}", connect).contains("ConnectToPty"));
        assert!(format!("{:?}", disconnect).contains("DisconnectFromPty"));
    }

    #[test]
    fn test_tui_agent_metadata_debug() {
        let metadata = TuiAgentMetadata {
            agent_id: "test-123".to_string(),
            agent_index: 0,
            has_server_pty: true,
        };
        let debug_str = format!("{:?}", metadata);
        assert!(debug_str.contains("TuiAgentMetadata"));
        assert!(debug_str.contains("test-123"));
    }
}
