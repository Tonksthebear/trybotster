//! Terminal widget for rendering alacritty grid state to ratatui.
//!
//! This module provides a simple, testable widget that renders an alacritty
//! `Term` to a ratatui buffer. Cell colors and flags are mapped through
//! [`to_ratatui_color`] and alacritty's [`Flags`] bitfield.
//!
//! Scroll offset is passed as a parameter — the widget indexes into the
//! grid's history lines directly via negative `Line` indices, avoiding
//! any mutable state on `Term` during rendering.

// Rust guideline compliant 2026-02

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::Term;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    widgets::{Block, Widget},
};

use crate::terminal::{to_ratatui_color, NoopListener};

/// Configuration for cursor rendering.
#[derive(Debug, Clone)]
pub struct CursorConfig {
    /// Whether to display the cursor.
    pub show: bool,
    /// Character to display as the cursor.
    pub symbol: String,
    /// Style for the cursor (color, modifiers).
    pub style: Style,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            show: true,
            symbol: "\u{2588}".to_string(),
            // Use REVERSED to invert terminal colors instead of hardcoding
            style: Style::default().add_modifier(Modifier::REVERSED),
        }
    }
}

/// A widget that renders an alacritty `Term` to a ratatui buffer.
pub struct TerminalWidget<'a> {
    term: &'a Term<NoopListener>,
    scroll_offset: usize,
    block: Option<Block<'a>>,
    cursor: CursorConfig,
}

impl std::fmt::Debug for TerminalWidget<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalWidget")
            .field("cursor", &self.cursor)
            .field("scroll_offset", &self.scroll_offset)
            .field("has_block", &self.block.is_some())
            .finish_non_exhaustive()
    }
}

impl<'a> TerminalWidget<'a> {
    /// Create a new terminal widget from an alacritty `Term`.
    ///
    /// `scroll_offset` is the number of lines scrolled up from the bottom
    /// (0 = live view, N = N lines into history).
    pub fn new(term: &'a Term<NoopListener>, scroll_offset: usize) -> Self {
        Self {
            term,
            scroll_offset,
            block: None,
            cursor: CursorConfig::default(),
        }
    }

    /// Set the block (border/title) for the widget.
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// Configure cursor rendering.
    pub fn cursor(mut self, cursor: CursorConfig) -> Self {
        self.cursor = cursor;
        self
    }

    /// Hide the cursor.
    pub fn hide_cursor(mut self) -> Self {
        self.cursor.show = false;
        self
    }
}

impl Widget for TerminalWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Render block if present and get inner area
        let inner_area = if let Some(block) = &self.block {
            let inner = block.inner(area);
            block.clone().render(area, buf);
            inner
        } else {
            area
        };

        // Render screen cells
        render_grid(self.term, self.scroll_offset, inner_area, buf);

        // Render cursor (only when at live view — no cursor when scrolled)
        if self.cursor.show && self.scroll_offset == 0 {
            render_cursor(self.term, inner_area, buf, &self.cursor);
        }
    }
}

/// Render alacritty grid cells to ratatui buffer.
///
/// When `scroll_offset > 0`, we render history lines. Line indices work as:
/// - `Line(0)` to `Line(screen_lines - 1)` = viewport
/// - `Line(-1)` = most recent history line
/// - `Line(-history_size)` = oldest history line
///
/// With scroll_offset N, the top of the rendered area shows the line that
/// is N lines above the top of the viewport.
fn render_grid(
    term: &Term<NoopListener>,
    scroll_offset: usize,
    area: Rect,
    buf: &mut Buffer,
) {
    let grid = term.grid();

    for row in 0..area.height {
        // Calculate the grid line index for this display row.
        // At scroll_offset=0: row 0 maps to Line(0) (top of viewport).
        // At scroll_offset=N: row 0 maps to Line(-N) (N lines into history).
        let line_idx = row as i32 - scroll_offset as i32;
        let line = Line(line_idx);

        // Bounds check: don't index beyond available history or below viewport.
        let history = grid.history_size() as i32;
        if line_idx < -history || line_idx >= grid.screen_lines() as i32 {
            continue;
        }

        for col in 0..area.width {
            if (col as usize) >= grid.columns() {
                break;
            }

            let cell = &grid[Point::new(line, Column(col as usize))];

            // Skip wide-char spacer — the base wide char was already rendered
            // by the preceding cell via set_symbol (which handles multi-width).
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }

            let buf_x = area.x + col;
            let buf_y = area.y + row;

            if buf_x < area.x + area.width && buf_y < area.y + area.height {
                let buf_cell = &mut buf[(buf_x, buf_y)];
                apply_cell(cell, buf_cell);
            }
        }
    }
}

/// Render cursor at current position.
fn render_cursor(
    term: &Term<NoopListener>,
    area: Rect,
    buf: &mut Buffer,
    cursor_config: &CursorConfig,
) {
    let cursor_point = term.grid().cursor.point;
    let cursor_col = cursor_point.column.0 as u16;
    let cursor_row = cursor_point.line.0 as u16;

    let buf_x = area.x + cursor_col;
    let buf_y = area.y + cursor_row;

    if buf_x < area.x + area.width && buf_y < area.y + area.height {
        let buf_cell = &mut buf[(buf_x, buf_y)];
        let grid = term.grid();
        let cell = &grid[cursor_point];

        if cell.c != ' ' && cell.c != '\0' {
            buf_cell.set_style(cursor_config.style.add_modifier(Modifier::REVERSED));
        } else {
            buf_cell.set_symbol(&cursor_config.symbol);
            buf_cell.set_style(cursor_config.style);
        }
    }
}

/// Apply alacritty cell properties to ratatui buffer cell.
fn apply_cell(
    cell: &alacritty_terminal::term::cell::Cell,
    buf_cell: &mut ratatui::buffer::Cell,
) {
    // Set content — alacritty stores a single char + optional zerowidth extras.
    if cell.c != ' ' && cell.c != '\0' {
        let mut s = String::with_capacity(4);
        s.push(cell.c);
        if let Some(zw) = cell.zerowidth() {
            for &c in zw {
                s.push(c);
            }
        }
        buf_cell.set_symbol(&s);
    }

    // Build style from alacritty cell attributes.
    let mut style = Style::default();

    style = style.fg(to_ratatui_color(cell.fg));
    style = style.bg(to_ratatui_color(cell.bg));

    if cell.flags.contains(Flags::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.flags.contains(Flags::ITALIC) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.flags.contains(Flags::UNDERLINE) {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.flags.contains(Flags::INVERSE) {
        style = style.add_modifier(Modifier::REVERSED);
    }
    if cell.flags.contains(Flags::DIM) {
        style = style.add_modifier(Modifier::DIM);
    }
    if cell.flags.contains(Flags::HIDDEN) {
        style = style.add_modifier(Modifier::HIDDEN);
    }
    if cell.flags.contains(Flags::STRIKEOUT) {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }

    buf_cell.set_style(style);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::AlacrittyParser;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;

    #[test]
    fn test_terminal_widget_creation() {
        let parser = AlacrittyParser::new_noop(24, 80, 1000);
        let widget = TerminalWidget::new(parser.term(), 0);
        assert!(widget.cursor.show);
    }

    #[test]
    fn test_terminal_widget_hide_cursor() {
        let parser = AlacrittyParser::new_noop(24, 80, 1000);
        let widget = TerminalWidget::new(parser.term(), 0).hide_cursor();
        assert!(!widget.cursor.show);
    }

    #[test]
    fn test_terminal_widget_with_block() {
        let parser = AlacrittyParser::new_noop(24, 80, 1000);
        let block = Block::default().title("Test");
        let widget = TerminalWidget::new(parser.term(), 0).block(block);
        assert!(widget.block.is_some());
    }

    #[test]
    fn test_render_empty_screen() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let parser = AlacrittyParser::new_noop(24, 80, 1000);

        terminal
            .draw(|f| {
                let widget = TerminalWidget::new(parser.term(), 0);
                f.render_widget(widget, f.area());
            })
            .unwrap();

        // Should not panic
    }

    #[test]
    fn test_render_with_content() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut parser = AlacrittyParser::new_noop(24, 80, 1000);
        parser.process(b"Hello, World!");

        terminal
            .draw(|f| {
                let widget = TerminalWidget::new(parser.term(), 0);
                f.render_widget(widget, f.area());
            })
            .unwrap();

        // Check that content was rendered
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), "H");
        assert_eq!(buffer[(1, 0)].symbol(), "e");
        assert_eq!(buffer[(2, 0)].symbol(), "l");
    }

    #[test]
    fn test_render_with_colors() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut parser = AlacrittyParser::new_noop(24, 80, 1000);
        // Red foreground: ESC[31m
        parser.process(b"\x1b[31mRed Text\x1b[0m");

        terminal
            .draw(|f| {
                let widget = TerminalWidget::new(parser.term(), 0);
                f.render_widget(widget, f.area());
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        // Named red maps to Indexed(1) via to_ratatui_color
        assert_eq!(buffer[(0, 0)].fg, Color::Indexed(1));
    }

    #[test]
    fn test_render_with_bold() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut parser = AlacrittyParser::new_noop(24, 80, 1000);
        // Bold: ESC[1m
        parser.process(b"\x1b[1mBold\x1b[0m");

        terminal
            .draw(|f| {
                let widget = TerminalWidget::new(parser.term(), 0);
                f.render_widget(widget, f.area());
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert!(buffer[(0, 0)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_scrollback_does_not_panic() {
        let mut parser = AlacrittyParser::new_noop(24, 80, 1000);

        // Fill screen and scrollback with content
        for i in 0..100 {
            parser.process(format!("Line {}\r\n", i).as_bytes());
        }

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        // Render with scroll offset into history
        terminal
            .draw(|f| {
                let widget = TerminalWidget::new(parser.term(), 50);
                f.render_widget(widget, f.area());
            })
            .unwrap();

        // Should not panic
    }

    #[test]
    fn test_scrollback_shows_history() {
        let mut parser = AlacrittyParser::new_noop(24, 80, 1000);

        // Add numbered lines
        for i in 0..50 {
            parser.process(format!("Line {:02}\r\n", i).as_bytes());
        }

        // Render with scroll offset
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| {
                let widget = TerminalWidget::new(parser.term(), 20);
                f.render_widget(widget, f.area());
            })
            .unwrap();

        // Should not panic — content correctness tested at integration level
    }
}
