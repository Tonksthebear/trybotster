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

use super::menu::{build_menu, MenuContext, MenuItem};
use crate::{BrowserDimensions, PtyView, VpnStatus};

use super::events::CreationStage;

/// State for QR image rendering via Kitty graphics protocol.
/// This is returned from render and should be written to stdout after the frame.
#[derive(Default, Debug)]
pub struct QrImageState {
    /// If set, this escape sequence should be written to stdout after the frame.
    pub kitty_escape: Option<String>,
    /// Row position where the image should be displayed.
    pub row: u16,
    /// Column position where the image should be displayed.
    pub col: u16,
}

/// Information about an agent for rendering.
///
/// Extracted subset of Agent data needed for TUI display.
#[derive(Debug, Clone)]
pub struct AgentRenderInfo {
    /// Unique key for this agent (e.g., "repo-42").
    pub key: String,
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
    /// Whether this agent has a server PTY.
    pub has_server_pty: bool,
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
    /// Whether the QR image has been displayed (to avoid re-rendering every frame).
    pub qr_image_displayed: bool,
    /// Agent creation progress (identifier, stage).
    pub creating_agent: Option<(&'a str, CreationStage)>,
    /// Connection code data (URL + QR PNG) for display.
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
    /// Which PTY view is currently active.
    pub active_pty_view: PtyView,
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
}

impl<'a> std::fmt::Debug for RenderContext<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderContext")
            .field("mode", &self.mode)
            .field("menu_selected", &self.menu_selected)
            .field("selected_agent_index", &self.selected_agent_index)
            .field("agents_count", &self.agents.len())
            .field("has_active_parser", &self.active_parser.is_some())
            .field("active_pty_view", &self.active_pty_view)
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
        let selected_agent = self.selected_agent();
        MenuContext {
            has_agent: selected_agent.is_some(),
            has_server_pty: selected_agent.map_or(false, |a| a.has_server_pty),
            active_pty: self.active_pty_view,
        }
    }
}

/// Render result containing ANSI output and QR state.
#[derive(Debug)]
pub struct RenderResult {
    /// ANSI output for browser streaming.
    pub ansi_output: String,
    /// Number of rows in the output.
    pub rows: u16,
    /// Number of columns in the output.
    pub cols: u16,
    /// Whether a QR image was written to stdout.
    pub qr_image_written: bool,
}

impl Default for RenderResult {
    fn default() -> Self {
        Self {
            ansi_output: String::new(),
            rows: 0,
            cols: 0,
            qr_image_written: false,
        }
    }
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
/// A `RenderResult` with ANSI output and QR image state.
pub fn render<B>(
    terminal: &mut Terminal<B>,
    ctx: &RenderContext,
    browser_dims: Option<BrowserDimensions>,
) -> Result<RenderResult>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    // Build menu items from context
    let menu_items = build_menu(&ctx.menu_context());

    // Helper to render UI to a frame
    let render_ui = |f: &mut Frame| -> Option<QrImageState> { render_frame(f, ctx, &menu_items) };

    // Capture QR image state from render
    let mut captured_qr_state: Option<QrImageState> = None;

    // Always render to real terminal for local display
    terminal.draw(|f| {
        captured_qr_state = render_ui(f);
    })?;

    // Track whether we wrote a QR image (for preventing re-rendering)
    let mut qr_image_written = false;

    // If Kitty image needs to be rendered, write it after the frame
    // IMPORTANT: Only write if not already displayed to prevent memory leak.
    // Writing 60 images/second causes 150GB+ memory usage in terminal emulators.
    if let Some(qr_state) = captured_qr_state {
        if let Some(escape_seq) = qr_state.kitty_escape {
            if !ctx.qr_image_displayed {
                use std::io::Write;
                // Position cursor and write image
                let cursor_pos = format!("\x1b[{};{}H", qr_state.row + 1, qr_state.col + 1);
                let mut stdout = std::io::stdout();
                let _ = stdout.write_all(cursor_pos.as_bytes());
                let _ = stdout.write_all(escape_seq.as_bytes());
                let _ = stdout.flush();
                qr_image_written = true;
            }
        }
    }

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
        qr_image_written,
    })
}

/// Render the full TUI frame.
///
/// Internal function that does the actual rendering work.
fn render_frame(
    f: &mut Frame,
    ctx: &RenderContext,
    menu_items: &[MenuItem],
) -> Option<QrImageState> {
    let frame_area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)].as_ref())
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

    // Render agent list
    render_agent_list(f, ctx, chunks[0]);

    // Render terminal view
    render_terminal_panel(f, ctx, chunks[1]);

    // Render modal overlays based on mode
    match ctx.mode {
        AppMode::Menu => {
            render_menu_modal(f, menu_items, ctx.menu_selected);
            None
        }
        AppMode::NewAgentSelectWorktree => {
            render_worktree_select_modal(f, ctx.available_worktrees, ctx.worktree_selected);
            None
        }
        AppMode::NewAgentCreateWorktree => {
            render_create_worktree_modal(f, ctx.input_buffer);
            None
        }
        AppMode::NewAgentPrompt => {
            render_prompt_modal(f, ctx.input_buffer);
            None
        }
        AppMode::CloseAgentConfirm => {
            render_close_confirm_modal(f);
            None
        }
        AppMode::ConnectionCode => {
            render_connection_code_modal(f, ctx.connection_code, ctx.bundle_used)
        }
        AppMode::Error => {
            render_error_modal(f, ctx.error_message);
            None
        }
        AppMode::Normal => None,
    }
}

/// Render the agent list panel.
fn render_agent_list(f: &mut Frame, ctx: &RenderContext, area: Rect) {
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
        let base_text = if let Some(issue_num) = agent.issue_number {
            format!("{}#{}", agent.repo, issue_num)
        } else {
            format!("{}/{}", agent.repo, agent.branch_name)
        };

        // Add server status indicator if HTTP forwarding port is assigned
        let server_info = if let Some(p) = agent.port {
            let server_icon = if agent.server_running {
                ">" // Server running
            } else {
                "o" // Server not running
            };
            format!(" {}:{}", server_icon, p)
        } else {
            String::new()
        };

        ListItem::new(format!("{}{}", base_text, server_info))
    }));

    let mut state = ListState::default();
    // Offset selection by 1 if creating indicator is shown
    let creating_offset = if ctx.creating_agent.is_some() { 1 } else { 0 };
    state.select(Some(
        (ctx.selected_agent_index + creating_offset).min(items.len().saturating_sub(1)),
    ));

    // Add polling indicator
    let poll_status = if ctx.seconds_since_poll < 1 {
        "*"
    } else {
        "o"
    };

    // Add VPN status indicator (if VPN manager is available)
    let vpn_indicator = match ctx.vpn_status {
        Some(VpnStatus::Connected) => "*",    // Filled = connected
        Some(VpnStatus::Connecting) => "~",   // Half = connecting
        Some(VpnStatus::Error) => "x",        // X = error
        Some(VpnStatus::Disconnected) => "o", // Empty = disconnected
        None => "-",                          // Dash = VPN disabled
    };

    let agent_title = format!(
        " Agents ({}) {} {}s V:{} ",
        ctx.agents.len(),
        poll_status,
        ctx.poll_interval - ctx.seconds_since_poll.min(ctx.poll_interval),
        vpn_indicator
    );

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(agent_title))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED))
        .highlight_symbol("> ");

    f.render_stateful_widget(list, area, &mut state);
}

/// Render the terminal panel showing the selected agent's PTY output.
fn render_terminal_panel(f: &mut Frame, ctx: &RenderContext, area: Rect) {
    let Some(agent) = ctx.selected_agent() else {
        // No agent selected - show placeholder
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Terminal [No agent selected] ");
        f.render_widget(block, area);
        return;
    };

    // Build terminal title with view indicator
    let view_indicator = match ctx.active_pty_view {
        PtyView::Cli => {
            if agent.has_server_pty {
                "[CLI | Ctrl+]: Server]"
            } else {
                "[CLI]"
            }
        }
        PtyView::Server => "[SERVER | Ctrl+]: CLI]",
    };

    // Add scroll indicator if scrolled
    let scroll_indicator = if ctx.is_scrolled {
        format!(" [SCROLLBACK +{} | Shift+End: live]", ctx.scroll_offset)
    } else {
        String::new()
    };

    let terminal_title = if let Some(issue_num) = agent.issue_number {
        format!(
            " {}#{} {}{} [Ctrl+P | Ctrl+J/K | Shift+PgUp/Dn scroll] ",
            agent.repo, issue_num, view_indicator, scroll_indicator
        )
    } else {
        format!(
            " {}/{} {}{} [Ctrl+P | Ctrl+J/K | Shift+PgUp/Dn scroll] ",
            agent.repo, agent.branch_name, view_indicator, scroll_indicator
        )
    };

    let block = Block::default().borders(Borders::ALL).title(terminal_title);

    // Render the parser content if available
    if let Some(ref parser) = ctx.active_parser {
        let parser_lock = parser.lock().expect("parser lock not poisoned");
        let screen = parser_lock.screen();

        let widget = crate::TerminalWidget::new(screen).block(block);

        // Hide cursor if scrolled
        let widget = if ctx.is_scrolled {
            widget.hide_cursor()
        } else {
            widget
        };

        widget.render(area, f.buffer_mut());
    } else {
        // No parser - just show the bordered block
        f.render_widget(block, area);
    }
}

// === Modal Rendering Helpers ===

fn render_menu_modal(f: &mut Frame, menu_items: &[MenuItem], menu_selected: usize) {
    use super::menu::selectable_count;

    // Build display lines with selection indicator
    let mut lines: Vec<Line> = Vec::new();
    let mut selectable_idx = 0;

    for item in menu_items {
        if item.is_header {
            // Section headers are dimmed and not selectable (use terminal default with DIM modifier)
            lines.push(Line::from(Span::styled(
                item.label.clone(),
                Style::default()
                    .add_modifier(Modifier::DIM)
                    .add_modifier(Modifier::BOLD),
            )));
        } else {
            // Selectable items with cursor indicator
            let is_selected = selectable_idx == menu_selected;
            let cursor = if is_selected { ">" } else { " " };
            let style = if is_selected {
                // Use REVERSED modifier to invert terminal colors instead of hardcoding
                Style::default()
                    .add_modifier(Modifier::REVERSED)
                    .add_modifier(Modifier::BOLD)
            } else {
                // Use terminal default colors
                Style::default()
            };
            lines.push(Line::from(Span::styled(
                format!("{} {}", cursor, item.label),
                style,
            )));
            selectable_idx += 1;
        }
    }

    // Calculate modal height as percentage of terminal
    // Need enough space for content + borders (lines.len() + 4 padding)
    let content_rows = lines.len() as u16 + 4;
    let terminal_height = f.area().height;
    // Convert absolute rows to percentage, with minimum 30% for visibility
    let height_percent = ((content_rows * 100) / terminal_height.max(1))
        .max(30)
        .min(60);

    let area = centered_rect(50, height_percent, f.area());
    f.render_widget(Clear, area);

    let menu = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Menu [Up/Down navigate | Enter select | Esc cancel] "),
        )
        .alignment(Alignment::Left);

    f.render_widget(menu, area);

    // Suppress unused warning - selectable_count is used for validation elsewhere
    let _ = selectable_count(menu_items);
}

fn render_worktree_select_modal(
    f: &mut Frame,
    available_worktrees: &[(String, String)],
    worktree_selected: usize,
) {
    let mut worktree_items: Vec<String> = vec![format!(
        "{} [Create New Worktree]",
        if worktree_selected == 0 { ">" } else { " " }
    )];

    // Add existing worktrees (index offset by 1)
    for (i, (path, branch)) in available_worktrees.iter().enumerate() {
        worktree_items.push(format!(
            "{} {} ({})",
            if i + 1 == worktree_selected { ">" } else { " " },
            branch,
            path
        ));
    }

    let area = centered_rect(70, 50, f.area());
    f.render_widget(Clear, area);

    let worktree_text: Vec<Line> = worktree_items
        .iter()
        .map(|item| Line::from(item.clone()))
        .collect();

    let worktree_list = Paragraph::new(worktree_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Select Worktree [Up/Down navigate | Enter select | Esc cancel] "),
        )
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: false });

    f.render_widget(worktree_list, area);
}

fn render_create_worktree_modal(f: &mut Frame, input_buffer: &str) {
    let area = centered_rect(60, 30, f.area());
    f.render_widget(Clear, area);

    let prompt_text = vec![
        Line::from("Enter branch name or issue number:"),
        Line::from(""),
        Line::from("Examples: 123, feature-auth, bugfix-login"),
        Line::from(""),
        Line::from(Span::raw(input_buffer)),
    ];

    let prompt_widget = Paragraph::new(prompt_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Create Worktree [Enter confirm | Esc cancel] "),
        )
        .alignment(Alignment::Left);

    f.render_widget(prompt_widget, area);
}

fn render_prompt_modal(f: &mut Frame, input_buffer: &str) {
    let area = centered_rect(60, 20, f.area());
    f.render_widget(Clear, area);

    let prompt_text = vec![
        Line::from("Enter prompt for agent (leave empty for default):"),
        Line::from(""),
        Line::from(Span::raw(input_buffer)),
    ];

    let prompt_widget = Paragraph::new(prompt_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Agent Prompt [Enter confirm | Esc cancel] "),
        )
        .alignment(Alignment::Left);

    f.render_widget(prompt_widget, area);
}

fn render_close_confirm_modal(f: &mut Frame) {
    let area = centered_rect(50, 20, f.area());
    f.render_widget(Clear, area);

    let confirm_text = vec![
        Line::from("Close selected agent?"),
        Line::from(""),
        Line::from("Y - Close agent (keep worktree)"),
        Line::from("D - Close agent and delete worktree"),
        Line::from("N/Esc - Cancel"),
    ];

    let confirm_widget = Paragraph::new(confirm_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Confirm Close "),
        )
        .alignment(Alignment::Left);

    f.render_widget(confirm_widget, area);
}

fn render_connection_code_modal(
    f: &mut Frame,
    connection_code: Option<&super::qr::ConnectionCodeData>,
    bundle_used: bool,
) -> Option<QrImageState> {
    use super::qr::{build_kitty_escape_from_png, generate_qr_code_lines};

    let terminal = f.area();

    // Get URL from connection code, or show error
    let secure_url = connection_code
        .map(|c| c.url.as_str())
        .unwrap_or("Error: No connection URL generated");

    // Footer varies based on whether the bundle has been used
    let footer = if bundle_used {
        "[r] new link  [c] copy  [Esc] close"
    } else {
        "[r] new link  [c] copy  [Esc] close"
    };

    // Calculate available space for QR (terminal minus modal chrome)
    // Leave room for: borders (4), header, 2 blank lines, footer
    let max_qr_cols = terminal.width.saturating_sub(4);
    let max_qr_rows = terminal.height.saturating_sub(8);

    // Try Kitty graphics first using the pre-generated PNG from ConnectionCodeData
    if let Some(code_data) = connection_code {
        if let Some((escape_sequence, width_cells, height_cells)) =
            build_kitty_escape_from_png(&code_data.qr_png, max_qr_cols, max_qr_rows)
        {
        // Kitty image mode: render a compact modal sized to QR + text
        let header = if bundle_used {
            "Link used - [r] to pair new device"
        } else {
            "Scan QR to connect securely"
        };

        // Modal sized to fit QR image + header/footer
        let content_width = width_cells
            .max(header.len() as u16)
            .max(footer.len() as u16);
        let modal_width = content_width + 4; // +4 for borders and padding
        let modal_height = height_cells + 6; // header, blank, image rows, blank, footer, borders

        // Center the modal
        let x = terminal.x + (terminal.width.saturating_sub(modal_width)) / 2;
        let y = terminal.y + (terminal.height.saturating_sub(modal_height)) / 2;
        let area = Rect::new(
            x,
            y,
            modal_width.min(terminal.width),
            modal_height.min(terminal.height),
        );

        f.render_widget(Clear, area);

        // Build text with placeholder for image
        let mut text_lines = vec![
            Line::from(Span::styled(
                header,
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
        ];

        // Add empty lines where image will go
        for _ in 0..height_cells {
            text_lines.push(Line::from(""));
        }

        text_lines.push(Line::from(""));
        text_lines.push(Line::from(Span::styled(
            footer,
            Style::default().add_modifier(Modifier::DIM),
        )));

        let code_widget = Paragraph::new(text_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Secure Connection "),
            )
            .alignment(Alignment::Center);

        f.render_widget(code_widget, area);

        // Return image state - caller will write escape sequence after frame
        // Center image within modal
        let img_x = x + (modal_width.saturating_sub(width_cells)) / 2;
        let img_y = y + 3; // After border + header + blank line

            return Some(QrImageState {
                kitty_escape: Some(escape_sequence),
                row: img_y,
                col: img_x,
            });
        }
    }

    // Fallback: text-based QR using Unicode half-blocks
    let max_qr_width = terminal.width.saturating_sub(4);
    let max_qr_height = terminal.height.saturating_sub(8);
    let qr_lines = generate_qr_code_lines(secure_url, max_qr_width, max_qr_height);
    let qr_fits = !qr_lines.iter().any(|l| l.contains("Terminal"));

    let qr_width = qr_lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0) as u16;
    let qr_height = qr_lines.len() as u16;

    let header = if bundle_used {
        "Link used - [r] to pair new device"
    } else {
        "Scan QR to connect securely"
    };
    let footer = if qr_fits {
        "[r] new link  [c] copy  [Esc] close"
    } else {
        "No graphics. [r] new  [c] copy  [Esc] close"
    };

    let content_width = qr_width.max(header.len() as u16).max(footer.len() as u16);
    let modal_width = content_width + 4;
    let modal_height = qr_height + 6;

    let x = terminal.x + (terminal.width.saturating_sub(modal_width)) / 2;
    let y = terminal.y + (terminal.height.saturating_sub(modal_height)) / 2;
    let area = Rect::new(
        x,
        y,
        modal_width.min(terminal.width),
        modal_height.min(terminal.height),
    );

    f.render_widget(Clear, area);

    let mut text_lines = vec![
        Line::from(Span::styled(
            header,
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    for qr_line in &qr_lines {
        text_lines.push(Line::from(qr_line.clone()));
    }

    if !qr_fits {
        text_lines.push(Line::from(format!(
            "(Terminal: {}x{}, need {}x{})",
            terminal.width,
            terminal.height,
            qr_width + 4,
            qr_height + 6
        )));
    }

    text_lines.push(Line::from(""));
    text_lines.push(Line::from(Span::styled(
        footer,
        if qr_fits {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        },
    )));

    let code_widget = Paragraph::new(text_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Secure Connection "),
        )
        .alignment(Alignment::Center);

    f.render_widget(code_widget, area);

    None
}

/// Render an error modal.
fn render_error_modal(f: &mut Frame, error_message: Option<&str>) {
    let area = centered_rect(60, 30, f.area());
    f.render_widget(Clear, area);

    let message = error_message.unwrap_or("An error occurred");

    let text_lines = vec![
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
    ];

    let error_widget = Paragraph::new(text_lines)
        .block(Block::default().borders(Borders::ALL).title(" Error "))
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: false });

    f.render_widget(error_widget, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_context_selected_agent() {
        let agents = vec![
            AgentRenderInfo {
                key: "test-1".to_string(),
                repo: "test/repo".to_string(),
                issue_number: Some(1),
                branch_name: "botster-issue-1".to_string(),
                port: None,
                server_running: false,
                has_server_pty: false,
            },
            AgentRenderInfo {
                key: "test-2".to_string(),
                repo: "test/repo".to_string(),
                issue_number: Some(2),
                branch_name: "botster-issue-2".to_string(),
                port: Some(3000),
                server_running: true,
                has_server_pty: true,
            },
        ];

        let ctx = RenderContext {
            mode: AppMode::Normal,
            menu_selected: 0,
            input_buffer: "",
            worktree_selected: 0,
            available_worktrees: &[],
            error_message: None,
            qr_image_displayed: false,
            creating_agent: None,
            connection_code: None,
            bundle_used: false,
            agent_ids: &[],
            agents: &agents,
            selected_agent_index: 1,
            active_parser: None,
            active_pty_view: PtyView::Cli,
            scroll_offset: 0,
            is_scrolled: false,
            seconds_since_poll: 0,
            poll_interval: 10,
            vpn_status: None,
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
            repo: "test/repo".to_string(),
            issue_number: Some(1),
            branch_name: "botster-issue-1".to_string(),
            port: None,
            server_running: false,
            has_server_pty: true,
        }];

        let ctx = RenderContext {
            mode: AppMode::Normal,
            menu_selected: 0,
            input_buffer: "",
            worktree_selected: 0,
            available_worktrees: &[],
            error_message: None,
            qr_image_displayed: false,
            creating_agent: None,
            connection_code: None,
            bundle_used: false,
            agent_ids: &[],
            agents: &agents,
            selected_agent_index: 0,
            active_parser: None,
            active_pty_view: PtyView::Server,
            scroll_offset: 0,
            is_scrolled: false,
            seconds_since_poll: 5,
            poll_interval: 10,
            vpn_status: None,
        };

        let menu_ctx = ctx.menu_context();
        assert!(menu_ctx.has_agent);
        assert!(menu_ctx.has_server_pty);
        assert_eq!(menu_ctx.active_pty, PtyView::Server);
    }

    #[test]
    fn test_render_result_default() {
        let result = RenderResult::default();
        assert!(result.ansi_output.is_empty());
        assert_eq!(result.rows, 0);
        assert_eq!(result.cols, 0);
        assert!(!result.qr_image_written);
    }
}
