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

use std::sync::{Arc, Mutex};

use anyhow::Result;
use ratatui::{
    backend::{Backend, TestBackend},
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Widget, Wrap},
    Frame, Terminal,
};
use vt100::Parser;

use crate::app::buffer_to_ansi;

use super::render_tree::{
    InputProps, ListItemProps, ListProps, ParagraphAlignment, ParagraphProps, SpanStyle,
    StyledContent,
};
use crate::compat::{BrowserDimensions, VpnStatus};

use std::collections::HashMap;

/// Information about an agent for rendering.
///
/// Extracted subset of Agent data needed for TUI display.
#[derive(Debug, Clone)]
pub struct AgentRenderInfo {
    /// Unique key for this agent (e.g., "repo-42").
    pub key: String,
    /// Display name for agent list (e.g., "refactor-tui-2"). Computed by Lua.
    pub display_name: Option<String>,
    /// Repository name.
    pub repo: String,
    /// Issue number (if issue-based agent).
    pub issue_number: Option<u32>,
    /// Branch name.
    pub branch_name: String,
    /// HTTP forwarding port if assigned.
    pub port: Option<u16>,
    /// Whether the server is running.
    pub server_running: bool,
    /// Ordered session names (e.g., ["agent", "server", "watcher"]).
    /// Empty if sessions info not available (backward compat).
    pub session_names: Vec<String>,
}

/// Context required for rendering the TUI.
///
/// `TuiRunner` builds this struct from its internal state and passes it to
/// the render function. This creates a clear interface between the runner
/// and the renderer, making dependencies explicit.
pub struct RenderContext<'a> {
    // === UI State ===
    /// Current UI mode string (e.g., "normal", "menu").
    pub mode: String,
    /// Currently selected overlay list item index.
    pub list_selected: usize,
    /// Text input buffer for text entry modes.
    pub input_buffer: &'a str,
    /// Available worktrees for agent creation (path, branch).
    pub available_worktrees: &'a [(String, String)],
    /// Error message to display in Error mode.
    pub error_message: Option<&'a str>,
    /// Generic key-value store for pending operations (e.g., creating_agent_id, creating_agent_stage).
    pub pending_fields: &'a HashMap<String, String>,
    /// Connection code data (URL + QR ASCII) for display.
    pub connection_code: Option<&'a super::qr::ConnectionCodeData>,
    /// Whether the connection bundle has been used.
    pub bundle_used: bool,

    // === Agent State ===
    /// Ordered list of agent IDs.
    pub agent_ids: &'a [String],
    /// Agent information for display.
    pub agents: &'a [AgentRenderInfo],
    /// Currently selected agent index.
    pub selected_agent_index: usize,

    // === Terminal State ===
    /// The VT100 parser for the currently selected agent's active PTY.
    /// This is the parser that should be rendered in the terminal area.
    pub active_parser: Option<Arc<Mutex<Parser>>>,
    /// Pool of VT100 parsers keyed by `(agent_index, pty_index)`.
    /// Used by terminal widgets with explicit PTY bindings.
    pub parser_pool: &'a std::collections::HashMap<(usize, usize), Arc<Mutex<Parser>>>,
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
            .field("mode", &self.mode)
            .field("list_selected", &self.list_selected)
            .field("selected_agent_index", &self.selected_agent_index)
            .field("agents_count", &self.agents.len())
            .field("has_active_parser", &self.active_parser.is_some())
            .field("active_pty_index", &self.active_pty_index)
            .field("scroll_offset", &self.scroll_offset)
            .field("is_scrolled", &self.is_scrolled)
            .finish_non_exhaustive()
    }
}

impl<'a> RenderContext<'a> {
    /// Get the currently selected agent info.
    #[must_use]
    pub fn selected_agent(&self) -> Option<&AgentRenderInfo> {
        self.agents.get(self.selected_agent_index)
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
/// Builds a minimal layout using generic primitives. When Lua is
/// working, `interpret_tree()` handles rendering instead.
fn render_frame(f: &mut Frame, ctx: &RenderContext) {
    let frame_area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(20), Constraint::Percentage(80)].as_ref())
        .split(frame_area);

    // Build agent list as generic ListProps
    let mut items: Vec<ListItemProps> = Vec::new();
    if let (Some(identifier), Some(stage)) = (
        ctx.pending_fields.get("creating_agent_id"),
        ctx.pending_fields.get("creating_agent_stage"),
    ) {
        let stage_label = match stage.as_str() {
            "creating_worktree" => "Creating worktree...",
            "copying_config" => "Copying config...",
            "spawning_agent" => "Starting agent...",
            "ready" => "Ready",
            other => other,
        };
        items.push(ListItemProps {
            content: StyledContent::Plain(format!("-> {} ({})", identifier, stage_label)),
            header: false,
            style: Some(SpanStyle {
                fg: Some(super::render_tree::SpanColor::Cyan),
                ..SpanStyle::default()
            }),
            action: None,
        });
    }
    for agent in ctx.agents {
        let base_text = agent.display_name.as_deref().unwrap_or(&agent.branch_name);
        let server_info = if let Some(p) = agent.port {
            let icon = if agent.server_running { ">" } else { "o" };
            format!(" {}:{}", icon, p)
        } else {
            String::new()
        };
        items.push(ListItemProps {
            content: StyledContent::Plain(format!("{}{}", base_text, server_info)),
            header: false,
            style: None,
            action: None,
        });
    }

    let creating_offset = if ctx.pending_fields.contains_key("creating_agent_id") { 1 } else { 0 };
    let selected = ctx.selected_agent_index + creating_offset;

    let list_props = ListProps {
        items,
        selected: Some(selected),
        highlight_style: None,
        highlight_symbol: None,
    };

    let poll_status = if ctx.seconds_since_poll < 1 { "*" } else { "o" };
    let agent_title = format!(" Agents ({}) {} ", ctx.agents.len(), poll_status);
    let agent_block = Block::default().borders(Borders::ALL).title(agent_title);
    render_list_widget(f, chunks[0], agent_block, &list_props);

    // Render terminal view
    let term_title = if let Some(agent) = ctx.selected_agent() {
        format!(" {} [AGENT] ", agent.branch_name)
    } else {
        " Terminal [No agent selected] ".to_string()
    };
    let term_block = Block::default().borders(Borders::ALL).title(term_title);
    render_terminal_panel(f, ctx, chunks[1], term_block, None);
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
    // Resolve which parser to use: explicit binding → pool lookup, else active parser
    let (agent_idx, pty_idx) = if let Some(b) = binding {
        (
            b.agent_index.unwrap_or(ctx.selected_agent_index),
            b.pty_index.unwrap_or(ctx.active_pty_index),
        )
    } else {
        (ctx.selected_agent_index, ctx.active_pty_index)
    };

    let parser = if binding.is_some() {
        ctx.parser_pool.get(&(agent_idx, pty_idx)).cloned()
    } else {
        ctx.active_parser.clone()
    };

    // Record the inner area (minus borders) so the runner can resize parsers/PTYs
    let inner = block.inner(area);
    if inner.width > 0 && inner.height > 0 {
        ctx.terminal_areas
            .borrow_mut()
            .insert((agent_idx, pty_idx), (inner.height, inner.width));
    }

    if let Some(ref parser) = parser {
        let parser_lock = parser.lock().expect("parser lock not poisoned");
        let screen = parser_lock.screen();

        let widget = crate::TerminalWidget::new(screen).block(block);
        let widget = if binding.is_none() && ctx.is_scrolled {
            widget.hide_cursor()
        } else {
            widget
        };

        widget.render(area, f.buffer_mut());
    } else {
        f.render_widget(block, area);
    }
}

// === Generic Widget Renderers ===
//
// These render generic primitives with zero application knowledge.
// All content, styling, and behavior comes from Lua via props.

/// Render a generic list widget with optional selection and headers.
///
/// Headers are non-selectable items rendered dim+bold. The `selected` index
/// in `ListProps` counts only selectable items; this function maps it to
/// an absolute index accounting for headers.
pub(super) fn render_list_widget(f: &mut Frame, area: Rect, block: Block, props: &ListProps) {
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
            let line = item.content.to_line();
            let li = if let Some(ref style) = item.style {
                ListItem::new(line).style(style.to_ratatui_style())
            } else {
                ListItem::new(line)
            };
            list_items.push(li);
        }
    }

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

    let mut state = ListState::default();
    if let Some(sel) = props.selected {
        // Map selectable index to absolute index (past headers)
        let abs = selectable_to_absolute
            .get(sel)
            .copied()
            .unwrap_or(sel.min(props.items.len().saturating_sub(1)));
        state.select(Some(abs));
    }

    f.render_stateful_widget(list, area, &mut state);
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
pub(super) fn render_input_widget(f: &mut Frame, area: Rect, block: Block, props: &InputProps) {
    let mut lines: Vec<Line> = props.lines.iter().map(StyledContent::to_line).collect();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::raw(props.value.clone())));

    let alignment = match props.alignment {
        ParagraphAlignment::Left => Alignment::Left,
        ParagraphAlignment::Center => Alignment::Center,
        ParagraphAlignment::Right => Alignment::Right,
    };

    let widget = Paragraph::new(lines).block(block).alignment(alignment);

    f.render_widget(widget, area);
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
    fn test_render_context_selected_agent() {
        let agents = vec![
            AgentRenderInfo {
                key: "test-1".to_string(),
                display_name: None,
                repo: "test/repo".to_string(),
                issue_number: Some(1),
                branch_name: "botster-issue-1".to_string(),
                port: None,
                server_running: false,
                session_names: vec!["agent".to_string()],
            },
            AgentRenderInfo {
                key: "test-2".to_string(),
                display_name: None,
                repo: "test/repo".to_string(),
                issue_number: Some(2),
                branch_name: "botster-issue-2".to_string(),
                port: Some(3000),
                server_running: true,
                session_names: vec!["agent".to_string(), "server".to_string()],
            },
        ];

        let ctx = RenderContext {
            mode: "normal".to_string(),
            list_selected: 0,
            input_buffer: "",
            available_worktrees: &[],
            error_message: None,
            pending_fields: &std::collections::HashMap::new(),
            connection_code: None,
            bundle_used: false,
            agent_ids: &[],
            agents: &agents,
            selected_agent_index: 1,
            active_parser: None,
            parser_pool: &std::collections::HashMap::new(),
            active_pty_index: 0,
            scroll_offset: 0,
            is_scrolled: false,
            seconds_since_poll: 0,
            poll_interval: 10,
            vpn_status: None,
            terminal_cols: 80,
            terminal_rows: 24,
            terminal_areas: std::cell::RefCell::new(std::collections::HashMap::new()),
        };

        let selected = ctx.selected_agent();
        assert!(selected.is_some());
        assert_eq!(selected.unwrap().key, "test-2");
        assert_eq!(selected.unwrap().issue_number, Some(2));
    }

    #[test]
    fn test_render_result_default() {
        let result = RenderResult::default();
        assert!(result.ansi_output.is_empty());
        assert_eq!(result.rows, 0);
        assert_eq!(result.cols, 0);
    }
}
