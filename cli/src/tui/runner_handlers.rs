//! TUI Runner Handlers - action and event handlers for TuiRunner.
//!
//! This module contains the handler methods that process TUI actions and Hub events.
//! These are extracted from `TuiRunner` to keep the main module focused on the
//! event loop and state management.
//!
//! # Handler Categories
//!
//! - [`handle_tui_action`] - Processes `TuiAction` variants (UI state changes)
//! - [`handle_hub_event`] - Processes `HubEvent` variants (Hub broadcasts)
//! - [`handle_input_event`] - Converts terminal events to actions
//! - [`handle_pty_input`] - Sends input to connected PTY
//! - [`handle_resize`] - Handles terminal resize events

// Rust guideline compliant 2026-01

use ratatui::backend::Backend;
use vt100::Parser;

use crate::agent::PtyView;
use crate::client::Client;
use crate::hub::{HubAction, HubEvent};

use super::actions::TuiAction;
use super::events::CreationStage;
use super::runner::{TuiRunner, DEFAULT_SCROLLBACK};

impl<B> TuiRunner<B>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    /// Handle a TUI action generated from input.
    ///
    /// TUI actions are handled locally (UI state changes). This is the main
    /// dispatch function for all user interactions converted to actions.
    ///
    /// # Action Categories
    ///
    /// - Application Control: `Quit`
    /// - Modal State: `OpenMenu`, `CloseModal`
    /// - Menu Navigation: `MenuUp`, `MenuDown`, `MenuSelect`
    /// - Worktree Selection: `WorktreeUp`, `WorktreeDown`, `WorktreeSelect`
    /// - Text Input: `InputChar`, `InputBackspace`, `InputSubmit`
    /// - Connection Code: `ShowConnectionCode`, `RegenerateConnectionCode`, `CopyConnectionUrl`
    /// - Agent Close: `ConfirmCloseAgent`, `ConfirmCloseAgentDeleteWorktree`
    /// - Scrolling: `ScrollUp`, `ScrollDown`, `ScrollToTop`, `ScrollToBottom`
    /// - Agent Navigation: `SelectNext`, `SelectPrevious`
    /// - PTY View: `TogglePtyView`
    pub fn handle_tui_action(&mut self, action: TuiAction) {
        use crate::app::AppMode;

        match action {
            // === Application Control ===
            TuiAction::Quit => {
                self.quit = true;
                let _ = self.command_tx.quit_blocking();
            }

            // === Modal State ===
            TuiAction::OpenMenu => {
                self.mode = AppMode::Menu;
                self.menu_selected = 0;
            }

            TuiAction::CloseModal => {
                // Delete Kitty graphics images if closing ConnectionCode modal
                if self.mode == AppMode::ConnectionCode {
                    use crate::tui::qr::kitty_delete_images;
                    use std::io::Write;
                    let _ = std::io::stdout().write_all(kitty_delete_images().as_bytes());
                    let _ = std::io::stdout().flush();
                }
                self.mode = AppMode::Normal;
                self.input_buffer.clear();
                self.qr_image_displayed = false;
            }

            // === Menu Navigation ===
            TuiAction::MenuUp => {
                if self.menu_selected > 0 {
                    self.menu_selected -= 1;
                }
            }

            TuiAction::MenuDown => {
                // Use dynamic menu's selectable item count, not static constant
                let menu_context = self.build_menu_context();
                let menu_items = crate::tui::menu::build_menu(&menu_context);
                let max_idx = crate::tui::menu::selectable_count(&menu_items).saturating_sub(1);
                self.menu_selected = (self.menu_selected + 1).min(max_idx);
            }

            TuiAction::MenuSelect(idx) => {
                self.handle_menu_select(idx);
            }

            // === Worktree Selection ===
            TuiAction::WorktreeUp => {
                if self.worktree_selected > 0 {
                    self.worktree_selected -= 1;
                }
            }

            TuiAction::WorktreeDown => {
                let max = self.available_worktrees.len();
                self.worktree_selected = (self.worktree_selected + 1).min(max);
            }

            TuiAction::WorktreeSelect(idx) => {
                self.handle_worktree_select(idx);
            }

            // === Text Input ===
            TuiAction::InputChar(c) => {
                self.input_buffer.push(c);
            }

            TuiAction::InputBackspace => {
                self.input_buffer.pop();
            }

            TuiAction::InputSubmit => {
                self.handle_input_submit();
            }

            // === Connection Code ===
            TuiAction::ShowConnectionCode => {
                self.mode = AppMode::ConnectionCode;
                self.qr_image_displayed = false;
            }

            TuiAction::RegenerateConnectionCode => {
                // Use fire-and-forget dispatch to avoid blocking the TUI event loop.
                // The Hub spawns an async task for bundle regeneration. When the new
                // bundle arrives, the QR will refresh on next render (qr_image_displayed = false).
                let action = HubAction::RegenerateConnectionCode;
                if let Err(e) = self.command_tx.dispatch_action_blocking(action) {
                    log::error!("Failed to dispatch regenerate connection code: {}", e);
                }
                self.qr_image_displayed = false;
            }

            TuiAction::CopyConnectionUrl => {
                // Dispatch to Hub which has access to arboard clipboard
                let action = HubAction::CopyConnectionUrl;
                if let Err(e) = self.command_tx.dispatch_action_blocking(action) {
                    log::error!("Failed to send copy connection URL: {}", e);
                }
            }

            // === Agent Close Confirmation ===
            TuiAction::ConfirmCloseAgent => {
                self.handle_confirm_close_agent(false);
            }

            TuiAction::ConfirmCloseAgentDeleteWorktree => {
                self.handle_confirm_close_agent(true);
            }

            // === Scrolling (local to TUI parser) ===
            TuiAction::ScrollUp(lines) => {
                crate::tui::scroll::up_parser(&self.vt100_parser, lines);
            }

            TuiAction::ScrollDown(lines) => {
                crate::tui::scroll::down_parser(&self.vt100_parser, lines);
            }

            TuiAction::ScrollToTop => {
                crate::tui::scroll::to_top_parser(&self.vt100_parser);
            }

            TuiAction::ScrollToBottom => {
                crate::tui::scroll::to_bottom_parser(&self.vt100_parser);
            }

            // === Agent Navigation (request from Hub) ===
            TuiAction::SelectNext => {
                self.request_select_next();
            }

            TuiAction::SelectPrevious => {
                self.request_select_previous();
            }

            // === PTY View Toggle ===
            TuiAction::TogglePtyView => {
                self.handle_pty_view_toggle();
            }

            TuiAction::None => {}
        }
    }

    /// Handle PTY view toggle action.
    ///
    /// Toggles between CLI and Server PTY for the current agent.
    /// If no server PTY is available, this is a no-op.
    fn handle_pty_view_toggle(&mut self) {
        if let Some(ref handle) = self.agent_handle {
            // Toggle the view
            let current_view = self.active_pty_view;
            let new_view = match current_view {
                PtyView::Cli => PtyView::Server,
                PtyView::Server => PtyView::Cli,
            };

            // Get the PTY index for the new view (0 = CLI, 1 = Server)
            let pty_index = match new_view {
                PtyView::Cli => 0,
                PtyView::Server => 1,
            };

            // Check if the PTY exists for this view
            if handle.get_pty(pty_index).is_some() {
                log::debug!(
                    "Toggling PTY view to {:?} for agent {}",
                    new_view,
                    handle.agent_id()
                );

                // Find agent index in our local list
                let agent_id = handle.agent_id();
                let agent_index = self
                    .agents
                    .iter()
                    .position(|a| a.id == agent_id)
                    .unwrap_or(0);

                // Update TuiRunner state
                self.active_pty_view = new_view;
                self.current_pty_index = Some(pty_index);

                // Reset parser FIRST (before loading new scrollback)
                {
                    let mut parser = self.vt100_parser.lock().expect("parser lock poisoned");
                    let (rows, cols) = self.terminal_dims;
                    *parser = Parser::new(rows, cols, DEFAULT_SCROLLBACK);
                }

                // Connect to PTY - scrollback arrives via channel, gets processed
                // in poll_pty_events() and fed to the fresh parser above.
                if let Err(e) = self.client.connect_to_pty(agent_index, pty_index) {
                    log::warn!("Failed to connect to PTY: {}", e);
                }
            } else {
                log::debug!("Cannot toggle to Server PTY - no server PTY available");
            }
        }
    }

    /// Handle a Hub broadcast event.
    ///
    /// Hub events are broadcasts that notify the TUI of state changes in the Hub.
    /// These include agent lifecycle events, status changes, and shutdown signals.
    ///
    /// # Event Types
    ///
    /// - `AgentCreated` - New agent spawned
    /// - `AgentDeleted` - Agent terminated
    /// - `AgentStatusChanged` - Agent status updated
    /// - `AgentCreationProgress` - Creation stage updates
    /// - `Error` - Hub error occurred
    /// - `Shutdown` - Hub is shutting down
    pub fn handle_hub_event(&mut self, event: HubEvent) {
        use crate::app::AppMode;

        match event {
            HubEvent::AgentCreated { agent_id, info } => {
                log::debug!("TUI: Agent created: {}", agent_id);
                // Add to local cache
                if !self.agents.iter().any(|a| a.id == agent_id) {
                    self.agents.push(info.clone());
                }
                // Clear creating indicator and transition to Normal if this was our pending creation
                // Match against branch_name since that's what we stored in creating_agent
                let was_creating = self
                    .creating_agent
                    .as_ref()
                    .map(|(identifier, _)| {
                        info.branch_name.as_ref() == Some(identifier)
                            || info.issue_number.map(|n| n.to_string()).as_ref() == Some(identifier)
                    })
                    .unwrap_or(false);
                if was_creating {
                    self.creating_agent = None;
                    self.mode = AppMode::Normal;
                    // Auto-select the newly created agent so user sees PTY output
                    self.request_select_agent(&agent_id);
                }
            }

            HubEvent::AgentDeleted { agent_id } => {
                log::debug!("TUI: Agent deleted: {}", agent_id);
                // Find index before removing
                let index = self.agents.iter().position(|a| a.id == agent_id);
                // Remove from local cache
                self.agents.retain(|a| a.id != agent_id);
                // If this was the selected agent, clear selection
                if self.selected_agent.as_ref() == Some(&agent_id) {
                    // Disconnect from PTY if we have indices
                    if let Some(idx) = index {
                        let pty_index = self.current_pty_index.unwrap_or(0);
                        self.client.disconnect_from_pty(idx, pty_index);
                    }
                    self.selected_agent = None;
                    self.current_agent_index = None;
                    self.current_pty_index = None;
                }
                // Clear agent handle if it was for this agent
                if self.agent_handle.as_ref().map(|h| h.agent_id()) == Some(&agent_id) {
                    self.agent_handle = None;
                }
            }

            HubEvent::AgentStatusChanged { agent_id, status } => {
                log::debug!("TUI: Agent {} status: {:?}", agent_id, status);
                // Update local cache
                if let Some(agent) = self.agents.iter_mut().find(|a| a.id == agent_id) {
                    agent.status = Some(status.to_string());
                }
            }

            HubEvent::Shutdown => {
                log::info!("TUI: Hub shutdown received");
                self.quit = true;
            }

            HubEvent::AgentCreationProgress { identifier, stage } => {
                log::debug!("TUI: Agent {} creation: {:?}", identifier, stage);
                // Convert relay stage to TUI stage for display
                let tui_stage = match stage {
                    crate::relay::AgentCreationStage::CreatingWorktree => {
                        CreationStage::CreatingWorktree
                    }
                    crate::relay::AgentCreationStage::CopyingConfig => CreationStage::CopyingConfig,
                    crate::relay::AgentCreationStage::SpawningAgent => CreationStage::SpawningAgent,
                    crate::relay::AgentCreationStage::Ready => CreationStage::Ready,
                };
                self.creating_agent = Some((identifier, tui_stage));
            }

            HubEvent::Error { message } => {
                log::error!("TUI: Hub error: {}", message);
                self.error_message = Some(message);
                self.mode = AppMode::Error;
            }
        }
    }
}
