//! TUI rendering functions.
//!
//! This module provides the main rendering function for the botster-hub TUI.
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
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Widget, Wrap},
    Frame, Terminal,
};
use vt100::Parser;

use crate::app::{buffer_to_ansi, centered_rect, AppMode};

use super::menu::{build_menu, MenuContext};
use super::render_tree::StyledContent;
use crate::compat::{BrowserDimensions, VpnStatus};

use super::events::CreationStage;

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
    /// Current application mode (Normal, Menu, etc.).
    pub mode: AppMode,
    /// Currently selected menu item index.
    pub menu_selected: usize,
    /// Text input buffer for text entry modes.
    pub input_buffer: &'a str,
    /// Currently selected worktree index in selection modal.
    pub worktree_selected: usize,
    /// Available worktrees for agent creation (path, branch).
    pub available_worktrees: &'a [(String, String)],
    /// Error message to display in Error mode.
    pub error_message: Option<&'a str>,
    /// Agent creation progress (identifier, stage).
    pub creating_agent: Option<(&'a str, CreationStage)>,
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
            .field("menu_selected", &self.menu_selected)
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

    /// Build menu context from current state.
    #[must_use]
    pub fn menu_context(&self) -> MenuContext {
        let session_count = self
            .selected_agent()
            .map(|a| a.session_names.len())
            .unwrap_or(0);
        MenuContext {
            has_agent: self.selected_agent().is_some(),
            active_pty_index: self.active_pty_index,
            session_count,
        }
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

/// Render the full TUI frame.
///
/// Internal function that does the actual rendering work.
fn render_frame(f: &mut Frame, ctx: &RenderContext) {
    let frame_area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(20), Constraint::Percentage(80)].as_ref())
        .split(frame_area);

    // Log frame and chunk sizes once for debugging
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        let block = Block::default().borders(Borders::ALL);
        let inner = block.inner(chunks[1]);
        log::info!(
            "Render areas - Frame: {}x{}, Right chunk: {}x{}, Inner (visible): {}x{}",
            frame_area.width,
            frame_area.height,
            chunks[1].width,
            chunks[1].height,
            inner.width,
            inner.height
        );
    }

    // Render agent list with fallback title
    let agent_block = {
        let poll_status = if ctx.seconds_since_poll < 1 { "*" } else { "o" };
        let agent_title = format!(" Agents ({}) {} ", ctx.agents.len(), poll_status);
        Block::default().borders(Borders::ALL).title(agent_title)
    };
    render_agent_list(f, ctx, chunks[0], agent_block);

    // Render terminal view with fallback title
    let term_block = {
        let title = if let Some(agent) = ctx.selected_agent() {
            format!(" {} [AGENT] ", agent.branch_name)
        } else {
            " Terminal [No agent selected] ".to_string()
        };
        Block::default().borders(Borders::ALL).title(title)
    };
    render_terminal_panel(f, ctx, chunks[1], term_block, None);

    // Render modal overlays based on mode (using area-based widget functions)
    let modal_params: Option<(u16, u16, &str)> = match ctx.mode {
        AppMode::Menu => Some((50, 40, "menu")),
        AppMode::NewAgentSelectWorktree => Some((70, 50, "worktree_select")),
        AppMode::NewAgentCreateWorktree => Some((60, 30, "text_input")),
        AppMode::NewAgentPrompt => Some((60, 20, "text_input")),
        AppMode::CloseAgentConfirm => Some((50, 20, "close_confirm")),
        AppMode::ConnectionCode => Some((70, 80, "connection_code")),
        AppMode::Error => Some((60, 30, "error")),
        AppMode::Normal => None,
    };

    if let Some((width_pct, height_pct, widget)) = modal_params {
        let area = centered_rect(width_pct, height_pct, f.area());
        f.render_widget(Clear, area);
        let block = Block::default().borders(Borders::ALL);
        match widget {
            "menu" => render_menu_widget(f, ctx, area, block.title(" Menu [Up/Down navigate | Enter select | Esc cancel] ")),
            "worktree_select" => render_worktree_select_widget(f, ctx, area, block.title(" Select Worktree [Up/Down navigate | Enter select | Esc cancel] ")),
            "text_input" => {
                let title = if ctx.mode == AppMode::NewAgentCreateWorktree {
                    " Create Worktree [Enter confirm | Esc cancel] "
                } else {
                    " Agent Prompt [Enter confirm | Esc cancel] "
                };
                render_text_input_widget(f, ctx, area, block.title(title), None);
            }
            "close_confirm" => render_close_confirm_widget(f, area, block.title(" Confirm Close "), None),
            "connection_code" => render_connection_code_widget(f, ctx, area, block.title(" Secure Connection "), None),
            "error" => render_error_widget(f, ctx, area, block.title(" Error "), None),
            _ => {}
        }
    }
}

/// Render the agent list panel.
///
/// The `block` parameter provides the pre-built block with title from Lua
/// (or the fallback). This function handles list items and selection state.
pub(super) fn render_agent_list(f: &mut Frame, ctx: &RenderContext, area: Rect, block: Block) {
    let mut items: Vec<ListItem> = Vec::new();

    // Add creating indicator at top if agent creation is in progress
    if let Some((identifier, stage)) = &ctx.creating_agent {
        let stage_label = match stage {
            CreationStage::CreatingWorktree => "Creating worktree...",
            CreationStage::CopyingConfig => "Copying config...",
            CreationStage::SpawningAgent => "Starting agent...",
            CreationStage::Ready => "Ready",
        };
        let creating_text = format!("-> {} ({})", identifier, stage_label);
        items.push(
            ListItem::new(creating_text).style(Style::default().fg(ratatui::style::Color::Cyan)),
        );
    }

    // Add existing agents
    items.extend(ctx.agents.iter().map(|agent| {
        let base_text = agent.display_name.as_deref().unwrap_or(&agent.branch_name);
        let server_info = if let Some(p) = agent.port {
            let server_icon = if agent.server_running { ">" } else { "o" };
            format!(" {}:{}", server_icon, p)
        } else {
            String::new()
        };
        ListItem::new(format!("{}{}", base_text, server_info))
    }));

    let mut state = ListState::default();
    let creating_offset = if ctx.creating_agent.is_some() { 1 } else { 0 };
    state.select(Some(
        (ctx.selected_agent_index + creating_offset).min(items.len().saturating_sub(1)),
    ));

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED))
        .highlight_symbol("> ");

    f.render_stateful_widget(list, area, &mut state);
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

// === Area-based Widget Renderers ===
//
// These render content into a given area without self-centering.
// Used by both the Lua render tree (via render_tree.rs) and the
// fallback render_frame() path.

/// Render menu items with selection into a given area.
pub(super) fn render_menu_widget(f: &mut Frame, ctx: &RenderContext, area: Rect, block: Block) {
    let menu_items = build_menu(&ctx.menu_context());

    let mut lines: Vec<Line> = Vec::new();
    let mut selectable_idx = 0;

    for item in &menu_items {
        if item.is_header {
            lines.push(Line::from(Span::styled(
                item.label.clone(),
                Style::default()
                    .add_modifier(Modifier::DIM)
                    .add_modifier(Modifier::BOLD),
            )));
        } else {
            let is_selected = selectable_idx == ctx.menu_selected;
            let cursor = if is_selected { ">" } else { " " };
            let style = if is_selected {
                Style::default()
                    .add_modifier(Modifier::REVERSED)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(
                format!("{} {}", cursor, item.label),
                style,
            )));
            selectable_idx += 1;
        }
    }

    let menu = Paragraph::new(lines)
        .block(block)
        .alignment(Alignment::Left);

    f.render_widget(menu, area);
}

/// Render worktree selection list into a given area.
pub(super) fn render_worktree_select_widget(
    f: &mut Frame,
    ctx: &RenderContext,
    area: Rect,
    block: Block,
) {
    let mut items: Vec<String> = vec![format!(
        "{} [Create New Worktree]",
        if ctx.worktree_selected == 0 { ">" } else { " " }
    )];

    for (i, (path, branch)) in ctx.available_worktrees.iter().enumerate() {
        items.push(format!(
            "{} {} ({})",
            if i + 1 == ctx.worktree_selected {
                ">"
            } else {
                " "
            },
            branch,
            path
        ));
    }

    let text: Vec<Line> = items.iter().map(|s| Line::from(s.clone())).collect();

    let widget = Paragraph::new(text)
        .block(block)
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: false });

    f.render_widget(widget, area);
}

/// Render text input field into a given area.
///
/// When `custom_lines` is `Some`, uses those lines as prompt text (appending
/// the input buffer). Otherwise falls back to mode-specific hardcoded text.
pub(super) fn render_text_input_widget(
    f: &mut Frame,
    ctx: &RenderContext,
    area: Rect,
    block: Block,
    custom_lines: Option<&[StyledContent]>,
) {
    let prompt_lines = if let Some(lines) = custom_lines {
        let mut result: Vec<Line> = lines.iter().map(StyledContent::to_line).collect();
        result.push(Line::from(""));
        result.push(Line::from(Span::raw(ctx.input_buffer)));
        result
    } else {
        match ctx.mode {
            AppMode::NewAgentCreateWorktree => vec![
                Line::from("Enter branch name or issue number:"),
                Line::from(""),
                Line::from("Examples: 123, feature-auth, bugfix-login"),
                Line::from(""),
                Line::from(Span::raw(ctx.input_buffer)),
            ],
            AppMode::NewAgentPrompt => vec![
                Line::from("Enter prompt for agent (leave empty for default):"),
                Line::from(""),
                Line::from(Span::raw(ctx.input_buffer)),
            ],
            _ => vec![Line::from(Span::raw(ctx.input_buffer))],
        }
    };

    let widget = Paragraph::new(prompt_lines)
        .block(block)
        .alignment(Alignment::Left);

    f.render_widget(widget, area);
}

/// Render close agent confirmation dialog into a given area.
///
/// When `custom_lines` is `Some`, uses those lines instead of the
/// hardcoded defaults.
pub(super) fn render_close_confirm_widget(
    f: &mut Frame,
    area: Rect,
    block: Block,
    custom_lines: Option<&[StyledContent]>,
) {
    let text: Vec<Line> = if let Some(lines) = custom_lines {
        lines.iter().map(StyledContent::to_line).collect()
    } else {
        vec![
            Line::from("Close selected agent?"),
            Line::from(""),
            Line::from("Y - Close agent (keep worktree)"),
            Line::from("D - Close agent and delete worktree"),
            Line::from("N/Esc - Cancel"),
        ]
    };

    let widget = Paragraph::new(text)
        .block(block)
        .alignment(Alignment::Left);

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

/// Render error message into a given area.
///
/// When `custom_lines` is `Some`, uses those as the template. Any line
/// containing `{error}` is replaced with the actual error message. Falls
/// back to hardcoded layout when `None`.
pub(super) fn render_error_widget(
    f: &mut Frame,
    ctx: &RenderContext,
    area: Rect,
    block: Block,
    custom_lines: Option<&[StyledContent]>,
) {
    let message = ctx.error_message.unwrap_or("An error occurred");

    let text_lines: Vec<Line> = if let Some(lines) = custom_lines {
        lines
            .iter()
            .map(|l| match l {
                StyledContent::Plain(s) if s.contains("{error}") => {
                    Line::from(s.replace("{error}", message))
                }
                _ => l.to_line(),
            })
            .collect()
    } else {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                "Error",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(message),
            Line::from(""),
            Line::from(Span::styled(
                "[Esc/Enter] dismiss",
                Style::default().add_modifier(Modifier::DIM),
            )),
        ]
    };

    let widget = Paragraph::new(text_lines)
        .block(block)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: false });

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
            mode: AppMode::Normal,
            menu_selected: 0,
            input_buffer: "",
            worktree_selected: 0,
            available_worktrees: &[],
            error_message: None,
            creating_agent: None,
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
    fn test_render_context_menu_context() {
        let agents = vec![AgentRenderInfo {
            key: "test-1".to_string(),
            display_name: None,
            repo: "test/repo".to_string(),
            issue_number: Some(1),
            branch_name: "botster-issue-1".to_string(),
            port: None,
            server_running: false,
            session_names: vec!["agent".to_string(), "server".to_string()],
        }];

        let ctx = RenderContext {
            mode: AppMode::Normal,
            menu_selected: 0,
            input_buffer: "",
            worktree_selected: 0,
            available_worktrees: &[],
            error_message: None,
            creating_agent: None,
            connection_code: None,
            bundle_used: false,
            agent_ids: &[],
            agents: &agents,
            selected_agent_index: 0,
            active_parser: None,
            parser_pool: &std::collections::HashMap::new(),
            active_pty_index: 1,
            scroll_offset: 0,
            is_scrolled: false,
            seconds_since_poll: 5,
            poll_interval: 10,
            vpn_status: None,
            terminal_cols: 80,
            terminal_rows: 24,
            terminal_areas: std::cell::RefCell::new(std::collections::HashMap::new()),
        };

        let menu_ctx = ctx.menu_context();
        assert!(menu_ctx.has_agent);
        assert_eq!(menu_ctx.active_pty_index, 1);
        assert_eq!(menu_ctx.session_count, 2);
    }

    #[test]
    fn test_render_result_default() {
        let result = RenderResult::default();
        assert!(result.ansi_output.is_empty());
        assert_eq!(result.rows, 0);
        assert_eq!(result.cols, 0);
    }
}
