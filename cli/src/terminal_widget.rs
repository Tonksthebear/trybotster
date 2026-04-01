//! Terminal widget for rendering ghostty terminal state to ratatui.
//!
//! Uses the ghostty render state API — rows and cells are iterated via
//! a `RenderIterator` created per render frame from the `RenderState` snapshot.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    widgets::{Block, Widget},
};

use crate::ghostty_vt::{self, GhosttyCellWide, GhosttyColorRgb, RenderState};

/// A widget that renders a ghostty terminal to a ratatui buffer.
///
/// Cursor rendering is handled by the host terminal's hardware cursor via
/// `Frame::set_cursor_position()` in `render_terminal_panel`, not by cell painting.
pub struct TerminalWidget<'a> {
    render_state: &'a RenderState,
    block: Option<Block<'a>>,
    default_fg: Option<GhosttyColorRgb>,
    default_bg: Option<GhosttyColorRgb>,
}

impl std::fmt::Debug for TerminalWidget<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalWidget")
            .field("has_block", &self.block.is_some())
            .finish_non_exhaustive()
    }
}

impl<'a> TerminalWidget<'a> {
    /// Create a new terminal widget from a render state snapshot.
    ///
    /// The render state must have been updated from the terminal before
    /// creating this widget.
    pub fn new(render_state: &'a RenderState) -> Self {
        Self {
            render_state,
            block: None,
            default_fg: None,
            default_bg: None,
        }
    }

    /// Set the surrounding block decoration.
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// Override the terminal default foreground/background colors.
    pub fn default_colors(mut self, fg: GhosttyColorRgb, bg: GhosttyColorRgb) -> Self {
        self.default_fg = Some(fg);
        self.default_bg = Some(bg);
        self
    }
}

impl Widget for TerminalWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let inner_area = if let Some(block) = &self.block {
            let inner = block.inner(area);
            block.clone().render(area, buf);
            inner
        } else {
            area
        };

        let default_fg = self
            .default_fg
            .unwrap_or_else(|| self.render_state.foreground_color());
        let default_bg = self
            .default_bg
            .unwrap_or_else(|| self.render_state.background_color());
        fill_area(inner_area, buf, default_fg, default_bg);

        if let Ok(mut iter) = self.render_state.iterator() {
            render_grid(&mut iter, inner_area, buf, default_fg, default_bg);
        }
    }
}

fn fill_area(
    area: Rect,
    buf: &mut Buffer,
    default_fg: GhosttyColorRgb,
    default_bg: GhosttyColorRgb,
) {
    let style = Style::default()
        .fg(ghostty_rgb_to_ratatui(default_fg))
        .bg(ghostty_rgb_to_ratatui(default_bg));

    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            let cell = &mut buf[(x, y)];
            cell.set_symbol(" ");
            cell.set_style(style);
        }
    }
}

fn ghostty_rgb_to_ratatui(color: GhosttyColorRgb) -> ratatui::style::Color {
    ratatui::style::Color::Rgb(color.r, color.g, color.b)
}

fn render_grid(
    iter: &mut ghostty_vt::RenderIterator,
    area: Rect,
    buf: &mut Buffer,
    default_fg: GhosttyColorRgb,
    default_bg: GhosttyColorRgb,
) {
    let mut row_idx: u16 = 0;
    while iter.next_row() && row_idx < area.height {
        iter.begin_cells();

        let mut col_idx: u16 = 0;
        while iter.next_cell() && col_idx < area.width {
            let cell = iter.current_cell();

            let wide = ghostty_vt::cell_wide(cell);
            if wide == GhosttyCellWide::SpacerTail || wide == GhosttyCellWide::SpacerHead {
                col_idx += 1;
                continue;
            }

            let buf_x = area.x + col_idx;
            let buf_y = area.y + row_idx;

            if buf_x < area.x + area.width && buf_y < area.y + area.height {
                let buf_cell = &mut buf[(buf_x, buf_y)];

                let graphemes = iter.current_cell_graphemes();
                if !graphemes.is_empty() && graphemes[0] != ' ' && graphemes[0] != '\0' {
                    let s: String = graphemes.into_iter().collect();
                    buf_cell.set_symbol(&s);
                }

                let style = iter.current_cell_style();
                let mut rat_style = Style::default();

                let fg = iter.current_cell_fg().unwrap_or(default_fg);
                let bg = iter.current_cell_bg().unwrap_or(default_bg);
                rat_style = rat_style.fg(ghostty_rgb_to_ratatui(fg));
                rat_style = rat_style.bg(ghostty_rgb_to_ratatui(bg));

                if style.bold {
                    rat_style = rat_style.add_modifier(Modifier::BOLD);
                }
                if style.italic {
                    rat_style = rat_style.add_modifier(Modifier::ITALIC);
                }
                if style.underline != 0 {
                    rat_style = rat_style.add_modifier(Modifier::UNDERLINED);
                }
                if style.inverse {
                    rat_style = rat_style.add_modifier(Modifier::REVERSED);
                }
                if style.faint {
                    rat_style = rat_style.add_modifier(Modifier::DIM);
                }
                if style.invisible {
                    rat_style = rat_style.add_modifier(Modifier::HIDDEN);
                }
                if style.strikethrough {
                    rat_style = rat_style.add_modifier(Modifier::CROSSED_OUT);
                }

                buf_cell.set_style(rat_style);
            }

            col_idx += 1;
        }

        row_idx += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::TerminalParser;
    use crate::tui::terminal_panel::TerminalPanel;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn make_render_state(parser: &mut TerminalParser) -> RenderState {
        let mut rs = RenderState::new().expect("render state creation");
        rs.update(parser.terminal_mut())
            .expect("render state update");
        rs
    }

    #[test]
    fn test_terminal_widget_creation() {
        let mut parser = TerminalParser::new(24, 80, 1000);
        let rs = make_render_state(&mut parser);
        let _widget = TerminalWidget::new(&rs);
    }

    #[test]
    fn test_terminal_widget_with_block() {
        let mut parser = TerminalParser::new(24, 80, 1000);
        let rs = make_render_state(&mut parser);
        let block = Block::default().title("Test");
        let widget = TerminalWidget::new(&rs).block(block);
        assert!(widget.block.is_some());
    }

    #[test]
    fn test_render_empty_screen() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut parser = TerminalParser::new(24, 80, 1000);
        let rs = make_render_state(&mut parser);

        terminal
            .draw(|f| {
                let widget = TerminalWidget::new(&rs);
                f.render_widget(widget, f.area());
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].bg, Color::Rgb(0, 0, 0));
    }

    #[test]
    fn test_render_empty_screen_uses_terminal_default_background() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut parser = TerminalParser::new(24, 80, 1000);
        parser.terminal_mut().set_color_background(GhosttyColorRgb {
            r: 0xF0,
            g: 0xE0,
            b: 0xD0,
        });
        let rs = make_render_state(&mut parser);
        let default_fg = parser
            .foreground_color()
            .unwrap_or(crate::terminal::Rgb::new(255, 255, 255));
        let default_bg = parser
            .background_color()
            .unwrap_or(crate::terminal::Rgb::new(0, 0, 0));

        terminal
            .draw(|f| {
                let widget =
                    TerminalWidget::new(&rs).default_colors(default_fg.into(), default_bg.into());
                f.render_widget(widget, f.area());
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].bg, Color::Rgb(0xF0, 0xE0, 0xD0));
        assert_eq!(buffer[(40, 12)].bg, Color::Rgb(0xF0, 0xE0, 0xD0));
    }

    #[test]
    fn test_render_with_content() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut parser = TerminalParser::new(24, 80, 1000);
        parser.process(b"Hello, World!");
        let rs = make_render_state(&mut parser);

        terminal
            .draw(|f| {
                let widget = TerminalWidget::new(&rs);
                f.render_widget(widget, f.area());
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), "H");
        assert_eq!(buffer[(1, 0)].symbol(), "e");
        assert_eq!(buffer[(2, 0)].symbol(), "l");
    }

    #[test]
    fn test_render_with_content_keeps_terminal_default_background() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut parser = TerminalParser::new(24, 80, 1000);
        parser.terminal_mut().set_color_background(GhosttyColorRgb {
            r: 0xF0,
            g: 0xE0,
            b: 0xD0,
        });
        parser.process(b"Hello, World!");
        let rs = make_render_state(&mut parser);
        let default_fg = parser
            .foreground_color_default()
            .or_else(|| parser.foreground_color())
            .unwrap_or(crate::terminal::Rgb::new(255, 255, 255));
        let default_bg = parser
            .background_color_default()
            .or_else(|| parser.background_color())
            .unwrap_or(crate::terminal::Rgb::new(0, 0, 0));

        terminal
            .draw(|f| {
                let widget =
                    TerminalWidget::new(&rs).default_colors(default_fg.into(), default_bg.into());
                f.render_widget(widget, f.area());
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(20, 0)].bg, Color::Rgb(0xF0, 0xE0, 0xD0));
        assert_eq!(buffer[(40, 12)].bg, Color::Rgb(0xF0, 0xE0, 0xD0));
    }

    #[test]
    fn panel_refresh_repaints_terminal_widget_background() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let color_cache = Arc::new(Mutex::new(HashMap::from([(
            257usize,
            crate::terminal::Rgb::new(0xF0, 0xE0, 0xD0),
        )])));
        let mut panel = TerminalPanel::new_with_color_cache(24, 80, Arc::clone(&color_cache));
        panel.connect("sess-0");
        panel.on_scrollback(b"");
        panel.on_output(b"Hello, World!");

        terminal
            .draw(|f| {
                let default_fg = panel
                    .foreground_color_default()
                    .or_else(|| panel.foreground_color())
                    .unwrap_or(crate::terminal::Rgb::new(255, 255, 255));
                let default_bg = panel
                    .background_color_default()
                    .or_else(|| panel.background_color())
                    .unwrap_or(crate::terminal::Rgb::new(0, 0, 0));
                let widget = TerminalWidget::new(panel.render_state())
                    .default_colors(default_fg.into(), default_bg.into());
                f.render_widget(widget, f.area());
            })
            .unwrap();

        let before = terminal.backend().buffer()[(40, 12)].bg;
        assert_eq!(before, Color::Rgb(0xF0, 0xE0, 0xD0));

        color_cache
            .lock()
            .expect("color cache lock")
            .insert(257usize, crate::terminal::Rgb::new(0x10, 0x0F, 0x0F));
        panel.refresh_color_cache();

        terminal
            .draw(|f| {
                let default_fg = panel
                    .foreground_color_default()
                    .or_else(|| panel.foreground_color())
                    .unwrap_or(crate::terminal::Rgb::new(255, 255, 255));
                let default_bg = panel
                    .background_color_default()
                    .or_else(|| panel.background_color())
                    .unwrap_or(crate::terminal::Rgb::new(0, 0, 0));
                let widget = TerminalWidget::new(panel.render_state())
                    .default_colors(default_fg.into(), default_bg.into());
                f.render_widget(widget, f.area());
            })
            .unwrap();

        let after = terminal.backend().buffer()[(40, 12)].bg;
        assert_eq!(after, Color::Rgb(0x10, 0x0F, 0x0F));
    }

    #[test]
    fn panel_refresh_repaints_palette_backed_cell_background() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut initial_colors = HashMap::new();
        for index in 0usize..256usize {
            initial_colors.insert(index, crate::terminal::Rgb::new(0, 0, 0));
        }
        initial_colors.insert(4usize, crate::terminal::Rgb::new(0xF0, 0xE0, 0xD0));
        let color_cache = Arc::new(Mutex::new(initial_colors));
        let mut panel = TerminalPanel::new_with_color_cache(24, 80, Arc::clone(&color_cache));
        panel.connect("sess-0");
        panel.on_scrollback(b"");
        panel.on_output(b"\x1b[44mX\x1b[0m");

        terminal
            .draw(|f| {
                let default_fg = panel
                    .foreground_color_default()
                    .or_else(|| panel.foreground_color())
                    .unwrap_or(crate::terminal::Rgb::new(255, 255, 255));
                let default_bg = panel
                    .background_color_default()
                    .or_else(|| panel.background_color())
                    .unwrap_or(crate::terminal::Rgb::new(0, 0, 0));
                let widget = TerminalWidget::new(panel.render_state())
                    .default_colors(default_fg.into(), default_bg.into());
                f.render_widget(widget, f.area());
            })
            .unwrap();

        let before = terminal.backend().buffer()[(0, 0)].bg;
        assert_eq!(before, Color::Rgb(0xF0, 0xE0, 0xD0));

        color_cache
            .lock()
            .expect("color cache lock")
            .insert(4usize, crate::terminal::Rgb::new(0x10, 0x0F, 0x0F));
        panel.refresh_color_cache();

        terminal
            .draw(|f| {
                let default_fg = panel
                    .foreground_color_default()
                    .or_else(|| panel.foreground_color())
                    .unwrap_or(crate::terminal::Rgb::new(255, 255, 255));
                let default_bg = panel
                    .background_color_default()
                    .or_else(|| panel.background_color())
                    .unwrap_or(crate::terminal::Rgb::new(0, 0, 0));
                let widget = TerminalWidget::new(panel.render_state())
                    .default_colors(default_fg.into(), default_bg.into());
                f.render_widget(widget, f.area());
            })
            .unwrap();

        let after = terminal.backend().buffer()[(0, 0)].bg;
        assert_eq!(after, Color::Rgb(0x10, 0x0F, 0x0F));
    }

    #[test]
    fn test_render_with_bold() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut parser = TerminalParser::new(24, 80, 1000);
        parser.process(b"\x1b[1mBold\x1b[0m");
        let rs = make_render_state(&mut parser);

        terminal
            .draw(|f| {
                let widget = TerminalWidget::new(&rs);
                f.render_widget(widget, f.area());
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert!(buffer[(0, 0)].modifier.contains(Modifier::BOLD));
    }
}
