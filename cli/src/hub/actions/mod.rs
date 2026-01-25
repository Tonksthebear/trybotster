//! Hub actions - commands that can be dispatched to modify hub state.
//!
//! Actions represent user intent from any input source (TUI, browser, server).
//! The Hub processes actions uniformly regardless of their origin.
//!
//! # Module Structure
//!
//! - `mod.rs` - HubAction enum and dispatch() routing
//! - `agent_handlers.rs` - Agent lifecycle: spawn, close, kill
//! - `client_handlers.rs` - Client-scoped: selection, input, resize
//! - `menu_handlers.rs` - Menu/modal navigation and selection
//! - `input_handlers.rs` - Text input processing and agent creation
//!
//! # Dispatch
//!
//! The `dispatch()` function is the central handler for all actions. It pattern
//! matches on the action type and routes to the appropriate handler module.
//!
//! # Client-Scoped Actions
//!
//! Actions that operate on a specific client's view include a `client_id` field.
//! This enables TUI and browsers to independently select and interact with agents.

mod agent_handlers;
mod client_handlers;
mod input_handlers;
mod menu_handlers;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use crate::app::AppMode;
use crate::client::{ClientId, CreateAgentRequest, DeleteAgentRequest};

use super::{lifecycle, Hub};

// Re-export handler functions that need to be called from other modules
pub use client_handlers::handle_select_agent_for_client;
pub use input_handlers::spawn_agent_with_tunnel;

/// Scroll direction for client-scoped scroll actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrollDirection {
    /// Scroll up by N lines.
    Up(usize),
    /// Scroll down by N lines.
    Down(usize),
    /// Scroll to top of buffer.
    ToTop,
    /// Scroll to bottom (live view).
    ToBottom,
}

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
/// // From browser event
/// let action = HubAction::SpawnAgent { config };
/// hub.handle_action(action)?;
///
/// // Client-scoped selection (TUI or browser)
/// let action = HubAction::SelectAgentForClient { client_id, agent_key };
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

    // === Agent Interaction ===
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

    /// Regenerate the connection QR code with a fresh PreKey.
    RegenerateConnectionCode,

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

    /// Refresh available worktrees list.
    RefreshWorktrees,

    /// No action (used for unhandled inputs).
    None,

    // === Client-Scoped Actions ===
    // These include client_id for per-client agent selection.
    // Used by both TUI and browser clients.
    /// Select an agent for a specific client.
    /// Replaces SelectNext/SelectPrevious/SelectByIndex/SelectByKey for client-aware selection.
    SelectAgentForClient {
        /// Which client is selecting.
        client_id: ClientId,
        /// Agent session key to select.
        agent_key: String,
    },

    /// Send input to the client's selected agent.
    /// Client-scoped version of SendInput.
    SendInputForClient {
        /// Which client is sending input.
        client_id: ClientId,
        /// Input data.
        data: Vec<u8>,
    },

    /// Resize the client's terminal and optionally the viewed agent's PTY.
    ResizeForClient {
        /// Which client is resizing.
        client_id: ClientId,
        /// New terminal width.
        cols: u16,
        /// New terminal height.
        rows: u16,
    },

    /// Create a new agent (client-scoped for response routing).
    CreateAgentForClient {
        /// Which client is requesting creation.
        client_id: ClientId,
        /// Creation request details.
        request: CreateAgentRequest,
    },

    /// Delete an agent (client-scoped for response routing and viewer cleanup).
    DeleteAgentForClient {
        /// Which client is requesting deletion.
        client_id: ClientId,
        /// Deletion request details.
        request: DeleteAgentRequest,
    },

    /// Request agent list (client-scoped for response routing).
    RequestAgentList {
        /// Which client is requesting.
        client_id: ClientId,
    },

    /// Request worktree list (client-scoped for response routing).
    RequestWorktreeList {
        /// Which client is requesting.
        client_id: ClientId,
    },

    /// Scroll the client's selected agent's terminal.
    /// Client-scoped version of ScrollUp/ScrollDown/ScrollToTop/ScrollToBottom.
    ScrollForClient {
        /// Which client is scrolling.
        client_id: ClientId,
        /// Scroll direction and amount.
        scroll: ScrollDirection,
    },

    /// Toggle PTY view for the client's selected agent.
    /// Client-scoped version of TogglePtyView.
    TogglePtyViewForClient {
        /// Which client is toggling.
        client_id: ClientId,
    },

    // === Client Lifecycle ===
    /// A client has connected (browser handshake completed).
    ClientConnected {
        /// ID of the connected client.
        client_id: ClientId,
    },

    /// A client has disconnected.
    ClientDisconnected {
        /// ID of the disconnected client.
        client_id: ClientId,
    },
}


/// Dispatch a hub action, modifying hub state accordingly.
///
/// This is the central dispatch point for all actions. TUI input,
/// browser events, and server messages all eventually become actions
/// that are processed here.
pub fn dispatch(hub: &mut Hub, action: HubAction) {
    match action {
        // === Simple Inline Handlers (1-3 lines) ===
        HubAction::Quit => {
            hub.quit = true;
        }
        HubAction::None => {}

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
            agent_handlers::handle_spawn_agent(
                hub,
                issue_number,
                branch_name,
                worktree_path,
                repo_path,
                repo_name,
                prompt,
                message_id,
                invocation_url,
            );
        }

        HubAction::CloseAgent {
            session_key,
            delete_worktree,
        } => {
            agent_handlers::handle_close_agent(hub, &session_key, delete_worktree);
        }

        HubAction::KillSelectedAgent => {
            agent_handlers::handle_kill_selected_agent(hub);
        }

        // === UI Mode ===
        HubAction::OpenMenu => {
            hub.mode = AppMode::Menu;
            hub.menu_selected = 0;
        }

        HubAction::CloseModal => {
            menu_handlers::handle_close_modal(hub);
        }

        HubAction::ShowConnectionCode => {
            menu_handlers::handle_show_connection_code(hub);
        }

        HubAction::CopyConnectionUrl => {
            menu_handlers::handle_copy_connection_url(hub);
        }

        HubAction::RegenerateConnectionCode => {
            menu_handlers::handle_regenerate_connection_code(hub);
        }

        // === Menu Navigation ===
        HubAction::MenuUp => {
            menu_handlers::handle_menu_up(hub);
        }

        HubAction::MenuDown => {
            menu_handlers::handle_menu_down(hub);
        }

        HubAction::MenuSelect(index) => {
            menu_handlers::handle_menu_select(hub, index);
        }

        // === Worktree Selection ===
        HubAction::WorktreeUp => {
            if hub.worktree_selected > 0 {
                hub.worktree_selected -= 1;
            }
        }

        HubAction::WorktreeDown => {
            if hub.worktree_selected < hub.state.read().unwrap().available_worktrees.len() {
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
            input_handlers::handle_input_submit(hub);
        }

        HubAction::InputClear => {
            hub.input_buffer.clear();
        }

        // === Confirmation Dialogs ===
        HubAction::ConfirmCloseAgent => {
            // Uses TUI client's selection
            if let Some(key) = hub.get_tui_selected_agent_key() {
                let _ = lifecycle::close_agent(&mut hub.state.write().unwrap(), &key, false);
            }
            hub.mode = AppMode::Normal;
        }

        HubAction::ConfirmCloseAgentDeleteWorktree => {
            // Uses TUI client's selection
            if let Some(key) = hub.get_tui_selected_agent_key() {
                let _ = lifecycle::close_agent(&mut hub.state.write().unwrap(), &key, true);
            }
            hub.mode = AppMode::Normal;
        }

        HubAction::RefreshWorktrees => {
            if let Err(e) = hub.load_available_worktrees() {
                log::error!("Failed to refresh worktrees: {}", e);
            }
        }

        // === Client-Scoped Actions ===
        HubAction::SelectAgentForClient {
            client_id,
            agent_key,
        } => {
            client_handlers::handle_select_agent_for_client(hub, client_id, agent_key);
        }

        HubAction::SendInputForClient { client_id, data } => {
            client_handlers::handle_send_input_for_client(hub, client_id, data);
        }

        HubAction::ResizeForClient {
            client_id,
            cols,
            rows,
        } => {
            client_handlers::handle_resize_for_client(hub, client_id, cols, rows);
        }

        HubAction::CreateAgentForClient { client_id, request } => {
            client_handlers::handle_create_agent_for_client(hub, client_id, request);
        }

        HubAction::DeleteAgentForClient { client_id, request } => {
            client_handlers::handle_delete_agent_for_client(hub, client_id, request);
        }

        HubAction::RequestAgentList { client_id } => {
            // For browser clients, use targeted send via relay
            if let Some(identity) = client_id.browser_identity() {
                crate::relay::browser::send_agent_list_to_browser(hub, identity);
            } else {
                hub.send_agent_list_to(&client_id);
            }
        }

        HubAction::RequestWorktreeList { client_id } => {
            // For browser clients, use targeted send via relay
            if let Some(identity) = client_id.browser_identity() {
                crate::relay::browser::send_worktree_list_to_browser(hub, identity);
            } else {
                hub.send_worktree_list_to(&client_id);
            }
        }

        HubAction::ClientConnected { client_id } => {
            client_handlers::handle_client_connected(hub, client_id);
        }

        HubAction::ClientDisconnected { client_id } => {
            client_handlers::handle_client_disconnected(hub, client_id);
        }

        HubAction::ScrollForClient { client_id, scroll } => {
            // Scroll state is client-local (TuiClient owns scroll offset, xterm.js for browser).
            // This action is dispatched from browser events but handled client-side.
            log::debug!(
                "ScrollForClient from {}: {:?} (handled client-side)",
                client_id,
                scroll
            );
        }

        HubAction::TogglePtyViewForClient { client_id } => {
            // PTY view (CLI vs Server) is client-local state.
            // This action is dispatched from browser events but handled client-side.
            log::debug!(
                "TogglePtyViewForClient from {} (handled client-side)",
                client_id
            );
        }
    }
}
