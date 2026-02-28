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
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};

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
        let size = TermSize { columns: cols, screen_lines: rows };
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
        Self { term, processor }
    }

    /// Feed raw PTY bytes into the terminal emulator.
    ///
    /// Hot path — bytes from the broker or PTY reader arrive here and update
    /// the internal grid, cursor, and mode state atomically.
    pub fn process(&mut self, data: &[u8]) {
        self.processor.advance(&mut self.term, data);
    }

    /// Resize the terminal to new dimensions.
    ///
    /// Handles cursor clamping, grid reflow, and tab stop recalculation.
    /// Unlike vt100, no minimum-row clamp is required for correctness.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let rows = (rows.max(MIN_ROWS)) as usize;
        let cols = (cols.max(MIN_COLS)) as usize;
        let size = TermSize { columns: cols, screen_lines: rows };
        self.term.resize(size);
    }

    /// Borrow the underlying [`Term`] for reading grid state.
    pub fn term(&self) -> &Term<L> {
        &self.term
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

    /// Whether the Kitty keyboard protocol is currently active.
    ///
    /// alacritty_terminal tracks this via the composite `KITTY_KEYBOARD_PROTOCOL`
    /// mode flags, set by CSI > flags u sequences. Replaces the manual atomic
    /// bool previously updated by byte-scanning for `\x1b[>1u` / `\x1b[<u`.
    pub fn kitty_enabled(&self) -> bool {
        self.term.mode().intersects(TermMode::KITTY_KEYBOARD_PROTOCOL)
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

    // Reset all SGR attributes and move cursor home.
    out.extend_from_slice(b"\x1b[0m\x1b[H");

    if skip_visible {
        // Blank screen — the app will redraw after receiving SIGWINCH.
        out.extend_from_slice(b"\x1b[2J\x1b[H");
        return out;
    }

    let term = parser.term();
    let grid = term.grid();
    let cols = grid.columns();
    let screen_lines = grid.screen_lines();
    let history = grid.history_size();

    // Emit scrollback history oldest-first so RESTTY processes the lines in
    // chronological order and builds its own scrollback buffer. This preserves
    // the ability to scroll up in the browser after reconnect.
    //
    // Line(-history as i32) is the oldest stored history line.
    // Line(-1) is the most recent history line (one row above the viewport).
    if history > 0 {
        for hist in (1..=history).rev() {
            emit_grid_line(&mut out, grid, Line(-(hist as i32)), cols);
            out.extend_from_slice(b"\r\n");
        }
    }

    // Emit viewport lines.
    // Line(0) is the top of the viewport; Line(screen_lines - 1) is the bottom.
    for line_idx in 0..screen_lines {
        emit_grid_line(&mut out, grid, Line(line_idx as i32), cols);
        if line_idx < screen_lines - 1 {
            out.extend_from_slice(b"\r\n");
        }
    }

    // Reset SGR then position the cursor at its current location.
    // ANSI cursor addressing is 1-indexed; Line/Column are 0-indexed.
    out.extend_from_slice(b"\x1b[0m");
    let cursor = grid.cursor.point;
    // Cursor line is always in the viewport (non-negative Line index).
    let row = cursor.line.0 as usize + 1;
    let col = cursor.column.0 + 1;
    out.extend_from_slice(format!("\x1b[{row};{col}H").as_bytes());

    out
}

/// Emit a single grid row as ANSI bytes with incremental SGR transitions.
///
/// Wide-char spacer cells ([`Flags::WIDE_CHAR_SPACER`]) are skipped — the base
/// wide character was already emitted by the preceding cell. Zero-width
/// combining characters stored in [`alacritty_terminal::term::cell::CellExtra`]
/// are appended immediately after their base character.
fn emit_grid_line(out: &mut Vec<u8>, grid: &Grid<Cell>, line: Line, cols: usize) {
    let mut sgr = SgrState::reset();
    let mut char_buf = [0u8; 4];

    for col in 0..cols {
        let cell = &grid[Point::new(line, Column(col))];

        // Skip wide-char continuation spacer — rendered as part of the wide char.
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }

        // Emit only the SGR attributes that differ from the previous cell.
        let new_sgr = SgrState::from_cell(cell);
        if new_sgr != sgr {
            new_sgr.emit_diff(out, &sgr);
            sgr = new_sgr;
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

// ── SGR state ─────────────────────────────────────────────────────────────────

/// Accumulated SGR (Select Graphic Rendition) state for a single cell.
///
/// Tracks the visual attributes needed to render a cell. Used to diff-emit
/// only changed attributes when walking the grid, avoiding redundant escape
/// sequences on every cell (same technique Zellij uses in `serialize_chunks`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SgrState {
    fg: Color,
    bg: Color,
    /// Visual-only flags; structural flags (WIDE_CHAR, WRAPLINE) excluded.
    flags: Flags,
}

impl SgrState {
    /// The post-reset state — default terminal colors, no attributes.
    fn reset() -> Self {
        Self {
            fg: Color::Named(NamedColor::Foreground),
            bg: Color::Named(NamedColor::Background),
            flags: Flags::empty(),
        }
    }

    /// Extract the visual SGR state from a terminal cell.
    fn from_cell(cell: &Cell) -> Self {
        // Mask to only the visual flags we can express as SGR codes.
        // Structural flags (WIDE_CHAR, WIDE_CHAR_SPACER, WRAPLINE,
        // LEADING_WIDE_CHAR_SPACER) are not SGR attributes.
        const VISUAL_FLAGS: Flags = Flags::BOLD
            .union(Flags::ITALIC)
            .union(Flags::UNDERLINE)
            .union(Flags::DIM)
            .union(Flags::INVERSE)
            .union(Flags::HIDDEN)
            .union(Flags::STRIKEOUT);

        Self {
            fg: cell.fg,
            bg: cell.bg,
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
        if self.flags.contains(Flags::UNDERLINE) {
            out.extend_from_slice(b";4");
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
                out.extend_from_slice(
                    format!(";38;2;{};{};{}", rgb.r, rgb.g, rgb.b).as_bytes(),
                );
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
                out.extend_from_slice(
                    format!(";48;2;{};{};{}", rgb.r, rgb.g, rgb.b).as_bytes(),
                );
            }
        }

        out.push(b'm');
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
        let rgb = alacritty_terminal::vte::ansi::Rgb { r: 10, g: 20, b: 30 };
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
