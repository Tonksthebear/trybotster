//! TUI-local actions for UI state management.
//!
//! These actions are handled entirely within TuiRunner - they do NOT go to Hub.
//! For client operations (agent selection, PTY input, resize), TuiRunner uses
//! JSON messages through the Lua client protocol to communicate with Hub.

// Rust guideline compliant 2026-02

/// Actions handled entirely within the TUI.
///
/// These are pure UI state changes - generic primitives that Rust handles
/// without application knowledge. Application-specific workflow logic
/// lives in Lua (`actions.lua`), which returns compound operations that
/// Rust executes generically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiAction {
    // === Application Control ===
    /// Request quit.
    Quit,

    // === Mode ===
    /// Set the UI mode (e.g., "normal", "menu").
    SetMode(String),

    // === List Navigation (for current overlay list) ===
    /// Move overlay list selection up.
    ListUp,

    /// Move overlay list selection down.
    ListDown,

    /// Select overlay list item at index.
    ListSelect(usize),

    // === Text Input ===
    /// Add character to input buffer.
    InputChar(char),

    /// Delete last character from input buffer.
    InputBackspace,

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

    /// Store a key-value pair in pending fields.
    StoreField {
        /// Field key.
        key: String,
        /// Field value.
        value: String,
    },

    /// Remove a key from pending fields.
    ClearField(String),

    /// Clear the input buffer.
    ClearInput,

    /// Reset overlay list selection to 0.
    ResetList,

    /// No action.
    None,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tui_action_equality() {
        assert_eq!(TuiAction::ListUp, TuiAction::ListUp);
        assert_ne!(TuiAction::ListUp, TuiAction::ListDown);
        assert_eq!(TuiAction::ScrollUp(10), TuiAction::ScrollUp(10));
        assert_ne!(TuiAction::ScrollUp(10), TuiAction::ScrollUp(5));
    }
}
