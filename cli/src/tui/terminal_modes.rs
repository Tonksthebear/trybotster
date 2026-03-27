//! Terminal mode mirroring between inner PTY and outer terminal.
//!
//! Tracks whether the outer terminal has application cursor (DECCKM),
//! bracketed paste, kitty keyboard protocol, and cursor shape (DECSCUSR)
//! pushed, and syncs these to match the focused PTY's state.

use crate::terminal::{CursorShape, CursorStyle};

use super::terminal_panel::TerminalPanel;

/// Mirrors terminal modes from the focused PTY to the outer terminal.
#[derive(Debug)]
pub struct TerminalModes {
    outer_app_cursor: bool,
    outer_bracketed_paste: bool,
    inner_kitty_enabled: bool,
    outer_kitty_enabled: bool,
    outer_cursor_style: Option<CursorStyle>,
    outer_cursor_visible: Option<bool>,
    terminal_focused: bool,
}

impl TerminalModes {
    /// Create with all modes at default (off/unset).
    pub fn new() -> Self {
        Self {
            outer_app_cursor: false,
            outer_bracketed_paste: false,
            inner_kitty_enabled: false,
            outer_kitty_enabled: false,
            outer_cursor_style: None,
            outer_cursor_visible: None,
            terminal_focused: true,
        }
    }

    /// Sync outer terminal modes to match the focused panel's PTY state.
    pub fn sync(&mut self, focused_panel: Option<&TerminalPanel>, has_overlay: bool) {
        let (app_cursor, bp, desired_cursor_style, desired_cursor_visible) = focused_panel
            .map(|panel| {
                (
                    panel.application_cursor(),
                    panel.bracketed_paste(),
                    Some(panel.cursor_style()),
                    !panel.cursor_hidden(),
                )
            })
            .unwrap_or((false, false, None, true));

        if app_cursor != self.outer_app_cursor {
            self.outer_app_cursor = app_cursor;
            let seq: &[u8] = if app_cursor { b"\x1b[?1h" } else { b"\x1b[?1l" };
            let _ = std::io::Write::write_all(&mut std::io::stdout(), seq);
        }

        if bp != self.outer_bracketed_paste {
            self.outer_bracketed_paste = bp;
            let seq: &[u8] = if bp { b"\x1b[?2004h" } else { b"\x1b[?2004l" };
            let _ = std::io::Write::write_all(&mut std::io::stdout(), seq);
        }

        let desired_kitty = self.inner_kitty_enabled && !has_overlay;
        if desired_kitty != self.outer_kitty_enabled {
            log::info!(
                "[KITTY] sync: inner={} overlay={} desired={} outer={}",
                self.inner_kitty_enabled,
                has_overlay,
                desired_kitty,
                self.outer_kitty_enabled
            );
            self.outer_kitty_enabled = desired_kitty;
            let seq: &[u8] = if desired_kitty {
                b"\x1b[>1u"
            } else {
                b"\x1b[<u"
            };
            unsafe {
                libc::write(
                    libc::STDOUT_FILENO,
                    seq.as_ptr() as *const libc::c_void,
                    seq.len(),
                );
            }
        }

        let effective_cursor_style = desired_cursor_style.unwrap_or_default();
        let needs_update = self.outer_cursor_style != Some(effective_cursor_style);
        if needs_update {
            self.outer_cursor_style = Some(effective_cursor_style);
            let seq: &[u8] = if desired_cursor_style.is_none() {
                b"\x1b[0q"
            } else {
                cursor_style_to_decscusr(effective_cursor_style)
            };
            let _ = std::io::Write::write_all(&mut std::io::stdout(), seq);
        }

        if self.outer_cursor_visible != Some(desired_cursor_visible) {
            self.outer_cursor_visible = Some(desired_cursor_visible);
            let seq: &[u8] = if desired_cursor_visible {
                b"\x1b[?25h"
            } else {
                b"\x1b[?25l"
            };
            let _ = std::io::Write::write_all(&mut std::io::stdout(), seq);
        }
    }

    /// Update inner kitty keyboard protocol state from a PtyEvent.
    pub fn on_kitty_changed(&mut self, enabled: bool) {
        self.inner_kitty_enabled = enabled;
    }

    /// Mark the terminal as focused.
    pub fn on_focus_gained(&mut self) {
        self.terminal_focused = true;
    }

    /// Mark the terminal as unfocused.
    pub fn on_focus_lost(&mut self) {
        self.terminal_focused = false;
    }

    /// Reset inner kitty state (e.g., on session disconnect).
    pub fn clear_inner_kitty(&mut self) {
        self.inner_kitty_enabled = false;
    }

    /// Whether the outer terminal currently has focus.
    pub fn terminal_focused(&self) -> bool {
        self.terminal_focused
    }

    /// Whether kitty keyboard protocol is active on the outer terminal.
    pub fn outer_kitty_enabled(&self) -> bool {
        self.outer_kitty_enabled
    }

    /// Whether the focused panel's PTY has kitty keyboard protocol active.
    pub fn inner_kitty_enabled(&self) -> bool {
        self.inner_kitty_enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_modes_start_with_visible_cursor() {
        let modes = TerminalModes::new();
        assert_eq!(modes.outer_cursor_visible, None);
    }
}

fn cursor_style_to_decscusr(style: CursorStyle) -> &'static [u8] {
    match (style.shape, style.blinking) {
        (CursorShape::Block, true) => b"\x1b[1q",
        (CursorShape::Block, false) => b"\x1b[2q",
        (CursorShape::Underline, true) => b"\x1b[3q",
        (CursorShape::Underline, false) => b"\x1b[4q",
        (CursorShape::Beam, true) => b"\x1b[5q",
        (CursorShape::Beam, false) => b"\x1b[6q",
        (CursorShape::HollowBlock, _) => b"\x1b[2q",
        (CursorShape::Hidden, _) => b"\x1b[2q",
    }
}
