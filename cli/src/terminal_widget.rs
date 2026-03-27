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

use crate::ghostty_vt::{
    self, GhosttyColorRgb, GhosttyCellWide, GhosttyRenderStateCursorVisualStyle, RenderState,
};

/// Configuration for cursor rendering.
#[derive(Debug, Clone)]
pub struct CursorConfig {
    /// Whether to render the cursor.
    pub show: bool,
    /// Character to render at cursor position.
    pub symbol: String,
    /// Ratatui style for the cursor cell.
    pub style: Style,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            show: true,
            symbol: "\u{2588}".to_string(),
            style: Style::default().add_modifier(Modifier::REVERSED),
        }
    }
}

/// A widget that renders a ghostty terminal to a ratatui buffer.
pub struct TerminalWidget<'a> {
    render_state: &'a RenderState,
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
    /// Create a new terminal widget from a render state snapshot.
    ///
    /// The render state must have been updated from the terminal before
    /// creating this widget.
    pub fn new(render_state: &'a RenderState) -> Self {
        Self {
            render_state,
            block: None,
            cursor: CursorConfig::default(),
        }
    }

    /// Set the surrounding block decoration.
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// Override cursor rendering configuration.
    pub fn cursor(mut self, cursor: CursorConfig) -> Self {
        self.cursor = cursor;
        self
    }

    /// Disable cursor rendering.
    pub fn hide_cursor(mut self) -> Self {
        self.cursor.show = false;
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

        let default_fg = self.render_state.foreground_color();
        let default_bg = self.render_state.background_color();

        if let Ok(mut iter) = self.render_state.iterator() {
            render_grid(&mut iter, inner_area, buf, default_fg, default_bg);
        }

        if self.cursor.show {
            render_cursor(self.render_state, inner_area, buf, &self.cursor);
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

fn render_cursor(
    rs: &RenderState,
    area: Rect,
    buf: &mut Buffer,
    cursor_config: &CursorConfig,
) {
    if !rs.cursor_visible() || !rs.cursor_in_viewport() {
        return;
    }

    let (cursor_x, cursor_y) = rs.cursor_viewport_position();
    let visual_style = rs.cursor_visual_style();

    let buf_x = area.x + cursor_x;
    let buf_y = area.y + cursor_y;

    if buf_x >= area.x + area.width || buf_y >= area.y + area.height {
        return;
    }

    let buf_cell = &mut buf[(buf_x, buf_y)];
    let has_char = {
        let s = buf_cell.symbol();
        !s.is_empty() && s != " "
    };

    match visual_style {
        GhosttyRenderStateCursorVisualStyle::Block => {
            if has_char {
                buf_cell.set_style(cursor_config.style.add_modifier(Modifier::REVERSED));
            } else {
                buf_cell.set_symbol(&cursor_config.symbol);
                buf_cell.set_style(cursor_config.style);
            }
        }
        GhosttyRenderStateCursorVisualStyle::Bar => {
            buf_cell.set_symbol("\u{258e}");
            buf_cell.set_style(cursor_config.style);
        }
        GhosttyRenderStateCursorVisualStyle::Underline => {
            if has_char {
                buf_cell.set_style(cursor_config.style.add_modifier(Modifier::UNDERLINED));
            } else {
                buf_cell.set_symbol("_");
                buf_cell.set_style(cursor_config.style);
            }
        }
        GhosttyRenderStateCursorVisualStyle::BlockHollow => {
            if has_char {
                buf_cell.set_style(cursor_config.style.add_modifier(Modifier::REVERSED));
            } else {
                buf_cell.set_symbol(&cursor_config.symbol);
                buf_cell.set_style(cursor_config.style);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::TerminalParser;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn make_render_state(parser: &mut TerminalParser) -> RenderState {
        let mut rs = RenderState::new().expect("render state creation");
        rs.update(parser.terminal_mut()).expect("render state update");
        rs
    }

    #[test]
    fn test_terminal_widget_creation() {
        let mut parser = TerminalParser::new(24, 80, 1000);
        let rs = make_render_state(&mut parser);
        let widget = TerminalWidget::new(&rs);
        assert!(widget.cursor.show);
    }

    #[test]
    fn test_terminal_widget_hide_cursor() {
        let mut parser = TerminalParser::new(24, 80, 1000);
        let rs = make_render_state(&mut parser);
        let widget = TerminalWidget::new(&rs).hide_cursor();
        assert!(!widget.cursor.show);
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
