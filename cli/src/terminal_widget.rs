//! Terminal widget for rendering vt100 screens to ratatui
//!
//! This module provides a simple, testable widget that renders a vt100::Screen
//! to a ratatui buffer. It replaces tui-term with a minimal implementation
//! that works with vt100 0.16+ (which has working scrollback).

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Widget},
};

/// Configuration for cursor rendering
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
            symbol: "█".to_string(),
            style: Style::default().fg(Color::White),
        }
    }
}

/// A widget that renders a vt100::Screen to a ratatui buffer
pub struct TerminalWidget<'a> {
    screen: &'a vt100::Screen,
    block: Option<Block<'a>>,
    cursor: CursorConfig,
}

impl std::fmt::Debug for TerminalWidget<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalWidget")
            .field("cursor", &self.cursor)
            .field("has_block", &self.block.is_some())
            .finish_non_exhaustive()
    }
}

impl<'a> TerminalWidget<'a> {
    /// Create a new terminal widget from a vt100 screen
    pub fn new(screen: &'a vt100::Screen) -> Self {
        Self {
            screen,
            block: None,
            cursor: CursorConfig::default(),
        }
    }

    /// Set the block (border/title) for the widget
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// Configure cursor rendering
    pub fn cursor(mut self, cursor: CursorConfig) -> Self {
        self.cursor = cursor;
        self
    }

    /// Hide the cursor
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
        render_screen(self.screen, inner_area, buf);

        // Render cursor
        if self.cursor.show && !self.screen.hide_cursor() {
            render_cursor(self.screen, inner_area, buf, &self.cursor);
        }
    }
}

/// Render vt100 screen cells to ratatui buffer
fn render_screen(screen: &vt100::Screen, area: Rect, buf: &mut Buffer) {
    for row in 0..area.height {
        for col in 0..area.width {
            if let Some(cell) = screen.cell(row, col) {
                let buf_x = area.x + col;
                let buf_y = area.y + row;

                if buf_x < area.x + area.width && buf_y < area.y + area.height {
                    let buf_cell = &mut buf[(buf_x, buf_y)];
                    apply_cell(cell, buf_cell);
                }
            }
        }
    }
}

/// Render cursor at current position
fn render_cursor(
    screen: &vt100::Screen,
    area: Rect,
    buf: &mut Buffer,
    cursor_config: &CursorConfig,
) {
    let (cursor_row, cursor_col) = screen.cursor_position();

    let buf_x = area.x + cursor_col;
    let buf_y = area.y + cursor_row;

    if buf_x < area.x + area.width && buf_y < area.y + area.height {
        let buf_cell = &mut buf[(buf_x, buf_y)];

        // If cell has content, just style it; otherwise show cursor symbol
        if let Some(cell) = screen.cell(cursor_row, cursor_col) {
            if cell.has_contents() {
                buf_cell.set_style(cursor_config.style.add_modifier(Modifier::REVERSED));
            } else {
                buf_cell.set_symbol(&cursor_config.symbol);
                buf_cell.set_style(cursor_config.style);
            }
        }
    }
}

/// Apply vt100 cell properties to ratatui buffer cell
fn apply_cell(vt_cell: &vt100::Cell, buf_cell: &mut ratatui::buffer::Cell) {
    // Set content
    if vt_cell.has_contents() {
        buf_cell.set_symbol(vt_cell.contents());
    }

    // Build style from vt100 cell attributes
    let mut style = Style::default();

    // Foreground color
    style = style.fg(convert_color(vt_cell.fgcolor()));

    // Background color
    style = style.bg(convert_color(vt_cell.bgcolor()));

    // Text modifiers
    if vt_cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if vt_cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if vt_cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if vt_cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }

    buf_cell.set_style(style);
}

/// Convert vt100 color to ratatui color
fn convert_color(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn test_terminal_widget_creation() {
        let parser = vt100::Parser::new(24, 80, 1000);
        let widget = TerminalWidget::new(parser.screen());
        assert!(widget.cursor.show);
    }

    #[test]
    fn test_terminal_widget_hide_cursor() {
        let parser = vt100::Parser::new(24, 80, 1000);
        let widget = TerminalWidget::new(parser.screen()).hide_cursor();
        assert!(!widget.cursor.show);
    }

    #[test]
    fn test_terminal_widget_with_block() {
        let parser = vt100::Parser::new(24, 80, 1000);
        let block = Block::default().title("Test");
        let widget = TerminalWidget::new(parser.screen()).block(block);
        assert!(widget.block.is_some());
    }

    #[test]
    fn test_convert_color_default() {
        assert_eq!(convert_color(vt100::Color::Default), Color::Reset);
    }

    #[test]
    fn test_convert_color_indexed() {
        assert_eq!(convert_color(vt100::Color::Idx(1)), Color::Indexed(1));
        assert_eq!(convert_color(vt100::Color::Idx(255)), Color::Indexed(255));
    }

    #[test]
    fn test_convert_color_rgb() {
        assert_eq!(
            convert_color(vt100::Color::Rgb(255, 128, 64)),
            Color::Rgb(255, 128, 64)
        );
    }

    #[test]
    fn test_render_empty_screen() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let parser = vt100::Parser::new(24, 80, 1000);

        terminal
            .draw(|f| {
                let widget = TerminalWidget::new(parser.screen());
                f.render_widget(widget, f.area());
            })
            .unwrap();

        // Should not panic
    }

    #[test]
    fn test_render_with_content() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut parser = vt100::Parser::new(24, 80, 1000);
        parser.process(b"Hello, World!");

        terminal
            .draw(|f| {
                let widget = TerminalWidget::new(parser.screen());
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

        let mut parser = vt100::Parser::new(24, 80, 1000);
        // Red foreground: ESC[31m
        parser.process(b"\x1b[31mRed Text\x1b[0m");

        terminal
            .draw(|f| {
                let widget = TerminalWidget::new(parser.screen());
                f.render_widget(widget, f.area());
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        // Index 1 is red in standard 16-color palette
        assert_eq!(buffer[(0, 0)].fg, Color::Indexed(1));
    }

    #[test]
    fn test_render_with_bold() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut parser = vt100::Parser::new(24, 80, 1000);
        // Bold: ESC[1m
        parser.process(b"\x1b[1mBold\x1b[0m");

        terminal
            .draw(|f| {
                let widget = TerminalWidget::new(parser.screen());
                f.render_widget(widget, f.area());
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert!(buffer[(0, 0)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_scrollback_does_not_panic() {
        // This is the key test - vt100 0.15 would panic here
        let mut parser = vt100::Parser::new(24, 80, 1000);

        // Fill screen and scrollback with content
        for i in 0..100 {
            parser.process(format!("Line {}\r\n", i).as_bytes());
        }

        // Scroll back beyond screen height - this would panic in vt100 0.15
        // In vt100 0.16, set_scrollback is on Screen
        parser.screen_mut().set_scrollback(50);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| {
                let widget = TerminalWidget::new(parser.screen());
                f.render_widget(widget, f.area());
            })
            .unwrap();

        // Should not panic!
    }

    #[test]
    fn test_scrollback_shows_history() {
        let mut parser = vt100::Parser::new(24, 80, 1000);

        // Add numbered lines
        for i in 0..50 {
            parser.process(format!("Line {:02}\r\n", i).as_bytes());
        }

        // At offset 0, we should see the latest lines
        assert_eq!(parser.screen().scrollback(), 0);

        // Scroll back (in vt100 0.16, set_scrollback is on Screen)
        parser.screen_mut().set_scrollback(20);
        assert_eq!(parser.screen().scrollback(), 20);

        // Screen should now show earlier content
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| {
                let widget = TerminalWidget::new(parser.screen());
                f.render_widget(widget, f.area());
            })
            .unwrap();
    }
}
