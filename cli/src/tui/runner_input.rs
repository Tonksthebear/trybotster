//! TUI Runner Input Handlers - menu selection and input submission.
//!
//! This module contains the handlers for menu selections, worktree selections,
//! and input submissions. These handlers process user input after it has been
//! converted to actions.
//!
//! # Handler Types
//!
//! - [`handle_menu_select`] - Processes menu item selection using dynamic menu
//! - [`handle_worktree_select`] - Handles worktree selection for agent creation
//! - [`handle_input_submit`] - Processes text input submission
//! - [`handle_confirm_close_agent`] - Handles agent close confirmation

// Rust guideline compliant 2026-01

use std::path::PathBuf;

use ratatui::backend::Backend;
use vt100::Parser;

use crate::agent::PtyView;
use crate::app::AppMode;
use crate::client::{CreateAgentRequest, DeleteAgentRequest, TuiRequest};
use crate::tui::events::CreationStage;
use crate::tui::menu::{build_menu, get_action_for_selection, MenuAction, MenuContext};

use super::runner::{TuiRunner, DEFAULT_SCROLLBACK};

impl<B> TuiRunner<B>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    /// Handle menu selection using the dynamic menu system.
    ///
    /// Builds a `MenuContext` from current state and uses `menu::get_action_for_selection()`
    /// to determine which action to execute. This ensures menu behavior stays consistent
    /// with the dynamic menu structure (which changes based on whether an agent is selected,
    /// etc.).
    ///
    /// # Arguments
    ///
    /// * `idx` - The selection index (0-based among selectable items)
    pub fn handle_menu_select(&mut self, idx: usize) {
        // Build menu context from current state
        let menu_context = self.build_menu_context();
        let menu_items = build_menu(&menu_context);

        // Get the action for this selection index
        let Some(action) = get_action_for_selection(&menu_items, idx) else {
            // Invalid selection - close menu and return to normal
            self.mode = AppMode::Normal;
            return;
        };

        match action {
            MenuAction::NewAgent => {
                self.mode = AppMode::NewAgentSelectWorktree;
                self.worktree_selected = 0;
                // Request worktree list via TuiRequest (blocking with oneshot response)
                let (response_tx, response_rx) = tokio::sync::oneshot::channel();
                if self.request_tx.send(TuiRequest::ListWorktrees { response_tx }).is_ok() {
                    match response_rx.blocking_recv() {
                        Ok(worktrees) => {
                            self.available_worktrees = worktrees;
                            log::debug!("Loaded {} worktrees", self.available_worktrees.len());
                        }
                        Err(_) => {
                            log::error!("Failed to receive worktree list");
                            self.available_worktrees = Vec::new();
                        }
                    }
                } else {
                    log::error!("Failed to send list worktrees request");
                    self.available_worktrees = Vec::new();
                }
            }
            MenuAction::CloseAgent => {
                if self.selected_agent.is_some() {
                    self.mode = AppMode::CloseAgentConfirm;
                }
            }
            MenuAction::ShowConnectionCode => {
                self.mode = AppMode::ConnectionCode;
                self.qr_image_displayed = false;
            }
            MenuAction::TogglePtyView => {
                // Toggle PTY view - same logic as TuiAction::TogglePtyView
                if let Some(agent_index) = self.current_agent_index {
                    let current_view = self.active_pty_view;
                    let new_view = match current_view {
                        PtyView::Cli => PtyView::Server,
                        PtyView::Server => PtyView::Cli,
                    };

                    // Get PTY index (0 = CLI, 1 = Server)
                    let pty_index = match new_view {
                        PtyView::Cli => 0,
                        PtyView::Server => 1,
                    };

                    // Check if PTY exists and connect
                    let can_toggle = match new_view {
                        PtyView::Cli => true, // CLI PTY always exists
                        PtyView::Server => self.has_server_pty,
                    };

                    if can_toggle {
                        self.active_pty_view = new_view;
                        self.current_pty_index = Some(pty_index);

                        if let Err(e) = self.request_tx.send(TuiRequest::ConnectToPty {
                            agent_index,
                            pty_index,
                        }) {
                            log::warn!("Failed to send connect to PTY request: {}", e);
                        }

                        // Clear and reset parser for new PTY stream
                        let mut parser = self.vt100_parser.lock().expect("parser lock poisoned");
                        let (rows, cols) = self.terminal_dims;
                        *parser = Parser::new(rows, cols, DEFAULT_SCROLLBACK);
                    }
                }
                self.mode = AppMode::Normal;
            }
        }
    }

    /// Build a `MenuContext` from current TuiRunner state.
    ///
    /// Used by `handle_menu_select()` to determine which menu action maps to a given
    /// selection index.
    ///
    /// # Returns
    ///
    /// A `MenuContext` reflecting the current TUI state for dynamic menu building.
    pub fn build_menu_context(&self) -> MenuContext {
        MenuContext {
            has_agent: self.selected_agent.is_some(),
            has_server_pty: self.has_server_pty,
            active_pty: self.active_pty_view,
        }
    }

    /// Handle worktree selection.
    ///
    /// Index 0 means "Create new worktree", which transitions to the worktree
    /// creation input mode. Any other index selects an existing worktree and
    /// immediately creates an agent for it.
    ///
    /// # Arguments
    ///
    /// * `idx` - The selection index (0 = create new, 1+ = existing worktree)
    pub fn handle_worktree_select(&mut self, idx: usize) {
        if idx == 0 {
            // Create new worktree
            self.mode = AppMode::NewAgentCreateWorktree;
            self.input_buffer.clear();
        } else {
            // Reopen existing worktree
            let worktree_idx = idx - 1;
            if worktree_idx < self.available_worktrees.len() {
                let (path, branch) = &self.available_worktrees[worktree_idx];
                log::info!("Reopening worktree: {} (branch: {})", path, branch);

                // Create agent request with the existing worktree
                let request =
                    CreateAgentRequest::new(branch.clone()).from_worktree(PathBuf::from(path));
                if let Err(e) = self.request_tx.send(TuiRequest::CreateAgent { request }) {
                    // Channel closed - TuiClient is shutting down
                    log::error!("Failed to send create agent request: {}", e);
                    self.mode = AppMode::Normal;
                } else {
                    // Track pending creation for sidebar display
                    self.creating_agent = Some((branch.clone(), CreationStage::CreatingWorktree));
                    // Close modal immediately - creation progress shown in sidebar
                    self.mode = AppMode::Normal;
                }
            }
        }
    }

    /// Handle text input submission.
    ///
    /// The behavior depends on the current mode:
    ///
    /// - `NewAgentCreateWorktree`: Validates non-empty input, stores it as pending
    ///   issue/branch, and transitions to prompt mode.
    /// - `NewAgentPrompt`: Creates the agent with optional prompt and returns to Normal.
    ///
    /// Empty issue/branch names are rejected (stays in current mode).
    pub fn handle_input_submit(&mut self) {
        match self.mode {
            AppMode::NewAgentCreateWorktree => {
                if !self.input_buffer.is_empty() {
                    // Store the issue/branch and transition to prompt mode
                    self.pending_issue_or_branch = Some(self.input_buffer.clone());
                    self.input_buffer.clear();
                    self.mode = AppMode::NewAgentPrompt;
                }
            }
            AppMode::NewAgentPrompt => {
                // Get the issue/branch from pending field
                if let Some(issue_or_branch) = self.pending_issue_or_branch.take() {
                    // Create the agent request with optional prompt
                    let prompt = if self.input_buffer.is_empty() {
                        None
                    } else {
                        Some(self.input_buffer.clone())
                    };

                    log::info!(
                        "Creating agent for '{}' with prompt: {:?}",
                        issue_or_branch,
                        prompt
                    );

                    // Build the CreateAgentRequest
                    let mut request = CreateAgentRequest::new(issue_or_branch.clone());
                    if let Some(p) = prompt {
                        request = request.with_prompt(p);
                    }

                    // Send the create agent request via TuiClient
                    if let Err(e) = self.request_tx.send(TuiRequest::CreateAgent { request }) {
                        // Channel closed - TuiClient is shutting down
                        log::error!("Failed to send create agent request: {}", e);
                        self.mode = AppMode::Normal;
                        self.input_buffer.clear();
                    } else {
                        // Track pending creation for sidebar display
                        self.creating_agent =
                            Some((issue_or_branch, CreationStage::CreatingWorktree));
                        self.input_buffer.clear();
                        // Close modal immediately - creation progress shown in sidebar
                        self.mode = AppMode::Normal;
                    }
                } else {
                    // No pending issue/branch - just return to normal
                    self.mode = AppMode::Normal;
                    self.input_buffer.clear();
                }
            }
            _ => {}
        }
    }

    /// Handle confirm close agent.
    ///
    /// Sends a `DeleteAgent` request via TuiClient with the current selected agent.
    /// The `delete_worktree` flag determines whether to also delete the worktree.
    ///
    /// # Arguments
    ///
    /// * `delete_worktree` - If true, also delete the agent's worktree
    pub fn handle_confirm_close_agent(&mut self, delete_worktree: bool) {
        if let Some(ref key) = self.selected_agent {
            let request = if delete_worktree {
                DeleteAgentRequest::new(key).with_worktree_deletion()
            } else {
                DeleteAgentRequest::new(key)
            };
            let _ = self.request_tx.send(TuiRequest::DeleteAgent { request });
        }
        self.mode = AppMode::Normal;
    }
}
