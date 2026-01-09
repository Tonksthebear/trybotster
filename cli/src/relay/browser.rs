//! Browser event handling for the Hub.
//!
//! This module provides browser event processing functions that are called from
//! the Hub's event loop. Functions take `&mut Hub` to access state and dispatch actions.
//!
//! # Architecture
//!
//! Browser events flow from the WebSocket relay to these handlers:
//!
//! ```text
//! TerminalRelay → BrowserEvent → browser::poll_events() → Hub state changes
//!                                                       → HubAction dispatch
//!                                                       → Browser responses
//! ```
//!
//! # Functions
//!
//! - [`poll_events`] - Main event loop integration point
//! - [`send_agent_list`] - Send agent list to browser
//! - [`send_worktree_list`] - Send worktree list to browser
//! - [`create_agent`] - Handle browser create agent request
//! - [`reopen_worktree`] - Handle browser reopen worktree request

// Rust guideline compliant 2025-01

use std::path::PathBuf;

use anyhow::Result;

use crate::hub::{actions, Hub, HubAction};
use crate::relay::{BrowserEvent, BrowserSendContext};
use crate::WorktreeManager;

/// Get browser send context if browser is connected.
fn browser_ctx(hub: &Hub) -> Option<BrowserSendContext<'_>> {
    hub.browser.sender.as_ref().map(|sender| BrowserSendContext {
        sender,
        runtime: &hub.tokio_runtime,
    })
}

/// Poll and handle browser events from the terminal relay.
///
/// This is the main integration point between the browser relay and the Hub.
/// Called from the Hub's event loop to process incoming browser events.
///
/// # Arguments
///
/// * `hub` - Mutable reference to the Hub
/// * `_terminal` - Currently unused, kept for API compatibility
///
/// # Errors
///
/// Returns an error if event handling fails.
pub fn poll_events(
    hub: &mut Hub,
    _terminal: &ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
) -> Result<()> {
    let browser_events = hub.browser.drain_events();

    for event in browser_events {
        match event {
            // State changes - handled by BrowserState methods
            BrowserEvent::Connected { device_name, .. } => {
                hub.browser.handle_connected(&device_name);
            }
            BrowserEvent::Disconnected => {
                hub.browser.handle_disconnected();
            }
            BrowserEvent::Resize(resize) => {
                let (rows, cols) = hub.browser.handle_resize(resize);
                for agent in hub.state.agents.values() {
                    agent.resize(rows, cols);
                }
            }
            BrowserEvent::SetMode { mode } => {
                hub.browser.handle_set_mode(&mode);
            }

            // Data requests - respond with current state
            BrowserEvent::ListAgents => {
                send_agent_list(hub);
            }
            BrowserEvent::ListWorktrees => {
                send_worktree_list(hub);
            }

            // Input handling - send raw input to PTY
            BrowserEvent::Input(data) => {
                actions::dispatch(hub, HubAction::SendInput(data.as_bytes().to_vec()));
            }

            // Agent operations - dispatch and notify
            BrowserEvent::SelectAgent { id } => {
                actions::dispatch(hub, HubAction::SelectByKey(id.clone()));
                hub.browser.invalidate_screen();
                send_agent_selected(hub, &id);
                send_scrollback_for_selected_agent(hub);
            }
            BrowserEvent::CreateAgent { issue_or_branch, prompt } => {
                if let Some(input) = issue_or_branch {
                    create_agent(hub, &input, prompt);
                }
            }
            BrowserEvent::ReopenWorktree { path, branch, prompt } => {
                reopen_worktree(hub, &path, &branch, prompt);
            }
            BrowserEvent::DeleteAgent { id, delete_worktree } => {
                actions::dispatch(hub, HubAction::CloseAgent { session_key: id, delete_worktree });
                hub.browser.invalidate_screen();
                send_agent_list(hub);
            }

            // PTY operations - dispatch and invalidate
            BrowserEvent::TogglePtyView => {
                actions::dispatch(hub, HubAction::TogglePtyView);
                hub.browser.invalidate_screen();
                send_agent_list(hub);
            }
            BrowserEvent::Scroll { direction, lines } => {
                let action = match direction.as_str() {
                    "up" => HubAction::ScrollUp(lines as usize),
                    "down" => HubAction::ScrollDown(lines as usize),
                    _ => HubAction::None,
                };
                actions::dispatch(hub, action);
                hub.browser.invalidate_screen();
            }
            BrowserEvent::ScrollToTop => {
                actions::dispatch(hub, HubAction::ScrollToTop);
                hub.browser.invalidate_screen();
            }
            BrowserEvent::ScrollToBottom => {
                actions::dispatch(hub, HubAction::ScrollToBottom);
                hub.browser.invalidate_screen();
            }
        }
    }

    Ok(())
}

/// Send agent list to browser.
///
/// Collects agent information and sends it to the connected browser client.
pub fn send_agent_list(hub: &Hub) {
    let Some(ctx) = browser_ctx(hub) else { return };

    let agents = hub.state.agent_keys_ordered.iter()
        .filter_map(|key| hub.state.agents.get(key).map(|a| (key, a)))
        .map(|(id, a)| crate::relay::build_agent_info(id, a, &hub.hub_identifier))
        .collect();

    crate::relay::send_agent_list(&ctx, agents);
}

/// Send worktree list to browser.
///
/// Loads and sends available worktree information to the connected browser client.
pub fn send_worktree_list(hub: &mut Hub) {
    // Load worktrees fresh (they may not have been loaded yet)
    if let Err(e) = hub.load_available_worktrees() {
        log::warn!("Failed to load worktrees: {}", e);
    }

    // Get browser context after loading worktrees (borrow checker)
    let Some(ctx) = browser_ctx(hub) else { return };

    let worktrees = hub.state.available_worktrees.iter()
        .map(|(path, branch)| crate::relay::build_worktree_info(path, branch))
        .collect();

    crate::relay::send_worktree_list(&ctx, worktrees);
}

/// Send selected agent notification to browser.
///
/// Notifies the browser that an agent has been selected.
pub fn send_agent_selected(hub: &Hub, agent_id: &str) {
    let Some(ctx) = browser_ctx(hub) else { return };
    crate::relay::send_agent_selected(&ctx, agent_id);
}

/// Send scrollback history for the selected agent to browser.
///
/// Called when an agent is selected so the browser can populate
/// xterm's scrollback buffer with historical output.
pub fn send_scrollback_for_selected_agent(hub: &Hub) {
    let Some(ctx) = browser_ctx(hub) else { return };
    let Some(agent) = hub.state.selected_agent() else { return };

    let lines = agent.get_buffer_snapshot();
    log::info!("Sending {} scrollback lines to browser", lines.len());
    crate::relay::send_scrollback(&ctx, lines);
}

/// Handle browser create agent request.
///
/// Creates a new worktree and spawns an agent based on browser input.
/// The input can be either an issue number or a branch name.
///
/// # Arguments
///
/// * `hub` - Mutable reference to the Hub
/// * `input` - Issue number or branch name from the browser
/// * `prompt` - Optional custom prompt for the agent
pub fn create_agent(hub: &mut Hub, input: &str, prompt: Option<String>) {
    let branch_name = input.trim();
    if branch_name.is_empty() {
        return;
    }

    let (issue_number, actual_branch_name) = if let Ok(num) = branch_name.parse::<u32>() {
        (Some(num), format!("botster-issue-{}", num))
    } else {
        (None, branch_name.to_string())
    };

    let (repo_path, repo_name) = match WorktreeManager::detect_current_repo() {
        Ok(result) => result,
        Err(e) => {
            log::error!("Failed to detect repo: {}", e);
            return;
        }
    };

    let worktree_path = match hub.state.git_manager.create_worktree_with_branch(&actual_branch_name) {
        Ok(path) => path,
        Err(e) => {
            log::error!("Failed to create worktree: {}", e);
            return;
        }
    };

    let final_prompt = prompt.unwrap_or_else(|| {
        issue_number.map_or_else(
            || format!("Work on {}", actual_branch_name),
            |num| format!("Work on issue #{}", num),
        )
    });

    actions::dispatch(hub, HubAction::SpawnAgent {
        issue_number,
        branch_name: actual_branch_name,
        worktree_path,
        repo_path,
        repo_name,
        prompt: final_prompt,
        message_id: None,
        invocation_url: None,
    });

    hub.browser.last_screen_hash = None;
    send_agent_list(hub);

    // Select newly created agent
    if let Some(key) = hub.state.agent_keys_ordered.last().cloned() {
        hub.state.selected = hub.state.agent_keys_ordered.len() - 1;
        send_agent_selected(hub, &key);
    }
}

/// Handle browser reopen worktree request.
///
/// Reopens an existing worktree and spawns an agent on it.
///
/// # Arguments
///
/// * `hub` - Mutable reference to the Hub
/// * `path` - Path to the existing worktree
/// * `branch` - Branch name of the worktree
/// * `prompt` - Optional custom prompt for the agent
pub fn reopen_worktree(hub: &mut Hub, path: &str, branch: &str, prompt: Option<String>) {
    let issue_number = branch.strip_prefix("botster-issue-")
        .and_then(|s| s.parse::<u32>().ok());

    let (repo_path, repo_name) = match WorktreeManager::detect_current_repo() {
        Ok(result) => result,
        Err(e) => {
            log::error!("Failed to detect repo: {}", e);
            return;
        }
    };

    let final_prompt = prompt.unwrap_or_else(|| {
        issue_number.map_or_else(
            || format!("Work on {}", branch),
            |num| format!("Work on issue #{}", num),
        )
    });

    actions::dispatch(hub, HubAction::SpawnAgent {
        issue_number,
        branch_name: branch.to_string(),
        worktree_path: PathBuf::from(path),
        repo_path,
        repo_name,
        prompt: final_prompt,
        message_id: None,
        invocation_url: None,
    });

    hub.browser.last_screen_hash = None;
    send_agent_list(hub);

    if let Some(key) = hub.state.agent_keys_ordered.last().cloned() {
        hub.state.selected = hub.state.agent_keys_ordered.len() - 1;
        send_agent_selected(hub, &key);
    }
}

/// Send output to browser via E2E encrypted relay.
///
/// For TUI mode (if ever needed): sends rendered TUI.
/// For GUI mode: sends raw PTY bytes so xterm.js can handle scrollback.
pub fn send_output(hub: &Hub, _ansi_output: &str) {
    let Some(ctx) = browser_ctx(hub) else { return };
    if !hub.browser.connected { return; }

    // Always stream raw PTY output to browser
    // This lets xterm.js handle scrollback naturally
    let Some(agent) = hub.state.selected_agent() else { return };

    let raw_bytes = agent.drain_raw_output();
    if raw_bytes.is_empty() {
        return;
    }

    // Convert to string (lossy - invalid UTF-8 becomes replacement chars)
    let output = String::from_utf8_lossy(&raw_bytes);
    crate::relay::state::send_output(&ctx, &output);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::types::BrowserCommand;

    /// Verify BrowserCommand::Input -> BrowserEvent::Input mapping.
    /// This is critical for keyboard input from browser to reach CLI.
    #[test]
    fn test_browser_command_input_converts_to_event() {
        let json = r#"{"type":"input","data":"hello world"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();

        // The conversion happens in connection.rs, but we verify the type structure
        match cmd {
            BrowserCommand::Input { data } => {
                assert_eq!(data, "hello world");
                // In connection.rs line 402, this becomes BrowserEvent::Input(data)
            }
            _ => panic!("Expected Input variant"),
        }
    }

    /// Verify BrowserCommand::Scroll -> BrowserEvent::Scroll mapping.
    #[test]
    fn test_browser_command_scroll_converts_to_event() {
        let json = r#"{"type":"scroll","direction":"up","lines":10}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();

        match cmd {
            BrowserCommand::Scroll { direction, lines } => {
                assert_eq!(direction, "up");
                assert_eq!(lines, Some(10));
            }
            _ => panic!("Expected Scroll variant"),
        }
    }

    /// Verify BrowserCommand::Resize -> BrowserEvent::Resize mapping.
    #[test]
    fn test_browser_command_resize_converts_to_event() {
        let json = r#"{"type":"resize","cols":120,"rows":40}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();

        match cmd {
            BrowserCommand::Resize { cols, rows } => {
                assert_eq!(cols, 120);
                assert_eq!(rows, 40);
                // In connection.rs line 425-427, this becomes:
                // BrowserEvent::Resize(BrowserResize { cols, rows })
            }
            _ => panic!("Expected Resize variant"),
        }
    }

    /// Verify BrowserCommand::SetMode parsing for gui mode.
    #[test]
    fn test_browser_command_set_mode_gui() {
        let json = r#"{"type":"set_mode","mode":"gui"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();

        match cmd {
            BrowserCommand::SetMode { mode } => {
                assert_eq!(mode, "gui");
            }
            _ => panic!("Expected SetMode variant"),
        }
    }

    /// Test the actual event handling in poll_events would require a full Hub,
    /// which is tested in hub/actions.rs. This module tests the parsing layer.

    /// Verify browser input with special characters (Ctrl+C, etc.)
    #[test]
    fn test_browser_command_input_with_control_chars() {
        // Ctrl+C is \x03
        let json = r#"{"type":"input","data":"\u0003"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();

        match cmd {
            BrowserCommand::Input { data } => {
                assert_eq!(data, "\x03");
            }
            _ => panic!("Expected Input variant"),
        }
    }

    /// Verify browser input with escape sequences (arrow keys, etc.)
    #[test]
    fn test_browser_command_input_with_escape_sequences() {
        // Arrow up is \x1b[A
        let json = r#"{"type":"input","data":"\u001b[A"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();

        match cmd {
            BrowserCommand::Input { data } => {
                assert_eq!(data, "\x1b[A");
            }
            _ => panic!("Expected Input variant"),
        }
    }
}
