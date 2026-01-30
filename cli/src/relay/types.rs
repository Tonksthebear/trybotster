//! Data types for the terminal relay protocol.
//!
//! This module defines the message and event types used for communication
//! between the CLI and browser via Signal Protocol E2E encryption.
//!
//! # Message Types
//!
//! - [`TerminalMessage`] - CLI → Browser messages (output, agent lists, etc.)
//! - [`BrowserCommand`] - Browser → CLI commands (input, actions)
//!
//! # Transport
//!
//! Messages are sent as JSON over ActionCable WebSocket. E2E encryption is
//! handled by Signal Protocol (X3DH + Double Ratchet).

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
    /// Agent creation started notification.
    ///
    /// Sent immediately when agent creation begins, before blocking operations.
    /// Allows browser to show loading state.
    #[serde(rename = "agent_creating")]
    AgentCreating {
        /// The branch or issue identifier being created.
        identifier: String,
    },
    /// Agent creation progress update.
    ///
    /// Sent during agent creation to show progress through stages.
    #[serde(rename = "agent_creating_progress")]
    AgentCreatingProgress {
        /// The branch or issue identifier being created.
        identifier: String,
        /// Current stage of creation.
        stage: AgentCreationStage,
        /// Human-readable message for this stage.
        message: String,
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
    /// Data is gzip compressed and base64 encoded for efficient transport.
    #[serde(rename = "scrollback")]
    Scrollback {
        /// Base64-encoded gzip-compressed scrollback data.
        data: String,
        /// Whether data is compressed (always true for new messages).
        compressed: bool,
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
    /// HTTP response for preview proxy (CLI -> browser).
    ///
    /// Part of the preview feature that tunnels HTTP through the encrypted channel.
    #[serde(rename = "http_response")]
    HttpResponse {
        /// Request ID for correlation with the original request.
        request_id: u64,
        /// HTTP status code.
        status: u16,
        /// HTTP status text.
        #[serde(default)]
        status_text: String,
        /// Response headers.
        #[serde(default)]
        headers: std::collections::HashMap<String, String>,
        /// Response body (base64 encoded, possibly gzip compressed).
        #[serde(default)]
        body: Option<String>,
        /// Whether body is gzip compressed.
        #[serde(default)]
        compressed: bool,
    },
    /// HTTP proxy error response (CLI -> browser).
    #[serde(rename = "http_error")]
    HttpError {
        /// Request ID for correlation.
        request_id: u64,
        /// Error message.
        error: String,
    },
    /// PTY process exited notification.
    ///
    /// Sent when the PTY process terminates. Browser should handle this
    /// appropriately (e.g., show exit status, disable input).
    #[serde(rename = "process_exited")]
    ProcessExited {
        /// Exit code from the PTY process, if available.
        exit_code: Option<i32>,
    },
    /// Handshake acknowledgment (CLI -> Browser).
    ///
    /// Sent in response to browser's `connected` message to complete
    /// the E2E session establishment.
    #[serde(rename = "handshake_ack")]
    HandshakeAck,
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
    /// Port number for the agent's HTTP forwarding.
    pub port: Option<u16>,
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

/// Stages of agent creation for progress reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCreationStage {
    /// Creating the git worktree (slowest step).
    CreatingWorktree,
    /// Copying .botster_copy configuration files.
    CopyingConfig,
    /// Spawning the agent PTY process.
    SpawningAgent,
    /// Agent is ready.
    Ready,
}

impl AgentCreationStage {
    /// Get a human-readable description for this stage.
    #[must_use]
    pub fn description(&self) -> &'static str {
        match self {
            Self::CreatingWorktree => "Creating git worktree...",
            Self::CopyingConfig => "Copying configuration files...",
            Self::SpawningAgent => "Starting agent...",
            Self::Ready => "Agent ready",
        }
    }

    /// Get a short label for this stage.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::CreatingWorktree => "Worktree",
            Self::CopyingConfig => "Config",
            Self::SpawningAgent => "Starting",
            Self::Ready => "Ready",
        }
    }
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
                port: Some(3000),
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
        assert!(
            parsed.is_err(),
            "Raw output should not parse as TerminalMessage"
        );
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
            data: "H4sIAAAAAAAA".to_string(), // base64 gzip data
            compressed: true,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"scrollback""#));
        assert!(json.contains(r#""data":"#));
        assert!(json.contains(r#""compressed":true"#));
    }

    #[test]
    fn test_terminal_message_scrollback_empty() {
        let msg = TerminalMessage::Scrollback {
            data: String::new(),
            compressed: true,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"scrollback""#));
        assert!(json.contains(r#""data":"""#));
    }

    #[test]
    fn test_terminal_message_scrollback_deserialization() {
        let json = r#"{"type":"scrollback","data":"H4sIAAAAAAAA","compressed":true}"#;
        let parsed: TerminalMessage = serde_json::from_str(json).unwrap();
        match parsed {
            TerminalMessage::Scrollback { data, compressed } => {
                assert!(!data.is_empty());
                assert!(compressed);
            }
            _ => panic!("Wrong variant"),
        }
    }

    // ========== ProcessExited Message Tests ==========

    #[test]
    fn test_terminal_message_process_exited_with_code() {
        let msg = TerminalMessage::ProcessExited { exit_code: Some(0) };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"process_exited""#));
        assert!(json.contains(r#""exit_code":0"#));
    }

    #[test]
    fn test_terminal_message_process_exited_without_code() {
        let msg = TerminalMessage::ProcessExited { exit_code: None };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"process_exited""#));
        assert!(json.contains(r#""exit_code":null"#));
    }

    #[test]
    fn test_terminal_message_process_exited_deserialization() {
        let json = r#"{"type":"process_exited","exit_code":1}"#;
        let parsed: TerminalMessage = serde_json::from_str(json).unwrap();
        match parsed {
            TerminalMessage::ProcessExited { exit_code } => {
                assert_eq!(exit_code, Some(1));
            }
            _ => panic!("Wrong variant"),
        }
    }
}
