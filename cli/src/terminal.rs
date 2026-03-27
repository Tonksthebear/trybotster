//! Terminal emulator abstraction wrapping [`ghostty_vt`].
//!
//! Provides [`TerminalParser`] — a monomorphic wrapper around ghostty's terminal
//! with effect callbacks for write_pty, title_changed, and bell events.
//!
//! Also provides [`generate_snapshot`] which produces clean ANSI/VT output using
//! ghostty's built-in formatter — replacing the old hand-rolled serializer.

use std::ffi::c_void;
use std::pin::Pin;

use crate::ghostty_vt;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const MAX_OSC_QUERY_BUFFER_BYTES: usize = 512;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Default scrollback line limit for shadow terminals.
pub const DEFAULT_SCROLLBACK_LINES: usize = 5_000;

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
    semantic_prompt: Option<Box<dyn FnMut(u8) + Send>>,
    mode_changed: Option<Box<dyn FnMut(u16, bool) + Send>>,
    kitty_keyboard_changed: Option<Box<dyn FnMut() + Send>>,
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

unsafe extern "C" fn notification_trampoline(
    _terminal: *mut ghostty_vt::GhosttyTerminalOpaque,
    userdata: *mut c_void,
    title: *const u8,
    title_len: usize,
    body: *const u8,
    body_len: usize,
) {
    let state = unsafe { &mut *(userdata as *mut CallbackState) };
    if let Some(ref mut cb) = state.notification {
        let title_str = if title.is_null() || title_len == 0 {
            ""
        } else {
            std::str::from_utf8(unsafe { std::slice::from_raw_parts(title, title_len) })
                .unwrap_or("")
        };
        let body_str = if body.is_null() || body_len == 0 {
            ""
        } else {
            std::str::from_utf8(unsafe { std::slice::from_raw_parts(body, body_len) }).unwrap_or("")
        };
        cb(title_str, body_str);
    }
}

unsafe extern "C" fn semantic_prompt_trampoline(
    _terminal: *mut ghostty_vt::GhosttyTerminalOpaque,
    userdata: *mut c_void,
    mark: u8,
) {
    let state = unsafe { &mut *(userdata as *mut CallbackState) };
    if let Some(ref mut cb) = state.semantic_prompt {
        cb(mark);
    }
}

unsafe extern "C" fn mode_changed_trampoline(
    _terminal: *mut ghostty_vt::GhosttyTerminalOpaque,
    userdata: *mut c_void,
    mode: u16,
    enabled: bool,
) {
    let state = unsafe { &mut *(userdata as *mut CallbackState) };
    if let Some(ref mut cb) = state.mode_changed {
        cb(mode, enabled);
    }
}

unsafe extern "C" fn kitty_keyboard_changed_trampoline(
    _terminal: *mut ghostty_vt::GhosttyTerminalOpaque,
    userdata: *mut c_void,
) {
    let state = unsafe { &mut *(userdata as *mut CallbackState) };
    if let Some(ref mut cb) = state.kitty_keyboard_changed {
        cb();
    }
}

// ── CallbackConfig ────────────────────────────────────────────────────────────

/// Configuration for terminal effect callbacks.
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
    /// Called when an OSC notification is received (title, body).
    pub notification: Option<Box<dyn FnMut(&str, &str) + Send>>,
    /// Called on shell prompt marks (OSC 133/633). Argument is the mark byte (A/B/C/D).
    pub semantic_prompt: Option<Box<dyn FnMut(u8) + Send>>,
    /// Called when a terminal mode changes (mode_id, enabled).
    pub mode_changed: Option<Box<dyn FnMut(u16, bool) + Send>>,
    /// Called when kitty keyboard protocol state changes.
    pub kitty_keyboard_changed: Option<Box<dyn FnMut() + Send>>,
}

impl Default for CallbackConfig {
    fn default() -> Self {
        Self {
            write_pty: None,
            title_changed: None,
            bell: None,
            pwd_changed: None,
            notification: None,
            semantic_prompt: None,
            mode_changed: None,
            kitty_keyboard_changed: None,
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
    osc_query_buffer: Vec<u8>,
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
            osc_query_buffer: Vec::new(),
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
            semantic_prompt: config.semantic_prompt,
            mode_changed: config.mode_changed,
            kitty_keyboard_changed: config.kitty_keyboard_changed,
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
                terminal.set_notification_callback(Some(notification_trampoline));
            }
            if state.semantic_prompt.is_some() {
                terminal.set_semantic_prompt_callback(Some(semantic_prompt_trampoline));
            }
            if state.mode_changed.is_some() {
                terminal.set_mode_changed_callback(Some(mode_changed_trampoline));
            }
            if state.kitty_keyboard_changed.is_some() {
                terminal
                    .set_kitty_keyboard_changed_callback(Some(kitty_keyboard_changed_trampoline));
            }
        }

        Self {
            terminal,
            _callback_state: Some(state),
            osc_query_buffer: Vec::new(),
        }
    }

    /// Feed raw PTY bytes into the terminal emulator.
    pub fn process(&mut self, data: &[u8]) {
        self.terminal.write(data);
        self.answer_osc_color_queries(data);
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

    /// Effective foreground color (override or default), if set.
    pub fn foreground_color(&self) -> Option<Rgb> {
        self.terminal.foreground_color().map(Into::into)
    }

    /// Effective background color (override or default), if set.
    pub fn background_color(&self) -> Option<Rgb> {
        self.terminal.background_color().map(Into::into)
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
    /// Sets the default foreground, background, and cursor colors on the ghostty
    /// terminal so OSC 10/11/12 queries from running processes are answered
    /// correctly via the `write_pty` callback.
    pub fn apply_color_cache(
        &mut self,
        cache: &std::sync::Arc<std::sync::Mutex<std::collections::HashMap<usize, Rgb>>>,
    ) {
        if let Ok(colors) = cache.lock() {
            // OSC 10 = foreground (index 256 in alacritty convention)
            if let Some(fg) = colors.get(&256) {
                self.terminal
                    .set_color_foreground(ghostty_vt::GhosttyColorRgb {
                        r: fg.r,
                        g: fg.g,
                        b: fg.b,
                    });
            }
            // OSC 11 = background (index 257)
            if let Some(bg) = colors.get(&257) {
                self.terminal
                    .set_color_background(ghostty_vt::GhosttyColorRgb {
                        r: bg.r,
                        g: bg.g,
                        b: bg.b,
                    });
            }
            // OSC 12 = cursor (index 258)
            if let Some(cursor) = colors.get(&258) {
                self.terminal.set_color_cursor(ghostty_vt::GhosttyColorRgb {
                    r: cursor.r,
                    g: cursor.g,
                    b: cursor.b,
                });
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

    fn answer_osc_color_queries(&mut self, data: &[u8]) {
        if self._callback_state.is_none() {
            return;
        }

        append_with_limit(&mut self.osc_query_buffer, data, MAX_OSC_QUERY_BUFFER_BYTES);
        let queries = extract_complete_osc_color_queries(&mut self.osc_query_buffer);
        let responses: Vec<Vec<u8>> = queries
            .into_iter()
            .filter_map(|query| self.format_osc_color_query_response(&query))
            .collect();
        let Some(state) = self
            ._callback_state
            .as_mut()
            .map(|state| state.as_mut().get_mut())
        else {
            return;
        };
        let Some(write_pty) = state.write_pty.as_mut() else {
            return;
        };
        for response in responses {
            write_pty(&response);
        }
    }

    fn format_osc_color_query_response(&self, query: &[u8]) -> Option<Vec<u8>> {
        let (code, terminator) = parse_osc_color_query(query)?;
        let color = match code {
            10 => self
                .terminal
                .foreground_color_default()
                .or_else(|| self.terminal.foreground_color())?,
            11 => self
                .terminal
                .background_color_default()
                .or_else(|| self.terminal.background_color())?,
            12 => self
                .terminal
                .cursor_color_default()
                .or_else(|| self.terminal.cursor_color())?,
            _ => return None,
        };

        let mut response = format!(
            "\x1b]{code};rgb:{:02x}{:02x}/{:02x}{:02x}/{:02x}{:02x}",
            color.r, color.r, color.g, color.g, color.b, color.b
        )
        .into_bytes();
        response.extend_from_slice(terminator);
        Some(response)
    }
}

fn append_with_limit(buffer: &mut Vec<u8>, data: &[u8], max_bytes: usize) {
    if data.len() >= max_bytes {
        buffer.clear();
        buffer.extend_from_slice(&data[data.len() - max_bytes..]);
        return;
    }

    let overflow = buffer
        .len()
        .saturating_add(data.len())
        .saturating_sub(max_bytes);
    if overflow > 0 {
        buffer.drain(..overflow);
    }
    buffer.extend_from_slice(data);
}

fn extract_complete_osc_color_queries(buffer: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let mut sequences = Vec::new();
    let mut idx = 0usize;
    let mut remainder_start = None;

    while idx + 1 < buffer.len() {
        if buffer[idx] == ESC && buffer[idx + 1] == b']' {
            let start = idx;
            let mut scan = idx + 2;
            let mut end = None;

            while scan < buffer.len() {
                if buffer[scan] == BEL {
                    end = Some(scan + 1);
                    break;
                }
                if buffer[scan] == ESC && scan + 1 < buffer.len() && buffer[scan + 1] == b'\\' {
                    end = Some(scan + 2);
                    break;
                }
                scan += 1;
            }

            if let Some(end_idx) = end {
                let seq = buffer[start..end_idx].to_vec();
                if parse_osc_color_query(&seq).is_some() {
                    sequences.push(seq);
                }
                idx = end_idx;
                continue;
            }

            remainder_start = Some(start);
            break;
        }

        idx += 1;
    }

    let remainder = if let Some(start) = remainder_start {
        buffer[start..].to_vec()
    } else if buffer.last() == Some(&ESC) {
        vec![ESC]
    } else {
        Vec::new()
    };

    buffer.clear();
    buffer.extend_from_slice(&remainder);
    sequences
}

fn parse_osc_color_query(seq: &[u8]) -> Option<(u8, &'static [u8])> {
    if seq.len() < 6 || seq[0] != ESC || seq[1] != b']' {
        return None;
    }

    let (payload, terminator) = if seq.ends_with(&[BEL]) {
        (&seq[2..seq.len() - 1], &[BEL][..])
    } else if seq.ends_with(&[ESC, b'\\']) {
        (&seq[2..seq.len() - 2], b"\x1b\\".as_slice())
    } else {
        return None;
    };

    let separator = payload.iter().position(|byte| *byte == b';')?;
    let code = &payload[..separator];
    let value = &payload[separator + 1..];
    if value != b"?" {
        return None;
    }

    let code = match code {
        b"10" => 10,
        b"11" => 11,
        b"12" => 12,
        _ => return None,
    };
    Some((code, terminator))
}

// ── Snapshot generation ───────────────────────────────────────────────────────

/// Generate an ANSI snapshot from the terminal for browser reconnect.
///
/// Uses ghostty's built-in formatter which handles SGR, hyperlinks, modes,
/// cursor, kitty keyboard, scrolling regions, and all other terminal state.
pub fn generate_snapshot(parser: &TerminalParser, skip_visible: bool) -> Vec<u8> {
    if skip_visible {
        let mut out = Vec::new();
        if parser.alt_screen_active() {
            out.extend_from_slice(b"\x1b[?1049h");
        }
        out.extend_from_slice(b"\x1b[0m\x1b[H\x1b[2J\x1b[H");
        if parser.cursor_hidden() {
            out.extend_from_slice(b"\x1b[?25l");
        }
        if parser.kitty_enabled() {
            out.extend_from_slice(b"\x1b[>1u");
        }
        return out;
    }
    parser.terminal().format_vt_full().unwrap_or_default()
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
    fn snapshot_non_empty() {
        let p = TerminalParser::new(24, 80, 100);
        let snap = generate_snapshot(&p, false);
        assert!(!snap.is_empty());
    }

    #[test]
    fn snapshot_skip_visible() {
        let p = TerminalParser::new(24, 80, 100);
        let snap = generate_snapshot(&p, true);
        assert!(snap.windows(4).any(|w| w == b"\x1b[2J"));
    }

    #[test]
    fn snapshot_contains_content() {
        let mut p = TerminalParser::new(24, 80, 100);
        p.process(b"hello world");
        let snap = generate_snapshot(&p, false);
        let snap_str = String::from_utf8_lossy(&snap);
        assert!(snap_str.contains("hello world"));
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
    fn min_rows_cols_clamped() {
        let p = TerminalParser::new(0, 0, 100);
        assert_eq!(p.terminal().rows(), MIN_ROWS);
        assert_eq!(p.terminal().cols(), MIN_COLS);
    }
}
