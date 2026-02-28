//! Terminal mode mirroring between inner PTY and outer terminal.
//!
//! Tracks whether the outer terminal has application cursor (DECCKM),
//! bracketed paste, kitty keyboard protocol, and cursor shape (DECSCUSR)
//! pushed, and syncs these to match the focused PTY's state. Also tracks
//! whether the outer terminal window has OS-level focus for synthetic focus
//! events.

// Rust guideline compliant 2026-02

use alacritty_terminal::vte::ansi::{CursorShape, CursorStyle};

use super::terminal_panel::TerminalPanel;

/// Mirrors terminal modes from the focused PTY to the outer terminal.
///
/// The outer terminal (Ghostty, iTerm, etc.) needs to match the inner PTY's
/// modes so that keyboard input is encoded correctly. This struct owns the
/// mirroring state and writes escape sequences to stdout when modes change.
///
/// Synced modes: DECCKM, bracketed paste, kitty keyboard protocol, DECSCUSR
/// (cursor shape). When focus shifts away from a panel or the panel is
/// unselected, cursor shape is reset to the terminal default (`\x1b[0q`).
///
/// Also tracks OS-level terminal focus for synthetic focus-in/focus-out
/// events forwarded to the PTY on mode or panel switches.
#[derive(Debug)]
pub struct TerminalModes {
    /// Whether DECCKM (application cursor) is pushed to the outer terminal.
    outer_app_cursor: bool,
    /// Whether bracketed paste mode is pushed to the outer terminal.
    outer_bracketed_paste: bool,
    /// Whether the focused PTY has kitty keyboard protocol enabled.
    inner_kitty_enabled: bool,
    /// Whether kitty keyboard protocol is pushed to the outer terminal.
    outer_kitty_enabled: bool,
    /// Last cursor shape pushed to the outer terminal via DECSCUSR.
    ///
    /// `None` means the default has not been set yet — next sync will
    /// always emit the sequence to establish a known baseline.
    outer_cursor_style: Option<CursorStyle>,
    /// Whether the outer terminal window has OS-level focus.
    terminal_focused: bool,
}

impl TerminalModes {
    /// Create with default state (no modes pushed, terminal assumed focused).
    pub fn new() -> Self {
        Self {
            outer_app_cursor: false,
            outer_bracketed_paste: false,
            inner_kitty_enabled: false,
            outer_kitty_enabled: false,
            outer_cursor_style: None,
            terminal_focused: true,
        }
    }

    /// Sync outer terminal modes to match the focused panel's PTY state.
    ///
    /// Reads DECCKM, bracketed paste, and DECSCUSR cursor shape from the
    /// panel's AlacrittyParser. Kitty keyboard protocol is gated on
    /// `has_overlay` — overlays use traditional key encoding for keybinding
    /// dispatch.
    ///
    /// Writes escape sequences directly to stdout when modes change.
    /// See [`sync_terminal_modes` doc on Ghostty workaround][ghostty].
    ///
    /// [ghostty]: https://github.com/ghostty-org/ghostty/discussions/7780
    pub fn sync(&mut self, focused_panel: Option<&TerminalPanel>, has_overlay: bool) {
        let (app_cursor, bp, desired_cursor_style) = focused_panel
            .map(|panel| {
                (
                    panel.application_cursor(),
                    panel.bracketed_paste(),
                    Some(panel.cursor_style()),
                )
            })
            .unwrap_or((false, false, None));

        if app_cursor != self.outer_app_cursor {
            self.outer_app_cursor = app_cursor;
            let seq: &[u8] = if app_cursor {
                b"\x1b[?1h"
            } else {
                b"\x1b[?1l"
            };
            let _ = std::io::Write::write_all(&mut std::io::stdout(), seq);
        }

        if bp != self.outer_bracketed_paste {
            self.outer_bracketed_paste = bp;
            let seq: &[u8] = if bp {
                b"\x1b[?2004h"
            } else {
                b"\x1b[?2004l"
            };
            let _ = std::io::Write::write_all(&mut std::io::stdout(), seq);
        }

        // Kitty: only push when PTY wants it AND there's no overlay.
        // In modal modes (menu, input, etc.) we want traditional bytes for keybindings.
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
            // Write raw bytes via libc to bypass any buffering. Push = CSI > 1 u,
            // Pop = CSI < u (flag 1 = DISAMBIGUATE_ESCAPE_CODES).
            let seq: &[u8] = if desired_kitty {
                b"\x1b[>1u"
            } else {
                b"\x1b[<u"
            };
            // SAFETY: writing a short byte sequence to stdout fd. This is the
            // standard pattern for terminal escape sequences that must bypass
            // Rust's buffered I/O.
            unsafe {
                libc::write(
                    libc::STDOUT_FILENO,
                    seq.as_ptr() as *const libc::c_void,
                    seq.len(),
                );
            }
        }

        // Cursor shape (DECSCUSR): mirror the focused PTY's cursor style to the
        // outer terminal so the user sees the correct cursor shape (beam in vim
        // insert mode, etc.). Reset to default when no panel is focused.
        //
        // `\x1b[0q` resets DECSCUSR to the terminal's configured default.
        // Compares by shape+blink so any change triggers an update.
        let effective_cursor_style = desired_cursor_style.unwrap_or_default();
        let needs_update = self.outer_cursor_style != Some(effective_cursor_style);
        if needs_update {
            self.outer_cursor_style = Some(effective_cursor_style);
            let seq: &[u8] = if desired_cursor_style.is_none() {
                // No focused panel — reset to terminal default.
                b"\x1b[0q"
            } else {
                cursor_style_to_decscusr(effective_cursor_style)
            };
            let _ = std::io::Write::write_all(&mut std::io::stdout(), seq);
        }
    }

    /// Update inner kitty state when the focused PTY's kitty mode changes.
    pub fn on_kitty_changed(&mut self, enabled: bool) {
        self.inner_kitty_enabled = enabled;
    }

    /// Handle OS-level focus gained event.
    pub fn on_focus_gained(&mut self) {
        self.terminal_focused = true;
    }

    /// Handle OS-level focus lost event.
    pub fn on_focus_lost(&mut self) {
        self.terminal_focused = false;
    }

    /// Clear inner kitty state (e.g. when PTY process exits or focus clears).
    pub fn clear_inner_kitty(&mut self) {
        self.inner_kitty_enabled = false;
    }

    /// Whether the outer terminal window has OS-level focus.
    pub fn terminal_focused(&self) -> bool {
        self.terminal_focused
    }

    /// Whether kitty keyboard protocol is active on the outer terminal.
    pub fn outer_kitty_enabled(&self) -> bool {
        self.outer_kitty_enabled
    }

    /// Whether the inner PTY has kitty keyboard protocol enabled.
    pub fn inner_kitty_enabled(&self) -> bool {
        self.inner_kitty_enabled
    }

    /// Reset cursor shape to terminal default.
    ///
    /// Called when the focused panel changes or when the TUI exits, ensuring
    /// the outer terminal cursor is left in its configured default state.
    pub fn reset_cursor_style(&mut self) {
        if self.outer_cursor_style.is_some() {
            self.outer_cursor_style = None;
            let _ = std::io::Write::write_all(&mut std::io::stdout(), b"\x1b[0q");
        }
    }
}

/// Map a [`CursorStyle`] to the corresponding DECSCUSR escape sequence bytes.
///
/// The DECSCUSR parameter encodes both shape and blink state:
/// 1 blinking block, 2 steady block, 3 blinking underline, 4 steady underline,
/// 5 blinking beam (bar), 6 steady beam (bar).
fn cursor_style_to_decscusr(style: CursorStyle) -> &'static [u8] {
    match (style.shape, style.blinking) {
        (CursorShape::Block, true) => b"\x1b[1q",
        (CursorShape::Block, false) => b"\x1b[2q",
        (CursorShape::Underline, true) => b"\x1b[3q",
        (CursorShape::Underline, false) => b"\x1b[4q",
        (CursorShape::Beam, true) => b"\x1b[5q",
        (CursorShape::Beam, false) => b"\x1b[6q",
        // HollowBlock is a vi-mode variant; treat as steady block.
        (CursorShape::HollowBlock, _) => b"\x1b[2q",
        // Hidden is handled separately via DECTCEM — default to steady block.
        (CursorShape::Hidden, _) => b"\x1b[2q",
    }
}
