//! TUI-local actions for UI state management.
//!
//! These actions are handled entirely within TuiRunner - they do NOT go to Hub.
//! For client operations (agent selection, PTY input, resize), TuiRunner uses
//! JSON messages through the Lua client protocol to communicate with Hub.

// Rust guideline compliant 2026-01

/// Actions handled entirely within the TUI.
///
/// These are pure UI state changes - menus, modals, text input, scrolling.
/// Client operations send JSON through the Lua client protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiAction {
    // === Application Control ===
    /// Request quit.
    Quit,

    // === Modal State ===
    /// Open the command menu.
    OpenMenu,

    /// Close any open modal/dialog.
    CloseModal,

    // === Menu Navigation ===
    /// Move menu selection up.
    MenuUp,

    /// Move menu selection down.
    MenuDown,

    /// Select menu item at index.
    MenuSelect(usize),

    // === Worktree Selection ===
    /// Move worktree selection up.
    WorktreeUp,

    /// Move worktree selection down.
    WorktreeDown,

    /// Select worktree at index.
    WorktreeSelect(usize),

    // === Text Input ===
    /// Add character to input buffer.
    InputChar(char),

    /// Delete last character from input buffer.
    InputBackspace,

    /// Submit the input buffer.
    InputSubmit,

    // === Connection Code ===
    /// Show the connection code modal.
    ShowConnectionCode,

    /// Regenerate the connection code/QR.
    RegenerateConnectionCode,

    /// Copy connection URL to clipboard.
    CopyConnectionUrl,

    // === Agent Close Confirmation ===
    /// Confirm closing agent (keep worktree).
    ConfirmCloseAgent,

    /// Confirm closing agent and delete worktree.
    ConfirmCloseAgentDeleteWorktree,

    // === Scrolling (TUI-local parser state) ===
    /// Scroll up by N lines.
    ScrollUp(usize),

    /// Scroll down by N lines.
    ScrollDown(usize),

    /// Scroll to top of buffer.
    ScrollToTop,

    /// Scroll to bottom (live view).
    ScrollToBottom,

    // === Agent Navigation (sends Lua subscribe/unsubscribe) ===
    /// Select next agent in list.
    SelectNext,

    /// Select previous agent in list.
    SelectPrevious,

    /// Toggle between CLI and Server PTY view.
    TogglePtyView,

    /// No action.
    None,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tui_action_equality() {
        assert_eq!(TuiAction::OpenMenu, TuiAction::OpenMenu);
        assert_ne!(TuiAction::MenuUp, TuiAction::MenuDown);
        assert_eq!(TuiAction::ScrollUp(10), TuiAction::ScrollUp(10));
        assert_ne!(TuiAction::ScrollUp(10), TuiAction::ScrollUp(5));
    }
}
