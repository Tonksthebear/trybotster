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
//! - [`handle_input`] - Terminal input parsing and dispatch
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
/// * `terminal` - Reference to the terminal for size queries
///
/// # Errors
///
/// Returns an error if input handling fails.
pub fn poll_events(
    hub: &mut Hub,
    terminal: &ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
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

            // Input handling - parse and dispatch
            BrowserEvent::Input(data) => {
                handle_input(hub, &data, terminal)?;
            }

            // Agent operations - dispatch and notify
            BrowserEvent::SelectAgent { id } => {
                actions::dispatch(hub, HubAction::SelectByKey(id.clone()));
                hub.browser.invalidate_screen();
                send_agent_selected(hub, &id);
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

/// Handle browser input by parsing terminal escape sequences and dispatching.
///
/// Parses raw terminal input from the browser and converts it to `HubAction`s.
/// Filters out Quit actions to prevent accidental browser disconnections.
///
/// # Arguments
///
/// * `hub` - Mutable reference to the Hub
/// * `data` - Raw terminal input data
/// * `terminal` - Reference to the terminal for size queries
///
/// # Errors
///
/// Returns an error if terminal size query fails.
pub fn handle_input(
    hub: &mut Hub,
    data: &str,
    terminal: &ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
) -> Result<()> {
    use crate::app::parse_terminal_input;
    use crate::constants;
    use crate::tui;

    let keys = parse_terminal_input(data);
    for (code, modifiers) in keys {
        // Build context for input dispatch
        let context = tui::InputContext {
            terminal_rows: terminal.size()?.height,
            menu_selected: hub.menu_selected,
            menu_count: constants::MENU_ITEMS.len(),
            worktree_selected: hub.worktree_selected,
            worktree_count: hub.state.available_worktrees.len(),
        };

        // Use tui's key_event_to_action for consistency
        let key_event = crossterm::event::KeyEvent::new(code, modifiers);
        if let Some(action) = tui::input::key_event_to_action(&key_event, &hub.mode, &context) {
            // Skip Quit from browser to prevent accidental disconnects
            if !matches!(action, HubAction::Quit) {
                actions::dispatch(hub, action);
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
/// Collects available worktree information and sends it to the connected browser client.
pub fn send_worktree_list(hub: &Hub) {
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
/// Sends terminal output to the connected browser, selecting the appropriate
/// content based on the current browser mode.
pub fn send_output(hub: &Hub, ansi_output: &str) {
    let Some(ctx) = browser_ctx(hub) else { return };
    if !hub.browser.connected { return; }

    let agent_output = hub.state.selected_agent().map(crate::agent::Agent::get_screen_as_ansi);
    let output = crate::relay::state::get_output_for_mode(hub.browser.mode, ansi_output, agent_output);
    crate::relay::state::send_output(&ctx, &output);
}

#[cfg(test)]
mod tests {
    // Browser functions require runtime integration and are tested via hub tests
}
