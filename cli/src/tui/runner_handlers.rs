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
                if let Some(panel) = self.focused_panel_mut() {
                    panel.scroll_up(lines);
                }
            }

            TuiAction::ScrollDown(lines) => {
                if let Some(panel) = self.focused_panel_mut() {
                    panel.scroll_down(lines);
                }
            }

            TuiAction::ScrollToTop => {
                if let Some(panel) = self.focused_panel_mut() {
                    panel.scroll_to_top();
                }
            }

            TuiAction::ScrollToBottom => {
                if let Some(panel) = self.focused_panel_mut() {
                    panel.scroll_to_bottom();
                }
            }

            TuiAction::SendMessage(msg) => {
                self.send_msg(msg);
            }

            TuiAction::None => {}
        }
    }

    /// Get a mutable reference to the currently focused terminal panel.
    fn focused_panel_mut(&mut self) -> Option<&mut crate::tui::terminal_panel::TerminalPanel> {
        self.panel_pool.focused_panel_mut()
    }
}
