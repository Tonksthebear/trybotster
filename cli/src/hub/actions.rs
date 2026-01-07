//! Hub actions - commands that can be dispatched to modify hub state.
//!
//! Actions represent user intent from any input source (TUI, browser, server).
//! The Hub processes actions uniformly regardless of their origin.

use std::path::PathBuf;

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
