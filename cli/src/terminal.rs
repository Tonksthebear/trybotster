//! Terminal emulator abstraction wrapping [`alacritty_terminal`].
//!
//! Provides [`AlacrittyParser`] — a thin wrapper around [`Term`] and
//! [`Processor`] that presents a simple `process(bytes)` / `resize(rows, cols)`
//! interface, replacing `vt100::Parser` across the codebase with minimal churn.
//!
//! Also provides [`generate_ansi_snapshot`] which serializes the terminal grid
//! directly to ANSI bytes for browser reconnect snapshots — replacing the old
//! `vt100::contents_formatted()` round-trip that lost active SGR state.
//!
//! # Architecture
//!
//! ```text
//! AlacrittyParser<L>
//!  ├── term: Term<L>          (alacritty grid, cursor, modes)
//!  ├── processor: Processor   (VTE state machine — feeds bytes into term)
//!  └── L: EventListener       (routes title / pty-write events to callers)
//! ```
//!
//! Two concrete event listeners are provided:
//! - [`NoopListener`] — for TUI-local parsers where events are managed elsewhere
//! - Hub-side callers define their own listener (see `agent/pty/mod.rs`)
//!
//! # Thread Safety
//!
//! `AlacrittyParser<L>` is `Send` when `L: Send`. Shared access should use
//! `Arc<Mutex<AlacrittyParser<L>>>`, identical to the old `Arc<Mutex<vt100::Parser>>`.
//!
//! # Rust guideline compliant 2026-02-27

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, CursorStyle, NamedColor, Processor};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Default scrollback line limit for shadow terminals.
///
/// 5 000 lines matches the old `SHADOW_SCROLLBACK_LINES` constant and is
/// large enough to capture a full Claude coding session without excessive
/// memory use.
pub const DEFAULT_SCROLLBACK_LINES: usize = 5_000;

/// Minimum rows clamped on construction.
///
/// Unlike vt100 (which panicked on < 2 rows due to an arithmetic bug),
/// alacritty_terminal is robust at 1 row. We keep a floor of 1 for sanity.
pub const MIN_ROWS: u16 = 1;

/// Minimum columns clamped on construction.
pub const MIN_COLS: u16 = 1;

// ── Dimensions helper ─────────────────────────────────────────────────────────

/// Minimal [`Dimensions`] implementor for constructing and resizing a [`Term`].
///
/// `Term::new` and `Term::resize` require `&D: Dimensions`. This struct
/// satisfies that bound without pulling in alacritty's full `SizeInfo`.
#[derive(Debug, Clone, Copy)]
struct TermSize {
    columns: usize,
    screen_lines: usize,
}

impl Dimensions for TermSize {
    fn columns(&self) -> usize {
        self.columns
    }

    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    fn total_lines(&self) -> usize {
        // For construction/resize purposes the viewport height is sufficient;
        // scrollback grows dynamically via Config::scrolling_history.
        self.screen_lines
    }
}

// ── Event listeners ───────────────────────────────────────────────────────────

/// No-op event listener.
///
/// Use for TUI-local parsers where terminal events (title changes, bell, etc.)
/// are managed by the hub shadow screen rather than the local renderer.
#[derive(Debug, Clone, Copy)]
pub struct NoopListener;

impl EventListener for NoopListener {
    fn send_event(&self, _event: alacritty_terminal::event::Event) {}
}

// ── AlacrittyParser ───────────────────────────────────────────────────────────

/// Thin wrapper around [`Term<L>`] + [`Processor`].
///
/// `Term<L>` has no direct byte-ingestion method — bytes must be driven through
/// a [`Processor`] state machine. This wrapper bundles both so callers use a
/// single owned value with a simple `process` / `resize` / `term` API.
///
/// # Type parameter
///
/// `L` is the [`EventListener`] implementation. Use [`NoopListener`] for
/// TUI-local parsers; hub callers supply their own listener that routes
/// `Event::Title` etc. to the PTY event broadcast channel.
///
/// # Example
///
/// ```rust,ignore
/// // Hub shadow screen (event routing via custom listener)
/// let parser = AlacrittyParser::new_with_listener(24, 80, 5_000, my_listener);
///
/// // TUI local parser (no event routing needed)
/// let parser = AlacrittyParser::new_noop(24, 80, DEFAULT_SCROLLBACK_LINES);
/// ```
pub struct AlacrittyParser<L: EventListener> {
    term: Term<L>,
    processor: Processor,
    /// Tracked DECSTBM scrolling region (1-indexed top/bottom).
    ///
    /// `None` means default full-screen region.
    scroll_region: Option<(u16, u16)>,
    scan_state: ControlScanState,
}

#[derive(Debug, Clone)]
enum ControlScanState {
    Ground,
    Esc,
    Csi(Vec<u8>),
}

impl<L: EventListener> std::fmt::Debug for AlacrittyParser<L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // M-PUBLIC-DEBUG: custom impl avoids leaking Term internals.
        f.debug_struct("AlacrittyParser")
            .field("history_size", &self.history_size())
            .field("cols", &self.term.grid().columns())
            .field("rows", &self.term.grid().screen_lines())
            .finish_non_exhaustive()
    }
}

impl AlacrittyParser<NoopListener> {
    /// Create a no-op parser — events are silently discarded.
    ///
    /// Suitable for TUI-local parsers where title / bell events are not needed.
    pub fn new_noop(rows: u16, cols: u16, scrollback: usize) -> Self {
        Self::new_with_listener(rows, cols, scrollback, NoopListener)
    }
}

impl<L: EventListener> AlacrittyParser<L> {
    /// Create a parser with a custom event listener.
    ///
    /// `listener` receives [`alacritty_terminal::event::Event`] values as the
    /// terminal processes bytes — use this to route title changes, bell, etc.
    /// to the PTY event broadcast.
    pub fn new_with_listener(rows: u16, cols: u16, scrollback: usize, listener: L) -> Self {
        let rows = (rows.max(MIN_ROWS)) as usize;
        let cols = (cols.max(MIN_COLS)) as usize;
        let size = TermSize {
            columns: cols,
            screen_lines: rows,
        };
        let config = Config {
            scrolling_history: scrollback,
            // Enable kitty keyboard protocol processing so \x1b[>Nu push sequences
            // set TermMode::KITTY_KEYBOARD_PROTOCOL correctly. Without this flag
            // alacritty ignores all CSI > N u sequences.
            kitty_keyboard: true,
            ..Config::default()
        };
        let term = Term::new(config, &size, listener);
        let processor = Processor::new();
        Self {
            term,
            processor,
            scroll_region: None,
            scan_state: ControlScanState::Ground,
        }
    }

    /// Feed raw PTY bytes into the terminal emulator.
    ///
    /// Hot path — bytes from the broker or PTY reader arrive here and update
    /// the internal grid, cursor, and mode state atomically.
    pub fn process(&mut self, data: &[u8]) {
        self.track_control_state(data);
        self.processor.advance(&mut self.term, data);
    }

    /// Resize the terminal to new dimensions.
    ///
    /// Handles cursor clamping, grid reflow, and tab stop recalculation.
    /// Unlike vt100, no minimum-row clamp is required for correctness.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let rows = (rows.max(MIN_ROWS)) as usize;
        let cols = (cols.max(MIN_COLS)) as usize;
        let size = TermSize {
            columns: cols,
            screen_lines: rows,
        };
        self.term.resize(size);
        // Alacritty resets scroll region to full screen on resize.
        self.scroll_region = None;
    }

    /// Borrow the underlying [`Term`] for reading grid state.
    pub fn term(&self) -> &Term<L> {
        &self.term
    }

    /// Tracked DECSTBM scrolling region (1-indexed top/bottom).
    ///
    /// `None` means default full-screen region.
    pub fn scroll_region(&self) -> Option<(u16, u16)> {
        self.scroll_region
    }

    /// Mutably borrow the underlying [`Term`].
    ///
    /// Used by [`TerminalPanel`](crate::tui::terminal_panel::TerminalPanel) to
    /// adjust the display scroll offset for scrollback rendering.
    pub fn term_mut(&mut self) -> &mut Term<L> {
        &mut self.term
    }

    /// Total number of lines currently stored in scrollback history.
    pub fn history_size(&self) -> usize {
        self.term.grid().history_size()
    }

    /// Whether the terminal has requested the cursor be hidden (`\x1b[?25l`).
    pub fn cursor_hidden(&self) -> bool {
        !self.term.mode().contains(TermMode::SHOW_CURSOR)
    }

    /// Current cursor style (shape + blink) as set by the running application.
    ///
    /// Returns the style set via DECSCUSR, falling back to `CursorStyle::default()`
    /// (blinking block). Does not include hidden state — check `cursor_hidden()`
    /// separately, or use `term().renderable_content().cursor.shape` which
    /// synthesises `CursorShape::Hidden` from `SHOW_CURSOR` mode automatically.
    pub fn cursor_style(&self) -> CursorStyle {
        self.term.cursor_style()
    }

    /// Whether the Kitty keyboard protocol is currently active.
    ///
    /// alacritty_terminal tracks this via the composite `KITTY_KEYBOARD_PROTOCOL`
    /// mode flags, set by CSI > flags u sequences. Replaces the manual atomic
    /// bool previously updated by byte-scanning for `\x1b[>1u` / `\x1b[<u`.
    pub fn kitty_enabled(&self) -> bool {
        self.term
            .mode()
            .intersects(TermMode::KITTY_KEYBOARD_PROTOCOL)
    }

    /// Whether DECCKM (application cursor keys) mode is active.
    ///
    /// When true, arrow keys emit SS3 sequences (`\x1bOA`) instead of CSI
    /// sequences (`\x1b[A`). The outer terminal must mirror this mode so
    /// that keystrokes are encoded correctly for the inner PTY.
    pub fn application_cursor(&self) -> bool {
        self.term.mode().contains(TermMode::APP_CURSOR)
    }

    /// Whether bracketed paste mode is active (`\x1b[?2004h`).
    ///
    /// When true, pasted text should be wrapped in `\x1b[200~` / `\x1b[201~`
    /// delimiters so the inner application can distinguish pastes from typed input.
    pub fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// Extract a plain-text representation of the visible grid contents.
    ///
    /// Walks every cell in the viewport and collects characters into a string
    /// with newlines separating rows. Used in tests to verify terminal content
    /// without depending on ANSI serialization.
    pub fn contents(&self) -> String {
        let grid = self.term.grid();
        let lines = grid.screen_lines();
        let cols = grid.columns();
        let mut out = String::new();
        for row in 0..lines {
            if row > 0 {
                out.push('\n');
            }
            for col in 0..cols {
                let cell = &grid[Point::new(Line(row as i32), Column(col))];
                out.push(cell.c);
            }
        }
        out
    }

    fn track_control_state(&mut self, data: &[u8]) {
        let rows = self.term.grid().screen_lines() as u16;
        for &b in data {
            let state = std::mem::replace(&mut self.scan_state, ControlScanState::Ground);
            self.scan_state = match state {
                ControlScanState::Ground => {
                    if b == 0x1b {
                        ControlScanState::Esc
                    } else {
                        ControlScanState::Ground
                    }
                }
                ControlScanState::Esc => match b {
                    b'[' => ControlScanState::Csi(Vec::new()),
                    // RIS - Reset to Initial State.
                    b'c' => {
                        self.scroll_region = None;
                        ControlScanState::Ground
                    }
                    _ => ControlScanState::Ground,
                },
                ControlScanState::Csi(mut buf) => {
                    buf.push(b);
                    // Final bytes are 0x40..0x7e.
                    if (0x40..=0x7e).contains(&b) {
                        if b == b'r' {
                            self.update_scroll_region_from_csi(&buf, rows);
                        }
                        ControlScanState::Ground
                    } else if buf.len() > 64 {
                        // Defensive reset on malformed/unbounded CSI payloads.
                        ControlScanState::Ground
                    } else {
                        ControlScanState::Csi(buf)
                    }
                }
            };
        }
    }

    fn update_scroll_region_from_csi(&mut self, csi_payload: &[u8], rows: u16) {
        // `csi_payload` includes the final `r`.
        if csi_payload.is_empty() {
            return;
        }
        let params = &csi_payload[..csi_payload.len() - 1];
        if params.is_empty() {
            self.scroll_region = None;
            return;
        }
        if !params.iter().all(|b| b.is_ascii_digit() || *b == b';') {
            return;
        }

        let param_str = match std::str::from_utf8(params) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut parts = param_str.split(';');
        let top = parts
            .next()
            .and_then(|s| {
                if s.is_empty() {
                    Some(1)
                } else {
                    s.parse::<u16>().ok()
                }
            })
            .unwrap_or(1);
        let bottom = parts
            .next()
            .and_then(|s| {
                if s.is_empty() {
                    Some(rows)
                } else {
                    s.parse::<u16>().ok()
                }
            })
            .unwrap_or(rows);
        if top == 0 || bottom == 0 || top >= bottom {
            return;
        }

        if top == 1 && bottom == rows {
            self.scroll_region = None;
        } else {
            self.scroll_region = Some((top, bottom));
        }
    }
}

// ── Snapshot generation ───────────────────────────────────────────────────────

/// Generate a clean ANSI snapshot from the terminal grid.
///
/// Replaces `vt100::snapshot_with_scrollback()` which used `contents_formatted()`
/// — a lossy round-trip through vt100's ANSI serializer that dropped active SGR
/// state and mishandled many escape sequences.
///
/// This function walks the alacritty grid cells directly and emits precise ANSI:
/// - SGR codes diff-emitted (only changed attributes written per cell)
/// - Wide-character spacer cells skipped
/// - Zero-width combining characters appended after their base character
/// - Scrollback history emitted before the viewport so RESTTY builds its own
///   scrollback buffer (browser-side scroll-up preserved after reconnect)
///
/// # Wire format
///
/// ANSI bytes — same wire format as before, no browser-side changes required.
///
/// # Parameters
///
/// - `parser`: parser providing term + grid access (read-only during snapshot)
/// - `skip_visible`: emit a blank screen instead of grid content; used when a
///   resize is pending and the terminal app has not yet redrawn (avoids serving
///   a stale snapshot that will immediately be replaced by the app's redraw)
pub fn generate_ansi_snapshot<L: EventListener>(
    parser: &AlacrittyParser<L>,
    skip_visible: bool,
) -> Vec<u8> {
    let mut out = Vec::new();

    let term = parser.term();
    let grid = term.grid();

    // Enter alt screen on the receiving end before emitting content.
    // Without this, if vim/less/htop is running when a browser reconnects, the
    // alt-screen cells would be written to the *normal* screen — corrupting it
    // when the app later exits and restores the normal buffer.
    //
    // Limitation: `inactive_grid` is private in alacritty_terminal, so we cannot
    // serialize the normal buffer before entering alt screen. When the app exits
    // alt screen (`\x1b[?1049l`), the receiving terminal restores a blank normal
    // buffer rather than the pre-alt content. This is acceptable because:
    // 1. Alt-screen apps (vim, less, htop) immediately redraw on exit
    // 2. The alt-screen content itself (the important part) round-trips correctly
    if term.mode().contains(TermMode::ALT_SCREEN) {
        out.extend_from_slice(b"\x1b[?1049h");
    }

    // Capture kitty state before any early return — must be appended in all
    // paths including skip_visible, because the app won't re-push kitty on
    // a SIGWINCH redraw; only at startup.
    let wants_kitty = term.mode().intersects(TermMode::KITTY_KEYBOARD_PROTOCOL);

    // Use RenderableCursor for position, visibility, and shape — all in one call.
    // This handles wide-char column adjustment and the SHOW_CURSOR TermMode check.
    let renderable_cursor = term.renderable_content().cursor;
    let cursor_hidden = renderable_cursor.shape == CursorShape::Hidden;

    // Pre-compute the DECSCUSR sequence for cursor shape. Only emitted when the
    // cursor is visible; the default browser cursor is a blinking block so we
    // always emit to ensure the correct shape after reconnect.
    let cursor_shape_seq = cursor_shape_decscusr(term.cursor_style());

    // Reset all SGR attributes and move cursor home.
    out.extend_from_slice(b"\x1b[0m\x1b[H");

    if skip_visible {
        // Blank screen — the app will redraw after receiving SIGWINCH.
        out.extend_from_slice(b"\x1b[2J\x1b[H");
        if cursor_hidden {
            out.extend_from_slice(b"\x1b[?25l");
        } else {
            out.extend_from_slice(cursor_shape_seq);
        }
        if wants_kitty {
            out.extend_from_slice(b"\x1b[>1u");
        }
        return out;
    }
    let cols = grid.columns();
    let screen_lines = grid.screen_lines();
    let history = grid.history_size();

    // Emit scrollback + viewport oldest-first as one contiguous stream.
    // Preserve soft-wrap semantics: only emit CRLF between lines that are
    // not flagged WRAPLINE; wrapped lines should flow into the next row.
    let mut lines = Vec::with_capacity(history + screen_lines);
    if history > 0 {
        for hist in (1..=history).rev() {
            lines.push(Line(-(hist as i32)));
        }
    }
    for line_idx in 0..screen_lines {
        lines.push(Line(line_idx as i32));
    }
    let mut sgr = SgrState::reset();
    let mut active_hyperlink: Option<String> = None;
    for (idx, line) in lines.iter().enumerate() {
        let wrapped = line_wraps(grid, *line, cols);
        // Non-wrapped lines should avoid serializing default trailing cells.
        // Writing up to the last column can trigger implicit autowrap in some
        // terminals and shift subsequent rendered rows.
        emit_grid_line(
            &mut out,
            grid,
            *line,
            cols,
            !wrapped,
            &mut sgr,
            &mut active_hyperlink,
        );
        let has_next = idx + 1 < lines.len();
        if has_next && !wrapped {
            // Close any open hyperlink before the line break.
            if active_hyperlink.is_some() {
                out.extend_from_slice(b"\x1b]8;;\x1b\\");
                active_hyperlink = None;
            }
            // Reset SGR at non-wrapped line boundaries so color/attribute state
            // cannot leak into the next non-wrapped row when trailing default
            // cells were trimmed from the current line.
            out.extend_from_slice(b"\x1b[0m\r\n");
            sgr = SgrState::reset();
        }
        // Wrapped lines: sgr and hyperlink state carry over naturally.
    }

    // Close any hyperlink still open at the end of the grid.
    if active_hyperlink.is_some() {
        out.extend_from_slice(b"\x1b]8;;\x1b\\");
    }

    // Reset SGR before restoring terminal modes/cursor state.
    out.extend_from_slice(b"\x1b[0m");

    // Restore DECSTBM scroll region first.
    //
    // DECSTBM resets cursor to home; this must happen before final cursor CUP.
    restore_scroll_region(&mut out, parser.scroll_region());

    // Restore core terminal modes before cursor positioning.
    restore_core_modes(&mut out, term.mode());

    // Position cursor using RenderableCursor (wide-char corrected).
    // ANSI cursor addressing is 1-indexed; Line/Column are 0-indexed.
    let mut row = renderable_cursor.point.line.0 + 1;
    if term.mode().contains(TermMode::ORIGIN) {
        if let Some((top, _bottom)) = parser.scroll_region() {
            row -= i32::from(top.saturating_sub(1));
        }
        row = row.max(1);
    }
    let col = renderable_cursor.point.column.0 + 1;
    out.extend_from_slice(format!("\x1b[{row};{col}H").as_bytes());

    // Restore cursor visibility and shape. `\x1b[0m` (SGR reset) does not
    // affect DECTCEM or DECSCUSR — those are separate terminal state planes.
    if cursor_hidden {
        out.extend_from_slice(b"\x1b[?25l");
    } else {
        // Emit DECSCUSR so the browser shows the correct cursor shape (beam in
        // vim insert mode, underline, etc.).
        out.extend_from_slice(cursor_shape_seq);
    }

    // Restore kitty keyboard protocol if the terminal had it active.
    // Appended here (not in callers) so both the broker snapshot path and the
    // hub shadow-screen path get it automatically without separate bookkeeping.
    if wants_kitty {
        // CSI > 1 u = push with DISAMBIGUATE_ESCAPE_CODES flag.
        out.extend_from_slice(b"\x1b[>1u");
    }

    out
}

fn restore_scroll_region(out: &mut Vec<u8>, region: Option<(u16, u16)>) {
    match region {
        Some((top, bottom)) => out.extend_from_slice(format!("\x1b[{top};{bottom}r").as_bytes()),
        None => out.extend_from_slice(b"\x1b[r"),
    }
}

/// Emit core mode restore sequences based on the terminal's active mode bits.
fn restore_core_modes(out: &mut Vec<u8>, mode: &TermMode) {
    if mode.contains(TermMode::APP_CURSOR) {
        out.extend_from_slice(b"\x1b[?1h");
    } else {
        out.extend_from_slice(b"\x1b[?1l");
    }

    if mode.contains(TermMode::APP_KEYPAD) {
        out.extend_from_slice(b"\x1b=");
    } else {
        out.extend_from_slice(b"\x1b>");
    }

    if mode.contains(TermMode::LINE_WRAP) {
        out.extend_from_slice(b"\x1b[?7h");
    } else {
        out.extend_from_slice(b"\x1b[?7l");
    }

    if mode.contains(TermMode::LINE_FEED_NEW_LINE) {
        out.extend_from_slice(b"\x1b[20h");
    } else {
        out.extend_from_slice(b"\x1b[20l");
    }

    if mode.contains(TermMode::ORIGIN) {
        out.extend_from_slice(b"\x1b[?6h");
    } else {
        out.extend_from_slice(b"\x1b[?6l");
    }

    if mode.contains(TermMode::INSERT) {
        out.extend_from_slice(b"\x1b[4h");
    } else {
        out.extend_from_slice(b"\x1b[4l");
    }

    if mode.contains(TermMode::BRACKETED_PASTE) {
        out.extend_from_slice(b"\x1b[?2004h");
    } else {
        out.extend_from_slice(b"\x1b[?2004l");
    }

    if mode.contains(TermMode::FOCUS_IN_OUT) {
        out.extend_from_slice(b"\x1b[?1004h");
    } else {
        out.extend_from_slice(b"\x1b[?1004l");
    }

    // Mouse reporting modes — only emit the active set/reset.
    // ?1000 = click reporting, ?1002 = button-event (drag), ?1003 = any-event (motion).
    if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
        out.extend_from_slice(b"\x1b[?1000h");
    }
    if mode.contains(TermMode::MOUSE_DRAG) {
        out.extend_from_slice(b"\x1b[?1002h");
    }
    if mode.contains(TermMode::MOUSE_MOTION) {
        out.extend_from_slice(b"\x1b[?1003h");
    }

    // ?1006 = SGR mouse coordinate format (extended coordinates beyond 223).
    if mode.contains(TermMode::SGR_MOUSE) {
        out.extend_from_slice(b"\x1b[?1006h");
    }

    // ?1007 = alternate screen scroll (mouse scroll in alt screen sends arrows).
    if mode.contains(TermMode::ALTERNATE_SCROLL) {
        out.extend_from_slice(b"\x1b[?1007h");
    }
}

/// Whether a rendered grid line is soft-wrapped into the following line.
///
/// Alacritty stores wrap state on the last cell of a line.
fn line_wraps(grid: &Grid<Cell>, line: Line, cols: usize) -> bool {
    if cols == 0 {
        return false;
    }
    grid[Point::new(line, Column(cols - 1))]
        .flags
        .contains(Flags::WRAPLINE)
}

/// Map a [`CursorStyle`] to the corresponding DECSCUSR escape sequence bytes.
///
/// DECSCUSR encodes both shape and blink state in a single parameter:
/// - 1 blinking block, 2 steady block
/// - 3 blinking underline, 4 steady underline
/// - 5 blinking beam, 6 steady beam
///
/// Returns `b"\x1b[2q"` (steady block) for `HollowBlock` and `Hidden`, which
/// should not reach this function in practice (callers check visibility first).
fn cursor_shape_decscusr(style: CursorStyle) -> &'static [u8] {
    match (style.shape, style.blinking) {
        (CursorShape::Block, true) => b"\x1b[1q",
        (CursorShape::Block, false) => b"\x1b[2q",
        (CursorShape::Underline, true) => b"\x1b[3q",
        (CursorShape::Underline, false) => b"\x1b[4q",
        (CursorShape::Beam, true) => b"\x1b[5q",
        (CursorShape::Beam, false) => b"\x1b[6q",
        // HollowBlock is a vi-mode variant of block; treat as steady block.
        (CursorShape::HollowBlock, _) => b"\x1b[2q",
        // Hidden is handled before calling this function.
        (CursorShape::Hidden, _) => b"\x1b[2q",
    }
}

/// Emit a single grid row as ANSI bytes with incremental SGR transitions.
///
/// Wide-char spacer cells ([`Flags::WIDE_CHAR_SPACER`]) are skipped — the base
/// wide character was already emitted by the preceding cell. Zero-width
/// combining characters stored in [`alacritty_terminal::term::cell::CellExtra`]
/// are appended immediately after their base character.
///
/// `sgr` is the live SGR state carried across lines. For wrapped lines, the
/// caller passes the state from the end of the previous row so attributes
/// flow naturally without a spurious reset. For non-wrapped lines, the caller
/// resets SGR and passes a fresh `SgrState::reset()`.
fn emit_grid_line(
    out: &mut Vec<u8>,
    grid: &Grid<Cell>,
    line: Line,
    cols: usize,
    trim_trailing: bool,
    sgr: &mut SgrState,
    active_hyperlink: &mut Option<String>,
) {
    let mut char_buf = [0u8; 4];
    let mut end_col = cols;

    if trim_trailing {
        while end_col > 0 {
            let cell = &grid[Point::new(line, Column(end_col - 1))];
            if is_trimmable_trailing_blank(cell) {
                end_col -= 1;
            } else {
                break;
            }
        }
    }

    for col in 0..end_col {
        let cell = &grid[Point::new(line, Column(col))];

        // Skip wide-char spacers — rendered as part of the wide char.
        // WIDE_CHAR_SPACER: trailing spacer after a width-2 glyph.
        // LEADING_WIDE_CHAR_SPACER: placeholder at last column when a width-2
        // glyph doesn't fit and wraps to the next row.
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }

        // Emit only the SGR attributes that differ from the previous cell.
        let new_sgr = SgrState::from_cell(cell);
        if new_sgr != *sgr {
            new_sgr.emit_diff(out, sgr);
            *sgr = new_sgr;
        }

        // Handle hyperlink transitions (OSC 8).
        let cell_link = cell.hyperlink();
        let cell_uri = cell_link.as_ref().map(|h| h.uri().to_string());
        if cell_uri != *active_hyperlink {
            if active_hyperlink.is_some() {
                // Close the previous hyperlink.
                out.extend_from_slice(b"\x1b]8;;\x1b\\");
            }
            if let Some(ref link) = cell_link {
                // Open a new hyperlink.
                let id = link.id();
                if id.is_empty() {
                    out.extend_from_slice(format!("\x1b]8;;{}\x1b\\", link.uri()).as_bytes());
                } else {
                    out.extend_from_slice(
                        format!("\x1b]8;id={};{}\x1b\\", id, link.uri()).as_bytes(),
                    );
                }
            }
            *active_hyperlink = cell_uri;
        }

        // Emit base character.
        let encoded = cell.c.encode_utf8(&mut char_buf);
        out.extend_from_slice(encoded.as_bytes());

        // Emit zero-width combining characters (e.g. diacritic modifiers).
        if let Some(zerowidth) = cell.zerowidth() {
            for &zw in zerowidth {
                let encoded = zw.encode_utf8(&mut char_buf);
                out.extend_from_slice(encoded.as_bytes());
            }
        }
    }
}

fn is_trimmable_trailing_blank(cell: &Cell) -> bool {
    const VISUAL_FLAGS: Flags = Flags::BOLD
        .union(Flags::ITALIC)
        .union(Flags::ALL_UNDERLINES)
        .union(Flags::DIM)
        .union(Flags::INVERSE)
        .union(Flags::HIDDEN)
        .union(Flags::STRIKEOUT);

    let default_fg = cell.fg == Color::Named(NamedColor::Foreground);
    let default_bg = cell.bg == Color::Named(NamedColor::Background);
    let visual_default = cell.flags.intersection(VISUAL_FLAGS).is_empty();
    let no_underline_color = cell.underline_color().is_none();

    cell.c == ' '
        && cell.zerowidth().is_none()
        && default_fg
        && default_bg
        && visual_default
        && no_underline_color
}

// ── SGR state ─────────────────────────────────────────────────────────────────

/// Underline style variants supported by modern terminals.
///
/// Maps to SGR 4 sub-parameters: `4` (single), `4:2` (double), `4:3` (curly),
/// `4:4` (dotted), `4:5` (dashed). `None` means no underline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnderlineStyle {
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

/// Accumulated SGR (Select Graphic Rendition) state for a single cell.
///
/// Tracks the visual attributes needed to render a cell. Used to diff-emit
/// only changed attributes when walking the grid, avoiding redundant escape
/// sequences on every cell (same technique Zellij uses in `serialize_chunks`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SgrState {
    fg: Color,
    bg: Color,
    underline_color: Option<Color>,
    underline_style: UnderlineStyle,
    /// Visual-only flags; structural flags (WIDE_CHAR, WRAPLINE) and underline
    /// flags (handled by `underline_style`) excluded.
    flags: Flags,
}

impl SgrState {
    /// The post-reset state — default terminal colors, no attributes.
    fn reset() -> Self {
        Self {
            fg: Color::Named(NamedColor::Foreground),
            bg: Color::Named(NamedColor::Background),
            underline_color: None,
            underline_style: UnderlineStyle::None,
            flags: Flags::empty(),
        }
    }

    /// Extract the visual SGR state from a terminal cell.
    fn from_cell(cell: &Cell) -> Self {
        // Mask to only the visual flags we can express as SGR codes.
        // Structural flags (WIDE_CHAR, WIDE_CHAR_SPACER, WRAPLINE,
        // LEADING_WIDE_CHAR_SPACER) are not SGR attributes.
        // Underline variants are tracked via `underline_style` enum, not flags.
        const VISUAL_FLAGS: Flags = Flags::BOLD
            .union(Flags::ITALIC)
            .union(Flags::DIM)
            .union(Flags::INVERSE)
            .union(Flags::HIDDEN)
            .union(Flags::STRIKEOUT);

        let underline_style = if cell.flags.contains(Flags::UNDERCURL) {
            UnderlineStyle::Curly
        } else if cell.flags.contains(Flags::DOTTED_UNDERLINE) {
            UnderlineStyle::Dotted
        } else if cell.flags.contains(Flags::DASHED_UNDERLINE) {
            UnderlineStyle::Dashed
        } else if cell.flags.contains(Flags::DOUBLE_UNDERLINE) {
            UnderlineStyle::Double
        } else if cell.flags.contains(Flags::UNDERLINE) {
            UnderlineStyle::Single
        } else {
            UnderlineStyle::None
        };

        Self {
            fg: cell.fg,
            bg: cell.bg,
            underline_color: cell.underline_color(),
            underline_style,
            flags: cell.flags.intersection(VISUAL_FLAGS),
        }
    }

    /// Write the ANSI escape sequence to transition from `prev` state to `self`.
    ///
    /// Always emits a full reset (`\x1b[0`) followed by re-applying the new
    /// state. Incremental removal of individual attributes (e.g. SGR 22 to
    /// cancel bold) is terminal-dependent and error-prone; full reset + replay
    /// is simpler and universally correct.
    fn emit_diff(self, out: &mut Vec<u8>, _prev: &SgrState) {
        // Full reset — prev state is irrelevant after this.
        out.extend_from_slice(b"\x1b[0");

        if self.flags.contains(Flags::BOLD) {
            out.extend_from_slice(b";1");
        }
        if self.flags.contains(Flags::DIM) {
            out.extend_from_slice(b";2");
        }
        if self.flags.contains(Flags::ITALIC) {
            out.extend_from_slice(b";3");
        }
        match self.underline_style {
            UnderlineStyle::None => {}
            UnderlineStyle::Single => out.extend_from_slice(b";4"),
            UnderlineStyle::Double => out.extend_from_slice(b";4:2"),
            UnderlineStyle::Curly => out.extend_from_slice(b";4:3"),
            UnderlineStyle::Dotted => out.extend_from_slice(b";4:4"),
            UnderlineStyle::Dashed => out.extend_from_slice(b";4:5"),
        }
        if self.flags.contains(Flags::INVERSE) {
            out.extend_from_slice(b";7");
        }
        if self.flags.contains(Flags::HIDDEN) {
            out.extend_from_slice(b";8");
        }
        if self.flags.contains(Flags::STRIKEOUT) {
            out.extend_from_slice(b";9");
        }

        // Foreground color.
        match self.fg {
            Color::Named(name) => {
                if let Some(code) = named_fg_sgr(name) {
                    out.push(b';');
                    out.extend_from_slice(code.as_bytes());
                }
            }
            Color::Indexed(idx) => {
                out.extend_from_slice(format!(";38;5;{idx}").as_bytes());
            }
            Color::Spec(rgb) => {
                out.extend_from_slice(format!(";38;2;{};{};{}", rgb.r, rgb.g, rgb.b).as_bytes());
            }
        }

        // Background color.
        match self.bg {
            Color::Named(name) => {
                if let Some(code) = named_bg_sgr(name) {
                    out.push(b';');
                    out.extend_from_slice(code.as_bytes());
                }
            }
            Color::Indexed(idx) => {
                out.extend_from_slice(format!(";48;5;{idx}").as_bytes());
            }
            Color::Spec(rgb) => {
                out.extend_from_slice(format!(";48;2;{};{};{}", rgb.r, rgb.g, rgb.b).as_bytes());
            }
        }

        out.push(b'm');

        // Underline color uses SGR 58 (set) / 59 (reset) — separate from the
        // main SGR sequence because these are not universally colon-separated
        // sub-params but standalone sequences.
        if let Some(color) = self.underline_color {
            match color {
                Color::Indexed(idx) => {
                    out.extend_from_slice(format!("\x1b[58;5;{idx}m").as_bytes());
                }
                Color::Spec(rgb) => {
                    out.extend_from_slice(
                        format!("\x1b[58;2;{};{};{}m", rgb.r, rgb.g, rgb.b).as_bytes(),
                    );
                }
                Color::Named(name) => {
                    // Named underline colors are unusual but possible.
                    if let Some(code) = named_fg_sgr(name) {
                        // Map fg color codes (30-37, 90-97) to underline color
                        // via SGR 58;5;N indexed form.
                        let idx = match code {
                            "30" => 0,
                            "31" => 1,
                            "32" => 2,
                            "33" => 3,
                            "34" => 4,
                            "35" => 5,
                            "36" => 6,
                            "37" => 7,
                            "90" => 8,
                            "91" => 9,
                            "92" => 10,
                            "93" => 11,
                            "94" => 12,
                            "95" => 13,
                            "96" => 14,
                            "97" => 15,
                            _ => return,
                        };
                        out.extend_from_slice(format!("\x1b[58;5;{idx}m").as_bytes());
                    }
                }
            }
        }
    }
}

/// ANSI SGR code string for a named foreground color.
///
/// Returns `None` for the default foreground (`NamedColor::Foreground`) since
/// `\x1b[0m` already resets to the default; no additional code is needed.
fn named_fg_sgr(color: NamedColor) -> Option<&'static str> {
    match color {
        NamedColor::Black => Some("30"),
        NamedColor::Red => Some("31"),
        NamedColor::Green => Some("32"),
        NamedColor::Yellow => Some("33"),
        NamedColor::Blue => Some("34"),
        NamedColor::Magenta => Some("35"),
        NamedColor::Cyan => Some("36"),
        NamedColor::White => Some("37"),
        NamedColor::BrightBlack => Some("90"),
        NamedColor::BrightRed => Some("91"),
        NamedColor::BrightGreen => Some("92"),
        NamedColor::BrightYellow => Some("93"),
        NamedColor::BrightBlue => Some("94"),
        NamedColor::BrightMagenta => Some("95"),
        NamedColor::BrightCyan => Some("96"),
        NamedColor::BrightWhite => Some("97"),
        // Default foreground — reset already applied; no extra code needed.
        NamedColor::Foreground
        | NamedColor::DimForeground
        | NamedColor::BrightForeground
        | NamedColor::DimBlack
        | NamedColor::DimRed
        | NamedColor::DimGreen
        | NamedColor::DimYellow
        | NamedColor::DimBlue
        | NamedColor::DimMagenta
        | NamedColor::DimCyan
        | NamedColor::DimWhite
        | NamedColor::Background
        | NamedColor::Cursor => None,
    }
}

/// ANSI SGR code string for a named background color.
///
/// Returns `None` for the default background (`NamedColor::Background`).
fn named_bg_sgr(color: NamedColor) -> Option<&'static str> {
    match color {
        NamedColor::Black => Some("40"),
        NamedColor::Red => Some("41"),
        NamedColor::Green => Some("42"),
        NamedColor::Yellow => Some("43"),
        NamedColor::Blue => Some("44"),
        NamedColor::Magenta => Some("45"),
        NamedColor::Cyan => Some("46"),
        NamedColor::White => Some("47"),
        NamedColor::BrightBlack => Some("100"),
        NamedColor::BrightRed => Some("101"),
        NamedColor::BrightGreen => Some("102"),
        NamedColor::BrightYellow => Some("103"),
        NamedColor::BrightBlue => Some("104"),
        NamedColor::BrightMagenta => Some("105"),
        NamedColor::BrightCyan => Some("106"),
        NamedColor::BrightWhite => Some("107"),
        // Default background — reset already applied; no extra code needed.
        NamedColor::Background
        | NamedColor::Foreground
        | NamedColor::Cursor
        | NamedColor::DimBlack
        | NamedColor::DimRed
        | NamedColor::DimGreen
        | NamedColor::DimYellow
        | NamedColor::DimBlue
        | NamedColor::DimMagenta
        | NamedColor::DimCyan
        | NamedColor::DimWhite
        | NamedColor::BrightForeground
        | NamedColor::DimForeground => None,
    }
}

// ── Color conversion ──────────────────────────────────────────────────────────

/// Convert an alacritty [`Color`] to a [`ratatui::style::Color`] for TUI rendering.
///
/// Used by [`crate::terminal_widget`] when rendering grid cells to ratatui buffers.
pub fn to_ratatui_color(color: Color) -> ratatui::style::Color {
    match color {
        Color::Named(name) => named_to_ratatui(name),
        Color::Indexed(idx) => ratatui::style::Color::Indexed(idx),
        Color::Spec(rgb) => ratatui::style::Color::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

/// Map a [`NamedColor`] to the corresponding [`ratatui::style::Color`].
fn named_to_ratatui(color: NamedColor) -> ratatui::style::Color {
    use ratatui::style::Color as C;
    match color {
        NamedColor::Black => C::Indexed(0),
        NamedColor::Red => C::Indexed(1),
        NamedColor::Green => C::Indexed(2),
        NamedColor::Yellow => C::Indexed(3),
        NamedColor::Blue => C::Indexed(4),
        NamedColor::Magenta => C::Indexed(5),
        NamedColor::Cyan => C::Indexed(6),
        NamedColor::White => C::Indexed(7),
        NamedColor::BrightBlack => C::Indexed(8),
        NamedColor::BrightRed => C::Indexed(9),
        NamedColor::BrightGreen => C::Indexed(10),
        NamedColor::BrightYellow => C::Indexed(11),
        NamedColor::BrightBlue => C::Indexed(12),
        NamedColor::BrightMagenta => C::Indexed(13),
        NamedColor::BrightCyan => C::Indexed(14),
        NamedColor::BrightWhite => C::Indexed(15),
        // Default foreground/background map to terminal reset colors.
        NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => {
            C::Reset
        }
        NamedColor::Background => C::Reset,
        // Dim variants map to the dark standard colors.
        NamedColor::DimBlack => C::Indexed(0),
        NamedColor::DimRed => C::Indexed(1),
        NamedColor::DimGreen => C::Indexed(2),
        NamedColor::DimYellow => C::Indexed(3),
        NamedColor::DimBlue => C::Indexed(4),
        NamedColor::DimMagenta => C::Indexed(5),
        NamedColor::DimCyan => C::Indexed(6),
        NamedColor::DimWhite => C::Indexed(7),
        NamedColor::Cursor => C::Reset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_noop_creates_parser() {
        let p = AlacrittyParser::new_noop(24, 80, 100);
        assert_eq!(p.term().grid().screen_lines(), 24);
        assert_eq!(p.term().grid().columns(), 80);
        assert_eq!(p.history_size(), 0);
    }

    #[test]
    fn process_basic_text() {
        let mut p = AlacrittyParser::new_noop(24, 80, 100);
        p.process(b"Hello");
        let cell = p.term().grid()[Point::new(Line(0), Column(0))].clone();
        assert_eq!(cell.c, 'H');
    }

    #[test]
    fn resize_updates_dimensions() {
        let mut p = AlacrittyParser::new_noop(24, 80, 100);
        p.resize(30, 100);
        assert_eq!(p.term().grid().screen_lines(), 30);
        assert_eq!(p.term().grid().columns(), 100);
    }

    #[test]
    fn cursor_shown_by_default() {
        let p = AlacrittyParser::new_noop(24, 80, 100);
        assert!(!p.cursor_hidden());
    }

    #[test]
    fn hide_cursor_sequence() {
        let mut p = AlacrittyParser::new_noop(24, 80, 100);
        p.process(b"\x1b[?25l"); // DECTCEM hide
        assert!(p.cursor_hidden());
        p.process(b"\x1b[?25h"); // DECTCEM show
        assert!(!p.cursor_hidden());
    }

    #[test]
    fn snapshot_restores_cursor_visibility() {
        // Cursor visible by default — snapshot must NOT contain hide sequence.
        let p = AlacrittyParser::new_noop(24, 80, 100);
        let snap = generate_ansi_snapshot(&p, false);
        assert!(
            !snap.contains_slice(b"\x1b[?25l"),
            "visible cursor should not emit hide"
        );

        // After app hides cursor, snapshot must contain DECTCEM hide sequence.
        let mut p = AlacrittyParser::new_noop(24, 80, 100);
        p.process(b"\x1b[?25l");
        let snap = generate_ansi_snapshot(&p, false);
        assert!(
            snap.contains_slice(b"\x1b[?25l"),
            "hidden cursor must be preserved in snapshot"
        );
    }

    #[test]
    fn generate_snapshot_empty_screen() {
        let p = AlacrittyParser::new_noop(24, 80, 100);
        let snap = generate_ansi_snapshot(&p, false);
        // Should at minimum contain preamble and cursor position.
        assert!(!snap.is_empty());
        // Should start with ESC[0m ESC[H reset sequence.
        assert!(snap.starts_with(b"\x1b[0m\x1b[H"));
    }

    #[test]
    fn generate_snapshot_skip_visible() {
        let p = AlacrittyParser::new_noop(24, 80, 100);
        let snap = generate_ansi_snapshot(&p, true);
        assert!(snap.contains_slice(b"\x1b[2J"));
    }

    #[test]
    fn snapshot_replay_preserves_soft_wrap_and_cursor() {
        let mut src = AlacrittyParser::new_noop(2, 5, 100);
        src.process(b"abcdef");
        let src_cursor = src.term().renderable_content().cursor.point;

        let snap = generate_ansi_snapshot(&src, false);

        let mut dst = AlacrittyParser::new_noop(2, 5, 100);
        dst.process(&snap);
        let dst_cursor = dst.term().renderable_content().cursor.point;

        assert_eq!(
            src.term().grid()[Point::new(Line(0), Column(0))].c,
            dst.term().grid()[Point::new(Line(0), Column(0))].c
        );
        assert_eq!(
            src.term().grid()[Point::new(Line(0), Column(4))].c,
            dst.term().grid()[Point::new(Line(0), Column(4))].c
        );
        assert_eq!(
            src.term().grid()[Point::new(Line(1), Column(0))].c,
            dst.term().grid()[Point::new(Line(1), Column(0))].c
        );
        assert_eq!(src_cursor, dst_cursor, "cursor position must round-trip");
    }

    #[test]
    fn snapshot_replay_preserves_multiline_layout_without_extra_wrap() {
        let mut src = AlacrittyParser::new_noop(4, 12, 100);
        src.process(b"line-one\r\nline-two\r\nline-three");
        let src_cursor = src.term().renderable_content().cursor.point;

        let snap = generate_ansi_snapshot(&src, false);

        let mut dst = AlacrittyParser::new_noop(4, 12, 100);
        dst.process(&snap);
        let dst_cursor = dst.term().renderable_content().cursor.point;

        for row in 0..4 {
            for col in 0..12 {
                let src_cell = src.term().grid()[Point::new(Line(row), Column(col))].c;
                let dst_cell = dst.term().grid()[Point::new(Line(row), Column(col))].c;
                assert_eq!(
                    src_cell, dst_cell,
                    "cell mismatch at row={}, col={}",
                    row, col
                );
            }
        }
        assert_eq!(
            src_cursor, dst_cursor,
            "cursor position should not drift after snapshot replay"
        );
    }

    #[test]
    fn snapshot_replay_restores_core_modes() {
        let mut src = AlacrittyParser::new_noop(6, 20, 100);
        src.process(b"\x1b[?1h\x1b=\x1b[20h\x1b[4h\x1b[?6h\x1b[?7l\x1b[?2004h\x1b[?1004h");

        let snap = generate_ansi_snapshot(&src, false);

        let mut dst = AlacrittyParser::new_noop(6, 20, 100);
        dst.process(&snap);

        for flag in [
            TermMode::APP_CURSOR,
            TermMode::APP_KEYPAD,
            TermMode::LINE_FEED_NEW_LINE,
            TermMode::INSERT,
            TermMode::ORIGIN,
            TermMode::LINE_WRAP,
            TermMode::BRACKETED_PASTE,
            TermMode::FOCUS_IN_OUT,
        ] {
            assert_eq!(
                src.term().mode().contains(flag),
                dst.term().mode().contains(flag),
                "mode mismatch for flag {:?}",
                flag
            );
        }
    }

    #[test]
    fn snapshot_replay_preserves_post_attach_newline_behavior() {
        let mut src = AlacrittyParser::new_noop(4, 12, 100);
        src.process(b"\x1b[20hhello");

        let snap = generate_ansi_snapshot(&src, false);

        let mut dst = AlacrittyParser::new_noop(4, 12, 100);
        dst.process(&snap);

        src.process(b"\nX");
        dst.process(b"\nX");

        assert_eq!(
            src.term().renderable_content().cursor.point,
            dst.term().renderable_content().cursor.point,
            "cursor should stay aligned for post-snapshot newline input"
        );
    }

    #[test]
    fn snapshot_replay_preserves_cursor_with_origin_and_scroll_region() {
        let mut src = AlacrittyParser::new_noop(8, 20, 100);
        src.process(b"\x1b[2;7r\x1b[?6h\x1b[4;1Hhello");

        let snap = generate_ansi_snapshot(&src, false);

        let mut dst = AlacrittyParser::new_noop(8, 20, 100);
        dst.process(&snap);

        assert_eq!(
            src.term().renderable_content().cursor.point,
            dst.term().renderable_content().cursor.point,
            "cursor should stay aligned when origin mode is active"
        );
    }

    #[test]
    fn scroll_region_tracker_handles_chunked_decstbm() {
        let mut parser = AlacrittyParser::new_noop(8, 20, 100);

        parser.process(b"\x1b[2;");
        parser.process(b"7r");
        assert_eq!(parser.scroll_region(), Some((2, 7)));

        parser.process(b"\x1b[r");
        assert_eq!(parser.scroll_region(), None);
    }

    #[test]
    fn snapshot_replay_preserves_post_attach_origin_relative_cursor_moves() {
        let mut src = AlacrittyParser::new_noop(8, 20, 100);
        src.process(b"\x1b[2;7r\x1b[?6h\x1b[4;1H");

        let snap = generate_ansi_snapshot(&src, false);

        let mut dst = AlacrittyParser::new_noop(8, 20, 100);
        dst.process(&snap);

        // App continues emitting origin-relative CUP after reconnect.
        src.process(b"\x1b[4;1HX");
        dst.process(b"\x1b[4;1HX");

        assert_eq!(
            src.term().renderable_content().cursor.point,
            dst.term().renderable_content().cursor.point,
            "post-snapshot origin-relative cursor addressing must stay aligned"
        );
    }

    #[test]
    fn snapshot_replay_resets_sgr_between_non_wrapped_lines() {
        let mut src = AlacrittyParser::new_noop(2, 8, 100);
        src.process(b"\x1b[31mA\x1b[0m\r\nB");

        let snap = generate_ansi_snapshot(&src, false);
        assert!(
            snap.contains_slice(b"\x1b[0m\r\n"),
            "snapshot should reset SGR before a non-wrapped line break"
        );

        let mut dst = AlacrittyParser::new_noop(2, 8, 100);
        dst.process(&snap);

        let src_first = &src.term().grid()[Point::new(Line(0), Column(0))];
        let dst_first = &dst.term().grid()[Point::new(Line(0), Column(0))];
        assert_eq!(
            src_first.fg, dst_first.fg,
            "first row color should round-trip"
        );

        let src_second = &src.term().grid()[Point::new(Line(1), Column(0))];
        let dst_second = &dst.term().grid()[Point::new(Line(1), Column(0))];
        assert_eq!(
            src_second.fg, dst_second.fg,
            "second row should not inherit color from the prior line"
        );
    }

    #[test]
    fn snapshot_skips_leading_wide_char_spacer() {
        // Regression: width-2 glyph written at last column of wrapping row.
        // Alacritty places LEADING_WIDE_CHAR_SPACER at col 4, wide char at row 1 col 0.
        let mut src = AlacrittyParser::new_noop(3, 5, 100);
        // Fill 4 cols then write a wide char that doesn't fit on this row.
        src.process("ABCD\u{4e16}".as_bytes()); // 世 is width-2

        let snap = generate_ansi_snapshot(&src, false);
        let snap_str = String::from_utf8_lossy(&snap);

        // The snapshot should NOT contain a space for the leading spacer at col 4.
        // The wide char should appear in the output.
        assert!(
            snap_str.contains('\u{4e16}'),
            "wide char should be present in snapshot"
        );

        let mut dst = AlacrittyParser::new_noop(3, 5, 100);
        dst.process(&snap);

        // Row 1 col 0 should contain the wide char in both.
        let src_cell = &src.term().grid()[Point::new(Line(1), Column(0))];
        let dst_cell = &dst.term().grid()[Point::new(Line(1), Column(0))];
        assert_eq!(
            src_cell.c, dst_cell.c,
            "wide char should round-trip across wrap"
        );
    }

    #[test]
    fn snapshot_sgr_carries_across_wrapped_lines() {
        // Regression: wrapped row where previous row ends colored, continuation
        // starts with default — SGR must carry across the wrap boundary without
        // a spurious reset, then correctly transition for the new cell.
        let mut src = AlacrittyParser::new_noop(3, 5, 100);
        // Fill 5 cols with red text (wraps), then default text continues on row 2.
        src.process(b"\x1b[31mABCDE\x1b[0mF");

        let snap = generate_ansi_snapshot(&src, false);

        let mut dst = AlacrittyParser::new_noop(3, 5, 100);
        dst.process(&snap);

        // Row 0 col 0 should be red in both.
        let src_cell = &src.term().grid()[Point::new(Line(0), Column(0))];
        let dst_cell = &dst.term().grid()[Point::new(Line(0), Column(0))];
        assert_eq!(
            src_cell.fg, dst_cell.fg,
            "wrapped row color should round-trip"
        );

        // Row 1 col 0 ('F') should be default fg in both.
        let src_cell = &src.term().grid()[Point::new(Line(1), Column(0))];
        let dst_cell = &dst.term().grid()[Point::new(Line(1), Column(0))];
        assert_eq!(
            src_cell.fg, dst_cell.fg,
            "continuation after wrap should have correct (default) color"
        );
        assert_eq!(
            src_cell.c, dst_cell.c,
            "continuation char should round-trip"
        );
    }

    #[test]
    fn snapshot_restores_mouse_modes() {
        // Regression: mouse reporting modes must be restored after snapshot replay.
        let mut src = AlacrittyParser::new_noop(4, 20, 100);
        // Enable: ?1000 (click), ?1006 (SGR mouse), ?1007 (alternate scroll)
        src.process(b"\x1b[?1000h\x1b[?1006h\x1b[?1007h");

        assert!(
            src.term().mode().contains(TermMode::MOUSE_REPORT_CLICK),
            "source should have MOUSE_REPORT_CLICK"
        );
        assert!(
            src.term().mode().contains(TermMode::SGR_MOUSE),
            "source should have SGR_MOUSE"
        );
        assert!(
            src.term().mode().contains(TermMode::ALTERNATE_SCROLL),
            "source should have ALTERNATE_SCROLL"
        );

        let snap = generate_ansi_snapshot(&src, false);

        let mut dst = AlacrittyParser::new_noop(4, 20, 100);
        dst.process(&snap);

        assert!(
            dst.term().mode().contains(TermMode::MOUSE_REPORT_CLICK),
            "MOUSE_REPORT_CLICK should be restored"
        );
        assert!(
            dst.term().mode().contains(TermMode::SGR_MOUSE),
            "SGR_MOUSE should be restored"
        );
        assert!(
            dst.term().mode().contains(TermMode::ALTERNATE_SCROLL),
            "ALTERNATE_SCROLL should be restored"
        );
    }

    #[test]
    fn snapshot_alt_screen_round_trips_and_exit_works() {
        // Regression: alt-screen reconnect — content should round-trip, and
        // exiting alt screen (?1049l) should work after snapshot replay.
        let mut src = AlacrittyParser::new_noop(4, 20, 100);
        // Write normal screen content, enter alt screen, write alt content.
        src.process(b"NORMAL\x1b[?1049h\x1b[HALT_CONTENT");

        assert!(
            src.term().mode().contains(TermMode::ALT_SCREEN),
            "source should be in alt screen"
        );

        let snap = generate_ansi_snapshot(&src, false);
        let snap_str = String::from_utf8_lossy(&snap);

        // Snapshot must contain the alt screen entry sequence.
        assert!(
            snap_str.contains("\x1b[?1049h"),
            "snapshot should enter alt screen"
        );

        let mut dst = AlacrittyParser::new_noop(4, 20, 100);
        dst.process(&snap);

        // Destination should be in alt screen.
        assert!(
            dst.term().mode().contains(TermMode::ALT_SCREEN),
            "destination should be in alt screen after snapshot"
        );

        // Alt screen content should round-trip.
        let src_cell = &src.term().grid()[Point::new(Line(0), Column(0))];
        let dst_cell = &dst.term().grid()[Point::new(Line(0), Column(0))];
        assert_eq!(
            src_cell.c, dst_cell.c,
            "alt screen content should round-trip"
        );

        // Exiting alt screen should work without panic.
        dst.process(b"\x1b[?1049l");
        assert!(
            !dst.term().mode().contains(TermMode::ALT_SCREEN),
            "destination should exit alt screen cleanly"
        );
    }

    #[test]
    fn snapshot_preserves_hyperlinks() {
        // Regression: OSC 8 hyperlinks must open/close around linked text
        // and close before snapshot end.
        let mut src = AlacrittyParser::new_noop(2, 30, 100);
        // OSC 8 ; params ; URI BEL  text  OSC 8 ;; BEL
        // Using BEL (\x07) as string terminator (widely supported).
        src.process(b"\x1b]8;;https://example.com\x07LINK\x1b]8;;\x07 plain");

        // Verify the source cell has a hyperlink set by alacritty.
        let src_cell = &src.term().grid()[Point::new(Line(0), Column(0))];
        assert!(
            src_cell.hyperlink().is_some(),
            "alacritty should set hyperlink on cell via OSC 8 — got None. \
             Cell char: {:?}, cell flags: {:?}",
            src_cell.c,
            src_cell.flags
        );

        let snap = generate_ansi_snapshot(&src, false);
        let snap_str = String::from_utf8_lossy(&snap);

        // Snapshot should contain a hyperlink open with the URI.
        // Alacritty auto-assigns an internal id (e.g. "id=0_alacritty"), so
        // we check for the URI portion rather than an exact sequence.
        assert!(
            snap_str.contains("https://example.com"),
            "snapshot should contain hyperlink URI. Snapshot: {:?}",
            snap_str
        );
        assert!(
            snap_str.contains("\x1b]8;"),
            "snapshot should contain OSC 8 opener"
        );
        // Snapshot should contain the hyperlink close sequence.
        assert!(
            snap_str.contains("\x1b]8;;\x1b\\"),
            "snapshot should contain hyperlink close"
        );

        let mut dst = AlacrittyParser::new_noop(2, 30, 100);
        dst.process(&snap);

        let dst_cell = &dst.term().grid()[Point::new(Line(0), Column(0))];
        assert!(
            dst_cell.hyperlink().is_some(),
            "destination cell should have hyperlink after snapshot replay"
        );
        assert_eq!(
            src_cell.hyperlink().unwrap().uri(),
            dst_cell.hyperlink().unwrap().uri(),
            "hyperlink URI should round-trip"
        );

        // Plain text after the link should NOT have a hyperlink.
        let dst_plain = &dst.term().grid()[Point::new(Line(0), Column(5))];
        assert!(
            dst_plain.hyperlink().is_none(),
            "dest plain cell should have no link"
        );
    }

    #[test]
    fn snapshot_preserves_underline_variants() {
        // Regression: underline styles (curly, double, dotted, dashed) and
        // underline color must round-trip through snapshot.
        let mut src = AlacrittyParser::new_noop(4, 20, 100);
        // SGR 4:3 = curly underline, SGR 58;2;r;g;b = underline color red
        src.process(b"\x1b[4:3m\x1b[58;2;255;0;0mCURLY\x1b[0m ");
        // SGR 4:2 = double underline
        src.process(b"\x1b[4:2mDBL\x1b[0m");

        let snap = generate_ansi_snapshot(&src, false);

        let mut dst = AlacrittyParser::new_noop(4, 20, 100);
        dst.process(&snap);

        // Check curly underline on first char.
        let src_cell = &src.term().grid()[Point::new(Line(0), Column(0))];
        let dst_cell = &dst.term().grid()[Point::new(Line(0), Column(0))];
        assert!(
            src_cell.flags.contains(Flags::UNDERCURL),
            "source should have UNDERCURL"
        );
        assert_eq!(
            src_cell.flags.intersection(Flags::ALL_UNDERLINES),
            dst_cell.flags.intersection(Flags::ALL_UNDERLINES),
            "underline style flags should round-trip"
        );

        // Check double underline.
        let src_dbl = &src.term().grid()[Point::new(Line(0), Column(6))];
        let dst_dbl = &dst.term().grid()[Point::new(Line(0), Column(6))];
        assert!(
            src_dbl.flags.contains(Flags::DOUBLE_UNDERLINE),
            "source should have DOUBLE_UNDERLINE"
        );
        assert_eq!(
            src_dbl.flags.intersection(Flags::ALL_UNDERLINES),
            dst_dbl.flags.intersection(Flags::ALL_UNDERLINES),
            "double underline should round-trip"
        );
    }

    #[test]
    fn sgr_state_reset_matches_defaults() {
        let state = SgrState::reset();
        assert_eq!(state.fg, Color::Named(NamedColor::Foreground));
        assert_eq!(state.bg, Color::Named(NamedColor::Background));
        assert!(state.flags.is_empty());
    }

    #[test]
    fn to_ratatui_color_indexed() {
        assert_eq!(
            to_ratatui_color(Color::Indexed(42)),
            ratatui::style::Color::Indexed(42)
        );
    }

    #[test]
    fn to_ratatui_color_spec() {
        let rgb = alacritty_terminal::vte::ansi::Rgb {
            r: 10,
            g: 20,
            b: 30,
        };
        assert_eq!(
            to_ratatui_color(Color::Spec(rgb)),
            ratatui::style::Color::Rgb(10, 20, 30)
        );
    }

    #[test]
    fn min_rows_cols_clamped() {
        // Should not panic with 0 dimensions.
        let p = AlacrittyParser::new_noop(0, 0, 100);
        assert_eq!(p.term().grid().screen_lines(), MIN_ROWS as usize);
        assert_eq!(p.term().grid().columns(), MIN_COLS as usize);
    }
}

#[cfg(test)]
trait SliceContains {
    fn contains_slice(&self, needle: &[u8]) -> bool;
}

#[cfg(test)]
impl SliceContains for Vec<u8> {
    fn contains_slice(&self, needle: &[u8]) -> bool {
        self.windows(needle.len()).any(|w| w == needle)
    }
}
