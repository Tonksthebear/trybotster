//! Terminal state guard for RAII cleanup.
//!
//! This module provides a guard struct that ensures terminal state is
//! properly restored even if the application panics.

use crossterm::{
    execute,
    terminal::{disable_raw_mode, LeaveAlternateScreen},
    event::DisableMouseCapture,
};

/// Guard struct that ensures terminal cleanup on drop (including panics).
///
/// When dropped, this guard:
/// - Disables raw mode
/// - Leaves alternate screen
/// - Disables mouse capture
/// - Shows the cursor
///
/// # Example
///
/// ```ignore
/// fn run_tui() -> Result<()> {
///     // Setup terminal
///     enable_raw_mode()?;
///     execute!(stdout(), EnterAlternateScreen, EnableMouseCapture)?;
///
///     // Create guard - will cleanup on drop
///     let _guard = TerminalGuard;
///
///     // Run TUI loop...
///     // Guard automatically cleans up when function exits (normally or via panic)
///     Ok(())
/// }
/// ```
pub struct TerminalGuard;

impl TerminalGuard {
    /// Creates a new terminal guard.
    ///
    /// The guard will restore terminal state when dropped.
    pub fn new() -> Self {
        Self
    }
}

impl Default for TerminalGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Always attempt to restore terminal state, ignoring errors
        let _ = disable_raw_mode();
        let _ = execute!(
            std::io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        // Try to show cursor
        let _ = execute!(std::io::stdout(), crossterm::cursor::Show);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_terminal_guard_creation() {
        // Just verify we can create one without panicking
        let _guard = TerminalGuard::new();
        let _guard2 = TerminalGuard::default();
    }
}
