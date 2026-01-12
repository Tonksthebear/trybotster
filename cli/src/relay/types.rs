//! Data types for the terminal relay protocol.
//!
//! This module defines the message and event types used for communication
//! between the CLI and browser via Tailscale SSH.
//!
//! # Message Types
//!
//! - [`TerminalMessage`] - CLI → Browser messages (output, agent lists, etc.)
//! - [`BrowserCommand`] - Browser → CLI commands (input, actions)
//! - [`BrowserEvent`] - Parsed browser events for Hub consumption
//!
//! # Transport
//!
//! Messages are sent as JSON over Tailscale SSH. Encryption is handled by
//! WireGuard at the transport layer.

// Rust guideline compliant 2025-01

use serde::{Deserialize, Serialize};

/// Message types for terminal relay (CLI -> browser).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TerminalMessage {
    /// Terminal output from CLI to browser.
    #[serde(rename = "output")]
    Output {
        /// Terminal output data.
        data: String,
    },
    /// Agent list response.
    #[serde(rename = "agents")]
    Agents {
        /// List of available agents.
        agents: Vec<AgentInfo>,
    },
    /// Worktree list response.
    #[serde(rename = "worktrees")]
    Worktrees {
        /// List of available worktrees.
        worktrees: Vec<WorktreeInfo>,
        /// Repository name.
        repo: Option<String>,
    },
    /// Agent selected confirmation.
    #[serde(rename = "agent_selected")]
    AgentSelected {
        /// Selected agent's session key.
        id: String,
    },
    /// Agent created confirmation.
    #[serde(rename = "agent_created")]
    AgentCreated {
        /// Created agent's session key.
        id: String,
    },
    /// Agent deleted confirmation.
    #[serde(rename = "agent_deleted")]
    AgentDeleted {
        /// Deleted agent's session key.
        id: String,
    },
    /// Error message.
    #[serde(rename = "error")]
    Error {
        /// Error description.
        message: String,
    },
    /// Scrollback history for terminal.
    ///
    /// Sent when an agent is selected so the browser can populate
    /// xterm's scrollback buffer with historical output.
    #[serde(rename = "scrollback")]
    Scrollback {
        /// Lines of scrollback history (oldest first).
        lines: Vec<String>,
    },
    /// Invite bundle for sharing hub connection.
    ///
    /// Contains a fresh PreKeyBundle that can be shared via URL fragment.
    /// Server never sees this - stays in `#bundle=...` URL fragment.
    #[serde(rename = "invite_bundle")]
    InviteBundle {
        /// Base64-encoded PreKeyBundle JSON.
        bundle: String,
        /// Shareable URL with bundle in fragment.
        url: String,
    },
}

/// Agent info for list response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    /// Unique agent identifier (session key).
    pub id: String,
    /// Repository name in "owner/repo" format.
    pub repo: Option<String>,
    /// GitHub issue number the agent is working on.
    pub issue_number: Option<u64>,
    /// Git branch name for the agent's worktree.
    pub branch_name: Option<String>,
    /// Human-readable agent name.
    pub name: Option<String>,
    /// Current agent status (e.g., "Running", "Idle").
    pub status: Option<String>,
    /// Port number for the agent's HTTP tunnel.
    pub tunnel_port: Option<u16>,
    /// Whether a dev server is running.
    pub server_running: Option<bool>,
    /// Whether a server PTY exists.
    pub has_server_pty: Option<bool>,
    /// Currently active PTY view ("cli" or "server").
    pub active_pty_view: Option<String>,
    /// Scrollback offset in lines.
    pub scroll_offset: Option<u32>,
    /// Hub identifier where this agent runs.
    pub hub_identifier: Option<String>,
}

/// Worktree info for list response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeInfo {
    /// Filesystem path to the worktree.
    pub path: String,
    /// Git branch name.
    pub branch: String,
    /// Associated issue number, if any.
    pub issue_number: Option<u64>,
}

/// Browser command types (browser -> CLI).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BrowserCommand {
    /// Handshake message for session establishment/reconnection.
    /// Browser sends this after establishing Signal session.
    #[serde(rename = "connected")]
    Handshake {
        /// Browser's device name.
        device_name: String,
        /// Timestamp of connection (optional).
        #[serde(default)]
        timestamp: Option<u64>,
    },
    /// Terminal input from browser.
    #[serde(rename = "input")]
    Input {
        /// Input data to send to terminal.
        data: String,
    },
    /// Set display mode (tui/gui).
    #[serde(rename = "set_mode")]
    SetMode {
        /// Display mode ("tui" or "gui").
        mode: String,
    },
    /// List all agents.
    #[serde(rename = "list_agents")]
    ListAgents,
    /// List available worktrees.
    #[serde(rename = "list_worktrees")]
    ListWorktrees,
    /// Select an agent.
    #[serde(rename = "select_agent")]
    SelectAgent {
        /// Agent session key to select.
        id: String,
    },
    /// Create a new agent.
    #[serde(rename = "create_agent")]
    CreateAgent {
        /// Issue number or branch name.
        issue_or_branch: Option<String>,
        /// Initial prompt for the agent.
        prompt: Option<String>,
    },
    /// Reopen an existing worktree.
    #[serde(rename = "reopen_worktree")]
    ReopenWorktree {
        /// Path to the worktree.
        path: String,
        /// Branch name.
        branch: String,
        /// Initial prompt for the agent.
        prompt: Option<String>,
    },
    /// Delete an agent.
    #[serde(rename = "delete_agent")]
    DeleteAgent {
        /// Agent session key to delete.
        id: String,
        /// Whether to delete the worktree as well.
        delete_worktree: Option<bool>,
    },
    /// Toggle PTY view (CLI/Server).
    #[serde(rename = "toggle_pty_view")]
    TogglePtyView,
    /// Scroll terminal.
    #[serde(rename = "scroll")]
    Scroll {
        /// Scroll direction ("up" or "down").
        direction: String,
        /// Number of lines to scroll.
        lines: Option<u32>,
    },
    /// Scroll to bottom (return to live).
    #[serde(rename = "scroll_to_bottom")]
    ScrollToBottom,
    /// Scroll to top.
    #[serde(rename = "scroll_to_top")]
    ScrollToTop,
    /// Terminal resize.
    #[serde(rename = "resize")]
    Resize {
        /// Number of columns.
        cols: u16,
        /// Number of rows.
        rows: u16,
    },
    /// Request a fresh invite bundle for sharing hub connection.
    #[serde(rename = "generate_invite")]
    GenerateInvite,
}

/// Browser resize event.
#[derive(Debug, Clone)]
pub struct BrowserResize {
    /// Terminal width in columns.
    pub cols: u16,
    /// Terminal height in rows.
    pub rows: u16,
}

/// Events received from the browser via the relay.
///
/// These events are parsed from [`BrowserCommand`]s and enriched with
/// connection state (e.g., Connected/Disconnected events).
#[derive(Debug, Clone)]
pub enum BrowserEvent {
    /// Browser connected and sent its public key.
    Connected {
        /// Browser's public key for encryption.
        public_key: String,
        /// Name of the connected device.
        device_name: String,
    },
    /// Browser disconnected.
    Disconnected,
    /// Terminal input from browser (already decrypted).
    Input(String),
    /// Browser resized terminal.
    Resize(BrowserResize),
    /// Set display mode (tui/gui).
    SetMode {
        /// Display mode ("tui" or "gui").
        mode: String,
    },
    /// List all agents.
    ListAgents,
    /// List available worktrees.
    ListWorktrees,
    /// Select an agent.
    SelectAgent {
        /// Agent session key to select.
        id: String,
    },
    /// Create a new agent.
    CreateAgent {
        /// Issue number or branch name.
        issue_or_branch: Option<String>,
        /// Initial prompt for the agent.
        prompt: Option<String>,
    },
    /// Reopen an existing worktree.
    ReopenWorktree {
        /// Path to the worktree.
        path: String,
        /// Branch name.
        branch: String,
        /// Initial prompt for the agent.
        prompt: Option<String>,
    },
    /// Delete an agent.
    DeleteAgent {
        /// Agent session key to delete.
        id: String,
        /// Whether to delete the worktree as well.
        delete_worktree: bool,
    },
    /// Toggle PTY view (CLI/Server).
    TogglePtyView,
    /// Scroll terminal.
    Scroll {
        /// Scroll direction ("up" or "down").
        direction: String,
        /// Number of lines to scroll.
        lines: u32,
    },
    /// Scroll to bottom (return to live).
    ScrollToBottom,
    /// Scroll to top.
    ScrollToTop,
    /// Request invite bundle for sharing.
    /// Handled directly in relay, not forwarded to Hub.
    GenerateInvite,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== TerminalMessage Serialization Tests ==========

    #[test]
    fn test_terminal_message_output_serialization() {
        let msg = TerminalMessage::Output {
            data: "hello".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"output""#));
        assert!(json.contains(r#""data":"hello""#));
    }

    #[test]
    fn test_terminal_message_agents_serialization() {
        let msg = TerminalMessage::Agents {
            agents: vec![AgentInfo {
                id: "test-id".to_string(),
                repo: Some("owner/repo".to_string()),
                issue_number: Some(42),
                branch_name: Some("botster-issue-42".to_string()),
                name: None,
                status: Some("Running".to_string()),
                tunnel_port: Some(3000),
                server_running: Some(true),
                has_server_pty: Some(true),
                active_pty_view: Some("cli".to_string()),
                scroll_offset: Some(0),
                hub_identifier: Some("hub-123".to_string()),
            }],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"agents""#));
        assert!(json.contains(r#""id":"test-id""#));
        assert!(json.contains(r#""issue_number":42"#));
    }

    #[test]
    fn test_terminal_message_worktrees_serialization() {
        let msg = TerminalMessage::Worktrees {
            worktrees: vec![WorktreeInfo {
                path: "/path/to/worktree".to_string(),
                branch: "feature-branch".to_string(),
                issue_number: None,
            }],
            repo: Some("owner/repo".to_string()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"worktrees""#));
        assert!(json.contains(r#""path":"/path/to/worktree""#));
    }

    #[test]
    fn test_terminal_message_agent_selected_serialization() {
        let msg = TerminalMessage::AgentSelected {
            id: "agent-123".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"agent_selected""#));
        assert!(json.contains(r#""id":"agent-123""#));
    }

    #[test]
    fn test_terminal_message_error_serialization() {
        let msg = TerminalMessage::Error {
            message: "Something went wrong".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"error""#));
        assert!(json.contains(r#""message":"Something went wrong""#));
    }

    // ========== Structured Message Detection Tests ==========

    #[test]
    fn test_structured_message_detection_output() {
        let json = r#"{"type":"output","data":"hello"}"#;
        let parsed: Result<TerminalMessage, _> = serde_json::from_str(json);
        assert!(parsed.is_ok());
        match parsed.unwrap() {
            TerminalMessage::Output { data } => assert_eq!(data, "hello"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_structured_message_detection_agents() {
        let json = r#"{"type":"agents","agents":[]}"#;
        let parsed: Result<TerminalMessage, _> = serde_json::from_str(json);
        assert!(parsed.is_ok());
        match parsed.unwrap() {
            TerminalMessage::Agents { agents } => assert!(agents.is_empty()),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_raw_output_not_detected_as_structured() {
        let raw_output = "Hello, this is terminal output with special chars: \x1b[32mgreen\x1b[0m";
        let parsed: Result<TerminalMessage, _> = serde_json::from_str(raw_output);
        assert!(parsed.is_err(), "Raw output should not parse as TerminalMessage");
    }

    // ========== BrowserCommand Parsing Tests ==========

    #[test]
    fn test_browser_command_input_parsing() {
        let json = r#"{"type":"input","data":"ls -la"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();
        match cmd {
            BrowserCommand::Input { data } => assert_eq!(data, "ls -la"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_browser_command_set_mode_parsing() {
        let json = r#"{"type":"set_mode","mode":"gui"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();
        match cmd {
            BrowserCommand::SetMode { mode } => assert_eq!(mode, "gui"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_browser_command_list_agents_parsing() {
        let json = r#"{"type":"list_agents"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();
        assert!(matches!(cmd, BrowserCommand::ListAgents));
    }

    #[test]
    fn test_browser_command_select_agent_parsing() {
        let json = r#"{"type":"select_agent","id":"agent-abc-123"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();
        match cmd {
            BrowserCommand::SelectAgent { id } => assert_eq!(id, "agent-abc-123"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_browser_command_create_agent_parsing() {
        let json = r#"{"type":"create_agent","issue_or_branch":"42","prompt":"Fix the bug"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();
        match cmd {
            BrowserCommand::CreateAgent {
                issue_or_branch,
                prompt,
            } => {
                assert_eq!(issue_or_branch, Some("42".to_string()));
                assert_eq!(prompt, Some("Fix the bug".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_browser_command_scroll_parsing() {
        let json = r#"{"type":"scroll","direction":"up","lines":5}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();
        match cmd {
            BrowserCommand::Scroll { direction, lines } => {
                assert_eq!(direction, "up");
                assert_eq!(lines, Some(5));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_browser_command_toggle_pty_view_parsing() {
        let json = r#"{"type":"toggle_pty_view"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();
        assert!(matches!(cmd, BrowserCommand::TogglePtyView));
    }

    // ========== Scrollback Message Tests (TDD) ==========

    #[test]
    fn test_terminal_message_scrollback_serialization() {
        let msg = TerminalMessage::Scrollback {
            lines: vec![
                "Line 1: some output".to_string(),
                "Line 2: more output".to_string(),
                "Line 3: \x1b[32mcolored\x1b[0m output".to_string(),
            ],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"scrollback""#));
        assert!(json.contains(r#""lines":"#));
        assert!(json.contains("Line 1: some output"));
        assert!(json.contains("Line 3:"));
    }

    #[test]
    fn test_terminal_message_scrollback_empty() {
        let msg = TerminalMessage::Scrollback { lines: vec![] };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"scrollback""#));
        assert!(json.contains(r#""lines":[]"#));
    }

    #[test]
    fn test_terminal_message_scrollback_deserialization() {
        let json = r#"{"type":"scrollback","lines":["line1","line2"]}"#;
        let parsed: TerminalMessage = serde_json::from_str(json).unwrap();
        match parsed {
            TerminalMessage::Scrollback { lines } => {
                assert_eq!(lines.len(), 2);
                assert_eq!(lines[0], "line1");
                assert_eq!(lines[1], "line2");
            }
            _ => panic!("Wrong variant"),
        }
    }
}
