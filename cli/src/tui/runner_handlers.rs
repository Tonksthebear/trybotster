//! TUI Runner Handlers - transport-level action processing.
//!
//! Processes `TuiAction` variants — scroll and transport primitives only.
//! Application logic (mode, input, list navigation) is handled by Lua
//! via `_tui_state` mutations.

// Rust guideline compliant 2026-02

use ratatui::backend::Backend;

use super::actions::TuiAction;
use super::runner::TuiRunner;

impl<B> TuiRunner<B>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    /// Handle a TUI action generated from input.
    ///
    /// Only transport-level primitives (scroll, quit, send_msg) remain here.
    /// UI state (mode, input, list) is managed by Lua's `_tui_state`.
    pub fn handle_tui_action(&mut self, action: TuiAction) {
        match action {
            TuiAction::Quit => {
                self.quit = true;
            }

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

            TuiAction::SendMessage(msg) => {
                self.send_msg(msg);
            }

            TuiAction::None => {}
        }
    }
}
