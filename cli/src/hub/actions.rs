//! Hub actions - commands that can be dispatched to modify hub state.
//!
//! Actions represent user intent from any input source (TUI, browser, server).
//! The Hub processes actions uniformly regardless of their origin.
//!
//! # Dispatch
//!
//! The `dispatch()` function is the central handler for all actions. It pattern
//! matches on the action type and modifies hub state accordingly.

use std::path::PathBuf;
use std::sync::Arc;

use crate::app::AppMode;

use super::{lifecycle, Hub};

/// Actions that can be dispatched to the Hub.
///
/// These represent high-level user intentions that modify hub state.
/// The Hub's `handle_action()` method processes these uniformly,
/// regardless of whether they came from keyboard input, browser events,
/// or server messages.
///
/// # Example
///
/// ```ignore
/// // From TUI keyboard input
/// let action = HubAction::SelectNext;
/// hub.handle_action(action)?;
///
/// // From browser event
/// let action = HubAction::SpawnAgent { config };
/// hub.handle_action(action)?;
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum HubAction {
    // === Agent Lifecycle ===
    /// Spawn a new agent with the given configuration.
    SpawnAgent {
        /// Issue number (if issue-based).
        issue_number: Option<u32>,
        /// Branch name for the worktree.
        branch_name: String,
        /// Path to the worktree.
        worktree_path: PathBuf,
        /// Path to the main repository.
        repo_path: PathBuf,
        /// Repository name (owner/repo format).
        repo_name: String,
        /// Initial prompt/task description.
        prompt: String,
        /// Server message ID (for acknowledgment).
        message_id: Option<i64>,
        /// Invocation URL (for notifications).
        invocation_url: Option<String>,
    },

    /// Close an agent and optionally delete its worktree.
    CloseAgent {
        /// Session key of the agent to close.
        session_key: String,
        /// Whether to delete the worktree.
        delete_worktree: bool,
    },

    // === Agent Selection ===
    /// Select the next agent in the list.
    SelectNext,

    /// Select the previous agent in the list.
    SelectPrevious,

    /// Select an agent by 1-based index (for keyboard shortcuts 1-9).
    SelectByIndex(usize),

    /// Select an agent by session key.
    SelectByKey(String),

    // === Agent Interaction ===
    /// Send input to the currently selected agent's active PTY.
    SendInput(Vec<u8>),

    /// Toggle between CLI and Server PTY views.
    TogglePtyView,

    /// Scroll the terminal up by the given number of lines.
    ScrollUp(usize),

    /// Scroll the terminal down by the given number of lines.
    ScrollDown(usize),

    /// Scroll to the top of the scrollback buffer.
    ScrollToTop,

    /// Scroll to the bottom (live view).
    ScrollToBottom,

    /// Kill the currently selected agent.
    KillSelectedAgent,

    // === UI State ===
    /// Open the menu overlay.
    OpenMenu,

    /// Close any modal/overlay, return to normal mode.
    CloseModal,

    /// Menu navigation up.
    MenuUp,

    /// Menu navigation down.
    MenuDown,

    /// Select the current menu item.
    MenuSelect(usize),

    /// Show the connection QR code.
    ShowConnectionCode,

    /// Copy connection URL to clipboard.
    CopyConnectionUrl,

    // === Text Input ===
    /// Add a character to the input buffer.
    InputChar(char),

    /// Delete the last character from the input buffer.
    InputBackspace,

    /// Submit the current input buffer.
    InputSubmit,

    /// Clear the input buffer.
    InputClear,

    // === Worktree Selection ===
    /// Navigate up in worktree selection.
    WorktreeUp,

    /// Navigate down in worktree selection.
    WorktreeDown,

    /// Select a worktree for agent creation.
    WorktreeSelect(usize),

    // === Confirmation Dialogs ===
    /// Confirm closing the selected agent (keep worktree).
    ConfirmCloseAgent,

    /// Confirm closing the selected agent and delete worktree.
    ConfirmCloseAgentDeleteWorktree,

    // === Application Control ===
    /// Request application shutdown.
    Quit,

    /// Toggle server message polling.
    TogglePolling,

    /// Refresh available worktrees list.
    RefreshWorktrees,

    /// Handle terminal resize.
    Resize {
        /// New terminal height.
        rows: u16,
        /// New terminal width.
        cols: u16,
    },

    /// No action (used for unhandled inputs).
    None,
}

impl HubAction {
    /// Returns true if this action should be forwarded to the selected agent's PTY.
    pub fn is_pty_input(&self) -> bool {
        matches!(self, HubAction::SendInput(_))
    }

    /// Returns true if this action modifies agent selection.
    pub fn is_selection_change(&self) -> bool {
        matches!(
            self,
            HubAction::SelectNext
                | HubAction::SelectPrevious
                | HubAction::SelectByIndex(_)
                | HubAction::SelectByKey(_)
        )
    }

    /// Returns true if this action affects scroll state.
    pub fn is_scroll_action(&self) -> bool {
        matches!(
            self,
            HubAction::ScrollUp(_)
                | HubAction::ScrollDown(_)
                | HubAction::ScrollToTop
                | HubAction::ScrollToBottom
        )
    }
}

/// Dispatch a hub action, modifying hub state accordingly.
///
/// This is the central dispatch point for all actions. TUI input,
/// browser events, and server messages all eventually become actions
/// that are processed here.
pub fn dispatch(hub: &mut Hub, action: HubAction) {
    match action {
        HubAction::Quit => {
            hub.quit = true;
        }
        HubAction::SelectNext => {
            hub.state.select_next();
        }
        HubAction::SelectPrevious => {
            hub.state.select_previous();
        }
        HubAction::SelectByIndex(index) => {
            hub.state.select_by_index(index);
        }
        HubAction::SelectByKey(key) => {
            hub.state.select_by_key(&key);
        }
        HubAction::TogglePtyView => {
            if let Some(agent) = hub.state.selected_agent_mut() {
                agent.toggle_pty_view();
            }
        }
        HubAction::ScrollUp(lines) => {
            if let Some(agent) = hub.state.selected_agent_mut() {
                agent.scroll_up(lines);
            }
        }
        HubAction::ScrollDown(lines) => {
            if let Some(agent) = hub.state.selected_agent_mut() {
                agent.scroll_down(lines);
            }
        }
        HubAction::ScrollToTop => {
            if let Some(agent) = hub.state.selected_agent_mut() {
                agent.scroll_to_top();
            }
        }
        HubAction::ScrollToBottom => {
            if let Some(agent) = hub.state.selected_agent_mut() {
                agent.scroll_to_bottom();
            }
        }
        HubAction::SendInput(data) => {
            if let Some(agent) = hub.state.selected_agent_mut() {
                if let Err(e) = agent.write_input(&data) {
                    log::error!("Failed to send input to agent: {}", e);
                }
            }
        }
        HubAction::Resize { rows, cols } => {
            hub.terminal_dims = (rows, cols);
            for agent in hub.state.agents.values_mut() {
                agent.resize(rows, cols);
            }
        }
        HubAction::TogglePolling => {
            hub.polling_enabled = !hub.polling_enabled;
        }

        // === Agent Lifecycle ===
        HubAction::SpawnAgent {
            issue_number,
            branch_name,
            worktree_path,
            repo_path,
            repo_name,
            prompt,
            message_id,
            invocation_url,
        } => {
            let config = crate::agents::AgentSpawnConfig {
                issue_number,
                branch_name,
                worktree_path,
                repo_path,
                repo_name,
                prompt,
                message_id,
                invocation_url,
            };
            let dims = hub.browser.dims
                .as_ref()
                .map_or(hub.terminal_dims, |d| (d.rows, d.cols));

            match lifecycle::spawn_agent(&mut hub.state, &config, dims) {
                Ok(result) => {
                    log::info!("Spawned agent: {}", result.session_key);
                    if let Some(port) = result.tunnel_port {
                        let tm = Arc::clone(&hub.tunnel_manager);
                        let key = result.session_key.clone();
                        hub.tokio_runtime.spawn(async move {
                            tm.register_agent(key, port).await;
                        });
                    }
                }
                Err(e) => log::error!("Failed to spawn agent: {}", e),
            }
        }

        HubAction::CloseAgent { session_key, delete_worktree } => {
            if let Err(e) = lifecycle::close_agent(&mut hub.state, &session_key, delete_worktree) {
                log::error!("Failed to close agent {}: {}", session_key, e);
            }
        }

        HubAction::KillSelectedAgent => {
            if let Some(key) = hub.state.selected_session_key().map(String::from) {
                if let Err(e) = lifecycle::close_agent(&mut hub.state, &key, false) {
                    log::error!("Failed to kill agent: {}", e);
                }
            }
        }

        // === UI Mode ===
        HubAction::OpenMenu => {
            hub.mode = AppMode::Menu;
            hub.menu_selected = 0;
        }

        HubAction::CloseModal => {
            hub.mode = AppMode::Normal;
            hub.input_buffer.clear();
        }

        HubAction::ShowConnectionCode => {
            hub.connection_url = Some(format!(
                "{}/hub_connection#key={}&hub={}",
                hub.config.server_url,
                hub.device.public_key_base64url(),
                hub.hub_identifier
            ));
            hub.mode = AppMode::ConnectionCode;
        }

        HubAction::CopyConnectionUrl => {
            if let Some(url) = &hub.connection_url {
                match arboard::Clipboard::new() {
                    Ok(mut clipboard) => {
                        if clipboard.set_text(url.clone()).is_ok() {
                            log::info!("Connection URL copied to clipboard");
                        }
                    }
                    Err(e) => log::warn!("Could not access clipboard: {}", e),
                }
            }
        }

        // === Menu Navigation ===
        HubAction::MenuUp => {
            if hub.menu_selected > 0 {
                hub.menu_selected -= 1;
            }
        }

        HubAction::MenuDown => {
            let menu_ctx = build_menu_context(hub);
            let items = super::menu::build_menu(&menu_ctx);
            let selectable = super::menu::selectable_count(&items);
            if hub.menu_selected < selectable.saturating_sub(1) {
                hub.menu_selected += 1;
            }
        }

        HubAction::MenuSelect(index) => {
            handle_menu_select(hub, index);
        }

        // === Worktree Selection ===
        HubAction::WorktreeUp => {
            if hub.worktree_selected > 0 {
                hub.worktree_selected -= 1;
            }
        }

        HubAction::WorktreeDown => {
            if hub.worktree_selected < hub.state.available_worktrees.len() {
                hub.worktree_selected += 1;
            }
        }

        HubAction::WorktreeSelect(index) => {
            if index == 0 {
                hub.mode = AppMode::NewAgentCreateWorktree;
                hub.input_buffer.clear();
            } else {
                hub.mode = AppMode::NewAgentPrompt;
                hub.input_buffer.clear();
            }
        }

        // === Text Input ===
        HubAction::InputChar(c) => {
            hub.input_buffer.push(c);
        }

        HubAction::InputBackspace => {
            hub.input_buffer.pop();
        }

        HubAction::InputSubmit => {
            handle_input_submit(hub);
        }

        HubAction::InputClear => {
            hub.input_buffer.clear();
        }

        // === Confirmation Dialogs ===
        HubAction::ConfirmCloseAgent => {
            if let Some(key) = hub.state.selected_session_key().map(String::from) {
                let _ = lifecycle::close_agent(&mut hub.state, &key, false);
            }
            hub.mode = AppMode::Normal;
        }

        HubAction::ConfirmCloseAgentDeleteWorktree => {
            if let Some(key) = hub.state.selected_session_key().map(String::from) {
                let _ = lifecycle::close_agent(&mut hub.state, &key, true);
            }
            hub.mode = AppMode::Normal;
        }

        HubAction::RefreshWorktrees => {
            if let Err(e) = hub.load_available_worktrees() {
                log::error!("Failed to refresh worktrees: {}", e);
            }
        }

        HubAction::None => {}
    }
}

/// Build menu context from current hub state.
fn build_menu_context(hub: &Hub) -> super::MenuContext {
    let selected_agent = hub
        .state
        .agent_keys_ordered
        .get(hub.state.selected)
        .and_then(|key| hub.state.agents.get(key));

    super::MenuContext {
        has_agent: selected_agent.is_some(),
        has_server_pty: selected_agent.map_or(false, |a| a.has_server_pty()),
        active_pty: selected_agent.map_or(crate::PtyView::Cli, |a| a.active_pty),
        polling_enabled: hub.polling_enabled,
    }
}

/// Handle menu item selection.
fn handle_menu_select(hub: &mut Hub, selection_index: usize) {
    use super::menu::{build_menu, get_action_for_selection, MenuAction};

    let ctx = build_menu_context(hub);
    let items = build_menu(&ctx);

    let Some(action) = get_action_for_selection(&items, selection_index) else {
        hub.mode = AppMode::Normal;
        return;
    };

    match action {
        MenuAction::TogglePtyView => {
            dispatch(hub, HubAction::TogglePtyView);
            hub.mode = AppMode::Normal;
        }
        MenuAction::CloseAgent => {
            if hub.state.agent_keys_ordered.is_empty() {
                hub.mode = AppMode::Normal;
            } else {
                hub.mode = AppMode::CloseAgentConfirm;
            }
        }
        MenuAction::NewAgent => {
            if let Err(e) = hub.load_available_worktrees() {
                log::error!("Failed to load worktrees: {}", e);
                hub.mode = AppMode::Normal;
            } else {
                hub.mode = AppMode::NewAgentSelectWorktree;
                hub.worktree_selected = 0;
            }
        }
        MenuAction::ShowConnectionCode => {
            dispatch(hub, HubAction::ShowConnectionCode);
        }
        MenuAction::TogglePolling => {
            hub.polling_enabled = !hub.polling_enabled;
            hub.mode = AppMode::Normal;
        }
    }
}

/// Handle input submission based on current mode.
fn handle_input_submit(hub: &mut Hub) {
    match hub.mode {
        AppMode::NewAgentCreateWorktree => {
            if !hub.input_buffer.is_empty() {
                if let Err(e) = create_and_spawn_agent(hub) {
                    log::error!("Failed to create worktree and spawn agent: {}", e);
                }
            }
        }
        AppMode::NewAgentPrompt => {
            if let Err(e) = spawn_agent_from_worktree(hub) {
                log::error!("Failed to spawn agent: {}", e);
            }
        }
        _ => {}
    }
    hub.mode = AppMode::Normal;
    hub.input_buffer.clear();
}

/// Spawn an agent from a selected existing worktree.
fn spawn_agent_from_worktree(hub: &mut Hub) -> anyhow::Result<()> {
    let worktree_index = hub.worktree_selected.saturating_sub(1);

    if let Some((path, branch)) = hub.state.available_worktrees.get(worktree_index).cloned() {
        let issue_number = branch
            .strip_prefix("botster-issue-")
            .and_then(|s| s.parse::<u32>().ok());

        let (repo_path, repo_name) = crate::git::WorktreeManager::detect_current_repo()?;
        let worktree_path = std::path::PathBuf::from(&path);

        let prompt = if hub.input_buffer.is_empty() {
            issue_number.map_or_else(|| format!("Work on {branch}"), |n| format!("Work on issue #{n}"))
        } else {
            hub.input_buffer.clone()
        };

        let config = crate::agents::AgentSpawnConfig {
            issue_number,
            branch_name: branch,
            worktree_path,
            repo_path,
            repo_name,
            prompt,
            message_id: None,
            invocation_url: None,
        };
        spawn_agent_with_tunnel(hub, &config)?;
    }

    Ok(())
}

/// Create a new worktree and spawn an agent on it.
fn create_and_spawn_agent(hub: &mut Hub) -> anyhow::Result<()> {
    let branch_name = hub.input_buffer.trim();

    if branch_name.is_empty() {
        anyhow::bail!("Branch name cannot be empty");
    }

    let (issue_number, actual_branch_name) = if let Ok(num) = branch_name.parse::<u32>() {
        (Some(num), format!("botster-issue-{num}"))
    } else {
        (None, branch_name.to_string())
    };

    let (repo_path, repo_name) = crate::git::WorktreeManager::detect_current_repo()?;
    let worktree_path = hub.state.git_manager.create_worktree_with_branch(&actual_branch_name)?;

    let prompt = issue_number.map_or_else(
        || format!("Work on {actual_branch_name}"),
        |n| format!("Work on issue #{n}"),
    );

    let config = crate::agents::AgentSpawnConfig {
        issue_number,
        branch_name: actual_branch_name,
        worktree_path,
        repo_path,
        repo_name,
        prompt,
        message_id: None,
        invocation_url: None,
    };
    spawn_agent_with_tunnel(hub, &config)?;

    Ok(())
}

/// Helper to spawn an agent and register its tunnel.
fn spawn_agent_with_tunnel(hub: &mut Hub, config: &crate::agents::AgentSpawnConfig) -> anyhow::Result<()> {
    let dims = hub.browser.dims
        .as_ref()
        .map_or(hub.terminal_dims, |d| (d.rows, d.cols));

    let result = lifecycle::spawn_agent(&mut hub.state, config, dims)?;
    if let Some(port) = result.tunnel_port {
        let tm = Arc::clone(&hub.tunnel_manager);
        let key = result.session_key;
        hub.tokio_runtime.spawn(async move {
            tm.register_agent(key, port).await;
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_pty_input() {
        assert!(HubAction::SendInput(vec![b'a']).is_pty_input());
        assert!(!HubAction::SelectNext.is_pty_input());
        assert!(!HubAction::Quit.is_pty_input());
    }

    #[test]
    fn test_is_selection_change() {
        assert!(HubAction::SelectNext.is_selection_change());
        assert!(HubAction::SelectPrevious.is_selection_change());
        assert!(HubAction::SelectByIndex(1).is_selection_change());
        assert!(HubAction::SelectByKey("key".to_string()).is_selection_change());
        assert!(!HubAction::SendInput(vec![]).is_selection_change());
    }

    #[test]
    fn test_is_scroll_action() {
        assert!(HubAction::ScrollUp(1).is_scroll_action());
        assert!(HubAction::ScrollDown(1).is_scroll_action());
        assert!(HubAction::ScrollToTop.is_scroll_action());
        assert!(HubAction::ScrollToBottom.is_scroll_action());
        assert!(!HubAction::SelectNext.is_scroll_action());
    }
}
