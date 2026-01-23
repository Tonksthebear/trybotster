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
                // Send command to Hub to regenerate Signal keys and QR code
                let action = HubAction::RegenerateConnectionCode;
                if let Err(e) = self.command_tx.dispatch_action_blocking(action) {
                    log::error!("Failed to send regenerate connection code: {}", e);
                }
                self.qr_image_displayed = false;
            }

            TuiAction::CopyConnectionUrl => {
                if let Some(ref _url) = self.connection_url {
                    log::info!("Copy connection URL requested");
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
            let current_view = self.client.active_pty_view();
            let new_view = match current_view {
                PtyView::Cli => PtyView::Server,
                PtyView::Server => PtyView::Cli,
            };

            // Get the PTY handle for the new view
            let new_pty = match new_view {
                PtyView::Cli => Some(handle.cli_pty().clone()),
                PtyView::Server => handle.server_pty().cloned(),
            };

            if let Some(pty) = new_pty {
                log::debug!(
                    "Toggling PTY view to {:?} for agent {}",
                    new_view,
                    handle.agent_id()
                );
                // Update client state and reconnect to new PTY
                self.client.set_active_pty_view(new_view);
                self.client.connect_to_pty(handle.agent_id(), pty);

                // Clear and reset parser for new PTY stream
                let mut parser = self.vt100_parser.lock().expect("parser lock poisoned");
                let (rows, cols) = self.terminal_dims;
                *parser = Parser::new(rows, cols, DEFAULT_SCROLLBACK);
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
                    self.agents.push(info);
                }
                // Clear creating indicator
                if self.creating_agent.as_ref().map(|(id, _)| id) == Some(&agent_id) {
                    self.creating_agent = None;
                }
            }

            HubEvent::AgentDeleted { agent_id } => {
                log::debug!("TUI: Agent deleted: {}", agent_id);
                // Find index before removing (needed for TuiClient)
                let index = self.agents.iter().position(|a| a.id == agent_id);
                // Remove from local cache
                self.agents.retain(|a| a.id != agent_id);
                // Notify client (handles selection and PTY cleanup)
                if let Some(idx) = index {
                    self.client.on_agent_deleted(idx);
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
