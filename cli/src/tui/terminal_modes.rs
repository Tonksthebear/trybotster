//! Terminal mode mirroring between inner PTY and outer terminal.
//!
//! Tracks whether the outer terminal has application cursor (DECCKM),
//! bracketed paste, and kitty keyboard protocol pushed, and syncs
//! these to match the focused PTY's state. Also tracks whether the
//! outer terminal window has OS-level focus for synthetic focus events.

// Rust guideline compliant 2026-02

use super::terminal_panel::TerminalPanel;

/// Mirrors terminal modes from the focused PTY to the outer terminal.
///
/// The outer terminal (Ghostty, iTerm, etc.) needs to match the inner PTY's
/// modes so that keyboard input is encoded correctly. This struct owns the
/// mirroring state and writes escape sequences to stdout when modes change.
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
            terminal_focused: true,
        }
    }

    /// Sync outer terminal modes to match the focused panel's PTY state.
    ///
    /// Reads DECCKM and bracketed paste from the panel's vt100 screen.
    /// Kitty keyboard protocol is gated on `has_overlay` — overlays use
    /// traditional key encoding for keybinding dispatch.
    ///
    /// Writes escape sequences directly to stdout when modes change.
    /// See [`sync_terminal_modes` doc on Ghostty workaround][ghostty].
    ///
    /// [ghostty]: https://github.com/ghostty-org/ghostty/discussions/7780
    pub fn sync(&mut self, focused_panel: Option<&TerminalPanel>, has_overlay: bool) {
        let (app_cursor, bp) = focused_panel
            .map(|panel| {
                let screen = panel.screen();
                (screen.application_cursor(), screen.bracketed_paste())
            })
            .unwrap_or((false, false));

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
}
