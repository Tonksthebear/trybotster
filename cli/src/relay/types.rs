//! Data types for the terminal relay protocol.
//!
//! This module defines the message and event types used for communication
//! between the CLI and browser via the WebSocket relay.
//!
//! # Message Types
//!
//! - [`TerminalMessage`] - CLI → Browser messages (output, agent lists, etc.)
//! - [`BrowserCommand`] - Browser → CLI commands (input, actions)
//! - [`BrowserEvent`] - Parsed browser events for Hub consumption
//!
//! # Wire Format
//!
//! All messages are encrypted using [`EncryptedEnvelope`] before transmission.

// Rust guideline compliant 2025-01

use serde::{Deserialize, Serialize};

/// Message types for terminal relay (CLI -> browser).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TerminalMessage {
    /// Terminal output from CLI to browser.
    #[serde(rename = "output")]
    Output { data: String },
    /// Agent list response.
    #[serde(rename = "agents")]
    Agents { agents: Vec<AgentInfo> },
    /// Worktree list response.
    #[serde(rename = "worktrees")]
    Worktrees {
        worktrees: Vec<WorktreeInfo>,
        repo: Option<String>,
    },
    /// Agent selected confirmation.
    #[serde(rename = "agent_selected")]
    AgentSelected { id: String },
    /// Agent created confirmation.
    #[serde(rename = "agent_created")]
    AgentCreated { id: String },
    /// Agent deleted confirmation.
    #[serde(rename = "agent_deleted")]
    AgentDeleted { id: String },
    /// Error message.
    #[serde(rename = "error")]
    Error { message: String },
}

/// Agent info for list response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub id: String,
    pub repo: Option<String>,
    pub issue_number: Option<u64>,
    pub branch_name: Option<String>,
    pub name: Option<String>,
    pub status: Option<String>,
    pub tunnel_port: Option<u16>,
    pub server_running: Option<bool>,
    pub has_server_pty: Option<bool>,
    pub active_pty_view: Option<String>,
    pub scroll_offset: Option<u32>,
    pub hub_identifier: Option<String>,
}

/// Worktree info for list response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub path: String,
    pub branch: String,
    pub issue_number: Option<u64>,
}

/// Browser command types (browser -> CLI).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BrowserCommand {
    /// Terminal input from browser.
    #[serde(rename = "input")]
    Input { data: String },
    /// Set display mode (tui/gui).
    #[serde(rename = "set_mode")]
    SetMode { mode: String },
    /// List all agents.
    #[serde(rename = "list_agents")]
    ListAgents,
    /// List available worktrees.
    #[serde(rename = "list_worktrees")]
    ListWorktrees,
    /// Select an agent.
    #[serde(rename = "select_agent")]
    SelectAgent { id: String },
    /// Create a new agent.
    #[serde(rename = "create_agent")]
    CreateAgent {
        issue_or_branch: Option<String>,
        prompt: Option<String>,
    },
    /// Reopen an existing worktree.
    #[serde(rename = "reopen_worktree")]
    ReopenWorktree {
        path: String,
        branch: String,
        prompt: Option<String>,
    },
    /// Delete an agent.
    #[serde(rename = "delete_agent")]
    DeleteAgent {
        id: String,
        delete_worktree: Option<bool>,
    },
    /// Toggle PTY view (CLI/Server).
    #[serde(rename = "toggle_pty_view")]
    TogglePtyView,
    /// Scroll terminal.
    #[serde(rename = "scroll")]
    Scroll {
        direction: String,
        lines: Option<u32>,
    },
    /// Scroll to bottom (return to live).
    #[serde(rename = "scroll_to_bottom")]
    ScrollToBottom,
    /// Scroll to top.
    #[serde(rename = "scroll_to_top")]
    ScrollToTop,
}

/// Encrypted message envelope (sent via Action Cable).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedEnvelope {
    /// Base64 encrypted data.
    pub blob: String,
    /// Base64 nonce.
    pub nonce: String,
}

/// Browser resize event.
#[derive(Debug, Clone)]
pub struct BrowserResize {
    pub cols: u16,
    pub rows: u16,
}

/// Events received from the browser via the relay.
///
/// These events are parsed from [`BrowserCommand`]s and enriched with
/// connection state (e.g., Connected/Disconnected events).
#[derive(Debug, Clone)]
pub enum BrowserEvent {
    /// Browser connected and sent its public key.
    Connected { public_key: String, device_name: String },
    /// Browser disconnected.
    Disconnected,
    /// Terminal input from browser (already decrypted).
    Input(String),
    /// Browser resized terminal.
    Resize(BrowserResize),
    /// Set display mode (tui/gui).
    SetMode { mode: String },
    /// List all agents.
    ListAgents,
    /// List available worktrees.
    ListWorktrees,
    /// Select an agent.
    SelectAgent { id: String },
    /// Create a new agent.
    CreateAgent {
        issue_or_branch: Option<String>,
        prompt: Option<String>,
    },
    /// Reopen an existing worktree.
    ReopenWorktree {
        path: String,
        branch: String,
        prompt: Option<String>,
    },
    /// Delete an agent.
    DeleteAgent { id: String, delete_worktree: bool },
    /// Toggle PTY view (CLI/Server).
    TogglePtyView,
    /// Scroll terminal.
    Scroll { direction: String, lines: u32 },
    /// Scroll to bottom (return to live).
    ScrollToBottom,
    /// Scroll to top.
    ScrollToTop,
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
}
