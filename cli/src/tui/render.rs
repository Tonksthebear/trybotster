//! TUI rendering functions.
//!
//! This module provides the main rendering function for the botster TUI.
//! It renders TuiRunner state to a terminal and optionally produces ANSI output
//! for browser streaming.
//!
//! # Architecture
//!
//! Rendering is decoupled from TuiRunner via `RenderContext`:
//!
//! ```text
//! TuiRunner ──builds──> RenderContext ──passed to──> render()
//! ```
//!
//! This separation ensures:
//! - Clear contract for what rendering needs
//! - Testable rendering logic
//! - No tight coupling to TuiRunner internals

// Rust guideline compliant 2026-01

use anyhow::Result;
use ratatui::{
    backend::{Backend, TestBackend},
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState, StatefulWidget, Widget, Wrap,
    },
    Frame, Terminal,
};

use crate::app::buffer_to_ansi;

use super::render_tree::{
    InputProps, ListProps, ParagraphAlignment, ParagraphProps, SpanStyle,
    StyledContent,
};
use super::widget_state::WidgetStateStore;
use crate::compat::{BrowserDimensions, VpnStatus};

/// Context required for rendering the TUI.
///
/// `TuiRunner` builds this struct from its internal state and passes it to
/// the render function. This creates a clear interface between the runner
/// and the renderer, making dependencies explicit.
///
/// Application state (agents, pending_fields, worktrees) lives in Lua's
/// `_tui_state` global — the TUI is a client like the browser. RenderContext
/// only carries terminal/rendering primitives that Rust owns.
pub struct RenderContext<'a> {
    // === UI State ===
    // mode lives in Lua's _tui_state; widget state (selection, input)
    // is owned by WidgetStateStore, synced back to Lua for workflow actions
    /// Error message to display in Error mode.
    pub error_message: Option<&'a str>,
    /// Connection code data (URL + QR ASCII) for display.
    pub connection_code: Option<&'a super::qr::ConnectionCodeData>,
    /// Whether the connection bundle has been used.
    pub bundle_used: bool,

    // === Terminal State ===
    /// Terminal panels keyed by `(agent_index, pty_index)`.
    /// Used by terminal widgets with explicit PTY bindings.
    pub panels: &'a std::collections::HashMap<(usize, usize), super::terminal_panel::TerminalPanel>,
    /// Index of the currently active PTY session (0 = first session).
    pub active_pty_index: usize,
    /// Current scroll offset for the active PTY.
    pub scroll_offset: usize,
    /// Whether the active PTY is scrolled (not at bottom).
    pub is_scrolled: bool,

    // === Status Indicators ===
    /// Seconds since last poll.
    pub seconds_since_poll: u64,
    /// Poll interval in seconds.
    pub poll_interval: u64,
    /// VPN connection status.
    pub vpn_status: Option<VpnStatus>,

    // === Terminal Dimensions ===
    /// Terminal width in columns.
    pub terminal_cols: u16,
    /// Terminal height in rows.
    pub terminal_rows: u16,

    // === Widget Area Tracking ===
    /// Actual rendered area (rows, cols) of each terminal widget, keyed by (agent_index, pty_index).
    /// Populated during rendering so the runner can resize parsers and PTYs to match.
    pub terminal_areas: std::cell::RefCell<std::collections::HashMap<(usize, usize), (u16, u16)>>,
}

impl<'a> std::fmt::Debug for RenderContext<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderContext")
            .field("active_pty_index", &self.active_pty_index)
            .field("scroll_offset", &self.scroll_offset)
            .field("is_scrolled", &self.is_scrolled)
            .finish_non_exhaustive()
    }
}

/// Render result containing ANSI output for browser streaming.
#[derive(Debug, Default)]
pub struct RenderResult {
    /// ANSI output for browser streaming.
    pub ansi_output: String,
    /// Number of rows in the output.
    pub rows: u16,
    /// Number of columns in the output.
    pub cols: u16,
}

/// Render the TUI and return ANSI output for browser streaming.
///
/// Returns `RenderResult` containing ANSI output and metadata for browser streaming.
/// If `browser_dims` is provided, renders at those dimensions for proper layout.
///
/// # Arguments
///
/// * `terminal` - The ratatui terminal to render to
/// * `ctx` - Render context containing all state needed for display
/// * `browser_dims` - Optional browser dimensions for virtual terminal rendering
///
/// # Returns
///
/// A `RenderResult` with ANSI output.
pub fn render<B>(
    terminal: &mut Terminal<B>,
    ctx: &RenderContext,
    browser_dims: Option<BrowserDimensions>,
) -> Result<RenderResult>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    // Helper to render UI to a frame
    let render_ui = |f: &mut Frame| { render_frame(f, ctx) };

    // Always render to real terminal for local display
    terminal.draw(render_ui)?;

    // For browser streaming, render to browser-sized buffer if dimensions provided
    let (ansi_output, out_rows, out_cols) = if let Some(dims) = browser_dims {
        // Create a virtual terminal at browser dimensions
        let backend = TestBackend::new(dims.cols, dims.rows);
        let mut virtual_terminal = Terminal::new(backend)?;

        // Render to virtual terminal at browser dimensions
        let completed_frame = virtual_terminal.draw(|f| {
            // Log once when dimensions change
            static LAST_AREA: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let area = f.area();
            let combined = (u32::from(area.width) << 16) | u32::from(area.height);
            let last = LAST_AREA.swap(combined, std::sync::atomic::Ordering::Relaxed);
            if last != combined {
                log::info!(
                    "Virtual terminal rendering at {}x{}",
                    area.width,
                    area.height
                );
            }
            render_ui(f);
        })?;

        // Convert virtual buffer to ANSI
        let ansi = buffer_to_ansi(
            completed_frame.buffer,
            dims.cols,
            dims.rows,
            None, // No clipping needed, already at correct size
            None,
        );
        (ansi, dims.rows, dims.cols)
    } else {
        // No browser connected, return empty output
        (String::new(), 0, 0)
    };

    Ok(RenderResult {
        ansi_output,
        rows: out_rows,
        cols: out_cols,
    })
}

/// Render the full TUI frame (fallback when Lua layout is unavailable).
///
/// Minimal layout — agent data lives in Lua's `_tui_state`, so this
/// fallback can only show the terminal panel. When Lua is working,
/// `interpret_tree()` handles full rendering instead.
fn render_frame(f: &mut Frame, ctx: &RenderContext) {
    let frame_area = f.area();
    let term_block = Block::default()
        .borders(Borders::ALL)
        .title(" Terminal [Lua layout unavailable] ");
    render_terminal_panel(f, ctx, frame_area, term_block, None);
}

/// Render the terminal panel showing PTY output.
///
/// The `block` parameter provides the pre-built block with title from Lua
/// (or the fallback). The optional `binding` specifies which PTY to render;
/// if `None`, renders the currently active PTY.
pub(super) fn render_terminal_panel(
    f: &mut Frame,
    ctx: &RenderContext,
    area: Rect,
    block: Block,
    binding: Option<&super::render_tree::TerminalBinding>,
) {
    // Resolve which panel to use: explicit binding → pool lookup, else focused PTY
    let (agent_idx, pty_idx) = if let Some(b) = binding {
        (
            b.agent_index.unwrap_or(0),
            b.pty_index.unwrap_or(ctx.active_pty_index),
        )
    } else {
        // Fallback: no binding — only used by Rust fallback renderer (Lua is broken)
        (0, ctx.active_pty_index)
    };

    // Record the inner area (minus borders) so the runner can resize parsers/PTYs
    let inner = block.inner(area);
    if inner.width > 0 && inner.height > 0 {
        ctx.terminal_areas
            .borrow_mut()
            .insert((agent_idx, pty_idx), (inner.height, inner.width));
    }

    let panel = ctx.panels.get(&(agent_idx, pty_idx));

    if let Some(panel) = panel {
        let scroll_offset = panel.scroll_offset();
        let scrollback_depth = panel.scrollback_depth();
        let is_scrolled = panel.is_scrolled();

        let screen = panel.screen();
        let widget = crate::TerminalWidget::new(screen).block(block);
        let widget = if is_scrolled {
            widget.hide_cursor()
        } else {
            widget
        };

        widget.render(area, f.buffer_mut());

        // Scrollbar overlay when scrolled — uses panel's pre-computed
        // scrollback_depth, no mid-render mutations.
        if is_scrolled && scrollback_depth > 0 {
            let content_length = scrollback_depth + inner.height as usize;
            // scroll_offset=max means top of history; position=0 means scrollbar at top
            let position =
                content_length.saturating_sub(scroll_offset + inner.height as usize);
            let mut scrollbar_state =
                ScrollbarState::new(content_length).position(position);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            scrollbar.render(inner, f.buffer_mut(), &mut scrollbar_state);
        }
    } else {
        f.render_widget(block, area);
    }
}

// === Generic Widget Renderers ===
//
// These render generic primitives with zero application knowledge.
// All content, styling, and behavior comes from Lua via props.

/// Render a generic list widget with selection and headers.
///
/// Headers are non-selectable items rendered dim+bold. Selection is resolved
/// from either:
/// - **Controlled**: `ListProps.selected` (Lua owns state)
/// - **Uncontrolled**: `WidgetStateStore` keyed by `widget_id` (Rust owns state)
///
/// The selectable item count is synced to the widget state store each frame
/// for bounds clamping.
pub(super) fn render_list_widget(
    f: &mut Frame,
    area: Rect,
    block: Block,
    props: &ListProps,
    widget_id: Option<&str>,
    widget_states: &mut WidgetStateStore,
) {
    let mut list_items: Vec<ListItem> = Vec::new();
    let mut selectable_to_absolute: Vec<usize> = Vec::new();

    for (i, item) in props.items.iter().enumerate() {
        if item.header {
            // Headers: dim + bold, non-selectable
            let line = item.content.to_line();
            let li = ListItem::new(line).style(
                Style::default()
                    .add_modifier(Modifier::DIM)
                    .add_modifier(Modifier::BOLD),
            );
            list_items.push(li);
        } else {
            selectable_to_absolute.push(i);
            let primary = item.content.to_line();
            let li = if let Some(ref secondary) = item.secondary {
                let secondary_line = secondary
                    .to_line()
                    .style(Style::default().add_modifier(Modifier::DIM));
                ListItem::new(Text::from(vec![primary, secondary_line]))
            } else {
                ListItem::new(primary)
            };
            let li = if let Some(ref style) = item.style {
                li.style(style.to_ratatui_style())
            } else {
                li
            };
            list_items.push(li);
        }
    }

    let selectable_count = selectable_to_absolute.len();

    let highlight_style = props
        .highlight_style
        .as_ref()
        .map(SpanStyle::to_ratatui_style)
        .unwrap_or_else(|| Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED));

    let highlight_symbol = props.highlight_symbol.as_deref().unwrap_or("> ");

    let list = List::new(list_items)
        .block(block)
        .highlight_style(highlight_style)
        .highlight_symbol(highlight_symbol);

    // Resolve selection: controlled (props.selected) vs uncontrolled (widget state)
    if let Some(sel) = props.selected {
        // Controlled: Lua owns selection
        let abs = selectable_to_absolute
            .get(sel)
            .copied()
            .unwrap_or(sel.min(props.items.len().saturating_sub(1)));
        let mut state = ListState::default();
        state.select(Some(abs));
        f.render_stateful_widget(list, area, &mut state);
    } else if let Some(id) = widget_id {
        // Uncontrolled: Rust owns selection via WidgetStateStore
        let ws = widget_states.list_state(id);
        ws.set_selectable_count(selectable_count);
        let sel = ws.selected();
        let abs = selectable_to_absolute
            .get(sel)
            .copied()
            .unwrap_or(sel.min(props.items.len().saturating_sub(1)));
        let rstate = ws.ratatui_state_mut();
        rstate.select(Some(abs));
        f.render_stateful_widget(list, area, rstate);
    } else {
        // No selection at all
        let mut state = ListState::default();
        f.render_stateful_widget(list, area, &mut state);
    }
}

/// Render a paragraph widget with styled lines, alignment, and optional wrapping.
pub(super) fn render_paragraph_widget(
    f: &mut Frame,
    area: Rect,
    block: Block,
    props: &ParagraphProps,
) {
    let lines: Vec<Line> = props.lines.iter().map(StyledContent::to_line).collect();

    let alignment = match props.alignment {
        ParagraphAlignment::Left => Alignment::Left,
        ParagraphAlignment::Center => Alignment::Center,
        ParagraphAlignment::Right => Alignment::Right,
    };

    let mut widget = Paragraph::new(lines).block(block).alignment(alignment);

    if props.wrap {
        widget = widget.wrap(Wrap { trim: false });
    }

    f.render_widget(widget, area);
}

/// Render a text input widget with prompt lines and current value.
///
/// Supports controlled and uncontrolled modes:
/// - **Controlled**: `InputProps.value` is `Some` — renders that value directly.
/// - **Uncontrolled**: `InputProps.value` is `None` — reads from `WidgetStateStore`,
///   renders with cursor position and visual scrolling.
pub(super) fn render_input_widget(
    f: &mut Frame,
    area: Rect,
    block: Block,
    props: &InputProps,
    widget_id: Option<&str>,
    widget_states: &mut WidgetStateStore,
) {
    let inner = block.inner(area);
    let mut lines: Vec<Line> = props.lines.iter().map(StyledContent::to_line).collect();
    lines.push(Line::from(""));

    let alignment = match props.alignment {
        ParagraphAlignment::Left => Alignment::Left,
        ParagraphAlignment::Center => Alignment::Center,
        ParagraphAlignment::Right => Alignment::Right,
    };

    if let Some(ref value) = props.value {
        // Controlled: Lua owns value
        lines.push(Line::from(Span::raw(value.clone())));
        let widget = Paragraph::new(lines).block(block).alignment(alignment);
        f.render_widget(widget, area);
    } else if let Some(id) = widget_id {
        // Uncontrolled: Rust owns value via WidgetStateStore + tui-input
        let ws = widget_states.input_state(id);
        let input_width = inner.width.saturating_sub(1) as usize; // Leave room for cursor
        let scroll = ws.visual_scroll(input_width);
        let value = ws.value();

        if value.is_empty() {
            if let Some(ref placeholder) = props.placeholder {
                lines.push(Line::from(Span::styled(
                    placeholder.clone(),
                    Style::default().add_modifier(Modifier::DIM),
                )));
            } else {
                lines.push(Line::from(""));
            }
        } else {
            lines.push(Line::from(Span::raw(value.to_string())));
        }

        let widget = Paragraph::new(lines)
            .block(block)
            .alignment(alignment)
            .scroll((0, scroll as u16));
        f.render_widget(widget, area);

        // Place cursor at correct position within the input area
        let prompt_lines = props.lines.len() + 1; // +1 for the blank line
        let cursor_y = inner.y + prompt_lines as u16;
        let cursor_x = inner.x + (ws.visual_cursor().saturating_sub(scroll)) as u16;
        if cursor_y < inner.y + inner.height {
            f.set_cursor_position((cursor_x, cursor_y));
        }
    } else {
        // No state: render empty
        lines.push(Line::from(""));
        let widget = Paragraph::new(lines).block(block).alignment(alignment);
        f.render_widget(widget, area);
    }
}

/// Render connection code / QR display into a given area.
///
/// When `custom_lines` is `Some`, uses those for header/footer text around the
/// QR code. Expected format: first line = header, second line = footer. Falls
/// back to hardcoded text when `None`.
pub(super) fn render_connection_code_widget(
    f: &mut Frame,
    ctx: &RenderContext,
    area: Rect,
    block: Block,
    custom_lines: Option<&[StyledContent]>,
) {
    let qr_lines: Vec<String> = ctx
        .connection_code
        .map(|c| c.qr_ascii.clone())
        .unwrap_or_else(|| vec!["Error: No connection code".to_string()]);

    // Custom lines format: [header, used_header, footer]
    let (header, footer) = if let Some(lines) = custom_lines {
        let h = if ctx.bundle_used {
            lines.get(1).map(StyledContent::to_line)
                .unwrap_or_else(|| Line::from("Link used - [r] to pair new device"))
        } else {
            lines.first().map(StyledContent::to_line)
                .unwrap_or_else(|| Line::from("Scan QR to connect securely"))
        };
        let f = lines.last().map(StyledContent::to_line)
            .unwrap_or_else(|| Line::from("[r] new link  [c] copy  [Esc] close"));
        (h, f)
    } else {
        let h = if ctx.bundle_used {
            Line::from("Link used - [r] to pair new device")
        } else {
            Line::from("Scan QR to connect securely")
        };
        (h, Line::from("[r] new link  [c] copy  [Esc] close"))
    };

    let mut text_lines = vec![header, Line::from("")];

    for qr_line in &qr_lines {
        text_lines.push(Line::from(qr_line.clone()));
    }

    text_lines.push(Line::from(""));
    text_lines.push(footer);

    let widget = Paragraph::new(text_lines)
        .block(block)
        .alignment(Alignment::Center);

    f.render_widget(widget, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_result_default() {
        let result = RenderResult::default();
        assert!(result.ansi_output.is_empty());
        assert_eq!(result.rows, 0);
        assert_eq!(result.cols, 0);
    }
}
