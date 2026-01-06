//! Application state types for the botster-hub TUI.
//!
//! This module defines the core state types used by the TUI application,
//! including the mode enum that controls the current UI state.

/// The current operational mode of the TUI application.
///
/// The TUI operates as a state machine, with different modes controlling
/// what the user sees and can do. Transitions between modes are triggered
/// by user input (keypresses, menu selections).
///
/// # Mode Transitions
///
/// - `Normal` is the default mode, showing agents and terminal
/// - Pressing 'm' or 'M' opens the `Menu`
/// - From Menu, users can select actions that transition to other modes
/// - All modes can return to `Normal` via Escape
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum AppMode {
    /// Default mode: showing agent list and selected agent's terminal.
    ///
    /// Key bindings in this mode:
    /// - `m/M`: Open menu
    /// - `q`: Quit application
    /// - `↑/↓/j/k`: Navigate agent list
    /// - `t/T`: Toggle PTY view (CLI/Server)
    /// - `n/p`: Navigate scrollback
    /// - `1-9`: Quick select agent
    #[default]
    Normal,

    /// Menu popup is displayed over the terminal.
    ///
    /// Key bindings:
    /// - `↑/↓/j/k`: Navigate menu items
    /// - `Enter`: Select current item
    /// - `Esc/q`: Close menu, return to Normal
    Menu,

    /// Selecting an existing worktree to spawn a new agent.
    ///
    /// Shows a list of existing worktrees with an option to create new.
    /// Key bindings:
    /// - `↑/↓/j/k`: Navigate worktree list
    /// - `Enter`: Select worktree or open create dialog
    /// - `Esc/q`: Cancel, return to Normal
    NewAgentSelectWorktree,

    /// Creating a new worktree for an agent.
    ///
    /// User enters an issue number or branch name.
    /// Key bindings:
    /// - Text input for issue/branch
    /// - `Enter`: Create worktree and proceed to prompt
    /// - `Esc`: Cancel, return to Normal
    NewAgentCreateWorktree,

    /// Entering the initial prompt for a new agent.
    ///
    /// User types the prompt that will be sent to the agent.
    /// Key bindings:
    /// - Text input for prompt
    /// - `Enter`: Spawn agent with prompt
    /// - `Esc`: Cancel, return to Normal
    NewAgentPrompt,

    /// Confirming closure of the selected agent.
    ///
    /// Key bindings:
    /// - `y/Y/Enter`: Confirm close
    /// - `n/N/Esc/q`: Cancel, return to Normal
    CloseAgentConfirm,

    /// Displaying connection code and QR code for browser access.
    ///
    /// Shows the hub identifier and a QR code that can be scanned
    /// to connect from the web interface.
    /// Key bindings:
    /// - `Esc/q/Enter`: Close, return to Normal
    ConnectionCode,
}

impl AppMode {
    /// Returns true if this mode is a modal overlay (shown over the terminal).
    pub fn is_modal(&self) -> bool {
        !matches!(self, AppMode::Normal)
    }

    /// Returns true if this mode accepts text input.
    pub fn accepts_text_input(&self) -> bool {
        matches!(
            self,
            AppMode::NewAgentCreateWorktree | AppMode::NewAgentPrompt
        )
    }

    /// Returns a human-readable name for the mode.
    pub fn display_name(&self) -> &'static str {
        match self {
            AppMode::Normal => "Normal",
            AppMode::Menu => "Menu",
            AppMode::NewAgentSelectWorktree => "Select Worktree",
            AppMode::NewAgentCreateWorktree => "Create Worktree",
            AppMode::NewAgentPrompt => "Enter Prompt",
            AppMode::CloseAgentConfirm => "Confirm Close",
            AppMode::ConnectionCode => "Connection Code",
        }
    }
}

/// Represents a worktree selection in the UI.
///
/// Used when displaying the list of available worktrees for spawning
/// a new agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeSelection {
    /// Path to the worktree directory.
    pub path: String,
    /// Branch name the worktree is on.
    pub branch: String,
}

impl WorktreeSelection {
    /// Creates a new worktree selection.
    pub fn new(path: String, branch: String) -> Self {
        Self { path, branch }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_mode_default() {
        assert_eq!(AppMode::default(), AppMode::Normal);
    }

    #[test]
    fn test_app_mode_is_modal() {
        assert!(!AppMode::Normal.is_modal());
        assert!(AppMode::Menu.is_modal());
        assert!(AppMode::NewAgentSelectWorktree.is_modal());
        assert!(AppMode::NewAgentCreateWorktree.is_modal());
        assert!(AppMode::NewAgentPrompt.is_modal());
        assert!(AppMode::CloseAgentConfirm.is_modal());
        assert!(AppMode::ConnectionCode.is_modal());
    }

    #[test]
    fn test_app_mode_accepts_text_input() {
        assert!(!AppMode::Normal.accepts_text_input());
        assert!(!AppMode::Menu.accepts_text_input());
        assert!(!AppMode::NewAgentSelectWorktree.accepts_text_input());
        assert!(AppMode::NewAgentCreateWorktree.accepts_text_input());
        assert!(AppMode::NewAgentPrompt.accepts_text_input());
        assert!(!AppMode::CloseAgentConfirm.accepts_text_input());
        assert!(!AppMode::ConnectionCode.accepts_text_input());
    }

    #[test]
    fn test_app_mode_display_name() {
        assert_eq!(AppMode::Normal.display_name(), "Normal");
        assert_eq!(AppMode::Menu.display_name(), "Menu");
    }

    #[test]
    fn test_worktree_selection_creation() {
        let selection = WorktreeSelection::new(
            "/path/to/worktree".to_string(),
            "feature-branch".to_string(),
        );
        assert_eq!(selection.path, "/path/to/worktree");
        assert_eq!(selection.branch, "feature-branch");
    }
}
