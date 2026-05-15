//! TUI-local actions for UI state management.
//!
//! These actions are handled entirely within TuiRunner - they do NOT go to Hub.
//! For client operations (agent selection, PTY input, resize), TuiRunner uses
//! JSON messages through the Lua client protocol to communicate with Hub.

// Rust guideline compliant 2026-02

/// Actions handled entirely within the TUI.
///
/// These are transport-level primitives that Rust handles without application
/// knowledge. Application-specific workflow (mode, input, list navigation)
/// lives in Lua (`actions.lua`), which mutates `_tui_state` directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiAction {
    // === Application Control ===
    /// Request quit.
    Quit,

    // === Scrolling (TUI-local parser state) ===
    /// Scroll up by N lines.
    ScrollUp(usize),

    /// Scroll down by N lines.
    ScrollDown(usize),

    /// Scroll to top of buffer.
    ScrollToTop,

    /// Scroll to bottom (live view).
    ScrollToBottom,

    // === Generic Operations ===
    /// Send a JSON message to Hub.
    SendMessage(serde_json::Value),

    /// No action.
    None,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tui_action_equality() {
        assert_eq!(TuiAction::ScrollUp(10), TuiAction::ScrollUp(10));
        assert_ne!(TuiAction::ScrollUp(10), TuiAction::ScrollUp(5));
    }
}
