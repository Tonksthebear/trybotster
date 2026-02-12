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

// Rust guideline compliant 2026-02

use ratatui::backend::Backend;

use crate::app::AppMode;
use crate::tui::events::CreationStage;

use super::runner::TuiRunner;

impl<B> TuiRunner<B>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    /// Handle menu selection using cached overlay actions.
    ///
    /// Looks up the action string from `overlay_list_actions` (populated
    /// during the last Lua render pass) by selection index. This avoids
    /// rebuilding the menu — Lua is the single source of truth for menu
    /// items and their actions.
    ///
    /// # Arguments
    ///
    /// * `idx` - The selection index (0-based among selectable items)
    pub fn handle_menu_select(&mut self, idx: usize) {
        let Some(action) = self.overlay_list_actions.get(idx) else {
            self.mode = AppMode::Normal;
            return;
        };

        match action.as_str() {
            "new_agent" => {
                self.mode = AppMode::NewAgentSelectWorktree;
                self.worktree_selected = 0;
                // Request a fresh worktree list via Lua (non-blocking, response updates cache)
                self.send_msg(serde_json::json!({
                    "subscriptionId": "tui_hub",
                    "data": { "type": "list_worktrees" }
                }));
                log::debug!("Using {} cached worktrees", self.available_worktrees.len());
            }
            "close_agent" => {
                if self.selected_agent.is_some() {
                    self.mode = AppMode::CloseAgentConfirm;
                }
            }
            "show_connection_code" => {
                self.mode = AppMode::ConnectionCode;
                self.send_msg(serde_json::json!({
                    "subscriptionId": "tui_hub",
                    "data": { "type": "get_connection_code" }
                }));
            }
            "toggle_pty" => {
                self.handle_pty_view_toggle();
                self.mode = AppMode::Normal;
            }
            _ => {
                log::warn!("Unknown menu action: {action}");
                self.mode = AppMode::Normal;
            }
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

                // Send reopen_worktree via Lua client protocol (same path as browser)
                self.send_msg(serde_json::json!({
                    "subscriptionId": "tui_hub",
                    "data": {
                        "type": "reopen_worktree",
                        "path": path,
                        "branch": branch,
                    }
                }));

                // Track pending creation for sidebar display
                self.creating_agent = Some((branch.clone(), CreationStage::CreatingWorktree));
                // Close modal immediately - creation progress shown in sidebar
                self.mode = AppMode::Normal;
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
                        serde_json::Value::Null
                    } else {
                        serde_json::Value::String(self.input_buffer.clone())
                    };

                    log::info!(
                        "Creating agent for '{}' with prompt: {}",
                        issue_or_branch,
                        prompt
                    );

                    // Send create_agent via Lua client protocol (same path as browser)
                    self.send_msg(serde_json::json!({
                        "subscriptionId": "tui_hub",
                        "data": {
                            "type": "create_agent",
                            "issue_or_branch": issue_or_branch,
                            "prompt": prompt,
                        }
                    }));

                    // Track pending creation for sidebar display
                    self.creating_agent =
                        Some((issue_or_branch, CreationStage::CreatingWorktree));
                    self.input_buffer.clear();
                    // Close modal immediately - creation progress shown in sidebar
                    self.mode = AppMode::Normal;
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
    /// Sends a `delete_agent` request via Lua client protocol for the current
    /// selected agent. The `delete_worktree` flag determines whether to also
    /// delete the agent's worktree.
    ///
    /// # Arguments
    ///
    /// * `delete_worktree` - If true, also delete the agent's worktree
    pub fn handle_confirm_close_agent(&mut self, delete_worktree: bool) {
        if let Some(ref key) = self.selected_agent {
            // Send delete_agent via Lua client protocol (same path as browser)
            self.send_msg(serde_json::json!({
                "subscriptionId": "tui_hub",
                "data": {
                    "type": "delete_agent",
                    "agent_id": key,
                    "delete_worktree": delete_worktree,
                }
            }));
        }
        self.mode = AppMode::Normal;
    }
}
