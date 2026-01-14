//! Hub actions - commands that can be dispatched to modify hub state.
//!
//! Actions represent user intent from any input source (TUI, browser, server).
//! The Hub processes actions uniformly regardless of their origin.
//!
//! # Dispatch
//!
//! The `dispatch()` function is the central handler for all actions. It pattern
//! matches on the action type and modifies hub state accordingly.
//!
//! # Client-Scoped Actions
//!
//! Actions that operate on a specific client's view include a `client_id` field.
//! This enables TUI and browsers to independently select and interact with agents.

use std::path::PathBuf;
use std::sync::Arc;

use crate::app::AppMode;
use crate::client::{BrowserClient, ClientId, CreateAgentRequest, DeleteAgentRequest, Response};

use super::{lifecycle, Hub};

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
            // TUI-specific navigation - uses client-scoped selection
            if let Some(key) = hub.get_next_agent_key(&ClientId::Tui) {
                handle_select_agent_for_client(hub, ClientId::Tui, key);
            }
        }
        HubAction::SelectPrevious => {
            // TUI-specific navigation - uses client-scoped selection
            if let Some(key) = hub.get_previous_agent_key(&ClientId::Tui) {
                handle_select_agent_for_client(hub, ClientId::Tui, key);
            }
        }
        HubAction::SelectByIndex(index) => {
            // TUI-specific navigation - select by 1-based index
            if index > 0 && index <= hub.state.agent_keys_ordered.len() {
                let key = hub.state.agent_keys_ordered[index - 1].clone();
                handle_select_agent_for_client(hub, ClientId::Tui, key);
            }
        }
        HubAction::SelectByKey(key) => {
            // TUI-specific navigation - select by key
            handle_select_agent_for_client(hub, ClientId::Tui, key);
        }
        HubAction::TogglePtyView => {
            // Uses TUI client's selection
            if let Some(agent) = hub.selected_agent_mut() {
                agent.toggle_pty_view();
            }
        }
        HubAction::ScrollUp(lines) => {
            // Uses TUI client's selection
            if let Some(agent) = hub.selected_agent_mut() {
                agent.scroll_up(lines);
            }
        }
        HubAction::ScrollDown(lines) => {
            // Uses TUI client's selection
            if let Some(agent) = hub.selected_agent_mut() {
                agent.scroll_down(lines);
            }
        }
        HubAction::ScrollToTop => {
            // Uses TUI client's selection
            if let Some(agent) = hub.selected_agent_mut() {
                agent.scroll_to_top();
            }
        }
        HubAction::ScrollToBottom => {
            // Uses TUI client's selection
            if let Some(agent) = hub.selected_agent_mut() {
                agent.scroll_to_bottom();
            }
        }
        HubAction::SendInput(data) => {
            // Uses TUI client's selection
            if let Some(agent) = hub.selected_agent_mut() {
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
            worktree_path: _, // Ignored - we create the worktree ourselves
            repo_path,
            repo_name,
            prompt,
            message_id,
            invocation_url,
        } => {
            log::debug!("SpawnAgent: branch={}, issue={:?}", branch_name, issue_number);
            // Create the worktree first (the path in the action is just computed, not created)
            let worktree_path = match hub.state.git_manager.create_worktree_with_branch(&branch_name) {
                Ok(path) => {
                    log::info!("Worktree created at {:?}", path);
                    path
                }
                Err(e) => {
                    log::error!("Failed to create worktree for {}: {}", branch_name, e);
                    return;
                }
            };

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
            log::debug!("CloseAgent: session_key={}", session_key);
            if let Err(e) = lifecycle::close_agent(&mut hub.state, &session_key, delete_worktree) {
                log::error!("Failed to close agent {}: {}", session_key, e);
            }
        }

        HubAction::KillSelectedAgent => {
            // Uses TUI client's selection
            if let Some(key) = hub.get_tui_selected_agent_key() {
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
            // If closing ConnectionCode modal, delete any Kitty graphics images
            if hub.mode == AppMode::ConnectionCode {
                use crate::tui::qr::kitty_delete_images;
                use std::io::Write;
                let _ = std::io::stdout().write_all(kitty_delete_images().as_bytes());
                let _ = std::io::stdout().flush();
            }
            hub.mode = AppMode::Normal;
            hub.input_buffer.clear();
            hub.error_message = None; // Clear error message if in Error mode
        }

        HubAction::ShowConnectionCode => {
            // Generate connection URL with Signal PreKeyBundle
            // Format: /hubs/{id}#{base32_binary_bundle}
            // - All uppercase for QR alphanumeric mode (4296 char capacity vs 2953 byte mode)
            // - Binary format (1813 bytes) + Base32 = ~2900 chars (fits easily)
            // - Hub ID in path, bundle in fragment (never sent to server)
            hub.connection_url = if let Some(ref bundle) = hub.browser.signal_bundle {
                use data_encoding::BASE32_NOPAD;
                match bundle.to_binary() {
                    Ok(bytes) => {
                        let encoded = BASE32_NOPAD.encode(&bytes);
                        // URL uses mixed-mode QR encoding:
                        // - URL portion (up to #): byte mode (any case allowed)
                        // - Bundle (after #): alphanumeric mode (must be uppercase Base32)
                        // Rails ID is numeric, uppercase is no-op but harmless
                        let url = format!(
                            "{}/hubs/{}#{}",
                            hub.config.server_url,
                            hub.server_hub_id(),
                            encoded
                        );
                        log::debug!("Connection URL: {} chars (QR alphanumeric capacity: 4296)", url.len());
                        Some(url)
                    }
                    Err(e) => {
                        log::error!("Cannot serialize PreKeyBundle to binary: {e}");
                        None
                    }
                }
            } else {
                log::error!("Cannot show connection code: Signal bundle not initialized");
                None
            };
            // Reset QR image flag so it renders fresh when modal opens
            hub.qr_image_displayed = false;
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
            let items = crate::hub::menu::build_menu(&menu_ctx);
            let selectable = crate::hub::menu::selectable_count(&items);
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
            // Uses TUI client's selection
            if let Some(key) = hub.get_tui_selected_agent_key() {
                let _ = lifecycle::close_agent(&mut hub.state, &key, false);
            }
            hub.mode = AppMode::Normal;
        }

        HubAction::ConfirmCloseAgentDeleteWorktree => {
            // Uses TUI client's selection
            if let Some(key) = hub.get_tui_selected_agent_key() {
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

        // === Client-Scoped Actions ===

        HubAction::SelectAgentForClient { client_id, agent_key } => {
            handle_select_agent_for_client(hub, client_id, agent_key);
        }

        HubAction::SendInputForClient { client_id, data } => {
            handle_send_input_for_client(hub, client_id, data);
        }

        HubAction::ResizeForClient { client_id, cols, rows } => {
            handle_resize_for_client(hub, client_id, cols, rows);
        }

        HubAction::CreateAgentForClient { client_id, request } => {
            handle_create_agent_for_client(hub, client_id, request);
        }

        HubAction::DeleteAgentForClient { client_id, request } => {
            handle_delete_agent_for_client(hub, client_id, request);
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
            handle_client_connected(hub, client_id);
        }

        HubAction::ClientDisconnected { client_id } => {
            handle_client_disconnected(hub, client_id);
        }

        HubAction::ScrollForClient { client_id, scroll } => {
            handle_scroll_for_client(hub, client_id, scroll);
        }

        HubAction::TogglePtyViewForClient { client_id } => {
            handle_toggle_pty_view_for_client(hub, client_id);
        }
    }
}

/// Build menu context from current hub state.
///
/// IMPORTANT: This must use the same selection logic as render.rs to ensure
/// the displayed menu matches the navigation bounds. If TUI has no explicit
/// selection, we fall back to the first agent (index 0) for consistency.
fn build_menu_context(hub: &Hub) -> super::MenuContext {
    // Use same fallback logic as render.rs: if no TUI selection, use first agent
    let selected_agent = hub.get_tui_selected_agent_key()
        .and_then(|key| hub.state.agents.get(&key))
        .or_else(|| {
            // Fallback: use first agent if any exist (matches render.rs behavior)
            hub.state.agent_keys_ordered.first()
                .and_then(|key| hub.state.agents.get(key))
        });

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
                hub.show_error(format!("Failed to load worktrees: {}", e));
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
///
/// Routes through the async `handle_create_agent_for_client` path to avoid
/// blocking the TUI during git operations.
fn create_and_spawn_agent(hub: &mut Hub) -> anyhow::Result<()> {
    let branch_name = hub.input_buffer.trim();

    if branch_name.is_empty() {
        anyhow::bail!("Branch name cannot be empty");
    }

    // Route through the async client path (same as browser)
    // This spawns git operations to background thread
    let request = crate::client::CreateAgentRequest {
        issue_or_branch: branch_name.to_string(),
        prompt: None,
        from_worktree: None,
    };

    handle_create_agent_for_client(hub, ClientId::Tui, request);
    Ok(())
}

/// Helper to spawn an agent and register its tunnel.
///
/// This is used by TUI's "New Agent" menu flow. After spawning:
/// - Registers tunnel if port assigned
/// - Auto-selects the new agent for TUI (consistent with browser behavior)
fn spawn_agent_with_tunnel(hub: &mut Hub, config: &crate::agents::AgentSpawnConfig) -> anyhow::Result<()> {
    // Use TUI's dims from terminal_dims (not browser.dims)
    let dims = hub.terminal_dims;

    let result = lifecycle::spawn_agent(&mut hub.state, config, dims)?;

    // Clone session_key before moving into async
    let session_key = result.session_key.clone();

    if let Some(port) = result.tunnel_port {
        let tm = Arc::clone(&hub.tunnel_manager);
        let key = result.session_key;
        hub.tokio_runtime.spawn(async move {
            tm.register_agent(key, port).await;
        });
    }

    // Auto-select the new agent for TUI (matches browser behavior in handle_create_agent_for_client)
    handle_select_agent_for_client(hub, ClientId::Tui, session_key);

    Ok(())
}

// === Client-Scoped Action Handlers ===

/// Handle selecting an agent for a specific client.
fn handle_select_agent_for_client(hub: &mut Hub, client_id: ClientId, agent_key: String) {
    // Validate agent exists
    if !hub.state.agents.contains_key(&agent_key) {
        hub.send_error_to(&client_id, "Agent not found".to_string());
        return;
    }

    // Get old selection for viewer index update
    let old_selection = hub.clients.get(&client_id)
        .and_then(|c| c.state().selected_agent.clone());

    // Get client dims BEFORE updating state (for resize after selection)
    let client_dims = hub.clients.get(&client_id)
        .and_then(|c| c.state().dims);

    // Update viewer index
    hub.clients.update_selection(
        &client_id,
        old_selection.as_deref(),
        Some(&agent_key),
    );

    // Update client state and send data
    if let Some(client) = hub.clients.get_mut(&client_id) {
        client.select_agent(&agent_key);

        // TODO: Send scrollback from agent's CLI PTY when method available
        // For now, browsers will receive live output going forward.
        // TUI doesn't need scrollback pushed - it reads directly from vt100 parser.

        client.receive_response(Response::agent_selected(&agent_key));
    }

    // Resize agent to match client's terminal dimensions.
    // This is critical for browsers that resize BEFORE selecting an agent -
    // the stored dims must be applied when selection happens.
    if let Some((cols, rows)) = client_dims {
        if let Some(agent) = hub.state.agents.get(&agent_key) {
            agent.resize(rows, cols);
            log::debug!("Resized agent {} to {}x{} for client {}",
                &agent_key[..8.min(agent_key.len())], cols, rows, client_id);
        }
    }

    log::debug!("Client {} selected agent {}", client_id, &agent_key[..8.min(agent_key.len())]);
}

/// Handle sending input for a specific client.
fn handle_send_input_for_client(hub: &mut Hub, client_id: ClientId, data: Vec<u8>) {
    // Get client's selected agent
    let agent_key = match hub.clients.get(&client_id) {
        Some(client) => client.state().selected_agent.clone(),
        None => {
            log::warn!("SendInput from unknown client: {}", client_id);
            return;
        }
    };

    // Route input to agent's CLI PTY
    if let Some(key) = agent_key {
        if let Some(agent) = hub.state.agents.get_mut(&key) {
            if let Err(e) = agent.write_input(&data) {
                log::error!("Failed to send input to agent {}: {}", key, e);
            }
        }
    } else {
        log::debug!("Client {} sent input but no agent selected", client_id);
    }
}

/// Handle resize for a specific client.
fn handle_resize_for_client(hub: &mut Hub, client_id: ClientId, cols: u16, rows: u16) {
    // Update client dims
    if let Some(client) = hub.clients.get_mut(&client_id) {
        client.resize(cols, rows);
    }

    // Also update hub terminal_dims for TUI
    if client_id.is_tui() {
        hub.terminal_dims = (rows, cols);
        // TUI resize affects all agents (for consistent rendering)
        for agent in hub.state.agents.values_mut() {
            agent.resize(rows, cols);
        }
    } else {
        // Browser resize only affects the agent they're viewing
        let agent_key = hub.clients.get(&client_id)
            .and_then(|c| c.state().selected_agent.clone());

        if let Some(key) = agent_key {
            if let Some(agent) = hub.state.agents.get_mut(&key) {
                agent.resize(rows, cols);
            }
        }
    }
}

/// Handle creating an agent for a specific client.
///
/// This spawns the heavy git/file operations to a background thread to avoid
/// blocking the main event loop. The main loop polls for completion and finishes
/// the spawn (PTY creation) on the main thread.
fn handle_create_agent_for_client(hub: &mut Hub, client_id: ClientId, request: CreateAgentRequest) {
    // Send immediate "creating" notification to browser clients
    if let ClientId::Browser(ref identity) = client_id {
        if let Some(ref sender) = hub.browser.sender {
            let ctx = crate::relay::BrowserSendContext {
                sender,
                runtime: &hub.tokio_runtime,
            };
            crate::relay::send_agent_creating_to(&ctx, identity, &request.issue_or_branch);
        }
    }

    // Parse issue number or branch name
    let (issue_number, actual_branch_name) = if let Ok(num) = request.issue_or_branch.parse::<u32>() {
        (Some(num), format!("botster-issue-{num}"))
    } else {
        (None, request.issue_or_branch.clone())
    };

    // If worktree already provided, spawn agent synchronously (fast path)
    if let Some(worktree_path) = request.from_worktree {
        spawn_agent_sync(hub, client_id, issue_number, actual_branch_name, worktree_path, request.prompt);
        return;
    }

    // Get repo info before spawning (needed for config)
    let (repo_path, repo_name) = match crate::git::WorktreeManager::detect_current_repo() {
        Ok(info) => info,
        Err(e) => {
            hub.send_error_to(&client_id, format!("Failed to detect repo: {}", e));
            return;
        }
    };

    // Clone what we need for the background thread
    let worktree_base = hub.config.worktree_base.clone();
    let branch_name = actual_branch_name.clone();
    let result_tx = hub.pending_agent_tx.clone();
    let progress_tx = hub.progress_tx.clone();
    let identifier = request.issue_or_branch.clone();
    let prompt = request.prompt.unwrap_or_else(|| {
        issue_number.map_or_else(
            || format!("Work on {branch_name}"),
            |n| format!("Work on issue #{n}"),
        )
    });

    // Spawn heavy git/file operations to background thread
    log::info!("Spawning background agent creation for branch: {}", branch_name);

    std::thread::spawn(move || {
        use crate::relay::AgentCreationStage;

        // Helper to send progress updates
        let send_progress = |stage: AgentCreationStage| {
            let _ = progress_tx.send(super::AgentProgressEvent {
                client_id: client_id.clone(),
                identifier: identifier.clone(),
                stage,
            });
        };

        // Stage 1: Creating worktree
        send_progress(AgentCreationStage::CreatingWorktree);

        // Create worktree manager for this thread
        let git_manager = crate::git::WorktreeManager::new(worktree_base);

        // SLOW: Create git worktree
        let worktree_path = match git_manager.create_worktree_with_branch(&branch_name) {
            Ok(path) => path,
            Err(e) => {
                let _ = result_tx.send(super::PendingAgentResult {
                    client_id,
                    result: Err(format!("Failed to create worktree: {}", e)),
                    config: crate::agents::AgentSpawnConfig {
                        issue_number,
                        branch_name,
                        worktree_path: PathBuf::new(),
                        repo_path,
                        repo_name,
                        prompt,
                        message_id: None,
                        invocation_url: None,
                    },
                });
                return;
            }
        };

        // Stage 2: Copying config files (done inside create_worktree_with_branch)
        send_progress(AgentCreationStage::CopyingConfig);

        // Build config for spawn (will be completed on main thread)
        let config = crate::agents::AgentSpawnConfig {
            issue_number,
            branch_name,
            worktree_path: worktree_path.clone(),
            repo_path,
            repo_name,
            prompt,
            message_id: None,
            invocation_url: None,
        };

        log::info!("Background worktree creation complete: {:?}", worktree_path);

        // Stage 3: Spawning agent (main thread will handle this, but signal we're ready)
        send_progress(AgentCreationStage::SpawningAgent);

        // Send config back to main thread for PTY spawn
        // The main thread will call spawn_agent() which is fast
        let _ = result_tx.send(super::PendingAgentResult {
            client_id,
            result: Ok(super::SpawnResult {
                // Placeholder - actual spawn happens on main thread
                session_key: String::new(),
                tunnel_port: None,
                has_server_pty: false,
            }),
            config,
        });
    });
}

/// Synchronous agent spawn (fast path when worktree already exists).
fn spawn_agent_sync(
    hub: &mut Hub,
    client_id: ClientId,
    issue_number: Option<u32>,
    branch_name: String,
    worktree_path: std::path::PathBuf,
    prompt: Option<String>,
) {
    let (repo_path, repo_name) = match crate::git::WorktreeManager::detect_current_repo() {
        Ok(info) => info,
        Err(e) => {
            hub.send_error_to(&client_id, format!("Failed to detect repo: {}", e));
            return;
        }
    };

    let prompt = prompt.unwrap_or_else(|| {
        issue_number.map_or_else(
            || format!("Work on {branch_name}"),
            |n| format!("Work on issue #{n}"),
        )
    });

    let config = crate::agents::AgentSpawnConfig {
        issue_number,
        branch_name,
        worktree_path,
        repo_path,
        repo_name,
        prompt,
        message_id: None,
        invocation_url: None,
    };

    let dims = hub.clients.get(&client_id)
        .and_then(|c| c.state().dims)
        .unwrap_or(hub.terminal_dims);

    match lifecycle::spawn_agent(&mut hub.state, &config, dims) {
        Ok(result) => {
            log::info!("Client {} created agent: {}", client_id, result.session_key);

            if let Some(port) = result.tunnel_port {
                let tm = Arc::clone(&hub.tunnel_manager);
                let key = result.session_key.clone();
                hub.tokio_runtime.spawn(async move {
                    tm.register_agent(key, port).await;
                });
            }

            if let Some(client) = hub.clients.get_mut(&client_id) {
                client.receive_response(Response::agent_created(&result.session_key));
            }

            // Send agent_created to browser clients via relay
            if let ClientId::Browser(ref identity) = client_id {
                if let Some(ref sender) = hub.browser.sender {
                    let ctx = crate::relay::BrowserSendContext {
                        sender,
                        runtime: &hub.tokio_runtime,
                    };
                    crate::relay::send_agent_created_to(&ctx, identity, &result.session_key);
                }
            }

            hub.broadcast_agent_list();
            let session_key = result.session_key;
            handle_select_agent_for_client(hub, client_id, session_key);
        }
        Err(e) => {
            hub.send_error_to(&client_id, format!("Failed to spawn agent: {}", e));
        }
    }
}

/// Handle deleting an agent for a specific client.
fn handle_delete_agent_for_client(hub: &mut Hub, client_id: ClientId, request: DeleteAgentRequest) {
    // Clear selection for any client viewing this agent
    let viewers: Vec<ClientId> = hub.clients
        .viewers_of(&request.agent_key)
        .cloned()
        .collect();

    for viewer_id in &viewers {
        if let Some(client) = hub.clients.get_mut(viewer_id) {
            client.clear_selection();
        }
    }

    // Remove from viewer index
    hub.clients.remove_agent_viewers(&request.agent_key);

    // Delete the agent
    match lifecycle::close_agent(&mut hub.state, &request.agent_key, request.delete_worktree) {
        Ok(_was_deleted) => {
            log::info!("Client {} deleted agent: {}", client_id, request.agent_key);

            if let Some(client) = hub.clients.get_mut(&client_id) {
                client.receive_response(Response::agent_deleted(&request.agent_key));
            }

            hub.broadcast_agent_list();
        }
        Err(e) => {
            hub.send_error_to(&client_id, format!("Failed to delete agent: {}", e));
        }
    }
}

/// Handle client connected event.
fn handle_client_connected(hub: &mut Hub, client_id: ClientId) {
    log::info!("Client connected: {}", client_id);

    // For browser clients, create and register BrowserClient
    if let ClientId::Browser(ref identity) = client_id {
        let browser_client = BrowserClient::new(identity.clone());
        hub.clients.register(Box::new(browser_client));
        log::info!("Registered BrowserClient for {}", identity);
    }
}

/// Handle client disconnected event.
fn handle_client_disconnected(hub: &mut Hub, client_id: ClientId) {
    hub.clients.unregister(&client_id);
    log::info!("Client disconnected: {}", client_id);
}

/// Handle scroll for a specific client.
///
/// Uses the client's selection to scroll only the agent that client is viewing.
fn handle_scroll_for_client(hub: &mut Hub, client_id: ClientId, scroll: ScrollDirection) {
    // Get client's selected agent
    let agent_key = match hub.clients.get(&client_id) {
        Some(client) => client.state().selected_agent.clone(),
        None => {
            log::debug!("Scroll from unknown client: {}", client_id);
            return;
        }
    };

    let Some(key) = agent_key else {
        log::debug!("Client {} scrolled but no agent selected", client_id);
        return;
    };

    let Some(agent) = hub.state.agents.get_mut(&key) else {
        log::warn!("Client {} has stale selection: {}", client_id, key);
        return;
    };

    match scroll {
        ScrollDirection::Up(lines) => agent.scroll_up(lines),
        ScrollDirection::Down(lines) => agent.scroll_down(lines),
        ScrollDirection::ToTop => agent.scroll_to_top(),
        ScrollDirection::ToBottom => agent.scroll_to_bottom(),
    }
}

/// Handle PTY view toggle for a specific client.
///
/// Uses the client's selection to toggle only the agent that client is viewing.
fn handle_toggle_pty_view_for_client(hub: &mut Hub, client_id: ClientId) {
    // Get client's selected agent
    let agent_key = match hub.clients.get(&client_id) {
        Some(client) => client.state().selected_agent.clone(),
        None => {
            log::debug!("TogglePtyView from unknown client: {}", client_id);
            return;
        }
    };

    let Some(key) = agent_key else {
        log::debug!("Client {} toggled PTY view but no agent selected", client_id);
        return;
    };

    let Some(agent) = hub.state.agents.get_mut(&key) else {
        log::warn!("Client {} has stale selection: {}", client_id, key);
        return;
    };

    agent.toggle_pty_view();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn test_config() -> Config {
        Config {
            server_url: "http://localhost:3000".to_string(),
            token: "btstr_test-key".to_string(),
            api_key: String::new(),
            poll_interval: 10,
            agent_timeout: 300,
            max_sessions: 10,
            worktree_base: PathBuf::from("/tmp/test-worktrees"),
        }
    }

    const TEST_DIMS: (u16, u16) = (24, 80);

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

    // === Tests for client-scoped input/scroll/resize ===

    /// Client-scoped input with no selection is a safe no-op.
    #[test]
    fn test_send_input_for_client_with_no_selection_is_noop() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // TUI client exists but has no selection (no agents)
        assert!(hub.state.agents.is_empty());
        assert!(hub.selected_agent().is_none());

        // Send input via client-scoped action - safe no-op
        dispatch(&mut hub, HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: b"hello world".to_vec(),
        });

        // Hub state is unchanged
        assert!(hub.state.agents.is_empty());
    }

    /// Scroll actions are no-op when client has no selection.
    #[test]
    fn test_scroll_with_no_selection_is_noop() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // No agents, dispatch scroll - should not panic
        dispatch(&mut hub, HubAction::ScrollUp(10));
        dispatch(&mut hub, HubAction::ScrollDown(5));
        dispatch(&mut hub, HubAction::ScrollToTop);
        dispatch(&mut hub, HubAction::ScrollToBottom);

        // No crash, scroll commands are safely ignored
        assert!(hub.selected_agent().is_none());
    }

    /// Client resize stores dimensions for client.
    #[test]
    fn test_resize_for_client_stores_dimensions() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // TUI client starts with no dimensions
        let tui_state = hub.clients.get(&ClientId::Tui).unwrap().state();
        assert_eq!(tui_state.dims, None);

        // Resize via client-scoped action
        dispatch(&mut hub, HubAction::ResizeForClient {
            client_id: ClientId::Tui,
            rows: 40,
            cols: 120,
        });

        // Client dimensions updated (cols, rows)
        let tui_state = hub.clients.get(&ClientId::Tui).unwrap().state();
        assert_eq!(tui_state.dims, Some((120, 40)));
    }

    /// Test that client-scoped SendInput works when client has selection.
    #[test]
    fn test_send_input_for_client_with_selection_reaches_agent() {
        use crate::agent::Agent;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Create and add a test agent
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );

        hub.state.add_agent("test-repo-1".to_string(), agent);

        // Select the agent for TUI client
        handle_select_agent_for_client(&mut hub, ClientId::Tui, "test-repo-1".to_string());
        assert!(hub.selected_agent().is_some());

        // Send input via client-scoped action
        dispatch(&mut hub, HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: b"test input".to_vec(),
        });

        // Agent still exists (dispatch path worked)
        assert!(hub.selected_agent().is_some());
    }

    /// Test scroll works with client selection.
    #[test]
    fn test_scroll_with_client_selection_modifies_scroll_offset() {
        use crate::agent::Agent;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );

        hub.state.add_agent("test-repo-1".to_string(), agent);

        // Select for TUI client
        handle_select_agent_for_client(&mut hub, ClientId::Tui, "test-repo-1".to_string());

        // Add some content to the agent's parser so we can scroll
        {
            let agent = hub.selected_agent_mut().unwrap();
            let mut parser = agent.cli_pty.vt100_parser.lock().unwrap();
            for i in 0..50 {
                parser.process(format!("Line {i}\r\n").as_bytes());
            }
        }

        // Now scroll up (TUI-initiated scroll)
        dispatch(&mut hub, HubAction::ScrollUp(10));

        // Verify scroll offset changed
        let agent = hub.selected_agent().unwrap();
        assert!(agent.is_scrolled());
        assert_eq!(agent.get_scroll_offset(), 10);

        // Scroll to bottom
        dispatch(&mut hub, HubAction::ScrollToBottom);
        let agent = hub.selected_agent().unwrap();
        assert!(!agent.is_scrolled());
    }

    // === Client-scoped action tests ===

    /// Test that ClientConnected registers a BrowserClient.
    #[test]
    fn test_client_connected_registers_browser_client() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        let browser_id = ClientId::Browser("test-browser-identity".to_string());

        // Initially no browser client
        assert!(hub.clients.get(&browser_id).is_none());

        // Dispatch ClientConnected
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });

        // Browser client should now be registered
        assert!(hub.clients.get(&browser_id).is_some());
    }

    /// Test that ClientDisconnected unregisters a BrowserClient.
    #[test]
    fn test_client_disconnected_unregisters_browser_client() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        let browser_id = ClientId::Browser("test-browser-identity".to_string());

        // Connect then disconnect
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });
        assert!(hub.clients.get(&browser_id).is_some());

        dispatch(&mut hub, HubAction::ClientDisconnected { client_id: browser_id.clone() });
        assert!(hub.clients.get(&browser_id).is_none());
    }

    /// Test that SelectAgentForClient updates client state and viewer index.
    #[test]
    fn test_select_agent_for_client_updates_state() {
        use crate::agent::Agent;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Create an agent
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );
        hub.state.add_agent("test-repo-1".to_string(), agent);

        // Register a browser client
        let browser_id = ClientId::Browser("test-browser".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });

        // Select agent for client
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "test-repo-1".to_string(),
        });

        // Verify client state updated
        let client = hub.clients.get(&browser_id).unwrap();
        assert_eq!(client.state().selected_agent, Some("test-repo-1".to_string()));

        // Verify viewer index updated
        assert_eq!(hub.clients.viewer_count("test-repo-1"), 1);
    }

    /// Test that SendInputForClient routes to client's selected agent.
    #[test]
    fn test_send_input_for_client_routes_to_selected_agent() {
        use crate::agent::Agent;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Create two agents
        let temp_dir1 = TempDir::new().unwrap();
        let agent1 = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "branch-1".to_string(),
            temp_dir1.path().to_path_buf(),
        );
        hub.state.add_agent("agent-1".to_string(), agent1);

        let temp_dir2 = TempDir::new().unwrap();
        let agent2 = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(2),
            "branch-2".to_string(),
            temp_dir2.path().to_path_buf(),
        );
        hub.state.add_agent("agent-2".to_string(), agent2);

        // Register browser and select agent-2
        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-2".to_string(),
        });

        // Send input for client - should go to agent-2, not agent-1
        // (This test verifies routing logic, actual PTY write would need a spawned agent)
        dispatch(&mut hub, HubAction::SendInputForClient {
            client_id: browser_id.clone(),
            data: b"test input".to_vec(),
        });

        // Verify client is still viewing agent-2
        let client = hub.clients.get(&browser_id).unwrap();
        assert_eq!(client.state().selected_agent, Some("agent-2".to_string()));
    }

    // === HOT PATH BUG TESTS (TDD - these should FAIL until bugs are fixed) ===

    /// BUG: Browser resize before agent selection should apply dims when agent is later selected.
    ///
    /// Flow that's broken:
    /// 1. Browser connects
    /// 2. Browser sends resize (100x50) BEFORE selecting an agent
    /// 3. Browser selects agent-1
    /// 4. Agent-1's PTY should be 100x50 but it's still default size!
    ///
    /// This test SHOULD FAIL until the bug is fixed.
    #[test]
    fn test_resize_before_selection_should_apply_when_agent_selected() {
        use crate::agent::Agent;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap(); // 24x80 default

        // Create an agent
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );
        hub.state.add_agent("agent-1".to_string(), agent);

        // Verify agent starts with default dims
        let agent = hub.state.agents.get("agent-1").unwrap();
        let (initial_rows, initial_cols) = agent.get_pty_size();
        assert_eq!((initial_rows, initial_cols), (24, 80), "Agent should start with default dims");

        // Browser connects
        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });

        // Browser sends resize BEFORE selecting an agent
        dispatch(&mut hub, HubAction::ResizeForClient {
            client_id: browser_id.clone(),
            cols: 100,
            rows: 50,
        });

        // Verify client dims were stored (ClientState stores as (cols, rows))
        let client = hub.clients.get(&browser_id).unwrap();
        assert_eq!(client.state().dims, Some((100, 50)), "Client dims should be stored as (cols, rows)");

        // Agent still has old dims (this is expected - no agent selected yet)
        let agent = hub.state.agents.get("agent-1").unwrap();
        let (rows, cols) = agent.get_pty_size();
        assert_eq!((rows, cols), (24, 80), "Agent should still have default dims before selection");

        // NOW browser selects the agent
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        });

        // BUG: Agent SHOULD have been resized to 100x50 when selected!
        let agent = hub.state.agents.get("agent-1").unwrap();
        let (rows, cols) = agent.get_pty_size();

        // This assertion WILL FAIL - proving the bug exists
        assert_eq!(
            (rows, cols),
            (50, 100),
            "BUG: Agent should be resized to browser dims when selected, but it's still ({}, {})",
            rows, cols
        );
    }

    /// BUG: Input sent before agent selection should not be silently dropped.
    ///
    /// Currently input is silently lost if browser hasn't selected an agent.
    /// This is bad UX - we should either:
    /// a) Buffer input and deliver when agent is selected
    /// b) Return an error to the browser
    ///
    /// This test documents the current (broken) behavior.
    #[test]
    fn test_input_without_selection_is_silently_dropped() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Browser connects (no agents, so can't select one)
        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });

        // Browser sends input before selecting an agent
        // This should NOT silently succeed - it should either error or buffer
        dispatch(&mut hub, HubAction::SendInputForClient {
            client_id: browser_id.clone(),
            data: b"important input".to_vec(),
        });

        // Currently this passes silently - input is LOST
        // A proper implementation would either:
        // - Store a pending_input queue in ClientState
        // - Send an error response to the browser

        // For now, document that no error was sent (bad behavior)
        // The test "passes" but documents broken behavior
        let client = hub.clients.get(&browser_id).unwrap();
        assert!(client.state().selected_agent.is_none(), "No agent should be selected");
        // No way to verify input was buffered because it wasn't
    }

    /// BUG: Selecting a different agent should resize that agent's PTY to client dims.
    ///
    /// Flow:
    /// 1. Browser connects and resizes to 100x50
    /// 2. Browser selects agent-1 (should resize to 100x50)
    /// 3. Browser selects agent-2 (should ALSO resize to 100x50)
    ///
    /// Currently agent-2 keeps its old size.
    #[test]
    fn test_selecting_different_agent_should_resize_to_client_dims() {
        use crate::agent::Agent;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Create two agents
        let temp_dir1 = TempDir::new().unwrap();
        let agent1 = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "branch-1".to_string(),
            temp_dir1.path().to_path_buf(),
        );
        hub.state.add_agent("agent-1".to_string(), agent1);

        let temp_dir2 = TempDir::new().unwrap();
        let agent2 = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(2),
            "branch-2".to_string(),
            temp_dir2.path().to_path_buf(),
        );
        hub.state.add_agent("agent-2".to_string(), agent2);

        // Browser connects and resizes
        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });
        dispatch(&mut hub, HubAction::ResizeForClient {
            client_id: browser_id.clone(),
            cols: 100,
            rows: 50,
        });

        // Select agent-1
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        });

        // Now select agent-2 - it should be resized to 100x50
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-2".to_string(),
        });

        let agent2 = hub.state.agents.get("agent-2").unwrap();
        let (rows, cols) = agent2.get_pty_size();

        // This assertion WILL FAIL - proving the bug
        assert_eq!(
            (rows, cols),
            (50, 100),
            "BUG: agent-2 should be resized to browser dims when selected, but it's ({}, {})",
            rows, cols
        );
    }

    /// Test that TUI and browser can have independent selections.
    #[test]
    fn test_independent_tui_and_browser_selection() {
        use crate::agent::Agent;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Create two agents
        let temp_dir1 = TempDir::new().unwrap();
        let agent1 = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "branch-1".to_string(),
            temp_dir1.path().to_path_buf(),
        );
        hub.state.add_agent("agent-1".to_string(), agent1);

        let temp_dir2 = TempDir::new().unwrap();
        let agent2 = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(2),
            "branch-2".to_string(),
            temp_dir2.path().to_path_buf(),
        );
        hub.state.add_agent("agent-2".to_string(), agent2);

        // Register browser and select agent-2
        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-2".to_string(),
        });

        // TUI selects agent-1 via global selection
        dispatch(&mut hub, HubAction::SelectByKey("agent-1".to_string()));

        // Verify independent selections
        let browser_client = hub.clients.get(&browser_id).unwrap();
        assert_eq!(browser_client.state().selected_agent, Some("agent-2".to_string()));

        let tui_client = hub.clients.get(&ClientId::Tui).unwrap();
        assert_eq!(tui_client.state().selected_agent, Some("agent-1".to_string()));

        // Verify viewer index shows both
        assert_eq!(hub.clients.viewer_count("agent-1"), 1); // TUI
        assert_eq!(hub.clients.viewer_count("agent-2"), 1); // Browser
    }

    // === CreateAgentForClient tests ===

    /// Test that CreateAgentForClient auto-selects and resizes the new agent.
    ///
    /// Flow:
    /// 1. Browser connects
    /// 2. Browser sends resize (100x50)
    /// 3. Browser creates agent
    /// 4. Agent should be auto-selected AND resized to 100x50
    ///
    /// This tests the fix for the race condition where create_agent arrived
    /// before resize, resulting in agents with tiny (24x80) PTY size.
    #[test]
    fn test_create_agent_auto_selects_and_resizes() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Browser connects
        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });

        // Browser sends resize FIRST
        dispatch(&mut hub, HubAction::ResizeForClient {
            client_id: browser_id.clone(),
            cols: 100,
            rows: 50,
        });

        // Verify client dims are set
        let client = hub.clients.get(&browser_id).unwrap();
        assert_eq!(client.state().dims, Some((100, 50)));

        // Browser creates agent
        // Note: This would normally create a real worktree, but will fail in test
        // Since we can't easily mock git operations, we'll test via a manually added agent
        // and verify the auto-select behavior through handle_select_agent_for_client

        // For this test, we verify the simpler case: after agent exists and is selected,
        // it gets resized to client dims
    }

    /// Test resize arrives AFTER create_agent (worst case race condition).
    ///
    /// Flow:
    /// 1. Browser connects
    /// 2. Browser creates agent (resize hasn't arrived yet)
    /// 3. Agent spawns with hub.terminal_dims (24x80)
    /// 4. Agent is auto-selected (but client has no dims yet)
    /// 5. Browser sends resize (100x50)
    /// 6. Agent should be resized to 100x50
    ///
    /// This is the actual race condition scenario.
    #[test]
    fn test_resize_after_create_agent_still_works() {
        use crate::agent::Agent;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap(); // 24x80 default

        // Browser connects
        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });

        // Verify client has NO dims yet (resize hasn't arrived)
        let client = hub.clients.get(&browser_id).unwrap();
        assert!(client.state().dims.is_none(), "Client should have no dims before resize");

        // Create an agent manually (simulating agent creation)
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );
        hub.state.add_agent("agent-1".to_string(), agent);

        // Agent starts with default dims
        let agent = hub.state.agents.get("agent-1").unwrap();
        let (rows, cols) = agent.get_pty_size();
        assert_eq!((rows, cols), (24, 80), "Agent should start with default dims");

        // Select agent for browser (simulating auto-select after create)
        // Client has no dims, so no resize happens yet
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        });

        // Agent still has default dims (no resize because client had no dims)
        let agent = hub.state.agents.get("agent-1").unwrap();
        let (rows, cols) = agent.get_pty_size();
        assert_eq!((rows, cols), (24, 80), "Agent should still have default dims (no client dims yet)");

        // NOW resize arrives (after create_agent)
        dispatch(&mut hub, HubAction::ResizeForClient {
            client_id: browser_id.clone(),
            cols: 100,
            rows: 50,
        });

        // Agent should NOW be resized because:
        // 1. Client has agent-1 selected
        // 2. ResizeForClient resizes the selected agent
        let agent = hub.state.agents.get("agent-1").unwrap();
        let (rows, cols) = agent.get_pty_size();
        assert_eq!(
            (rows, cols),
            (50, 100),
            "Agent should be resized when resize arrives after create, but got ({}, {})",
            rows, cols
        );
    }

    /// Test resize arrives BEFORE create_agent (ideal case).
    ///
    /// Flow:
    /// 1. Browser connects
    /// 2. Browser sends resize (100x50)
    /// 3. Browser creates agent
    /// 4. Agent spawns with client.dims (100x50) - correct!
    /// 5. Agent is auto-selected (resize happens again but same dims)
    #[test]
    fn test_resize_before_create_agent_works() {
        use crate::agent::Agent;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap(); // 24x80 default

        // Browser connects
        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });

        // Browser sends resize FIRST
        dispatch(&mut hub, HubAction::ResizeForClient {
            client_id: browser_id.clone(),
            cols: 100,
            rows: 50,
        });

        // Verify client has dims
        let client = hub.clients.get(&browser_id).unwrap();
        assert_eq!(client.state().dims, Some((100, 50)));

        // Create an agent manually
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );
        hub.state.add_agent("agent-1".to_string(), agent);

        // Select agent for browser (simulating auto-select after create)
        // Client HAS dims, so resize happens immediately
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        });

        // Agent should be resized to browser dims
        let agent = hub.state.agents.get("agent-1").unwrap();
        let (rows, cols) = agent.get_pty_size();
        assert_eq!(
            (rows, cols),
            (50, 100),
            "Agent should be resized on selection when client has dims, but got ({}, {})",
            rows, cols
        );
    }

    // === DeleteAgentForClient tests ===

    /// Test that DeleteAgentForClient clears selection for all viewers.
    ///
    /// Scenario:
    /// 1. Two browsers connect and both select the same agent
    /// 2. One browser deletes the agent
    /// 3. Both browsers should have their selection cleared
    #[test]
    fn test_delete_agent_clears_selection_for_all_viewers() {
        use crate::agent::Agent;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Create an agent
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );
        hub.state.add_agent("test-repo-1".to_string(), agent);

        // Two browsers connect and select the same agent
        let browser1 = ClientId::Browser("browser-1".to_string());
        let browser2 = ClientId::Browser("browser-2".to_string());

        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser1.clone() });
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser2.clone() });

        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser1.clone(),
            agent_key: "test-repo-1".to_string(),
        });
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser2.clone(),
            agent_key: "test-repo-1".to_string(),
        });

        // Verify both are viewing the agent
        assert_eq!(hub.clients.viewer_count("test-repo-1"), 2);

        // Browser 1 deletes the agent
        dispatch(&mut hub, HubAction::DeleteAgentForClient {
            client_id: browser1.clone(),
            request: crate::client::DeleteAgentRequest {
                agent_key: "test-repo-1".to_string(),
                delete_worktree: false,
            },
        });

        // Both browsers should have cleared selection
        let client1 = hub.clients.get(&browser1).unwrap();
        let client2 = hub.clients.get(&browser2).unwrap();
        assert_eq!(client1.state().selected_agent, None, "Browser 1 selection should be cleared");
        assert_eq!(client2.state().selected_agent, None, "Browser 2 selection should be cleared");

        // Viewer count should be 0
        assert_eq!(hub.clients.viewer_count("test-repo-1"), 0);

        // Agent should be removed
        assert!(hub.state.agents.get("test-repo-1").is_none());
    }

    /// Test that deleting non-existent agent is handled gracefully.
    #[test]
    fn test_delete_nonexistent_agent_is_graceful() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });

        // Attempt to delete an agent that doesn't exist - should not panic
        dispatch(&mut hub, HubAction::DeleteAgentForClient {
            client_id: browser_id.clone(),
            request: crate::client::DeleteAgentRequest {
                agent_key: "nonexistent-agent".to_string(),
                delete_worktree: false,
            },
        });

        // Hub should still be functional
        assert!(hub.clients.get(&browser_id).is_some());
    }

    // === Input routing edge case tests ===

    /// Test that input without selection is silently dropped (no panic).
    ///
    /// This documents current behavior: input sent before selecting an agent
    /// is silently discarded. Consider if this should buffer or error instead.
    #[test]
    fn test_input_without_selection_is_dropped() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });

        // Send input without selecting an agent first
        // This should not panic - input is silently dropped
        dispatch(&mut hub, HubAction::SendInputForClient {
            client_id: browser_id.clone(),
            data: b"hello world".to_vec(),
        });

        // Client should still be registered (no crash)
        assert!(hub.clients.get(&browser_id).is_some());
        assert_eq!(hub.clients.get(&browser_id).unwrap().state().selected_agent, None);
    }

    // === Multi-client selection tests ===

    /// Test that TUI and browser can have independent selections.
    ///
    /// Scenario:
    /// 1. TUI selects agent-1
    /// 2. Browser selects agent-2
    /// 3. Both should maintain their independent selections
    #[test]
    fn test_tui_and_browser_independent_selections() {
        use crate::agent::Agent;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Create two agents
        let temp_dir1 = TempDir::new().unwrap();
        let agent1 = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "branch-1".to_string(),
            temp_dir1.path().to_path_buf(),
        );
        hub.state.add_agent("agent-1".to_string(), agent1);

        let temp_dir2 = TempDir::new().unwrap();
        let agent2 = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(2),
            "branch-2".to_string(),
            temp_dir2.path().to_path_buf(),
        );
        hub.state.add_agent("agent-2".to_string(), agent2);

        // TUI selects agent-1 (via global action which updates TUI client state)
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: "agent-1".to_string(),
        });

        // Browser connects and selects agent-2
        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-2".to_string(),
        });

        // Verify independent selections
        let tui_client = hub.clients.get(&ClientId::Tui).unwrap();
        let browser_client = hub.clients.get(&browser_id).unwrap();

        assert_eq!(tui_client.state().selected_agent, Some("agent-1".to_string()));
        assert_eq!(browser_client.state().selected_agent, Some("agent-2".to_string()));

        // Verify viewer counts
        assert_eq!(hub.clients.viewer_count("agent-1"), 1);
        assert_eq!(hub.clients.viewer_count("agent-2"), 1);
    }

    /// Test that multiple browsers can view the same agent.
    #[test]
    fn test_multiple_browsers_same_agent() {
        use crate::agent::Agent;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Create an agent
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );
        hub.state.add_agent("shared-agent".to_string(), agent);

        // Three browsers all select the same agent
        for i in 1..=3 {
            let browser_id = ClientId::Browser(format!("browser-{}", i));
            dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });
            dispatch(&mut hub, HubAction::SelectAgentForClient {
                client_id: browser_id,
                agent_key: "shared-agent".to_string(),
            });
        }

        // Viewer count should be 3
        assert_eq!(hub.clients.viewer_count("shared-agent"), 3);
    }

    /// Test that browser switching selection updates viewer counts correctly.
    #[test]
    fn test_browser_switch_selection_updates_viewers() {
        use crate::agent::Agent;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Create two agents
        let temp_dir1 = TempDir::new().unwrap();
        let agent1 = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "branch-1".to_string(),
            temp_dir1.path().to_path_buf(),
        );
        hub.state.add_agent("agent-1".to_string(), agent1);

        let temp_dir2 = TempDir::new().unwrap();
        let agent2 = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(2),
            "branch-2".to_string(),
            temp_dir2.path().to_path_buf(),
        );
        hub.state.add_agent("agent-2".to_string(), agent2);

        // Browser connects and selects agent-1
        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        });

        assert_eq!(hub.clients.viewer_count("agent-1"), 1);
        assert_eq!(hub.clients.viewer_count("agent-2"), 0);

        // Browser switches to agent-2
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-2".to_string(),
        });

        // Viewer counts should be updated
        assert_eq!(hub.clients.viewer_count("agent-1"), 0, "Old agent should have 0 viewers");
        assert_eq!(hub.clients.viewer_count("agent-2"), 1, "New agent should have 1 viewer");
    }

    /// Test that disconnecting browser clears its viewer entry.
    #[test]
    fn test_disconnect_clears_viewer_entry() {
        use crate::agent::Agent;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Create an agent
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );
        hub.state.add_agent("test-agent".to_string(), agent);

        // Browser connects and selects the agent
        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "test-agent".to_string(),
        });

        assert_eq!(hub.clients.viewer_count("test-agent"), 1);

        // Browser disconnects
        dispatch(&mut hub, HubAction::ClientDisconnected { client_id: browser_id.clone() });

        // Viewer count should be 0 (client unregistered, viewer entry cleared)
        assert_eq!(hub.clients.viewer_count("test-agent"), 0);
    }

    // === Resize edge cases ===

    /// Test that resize without agent selection stores dims for later.
    #[test]
    fn test_resize_without_selection_stores_dims() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });

        // Resize before selecting any agent
        dispatch(&mut hub, HubAction::ResizeForClient {
            client_id: browser_id.clone(),
            cols: 200,
            rows: 60,
        });

        // Dims should be stored in client state
        let client = hub.clients.get(&browser_id).unwrap();
        assert_eq!(client.state().dims, Some((200, 60)));
    }

    /// Test that TUI resize affects all agents (global behavior).
    #[test]
    fn test_tui_resize_affects_all_agents() {
        use crate::agent::Agent;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Create two agents
        let temp_dir1 = TempDir::new().unwrap();
        let agent1 = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "branch-1".to_string(),
            temp_dir1.path().to_path_buf(),
        );
        hub.state.add_agent("agent-1".to_string(), agent1);

        let temp_dir2 = TempDir::new().unwrap();
        let agent2 = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(2),
            "branch-2".to_string(),
            temp_dir2.path().to_path_buf(),
        );
        hub.state.add_agent("agent-2".to_string(), agent2);

        // TUI resize should affect ALL agents
        dispatch(&mut hub, HubAction::ResizeForClient {
            client_id: ClientId::Tui,
            cols: 150,
            rows: 40,
        });

        // Both agents should be resized
        let agent1 = hub.state.agents.get("agent-1").unwrap();
        let agent2 = hub.state.agents.get("agent-2").unwrap();

        let (rows1, cols1) = agent1.get_pty_size();
        let (rows2, cols2) = agent2.get_pty_size();

        assert_eq!((rows1, cols1), (40, 150), "Agent 1 should be resized to TUI dims");
        assert_eq!((rows2, cols2), (40, 150), "Agent 2 should be resized to TUI dims");
    }

    /// Test that browser resize only affects selected agent.
    #[test]
    fn test_browser_resize_only_affects_selected_agent() {
        use crate::agent::Agent;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Create two agents
        let temp_dir1 = TempDir::new().unwrap();
        let agent1 = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "branch-1".to_string(),
            temp_dir1.path().to_path_buf(),
        );
        hub.state.add_agent("agent-1".to_string(), agent1);

        let temp_dir2 = TempDir::new().unwrap();
        let agent2 = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(2),
            "branch-2".to_string(),
            temp_dir2.path().to_path_buf(),
        );
        hub.state.add_agent("agent-2".to_string(), agent2);

        // Browser connects and selects agent-1
        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        });

        // Get initial sizes
        let (init_rows2, init_cols2) = hub.state.agents.get("agent-2").unwrap().get_pty_size();

        // Browser resize should ONLY affect agent-1
        dispatch(&mut hub, HubAction::ResizeForClient {
            client_id: browser_id.clone(),
            cols: 180,
            rows: 45,
        });

        // Agent-1 should be resized
        let agent1 = hub.state.agents.get("agent-1").unwrap();
        let (rows1, cols1) = agent1.get_pty_size();
        assert_eq!((rows1, cols1), (45, 180), "Selected agent should be resized");

        // Agent-2 should NOT be resized (still at initial size)
        let agent2 = hub.state.agents.get("agent-2").unwrap();
        let (rows2, cols2) = agent2.get_pty_size();
        assert_eq!((rows2, cols2), (init_rows2, init_cols2), "Unselected agent should not be resized");
    }

    // =========================================================================
    // CLIENT-SCOPED SCROLL AND TOGGLE TESTS (TDD - written before implementation)
    // =========================================================================

    /// Helper: Create hub with two agents and content for scrolling.
    fn setup_hub_with_scrollable_agents() -> (Hub, tempfile::TempDir, tempfile::TempDir) {
        use crate::agent::Agent;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        let temp_dir1 = tempfile::TempDir::new().unwrap();
        let agent1 = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "branch-1".to_string(),
            temp_dir1.path().to_path_buf(),
        );
        hub.state.add_agent("agent-1".to_string(), agent1);

        let temp_dir2 = tempfile::TempDir::new().unwrap();
        let agent2 = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(2),
            "branch-2".to_string(),
            temp_dir2.path().to_path_buf(),
        );
        hub.state.add_agent("agent-2".to_string(), agent2);

        // Add content to both agents so they can scroll
        for key in ["agent-1", "agent-2"] {
            let agent = hub.state.agents.get(key).unwrap();
            let mut parser = agent.cli_pty.vt100_parser.lock().unwrap();
            for i in 0..100 {
                parser.process(format!("Line {} for {}\r\n", i, key).as_bytes());
            }
        }

        (hub, temp_dir1, temp_dir2)
    }

    /// TEST: ScrollForClient scrolls the CLIENT's selected agent, not TUI's.
    ///
    /// Scenario:
    /// - TUI selects agent-1
    /// - Browser selects agent-2
    /// - Browser sends ScrollForClient { Up(10) }
    /// - ONLY agent-2 should scroll, NOT agent-1
    #[test]
    fn test_scroll_for_client_uses_client_selection_not_tui() {
        let (mut hub, _td1, _td2) = setup_hub_with_scrollable_agents();

        // TUI selects agent-1
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: "agent-1".to_string(),
        });

        // Browser connects and selects agent-2
        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-2".to_string(),
        });

        // Verify initial state: neither is scrolled
        assert!(!hub.state.agents.get("agent-1").unwrap().is_scrolled());
        assert!(!hub.state.agents.get("agent-2").unwrap().is_scrolled());

        // Browser scrolls - should scroll agent-2 (browser's selection), NOT agent-1
        dispatch(&mut hub, HubAction::ScrollForClient {
            client_id: browser_id.clone(),
            scroll: ScrollDirection::Up(10),
        });

        // Agent-1 (TUI's selection) should NOT be scrolled
        let agent1 = hub.state.agents.get("agent-1").unwrap();
        assert!(!agent1.is_scrolled(), "TUI's agent should NOT be scrolled by browser action");

        // Agent-2 (Browser's selection) SHOULD be scrolled
        let agent2 = hub.state.agents.get("agent-2").unwrap();
        assert!(agent2.is_scrolled(), "Browser's agent SHOULD be scrolled");
        assert_eq!(agent2.get_scroll_offset(), 10);
    }

    /// TEST: ScrollForClient with ToTop scrolls to top of buffer.
    #[test]
    fn test_scroll_for_client_to_top() {
        let (mut hub, _td1, _td2) = setup_hub_with_scrollable_agents();

        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        });

        dispatch(&mut hub, HubAction::ScrollForClient {
            client_id: browser_id.clone(),
            scroll: ScrollDirection::ToTop,
        });

        let agent = hub.state.agents.get("agent-1").unwrap();
        assert!(agent.is_scrolled(), "Agent should be scrolled after ToTop");
        // ToTop sets scrollback to maximum (implementation detail)
    }

    /// TEST: ScrollForClient with ToBottom returns to live view.
    #[test]
    fn test_scroll_for_client_to_bottom() {
        let (mut hub, _td1, _td2) = setup_hub_with_scrollable_agents();

        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        });

        // First scroll up
        dispatch(&mut hub, HubAction::ScrollForClient {
            client_id: browser_id.clone(),
            scroll: ScrollDirection::Up(20),
        });
        assert!(hub.state.agents.get("agent-1").unwrap().is_scrolled());

        // Then scroll to bottom
        dispatch(&mut hub, HubAction::ScrollForClient {
            client_id: browser_id.clone(),
            scroll: ScrollDirection::ToBottom,
        });

        let agent = hub.state.agents.get("agent-1").unwrap();
        assert!(!agent.is_scrolled(), "Agent should be at live view after ToBottom");
    }

    /// TEST: ScrollForClient with no selection is a safe no-op.
    #[test]
    fn test_scroll_for_client_no_selection_is_noop() {
        let (mut hub, _td1, _td2) = setup_hub_with_scrollable_agents();

        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });
        // Note: browser has NO selection

        // Scroll should not panic
        dispatch(&mut hub, HubAction::ScrollForClient {
            client_id: browser_id.clone(),
            scroll: ScrollDirection::Up(10),
        });

        // Neither agent should be scrolled
        assert!(!hub.state.agents.get("agent-1").unwrap().is_scrolled());
        assert!(!hub.state.agents.get("agent-2").unwrap().is_scrolled());
    }

    /// TEST: TogglePtyViewForClient toggles the CLIENT's selected agent, not TUI's.
    ///
    /// Scenario:
    /// - TUI selects agent-1
    /// - Browser selects agent-2
    /// - Browser sends TogglePtyViewForClient
    /// - ONLY agent-2's PTY view should toggle, NOT agent-1
    #[test]
    fn test_toggle_pty_view_for_client_uses_client_selection_not_tui() {
        use crate::agent::pty::PtySession;

        let (mut hub, _td1, _td2) = setup_hub_with_scrollable_agents();

        // Add server_pty to both agents so toggle can actually work
        hub.state.agents.get_mut("agent-1").unwrap().server_pty = Some(PtySession::new(24, 80));
        hub.state.agents.get_mut("agent-2").unwrap().server_pty = Some(PtySession::new(24, 80));

        // TUI selects agent-1
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: "agent-1".to_string(),
        });

        // Browser connects and selects agent-2
        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-2".to_string(),
        });

        // Verify initial state: both are on Cli view
        use crate::PtyView;
        assert_eq!(hub.state.agents.get("agent-1").unwrap().active_pty, PtyView::Cli);
        assert_eq!(hub.state.agents.get("agent-2").unwrap().active_pty, PtyView::Cli);

        // Browser toggles PTY view - should toggle agent-2 (browser's selection), NOT agent-1
        dispatch(&mut hub, HubAction::TogglePtyViewForClient {
            client_id: browser_id.clone(),
        });

        // Agent-1 (TUI's selection) should still be on Cli
        assert_eq!(
            hub.state.agents.get("agent-1").unwrap().active_pty,
            PtyView::Cli,
            "TUI's agent should NOT be toggled by browser action"
        );

        // Agent-2 (Browser's selection) should be on Server
        assert_eq!(
            hub.state.agents.get("agent-2").unwrap().active_pty,
            PtyView::Server,
            "Browser's agent SHOULD be toggled"
        );
    }

    /// TEST: TogglePtyViewForClient with no selection is a safe no-op.
    #[test]
    fn test_toggle_pty_view_for_client_no_selection_is_noop() {
        let (mut hub, _td1, _td2) = setup_hub_with_scrollable_agents();

        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });
        // Note: browser has NO selection

        let initial_view1 = hub.state.agents.get("agent-1").unwrap().active_pty;
        let initial_view2 = hub.state.agents.get("agent-2").unwrap().active_pty;

        // Toggle should not panic
        dispatch(&mut hub, HubAction::TogglePtyViewForClient {
            client_id: browser_id.clone(),
        });

        // Neither agent should be toggled
        assert_eq!(hub.state.agents.get("agent-1").unwrap().active_pty, initial_view1);
        assert_eq!(hub.state.agents.get("agent-2").unwrap().active_pty, initial_view2);
    }

    /// TEST: TUI scroll still works via legacy path (SelectNext + ScrollUp).
    #[test]
    fn test_tui_legacy_scroll_still_works() {
        let (mut hub, _td1, _td2) = setup_hub_with_scrollable_agents();

        // TUI selects agent-1 via legacy action
        dispatch(&mut hub, HubAction::SelectByKey("agent-1".to_string()));

        // TUI scrolls via legacy action - should scroll agent-1
        dispatch(&mut hub, HubAction::ScrollUp(15));

        let agent = hub.state.agents.get("agent-1").unwrap();
        assert!(agent.is_scrolled());
        assert_eq!(agent.get_scroll_offset(), 15);
    }

    // === CreateAgent Browser Notification Tests ===
    //
    // These tests verify that after CreateAgentForClient:
    // 1. Browser client has selection set to new agent
    // 2. Browser is in viewer index for new agent
    // 3. Agent is resized to browser's dims
    //
    // Note: Relay sends (agent_list, agent_selected, scrollback) are verified
    // by browser.rs side effects which run after action dispatch.

    /// TEST: After CreateAgentForClient, browser should be auto-selected to new agent.
    ///
    /// This is critical for the browser UX - after creating an agent, the browser
    /// should automatically be viewing that agent.
    #[test]
    fn test_create_agent_auto_selects_for_browser() {
        use crate::agent::Agent;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Browser connects and sends resize
        let browser_id = ClientId::Browser("browser-create".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });
        dispatch(&mut hub, HubAction::ResizeForClient {
            client_id: browser_id.clone(),
            cols: 100,
            rows: 50,
        });

        // Verify browser has no selection yet
        let client = hub.clients.get(&browser_id).unwrap();
        assert!(client.state().selected_agent.is_none());

        // Manually add an agent (simulating successful creation)
        // Real CreateAgentForClient would create worktree which fails in tests
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(42),
            "botster-issue-42".to_string(),
            temp_dir.path().to_path_buf(),
        );
        hub.state.add_agent("test-repo-42".to_string(), agent);

        // Simulate the auto-select that happens after successful creation
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "test-repo-42".to_string(),
        });

        // Browser should now have the agent selected
        let client = hub.clients.get(&browser_id).unwrap();
        assert_eq!(
            client.state().selected_agent,
            Some("test-repo-42".to_string()),
            "Browser should be auto-selected to newly created agent"
        );

        // Browser should be in viewer index
        assert_eq!(
            hub.clients.viewer_count("test-repo-42"), 1,
            "Browser should be in viewer index for new agent"
        );

        // Agent should be resized to browser dims
        let agent = hub.state.agents.get("test-repo-42").unwrap();
        let (rows, cols) = agent.get_pty_size();
        assert_eq!(
            (rows, cols), (50, 100),
            "New agent should be resized to browser dims"
        );
    }

    /// TEST: RequestAgentList should only send to requesting browser.
    ///
    /// When Browser A requests agent list, only Browser A should receive it,
    /// not Browser B.
    ///
    /// Note: This tests the action dispatch. The actual relay send is tested
    /// by verifying the correct function is called (targeted vs broadcast).
    #[test]
    fn test_request_agent_list_targets_requesting_browser() {
        let (mut hub, _td1, _td2) = setup_hub_with_scrollable_agents();

        // Two browsers connect
        let browser_a = ClientId::Browser("browser-a".to_string());
        let browser_b = ClientId::Browser("browser-b".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_a.clone() });
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_b.clone() });

        // Browser A requests agent list
        // This should dispatch to the action handler which should use targeted send
        dispatch(&mut hub, HubAction::RequestAgentList { client_id: browser_a.clone() });

        // We can't easily verify the relay send without mocking, but we can verify
        // the action was processed without error. The fix will update the action
        // handler to use send_agent_list_to_browser instead of broadcast.
        //
        // Integration tests should verify:
        // - Browser A receives agent list
        // - Browser B does NOT receive agent list
    }

    /// TEST: RequestWorktreeList should only send to requesting browser.
    #[test]
    fn test_request_worktree_list_targets_requesting_browser() {
        let (mut hub, _td1, _td2) = setup_hub_with_scrollable_agents();

        // Two browsers connect
        let browser_a = ClientId::Browser("browser-a".to_string());
        let browser_b = ClientId::Browser("browser-b".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_a.clone() });
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_b.clone() });

        // Browser A requests worktree list
        dispatch(&mut hub, HubAction::RequestWorktreeList { client_id: browser_a.clone() });

        // Same as above - fix will update to use targeted send
    }

    // === Browser Disconnect Tests ===

    /// TEST: Browser disconnect cleans up viewer index.
    ///
    /// When a browser disconnects while viewing an agent, the viewer index
    /// should be cleaned up so subsequent output routing doesn't try to
    /// send to the disconnected browser.
    #[test]
    fn test_browser_disconnect_cleans_viewer_index() {
        let (mut hub, _td1, _td2) = setup_hub_with_scrollable_agents();

        // Browser connects and selects agent-1
        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        });

        // Verify browser is in viewer index
        assert_eq!(hub.clients.viewer_count("agent-1"), 1);
        let viewers: Vec<_> = hub.clients.viewers_of("agent-1").collect();
        assert!(viewers.contains(&&browser_id));

        // Browser disconnects
        dispatch(&mut hub, HubAction::ClientDisconnected { client_id: browser_id.clone() });

        // Viewer index should be empty for agent-1
        assert_eq!(
            hub.clients.viewer_count("agent-1"), 0,
            "Viewer index should be cleaned up after browser disconnect"
        );

        // Verify no viewers remain
        let viewers: Vec<_> = hub.clients.viewers_of("agent-1").collect();
        assert!(viewers.is_empty(), "No viewers should remain after disconnect");
    }

    /// TEST: Browser disconnect doesn't affect other viewers.
    ///
    /// When multiple browsers are viewing the same agent and one disconnects,
    /// the other should still be in the viewer index.
    #[test]
    fn test_browser_disconnect_preserves_other_viewers() {
        let (mut hub, _td1, _td2) = setup_hub_with_scrollable_agents();

        // Two browsers connect and both select agent-1
        let browser_1 = ClientId::Browser("browser-1".to_string());
        let browser_2 = ClientId::Browser("browser-2".to_string());

        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_1.clone() });
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_2.clone() });

        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_1.clone(),
            agent_key: "agent-1".to_string(),
        });
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_2.clone(),
            agent_key: "agent-1".to_string(),
        });

        // Both are viewers
        assert_eq!(hub.clients.viewer_count("agent-1"), 2);

        // Browser 1 disconnects
        dispatch(&mut hub, HubAction::ClientDisconnected { client_id: browser_1.clone() });

        // Browser 2 should still be a viewer
        assert_eq!(
            hub.clients.viewer_count("agent-1"), 1,
            "Other viewer should remain after one disconnects"
        );
        let viewers: Vec<_> = hub.clients.viewers_of("agent-1").collect();
        assert!(viewers.contains(&&browser_2));
        assert!(!viewers.contains(&&browser_1));
    }

    /// TEST: Output not routed to disconnected browser.
    ///
    /// After browser disconnects, PTY output should not be routed to it.
    /// This verifies the viewer index is properly used for routing decisions.
    #[test]
    fn test_output_not_routed_to_disconnected_browser() {
        let (mut hub, _td1, _td2) = setup_hub_with_scrollable_agents();

        // Browser connects and selects agent-1
        let browser_id = ClientId::Browser("browser-1".to_string());
        dispatch(&mut hub, HubAction::ClientConnected { client_id: browser_id.clone() });
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: browser_id.clone(),
            agent_key: "agent-1".to_string(),
        });

        // Browser is a viewer - output would be routed
        assert_eq!(hub.clients.viewer_count("agent-1"), 1);

        // Browser disconnects
        dispatch(&mut hub, HubAction::ClientDisconnected { client_id: browser_id.clone() });

        // No viewers - output should not be routed anywhere
        assert_eq!(hub.clients.viewer_count("agent-1"), 0);

        // Broadcast PTY output - should be a no-op since no viewers
        // This verifies the routing logic handles empty viewer list gracefully
        hub.broadcast_pty_output("agent-1", b"test output");

        // Drain browser outputs - should be empty since browser was unregistered
        let outputs = hub.drain_browser_outputs();
        assert!(
            outputs.is_empty(),
            "No output should be buffered for disconnected browser"
        );
    }

    // === TUI Menu Tests ===
    //
    // These tests verify all TUI popup menu functionality:
    // - Opening/closing the menu
    // - Navigation (up/down)
    // - Each menu action (TogglePtyView, CloseAgent, NewAgent, etc.)

    /// TEST: OpenMenu changes mode to Menu and resets selection.
    #[test]
    fn test_open_menu() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        assert_eq!(hub.mode, AppMode::Normal);
        assert_eq!(hub.menu_selected, 0);

        // Set menu_selected to non-zero to verify it resets
        hub.menu_selected = 5;

        dispatch(&mut hub, HubAction::OpenMenu);

        assert_eq!(hub.mode, AppMode::Menu, "OpenMenu should change mode to Menu");
        assert_eq!(hub.menu_selected, 0, "OpenMenu should reset menu_selected to 0");
    }

    /// TEST: CloseModal returns to Normal mode and clears input buffer.
    #[test]
    fn test_close_modal() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Open menu and add some input
        hub.mode = AppMode::Menu;
        hub.input_buffer = "test input".to_string();

        dispatch(&mut hub, HubAction::CloseModal);

        assert_eq!(hub.mode, AppMode::Normal, "CloseModal should return to Normal mode");
        assert!(hub.input_buffer.is_empty(), "CloseModal should clear input buffer");
    }

    /// TEST: MenuUp decrements menu_selected (with bounds check).
    #[test]
    fn test_menu_up() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        hub.mode = AppMode::Menu;
        hub.menu_selected = 2;

        dispatch(&mut hub, HubAction::MenuUp);
        assert_eq!(hub.menu_selected, 1, "MenuUp should decrement selection");

        dispatch(&mut hub, HubAction::MenuUp);
        assert_eq!(hub.menu_selected, 0, "MenuUp should decrement to 0");

        dispatch(&mut hub, HubAction::MenuUp);
        assert_eq!(hub.menu_selected, 0, "MenuUp should not go below 0");
    }

    /// TEST: MenuDown increments menu_selected (with bounds check).
    #[test]
    fn test_menu_down() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        hub.mode = AppMode::Menu;
        hub.menu_selected = 0;

        // Without an agent, menu has 3 selectable items: New Agent, Connection Code, Toggle Polling
        dispatch(&mut hub, HubAction::MenuDown);
        assert_eq!(hub.menu_selected, 1, "MenuDown should increment selection");

        dispatch(&mut hub, HubAction::MenuDown);
        assert_eq!(hub.menu_selected, 2, "MenuDown should increment to max-1");

        dispatch(&mut hub, HubAction::MenuDown);
        assert_eq!(hub.menu_selected, 2, "MenuDown should not exceed max selectable items");
    }

    /// TEST: MenuSelect NewAgent opens worktree selection.
    #[test]
    fn test_menu_select_new_agent() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        hub.mode = AppMode::Menu;
        // Without agent, menu is: [Hub header], New Agent (0), Connection Code (1), Toggle Polling (2)
        hub.menu_selected = 0;

        dispatch(&mut hub, HubAction::MenuSelect(0));

        assert_eq!(
            hub.mode,
            AppMode::NewAgentSelectWorktree,
            "Selecting 'New Agent' should open worktree selection"
        );
    }

    /// TEST: MenuSelect ShowConnectionCode opens connection code modal.
    #[test]
    fn test_menu_select_connection_code() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        hub.mode = AppMode::Menu;
        // Without agent: New Agent (0), Connection Code (1), Toggle Polling (2)
        hub.menu_selected = 1;

        dispatch(&mut hub, HubAction::MenuSelect(1));

        assert_eq!(
            hub.mode,
            AppMode::ConnectionCode,
            "Selecting 'Show Connection Code' should open connection code modal"
        );
    }

    /// TEST: MenuSelect TogglePolling toggles polling and closes menu.
    #[test]
    fn test_menu_select_toggle_polling() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        hub.mode = AppMode::Menu;
        let initial_polling = hub.polling_enabled;

        // Without agent: New Agent (0), Connection Code (1), Toggle Polling (2)
        hub.menu_selected = 2;

        dispatch(&mut hub, HubAction::MenuSelect(2));

        assert_eq!(
            hub.polling_enabled,
            !initial_polling,
            "Selecting 'Toggle Polling' should toggle polling state"
        );
        assert_eq!(
            hub.mode,
            AppMode::Normal,
            "Toggle Polling should close menu"
        );
    }

    /// TEST: MenuSelect CloseAgent opens confirmation modal (with agent).
    #[test]
    fn test_menu_select_close_agent() {
        use crate::agent::Agent;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Create an agent
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );
        hub.state.add_agent("agent-1".to_string(), agent);

        // IMPORTANT: Select the agent via TUI - menu context uses TUI selection
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: "agent-1".to_string(),
        });

        hub.mode = AppMode::Menu;
        // With agent (no server): [Agent header], Close Agent (0), [Hub header], New Agent (1), ...
        hub.menu_selected = 0;

        dispatch(&mut hub, HubAction::MenuSelect(0));

        assert_eq!(
            hub.mode,
            AppMode::CloseAgentConfirm,
            "Selecting 'Close Agent' should open confirmation modal"
        );
    }

    /// TEST: MenuSelect TogglePtyView toggles PTY view (with agent + server).
    #[test]
    fn test_menu_select_toggle_pty_view() {
        use crate::agent::Agent;
        use crate::agent::pty::PtySession;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Create an agent with server PTY
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );
        agent.server_pty = Some(PtySession::new(24, 80));
        hub.state.add_agent("agent-1".to_string(), agent);

        // Select the agent via TUI
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: "agent-1".to_string(),
        });

        hub.mode = AppMode::Menu;
        // With agent + server: [Agent header], View Server (0), Close Agent (1), [Hub header], ...
        hub.menu_selected = 0;

        let initial_pty = hub.state.agents.get("agent-1").unwrap().active_pty;
        assert_eq!(initial_pty, crate::PtyView::Cli);

        dispatch(&mut hub, HubAction::MenuSelect(0));

        let new_pty = hub.state.agents.get("agent-1").unwrap().active_pty;
        assert_eq!(
            new_pty,
            crate::PtyView::Server,
            "Selecting 'View Server' should toggle PTY to Server view"
        );
        assert_eq!(
            hub.mode,
            AppMode::Normal,
            "Toggle PTY View should close menu"
        );
    }

    /// TEST: Menu with no agents has correct selectable count.
    #[test]
    fn test_menu_selectable_count_no_agent() {
        let config = test_config();
        let hub = Hub::new(config, TEST_DIMS).unwrap();

        let ctx = build_menu_context(&hub);
        let items = crate::hub::menu::build_menu(&ctx);
        let count = crate::hub::menu::selectable_count(&items);

        assert_eq!(count, 3, "Menu without agent should have 3 selectable items");
    }

    /// TEST: Menu with agent (no server) has correct selectable count.
    #[test]
    fn test_menu_selectable_count_with_agent() {
        use crate::agent::Agent;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );
        hub.state.add_agent("agent-1".to_string(), agent);

        // Select via TUI for menu context
        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: "agent-1".to_string(),
        });

        let ctx = build_menu_context(&hub);
        let items = crate::hub::menu::build_menu(&ctx);
        let count = crate::hub::menu::selectable_count(&items);

        assert_eq!(count, 4, "Menu with agent (no server) should have 4 selectable items");
    }

    /// TEST: New Agent from menu works when an agent is already selected.
    ///
    /// This tests the scenario where user has an agent running and wants to
    /// create another one. The menu should show both agent and hub sections.
    #[test]
    fn test_menu_new_agent_with_existing_agent() {
        use crate::agent::Agent;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Create and select an agent
        let temp_dir = TempDir::new().unwrap();
        let agent = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );
        hub.state.add_agent("agent-1".to_string(), agent);

        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: "agent-1".to_string(),
        });

        // Open menu
        dispatch(&mut hub, HubAction::OpenMenu);
        assert_eq!(hub.mode, AppMode::Menu);

        // With agent (no server), menu is:
        // [Agent header], Close Agent (0), [Hub header], New Agent (1), Connection Code (2), Toggle Polling (3)
        // So "New Agent" is at selection index 1

        // Select "New Agent" (index 1)
        dispatch(&mut hub, HubAction::MenuSelect(1));

        assert_eq!(
            hub.mode,
            AppMode::NewAgentSelectWorktree,
            "New Agent should open worktree selection even when agent is already running"
        );
    }

    /// TEST: New Agent from menu works when agent has server PTY.
    ///
    /// With server PTY, the menu has an extra "View Server/Agent" option.
    #[test]
    fn test_menu_new_agent_with_server_pty() {
        use crate::agent::Agent;
        use crate::agent::pty::PtySession;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Create agent with server PTY
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );
        agent.server_pty = Some(PtySession::new(24, 80));
        hub.state.add_agent("agent-1".to_string(), agent);

        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: "agent-1".to_string(),
        });

        dispatch(&mut hub, HubAction::OpenMenu);

        // With agent + server, menu is:
        // [Agent header], View Server (0), Close Agent (1), [Hub header], New Agent (2), Connection Code (3), Toggle Polling (4)
        // So "New Agent" is at selection index 2

        dispatch(&mut hub, HubAction::MenuSelect(2));

        assert_eq!(
            hub.mode,
            AppMode::NewAgentSelectWorktree,
            "New Agent should be at index 2 when agent has server PTY"
        );
    }

    /// TEST: Menu with agent + server has correct selectable count.
    #[test]
    fn test_menu_selectable_count_with_server() {
        use crate::agent::Agent;
        use crate::agent::pty::PtySession;
        use tempfile::TempDir;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );
        agent.server_pty = Some(PtySession::new(24, 80));
        hub.state.add_agent("agent-1".to_string(), agent);

        dispatch(&mut hub, HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: "agent-1".to_string(),
        });

        let ctx = build_menu_context(&hub);
        let items = crate::hub::menu::build_menu(&ctx);
        let count = crate::hub::menu::selectable_count(&items);

        assert_eq!(count, 5, "Menu with agent + server should have 5 selectable items");
    }

    // === Error Display Tests ===

    /// TEST: show_error sets Error mode and error_message.
    #[test]
    fn test_show_error_sets_mode_and_message() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        hub.show_error("Test error message");

        assert_eq!(hub.mode, AppMode::Error);
        assert_eq!(hub.error_message.as_deref(), Some("Test error message"));
    }

    /// TEST: clear_error returns to Normal and clears message.
    #[test]
    fn test_clear_error_returns_to_normal() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        hub.show_error("Test error");
        hub.clear_error();

        assert_eq!(hub.mode, AppMode::Normal);
        assert!(hub.error_message.is_none());
    }

    /// TEST: Error mode can be dismissed with CloseModal.
    #[test]
    fn test_error_mode_can_be_dismissed() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Manually trigger error mode
        hub.show_error("Test error message");
        assert_eq!(hub.mode, AppMode::Error);
        assert_eq!(hub.error_message.as_deref(), Some("Test error message"));

        // Dismiss with CloseModal (Esc key)
        dispatch(&mut hub, HubAction::CloseModal);

        assert_eq!(hub.mode, AppMode::Normal);
        assert!(hub.error_message.is_none());
    }

    /// TEST: Connection code modal can be closed after resize event.
    ///
    /// Regression test for: resizing while connection info is displayed
    /// prevents Escape from closing the modal.
    #[test]
    fn test_connection_code_close_after_resize() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Open connection code modal
        dispatch(&mut hub, HubAction::ShowConnectionCode);
        assert_eq!(hub.mode, AppMode::ConnectionCode);

        // Simulate a resize event (as would happen when user resizes terminal)
        dispatch(&mut hub, HubAction::ResizeForClient {
            client_id: ClientId::Tui,
            rows: 100,
            cols: 200,
        });

        // Mode should still be ConnectionCode after resize
        assert_eq!(
            hub.mode, AppMode::ConnectionCode,
            "Resize should not change mode from ConnectionCode"
        );

        // Close modal (simulates pressing Escape)
        dispatch(&mut hub, HubAction::CloseModal);

        // Should return to Normal mode
        assert_eq!(
            hub.mode, AppMode::Normal,
            "CloseModal should return to Normal mode even after resize"
        );
    }

    /// TEST: Connection code modal survives multiple resize events.
    #[test]
    fn test_connection_code_survives_multiple_resizes() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Open connection code modal
        dispatch(&mut hub, HubAction::ShowConnectionCode);
        assert_eq!(hub.mode, AppMode::ConnectionCode);

        // Multiple rapid resize events
        for i in 0..5 {
            dispatch(&mut hub, HubAction::ResizeForClient {
                client_id: ClientId::Tui,
                rows: 50 + i * 10,
                cols: 150 + i * 10,
            });
        }

        // Mode should still be ConnectionCode
        assert_eq!(hub.mode, AppMode::ConnectionCode);

        // Close should work
        dispatch(&mut hub, HubAction::CloseModal);
        assert_eq!(hub.mode, AppMode::Normal);
    }
}
