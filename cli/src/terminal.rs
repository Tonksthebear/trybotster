//! Terminal emulator abstraction wrapping [`ghostty_vt`].
//!
//! Provides [`TerminalParser`] — a monomorphic wrapper around ghostty's terminal
//! with effect callbacks for write_pty, title_changed, and bell events.

use std::collections::HashMap;
use std::ffi::c_void;
use std::pin::Pin;

use crate::ghostty_vt;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Default scrollback limit in bytes (matches ghostty's default of 10MB).
/// ghostty's max_scrollback is measured in bytes, not lines.
pub const DEFAULT_SCROLLBACK_BYTES: usize = 10_000_000;

/// Minimum rows clamped on construction.
pub const MIN_ROWS: u16 = 1;

/// Minimum columns clamped on construction.
pub const MIN_COLS: u16 = 1;

// ── Cursor types ──────────────────────────────────────────────────────────────

/// Cursor shape for DECSCUSR mirroring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    /// Filled block cursor.
    Block,
    /// Underline cursor.
    Underline,
    /// Vertical beam cursor.
    Beam,
    /// Hollow (outline) block cursor.
    HollowBlock,
    /// Cursor is hidden (DECTCEM off).
    Hidden,
}

/// Cursor style (shape + blink) as set by the running application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorStyle {
    /// Current cursor shape.
    pub shape: CursorShape,
    /// Whether the cursor blinks.
    pub blinking: bool,
}

impl Default for CursorStyle {
    fn default() -> Self {
        Self {
            shape: CursorShape::Block,
            blinking: true,
        }
    }
}

impl CursorStyle {
    /// Build from ghostty render state cursor data.
    pub fn from_render_state(rs: &ghostty_vt::RenderState) -> Self {
        use ghostty_vt::GhosttyRenderStateCursorVisualStyle as G;
        let shape = match rs.cursor_visual_style() {
            G::Block => CursorShape::Block,
            G::Underline => CursorShape::Underline,
            G::Bar => CursorShape::Beam,
            G::BlockHollow => CursorShape::HollowBlock,
        };
        let visible = rs.cursor_visible();
        Self {
            shape: if visible { shape } else { CursorShape::Hidden },
            blinking: false,
        }
    }
}

// ── Color type ────────────────────────────────────────────────────────────────

/// Simple RGB color type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Rgb {
    /// Red component.
    pub r: u8,
    /// Green component.
    pub g: u8,
    /// Blue component.
    pub b: u8,
}

impl Rgb {
    /// Create a new RGB color.
    pub fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

impl From<ghostty_vt::GhosttyColorRgb> for Rgb {
    fn from(c: ghostty_vt::GhosttyColorRgb) -> Self {
        Self {
            r: c.r,
            g: c.g,
            b: c.b,
        }
    }
}

impl From<Rgb> for ghostty_vt::GhosttyColorRgb {
    fn from(c: Rgb) -> Self {
        Self {
            r: c.r,
            g: c.g,
            b: c.b,
        }
    }
}

// ── Callback trampolines ─────────────────────────────────────────────────────

struct CallbackState {
    write_pty: Option<Box<dyn FnMut(&[u8]) + Send>>,
    title_changed: Option<Box<dyn FnMut(&str) + Send>>,
    bell: Option<Box<dyn FnMut() + Send>>,
    pwd_changed: Option<Box<dyn FnMut() + Send>>,
    notification: Option<Box<dyn FnMut(&str, &str) + Send>>,
}

unsafe extern "C" fn write_pty_trampoline(
    _terminal: *mut ghostty_vt::GhosttyTerminalOpaque,
    userdata: *mut c_void,
    data: *const u8,
    len: usize,
) {
    let state = unsafe { &mut *(userdata as *mut CallbackState) };
    if let Some(ref mut cb) = state.write_pty {
        let bytes = unsafe { std::slice::from_raw_parts(data, len) };
        cb(bytes);
    }
}

unsafe extern "C" fn title_changed_trampoline(
    _terminal: *mut ghostty_vt::GhosttyTerminalOpaque,
    userdata: *mut c_void,
) {
    let state = unsafe { &mut *(userdata as *mut CallbackState) };
    if let Some(ref mut cb) = state.title_changed {
        cb("");
    }
}

unsafe extern "C" fn bell_trampoline(
    _terminal: *mut ghostty_vt::GhosttyTerminalOpaque,
    userdata: *mut c_void,
) {
    let state = unsafe { &mut *(userdata as *mut CallbackState) };
    if let Some(ref mut cb) = state.bell {
        cb();
    }
}

unsafe extern "C" fn pwd_changed_trampoline(
    _terminal: *mut ghostty_vt::GhosttyTerminalOpaque,
    userdata: *mut c_void,
) {
    let state = unsafe { &mut *(userdata as *mut CallbackState) };
    if let Some(ref mut cb) = state.pwd_changed {
        cb();
    }
}

unsafe extern "C" fn desktop_notification_trampoline(
    _terminal: *mut ghostty_vt::GhosttyTerminalOpaque,
    userdata: *mut c_void,
    notification: *const ghostty_vt::GhosttyTerminalDesktopNotification,
) {
    let state = unsafe { &mut *(userdata as *mut CallbackState) };
    if let Some(ref mut cb) = state.notification {
        if notification.is_null() {
            return;
        }
        // SAFETY: Ghostty borrows the notification for the duration of this call.
        let n = unsafe { &*notification };
        cb(n.title.as_str(), n.body.as_str());
    }
}

// ── CallbackConfig ────────────────────────────────────────────────────────────

/// Configuration for terminal effect callbacks.
///
/// Upstream ghostty-org no longer provides first-class hooks for OSC 133
/// semantic prompt marks or kitty keyboard change events. Mode transitions are
/// observed by polling `mode_get` / mode flags after VT writes (see session
/// reader loop), not via a push callback.
#[allow(missing_debug_implementations)]
pub struct CallbackConfig {
    /// Called when the terminal needs to write back to the PTY (e.g., color query responses).
    pub write_pty: Option<Box<dyn FnMut(&[u8]) + Send>>,
    /// Called when the window title changes (OSC 0/2).
    pub title_changed: Option<Box<dyn FnMut(&str) + Send>>,
    /// Called when a BEL character is received.
    pub bell: Option<Box<dyn FnMut() + Send>>,
    /// Called when the working directory changes (OSC 7).
    pub pwd_changed: Option<Box<dyn FnMut() + Send>>,
    /// Called when an OSC desktop notification is received (title, body).
    pub notification: Option<Box<dyn FnMut(&str, &str) + Send>>,
}

impl Default for CallbackConfig {
    fn default() -> Self {
        Self {
            write_pty: None,
            title_changed: None,
            bell: None,
            pwd_changed: None,
            notification: None,
        }
    }
}

// ── TerminalParser ────────────────────────────────────────────────────────────

/// Ghostty-backed terminal parser.
///
/// Monomorphic — callbacks are configured via `CallbackConfig` rather than
/// a generic event listener type parameter.
pub struct TerminalParser {
    terminal: ghostty_vt::Terminal,
    _callback_state: Option<Pin<Box<CallbackState>>>,
    color_cache: HashMap<usize, Rgb>,
}

impl std::fmt::Debug for TerminalParser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalParser")
            .field("terminal", &self.terminal)
            .finish()
    }
}

impl TerminalParser {
    /// Create a parser with no callbacks.
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Self {
        let rows = rows.max(MIN_ROWS);
        let cols = cols.max(MIN_COLS);
        let mut terminal =
            ghostty_vt::Terminal::new(cols, rows, scrollback).expect("ghostty terminal creation");
        unsafe {
            terminal.enable_builtin_color_scheme_callback();
        }
        Self {
            terminal,
            _callback_state: None,
            color_cache: HashMap::new(),
        }
    }

    /// Create a parser with effect callbacks.
    pub fn new_with_callbacks(
        rows: u16,
        cols: u16,
        scrollback: usize,
        config: CallbackConfig,
    ) -> Self {
        let rows = rows.max(MIN_ROWS);
        let cols = cols.max(MIN_COLS);
        let mut terminal =
            ghostty_vt::Terminal::new(cols, rows, scrollback).expect("ghostty terminal creation");

        let state = Box::pin(CallbackState {
            write_pty: config.write_pty,
            title_changed: config.title_changed,
            bell: config.bell,
            pwd_changed: config.pwd_changed,
            notification: config.notification,
        });

        let state_ptr = &*state as *const CallbackState as *mut c_void;

        unsafe {
            terminal.set_userdata(state_ptr);
            terminal.enable_builtin_color_scheme_callback();

            if state.write_pty.is_some() {
                terminal.set_write_pty_callback(Some(write_pty_trampoline));
            }
            if state.title_changed.is_some() {
                terminal.set_title_changed_callback(Some(title_changed_trampoline));
            }
            if state.bell.is_some() {
                terminal.set_bell_callback(Some(bell_trampoline));
            }
            if state.pwd_changed.is_some() {
                terminal.set_pwd_changed_callback(Some(pwd_changed_trampoline));
            }
            if state.notification.is_some() {
                terminal.set_desktop_notification_callback(Some(desktop_notification_trampoline));
            }
        }

        Self {
            terminal,
            _callback_state: Some(state),
            color_cache: HashMap::new(),
        }
    }

    /// Feed raw PTY bytes into the terminal emulator.
    ///
    /// OSC 4/10/11/12 color queries are answered by upstream Ghostty through
    /// the `write_pty` callback after colors are applied via
    /// [`Self::apply_color_cache_map`]. The previous host-side dual answer path
    /// double-fired on this pin and is intentionally removed.
    pub fn process(&mut self, data: &[u8]) {
        self.terminal.write(data);
    }

    /// Resize the terminal.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let rows = rows.max(MIN_ROWS);
        let cols = cols.max(MIN_COLS);
        let _ = self.terminal.resize(cols, rows);
    }

    /// Direct access to the underlying ghostty Terminal.
    pub fn terminal(&self) -> &ghostty_vt::Terminal {
        &self.terminal
    }

    /// Mutable access to the underlying ghostty Terminal.
    pub fn terminal_mut(&mut self) -> &mut ghostty_vt::Terminal {
        &mut self.terminal
    }

    /// Export an opaque `GHOSTSNP` snapshot of the underlying terminal.
    pub fn snapshot_export(&self) -> Result<Vec<u8>, ghostty_vt::SnapshotError> {
        self.terminal.snapshot_export()
    }

    /// Import an opaque `GHOSTSNP` snapshot, then re-install host callbacks.
    ///
    /// Upstream decode produces a **new** terminal handle. Userdata and all
    /// OPT callbacks (write_pty, title, bell, pwd, desktop_notification, builtin
    /// color-scheme) must be re-applied after the swap or attach paths lose
    /// live event delivery and OSC query replies.
    pub fn snapshot_import(&mut self, data: &[u8]) -> Result<(), ghostty_vt::SnapshotError> {
        self.terminal.snapshot_import(data)?;
        self.reinstall_terminal_hooks();
        // Re-seed host colors onto the new handle so OSC answers stay correct.
        if !self.color_cache.is_empty() {
            let colors = self.color_cache.clone();
            self.apply_color_cache_map(&colors);
        }
        Ok(())
    }

    /// Re-bind userdata and callbacks after a handle swap (snapshot import).
    fn reinstall_terminal_hooks(&mut self) {
        // SAFETY: Callback trampolines remain valid for the lifetime of
        // `_callback_state`; userdata points into the pinned box when present.
        unsafe {
            if let Some(state) = self._callback_state.as_ref() {
                let state_ptr =
                    std::ptr::from_ref::<CallbackState>(state.as_ref().get_ref()) as *mut c_void;
                self.terminal.set_userdata(state_ptr);
                self.terminal.enable_builtin_color_scheme_callback();
                if state.write_pty.is_some() {
                    self.terminal
                        .set_write_pty_callback(Some(write_pty_trampoline));
                }
                if state.title_changed.is_some() {
                    self.terminal
                        .set_title_changed_callback(Some(title_changed_trampoline));
                }
                if state.bell.is_some() {
                    self.terminal.set_bell_callback(Some(bell_trampoline));
                }
                if state.pwd_changed.is_some() {
                    self.terminal
                        .set_pwd_changed_callback(Some(pwd_changed_trampoline));
                }
                if state.notification.is_some() {
                    self.terminal
                        .set_desktop_notification_callback(Some(desktop_notification_trampoline));
                }
            } else {
                // No-callback constructor still installs the builtin color-scheme path.
                self.terminal.enable_builtin_color_scheme_callback();
            }
        }
    }

    /// Effective foreground color (override or default), if set.
    pub fn foreground_color(&self) -> Option<Rgb> {
        self.terminal.foreground_color().map(Into::into)
    }

    /// Effective background color (override or default), if set.
    pub fn background_color(&self) -> Option<Rgb> {
        self.terminal.background_color().map(Into::into)
    }

    /// Default foreground color, ignoring transient terminal state.
    pub fn foreground_color_default(&self) -> Option<Rgb> {
        self.terminal.foreground_color_default().map(Into::into)
    }

    /// Default background color, ignoring transient terminal state.
    pub fn background_color_default(&self) -> Option<Rgb> {
        self.terminal.background_color_default().map(Into::into)
    }

    /// Whether the cursor is hidden.
    pub fn cursor_hidden(&self) -> bool {
        self.terminal.cursor_hidden()
    }

    /// Whether the Kitty keyboard protocol is active.
    pub fn kitty_enabled(&self) -> bool {
        self.terminal.kitty_enabled()
    }

    /// Whether focus reporting mode is active.
    pub fn focus_reporting(&self) -> bool {
        self.terminal.focus_reporting()
    }

    /// Whether application cursor keys mode is active.
    pub fn application_cursor(&self) -> bool {
        self.terminal.application_cursor()
    }

    /// Whether bracketed paste mode is active.
    pub fn bracketed_paste(&self) -> bool {
        self.terminal.bracketed_paste()
    }

    /// Whether the alternate screen buffer is active.
    pub fn alt_screen_active(&self) -> bool {
        self.terminal.alt_screen_active()
    }

    /// Mouse tracking mode as a bitmask (0 = off).
    pub fn mouse_mode(&self) -> u8 {
        self.terminal.mouse_mode()
    }

    /// Total scrollback history lines.
    pub fn history_size(&self) -> usize {
        self.terminal.scrollback_rows()
    }

    /// Apply cached terminal colors from the hub's boot probe.
    ///
    /// Sets the default foreground/background/cursor and indexed palette colors on
    /// the ghostty terminal so OSC 4/10/11/12 queries from running processes are
    /// answered correctly via the `write_pty` callback.
    pub fn apply_color_cache(
        &mut self,
        cache: &std::sync::Arc<std::sync::Mutex<std::collections::HashMap<usize, Rgb>>>,
    ) {
        if let Ok(colors) = cache.lock() {
            self.apply_color_cache_map(&colors);
        }
    }

    /// Apply a plain color cache map keyed by terminal color index.
    pub fn apply_color_cache_map(&mut self, colors: &HashMap<usize, Rgb>) {
        self.color_cache = colors.clone();

        if let Some(fg) = colors.get(&256) {
            self.terminal.set_color_foreground((*fg).into());
        }
        if let Some(bg) = colors.get(&257) {
            self.terminal.set_color_background((*bg).into());
        }
        if let Some(cursor) = colors.get(&258) {
            self.terminal.set_color_cursor((*cursor).into());
        }

        // Prefer a single full-palette set when the cache is complete; otherwise
        // merge individual entries so OSC 4 queries hit the seeded colors.
        if let Some(palette) = complete_palette(colors) {
            self.terminal.set_color_palette(&palette);
        } else {
            for (index, color) in colors {
                if *index < 256 {
                    self.terminal
                        .set_palette_entry(*index, (*color).into());
                }
            }
        }
    }

    /// Plain-text contents of the visible grid.
    pub fn contents(&self) -> String {
        self.terminal
            .format_plain()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    }
}

fn complete_palette(colors: &HashMap<usize, Rgb>) -> Option<[ghostty_vt::GhosttyColorRgb; 256]> {
    let mut palette = [ghostty_vt::GhosttyColorRgb { r: 0, g: 0, b: 0 }; 256];
    for (index, slot) in palette.iter_mut().enumerate() {
        *slot = (*colors.get(&index)?).into();
    }
    Some(palette)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_parser() {
        let p = TerminalParser::new(24, 80, 100);
        assert_eq!(p.terminal().rows(), 24);
        assert_eq!(p.terminal().cols(), 80);
        assert_eq!(p.history_size(), 0);
    }

    #[test]
    fn process_basic_text() {
        let mut p = TerminalParser::new(24, 80, 100);
        p.process(b"Hello");
        let contents = p.contents();
        assert!(contents.contains('H'));
    }

    #[test]
    fn resize_updates_dimensions() {
        let mut p = TerminalParser::new(24, 80, 100);
        p.resize(30, 100);
        assert_eq!(p.terminal().rows(), 30);
        assert_eq!(p.terminal().cols(), 100);
    }

    #[test]
    fn color_scheme_query_reports_light_from_default_background() {
        let writes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writes_cb = std::sync::Arc::clone(&writes);
        let callbacks = CallbackConfig {
            write_pty: Some(Box::new(move |data: &[u8]| {
                writes_cb
                    .lock()
                    .expect("write buffer poisoned")
                    .extend_from_slice(data);
            })),
            ..CallbackConfig::default()
        };
        let mut parser = TerminalParser::new_with_callbacks(24, 80, 100, callbacks);
        parser
            .terminal_mut()
            .set_color_background(Rgb::new(255, 252, 240).into());

        parser.process(b"\x1b[?996n");

        assert_eq!(
            writes.lock().expect("write buffer poisoned").as_slice(),
            b"\x1b[?997;2n"
        );
    }

    #[test]
    fn color_scheme_query_reports_dark_from_default_background() {
        let writes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writes_cb = std::sync::Arc::clone(&writes);
        let callbacks = CallbackConfig {
            write_pty: Some(Box::new(move |data: &[u8]| {
                writes_cb
                    .lock()
                    .expect("write buffer poisoned")
                    .extend_from_slice(data);
            })),
            ..CallbackConfig::default()
        };
        let mut parser = TerminalParser::new_with_callbacks(24, 80, 100, callbacks);
        parser
            .terminal_mut()
            .set_color_background(Rgb::new(0, 0, 0).into());

        parser.process(b"\x1b[?996n");

        assert_eq!(
            writes.lock().expect("write buffer poisoned").as_slice(),
            b"\x1b[?997;1n"
        );
    }

    #[test]
    fn osc_foreground_query_reports_seeded_color_with_st_terminator() {
        let writes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writes_cb = std::sync::Arc::clone(&writes);
        let callbacks = CallbackConfig {
            write_pty: Some(Box::new(move |data: &[u8]| {
                writes_cb
                    .lock()
                    .expect("write buffer poisoned")
                    .extend_from_slice(data);
            })),
            ..CallbackConfig::default()
        };
        let mut parser = TerminalParser::new_with_callbacks(24, 80, 100, callbacks);
        parser
            .terminal_mut()
            .set_color_foreground(Rgb::new(16, 15, 15).into());

        parser.process(b"\x1b]10;?\x1b\\");

        assert_eq!(
            writes.lock().expect("write buffer poisoned").as_slice(),
            b"\x1b]10;rgb:1010/0f0f/0f0f\x1b\\"
        );
    }

    #[test]
    fn osc_background_query_reports_seeded_color_with_bel_terminator() {
        let writes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writes_cb = std::sync::Arc::clone(&writes);
        let callbacks = CallbackConfig {
            write_pty: Some(Box::new(move |data: &[u8]| {
                writes_cb
                    .lock()
                    .expect("write buffer poisoned")
                    .extend_from_slice(data);
            })),
            ..CallbackConfig::default()
        };
        let mut parser = TerminalParser::new_with_callbacks(24, 80, 100, callbacks);
        parser
            .terminal_mut()
            .set_color_background(Rgb::new(255, 252, 240).into());

        parser.process(b"\x1b]11;?\x07");

        assert_eq!(
            writes.lock().expect("write buffer poisoned").as_slice(),
            b"\x1b]11;rgb:ffff/fcfc/f0f0\x07"
        );
    }

    #[test]
    fn osc_palette_query_reports_seeded_palette_color() {
        let writes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writes_cb = std::sync::Arc::clone(&writes);
        let callbacks = CallbackConfig {
            write_pty: Some(Box::new(move |data: &[u8]| {
                writes_cb
                    .lock()
                    .expect("write buffer poisoned")
                    .extend_from_slice(data);
            })),
            ..CallbackConfig::default()
        };
        let mut parser = TerminalParser::new_with_callbacks(24, 80, 100, callbacks);
        let mut colors = HashMap::new();
        colors.insert(7usize, Rgb::new(0xaa, 0xbb, 0xcc));
        parser.apply_color_cache_map(&colors);

        parser.process(b"\x1b]4;7;?\x07");

        assert_eq!(
            writes.lock().expect("write buffer poisoned").as_slice(),
            b"\x1b]4;7;rgb:aaaa/bbbb/cccc\x07"
        );
    }

    #[test]
    fn osc_query_split_across_chunks_is_answered_once_complete() {
        let writes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writes_cb = std::sync::Arc::clone(&writes);
        let callbacks = CallbackConfig {
            write_pty: Some(Box::new(move |data: &[u8]| {
                writes_cb
                    .lock()
                    .expect("write buffer poisoned")
                    .extend_from_slice(data);
            })),
            ..CallbackConfig::default()
        };
        let mut parser = TerminalParser::new_with_callbacks(24, 80, 100, callbacks);
        parser
            .terminal_mut()
            .set_color_background(Rgb::new(255, 252, 240).into());

        parser.process(b"\x1b]11;?");
        assert!(writes.lock().expect("write buffer poisoned").is_empty());

        parser.process(b"\x07");

        assert_eq!(
            writes.lock().expect("write buffer poisoned").as_slice(),
            b"\x1b]11;rgb:ffff/fcfc/f0f0\x07"
        );
    }

    #[test]
    fn osc_palette_query_split_across_chunks_is_answered_once_complete() {
        let writes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writes_cb = std::sync::Arc::clone(&writes);
        let callbacks = CallbackConfig {
            write_pty: Some(Box::new(move |data: &[u8]| {
                writes_cb
                    .lock()
                    .expect("write buffer poisoned")
                    .extend_from_slice(data);
            })),
            ..CallbackConfig::default()
        };
        let mut parser = TerminalParser::new_with_callbacks(24, 80, 100, callbacks);
        let mut colors = HashMap::new();
        colors.insert(7usize, Rgb::new(0xaa, 0xbb, 0xcc));
        parser.apply_color_cache_map(&colors);

        parser.process(b"\x1b]4;7;");
        assert!(writes.lock().expect("write buffer poisoned").is_empty());

        parser.process(b"?\x07");

        assert_eq!(
            writes.lock().expect("write buffer poisoned").as_slice(),
            b"\x1b]4;7;rgb:aaaa/bbbb/cccc\x07"
        );
    }

    #[test]
    fn cursor_shown_by_default() {
        let p = TerminalParser::new(24, 80, 100);
        assert!(!p.cursor_hidden());
    }

    #[test]
    fn hide_cursor_sequence() {
        let mut p = TerminalParser::new(24, 80, 100);
        p.process(b"\x1b[?25l");
        assert!(p.cursor_hidden());
        p.process(b"\x1b[?25h");
        assert!(!p.cursor_hidden());
    }

    #[test]
    fn mode_queries() {
        let mut p = TerminalParser::new(24, 80, 100);
        assert!(!p.bracketed_paste());
        assert!(!p.alt_screen_active());
        assert!(!p.kitty_enabled());

        p.process(b"\x1b[?2004h");
        assert!(p.bracketed_paste());

        p.process(b"\x1b[?1049h");
        assert!(p.alt_screen_active());
    }

    #[test]
    fn notification_callback_smoke_test() {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_cb = std::sync::Arc::clone(&calls);
        let callbacks = CallbackConfig {
            notification: Some(Box::new(move |title: &str, body: &str| {
                calls_cb
                    .lock()
                    .expect("notification calls poisoned")
                    .push((title.to_string(), body.to_string()));
            })),
            ..CallbackConfig::default()
        };
        let mut parser = TerminalParser::new_with_callbacks(24, 80, 100, callbacks);

        parser.process(b"\x1b]9;Hello world\x07");

        let calls = calls.lock().expect("notification calls poisoned");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], (String::new(), "Hello world".to_string()));
    }

    /// Upstream decode swaps to a new terminal handle. Without re-binding
    /// userdata/callbacks after import, host hooks go silent (punch-list blocker).
    #[test]
    fn snapshot_import_reinstalls_callbacks_on_new_handle() {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_cb = std::sync::Arc::clone(&calls);
        let callbacks = CallbackConfig {
            notification: Some(Box::new(move |title: &str, body: &str| {
                calls_cb
                    .lock()
                    .expect("notification calls poisoned")
                    .push((title.to_string(), body.to_string()));
            })),
            ..CallbackConfig::default()
        };
        let mut parser = TerminalParser::new_with_callbacks(24, 80, 100, callbacks);

        let snapshot = parser
            .snapshot_export()
            .expect("snapshot export before import");
        parser
            .snapshot_import(&snapshot)
            .expect("snapshot import must succeed");

        // Must fire on the *new* handle after reinstall — red without the fix.
        parser.process(b"\x1b]9;after import\x07");

        let calls = calls.lock().expect("notification calls poisoned");
        assert_eq!(
            calls.len(),
            1,
            "desktop_notification callback must survive snapshot import handle swap"
        );
        assert_eq!(calls[0], (String::new(), "after import".to_string()));
    }

    #[test]
    fn mode_get_path_tracks_bracketed_paste_without_callback() {
        // Upstream removed mode_changed callbacks; session polls mode_get.
        let mut parser = TerminalParser::new(24, 80, 100);
        assert!(!parser.bracketed_paste());
        parser.process(b"\x1b[?2004h");
        assert!(parser.bracketed_paste());
        parser.process(b"\x1b[?2004l");
        assert!(!parser.bracketed_paste());
    }

    #[test]
    fn min_rows_cols_clamped() {
        let p = TerminalParser::new(0, 0, 100);
        assert_eq!(p.terminal().rows(), MIN_ROWS);
        assert_eq!(p.terminal().cols(), MIN_COLS);
    }
}
