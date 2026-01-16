//! TUI rendering functions.
//!
//! This module provides the main rendering function for the botster-hub TUI.
//! It renders Hub state to a terminal and optionally produces ANSI output
//! for browser streaming.

// Rust guideline compliant 2025-01

use std::collections::HashMap;

use anyhow::Result;
use ratatui::{
    backend::{CrosstermBackend, TestBackend},
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};

use crate::app::{buffer_to_ansi, centered_rect, AppMode};
use crate::hub::{build_menu, Hub, MenuContext};
use crate::render::render_agent_terminal;
use crate::tunnel::TunnelStatus;
use crate::{Agent, BrowserDimensions, PtyView, VpnStatus};

/// Render the TUI and return ANSI output for browser streaming.
///
/// Returns `(ansi_string, rows, cols, qr_image_written)` for sending to connected browsers.
/// If `browser_dims` is provided, renders at those dimensions for proper layout.
///
/// # Arguments
///
/// * `terminal` - The ratatui terminal to render to
/// * `hub` - Reference to the Hub containing all state
/// * `browser_dims` - Optional browser dimensions for virtual terminal rendering
///
/// # Returns
///
/// A tuple of (ANSI output string, rows, cols, qr_image_written) for browser streaming.
/// If no browser is connected, returns empty string with 0 dimensions.
/// The `qr_image_written` flag indicates if a QR image was written to stdout (for tracking).
pub fn render(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    hub: &Hub,
    browser_dims: Option<BrowserDimensions>,
) -> Result<(String, u16, u16, bool)> {
    // Collect all state needed for rendering
    let agent_keys_ordered = hub.state.agent_keys_ordered.clone();
    let agents = &hub.state.agents;
    // TuiClient is the source of truth for TUI selection
    let selected_key = hub.get_tui_selected_agent_key();
    let selected = selected_key
        .as_ref()
        .and_then(|key| agent_keys_ordered.iter().position(|k| k == key))
        .unwrap_or(0);
    let seconds_since_poll = hub.last_poll.elapsed().as_secs();
    let poll_interval = hub.config.poll_interval;
    let mode = hub.mode;
    let polling_enabled = hub.polling_enabled;
    let menu_selected = hub.menu_selected;
    let available_worktrees = hub.state.available_worktrees.clone();
    let worktree_selected = hub.worktree_selected;
    let input_buffer = hub.input_buffer.clone();
    let tunnel_status = hub.tunnel_manager.get_status();
    // VPN manager removed - using Action Cable terminal relay instead
    let vpn_status: Option<VpnStatus> = None;
    // E2E encryption: connection URL for QR code display
    let connection_url = hub.connection_url.clone();
    // Whether the connection bundle has been used (needs regeneration for new devices)
    let bundle_used = hub.browser.bundle_used;
    // Error message for Error mode
    let error_message = hub.error_message.clone();
    // TUI creation progress for display
    let creating_agent = hub.creating_agent.clone();

    // Build menu context from current state
    let selected_agent = agent_keys_ordered
        .get(selected)
        .and_then(|key| hub.state.agents.get(key));
    let menu_context = MenuContext {
        has_agent: selected_agent.is_some(),
        has_server_pty: selected_agent.map_or(false, |a| a.has_server_pty()),
        active_pty: selected_agent.map_or(PtyView::Cli, |a| a.active_pty),
        polling_enabled,
    };
    let menu_items = build_menu(&menu_context);

    // Helper to render UI to a frame
    let render_ui = |f: &mut Frame, agents: &HashMap<String, Agent>| {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(30), Constraint::Percentage(70)].as_ref())
            .split(f.area());

        // Render agent list
        let mut items: Vec<ListItem> = Vec::new();

        // Add creating indicator at top if agent creation is in progress
        if let Some((ref identifier, ref stage)) = creating_agent {
            use crate::relay::AgentCreationStage;
            let stage_label = match stage {
                AgentCreationStage::CreatingWorktree => "Creating worktree...",
                AgentCreationStage::CopyingConfig => "Copying config...",
                AgentCreationStage::SpawningAgent => "Starting agent...",
                AgentCreationStage::Ready => "Ready",
            };
            let creating_text = format!("⟳ {} ({})", identifier, stage_label);
            items.push(
                ListItem::new(creating_text)
                    .style(Style::default().fg(ratatui::style::Color::Cyan))
            );
        }

        // Add existing agents
        items.extend(agent_keys_ordered
            .iter()
            .filter_map(|key| agents.get(key))
            .map(|agent| {
                let base_text = if let Some(issue_num) = agent.issue_number {
                    format!("{}#{}", agent.repo, issue_num)
                } else {
                    format!("{}/{}", agent.repo, agent.branch_name)
                };

                // Add server status indicator if tunnel port is assigned
                let server_info = if let Some(port) = agent.tunnel_port {
                    let server_icon = if agent.is_server_running() {
                        "▶" // Server running
                    } else {
                        "○" // Server not running
                    };
                    format!(" {}:{}", server_icon, port)
                } else {
                    String::new()
                };

                ListItem::new(format!("{}{}", base_text, server_info))
            }));

        let mut state = ListState::default();
        // Offset selection by 1 if creating indicator is shown
        let creating_offset = if creating_agent.is_some() { 1 } else { 0 };
        state.select(Some(
            (selected + creating_offset).min(items.len().saturating_sub(1)),
        ));

        // Add polling indicator
        let poll_status = if !polling_enabled {
            "PAUSED"
        } else if seconds_since_poll < 1 {
            "●"
        } else {
            "○"
        };

        // Add tunnel status indicator
        let tunnel_indicator = match tunnel_status {
            TunnelStatus::Connected => "⬤",    // Filled circle = connected
            TunnelStatus::Connecting => "◐",   // Half circle = connecting
            TunnelStatus::Disconnected => "○", // Empty circle = disconnected
        };

        // Add VPN status indicator (if VPN manager is available)
        let vpn_indicator = match vpn_status {
            Some(VpnStatus::Connected) => "⬤",    // Filled = connected
            Some(VpnStatus::Connecting) => "◐",   // Half = connecting
            Some(VpnStatus::Error) => "✕",        // X = error
            Some(VpnStatus::Disconnected) => "○", // Empty = disconnected
            None => "-",                          // Dash = VPN disabled
        };

        let agent_title = format!(
            " Agents ({}) {} {}s T:{} V:{} ",
            agent_keys_ordered.len(),
            poll_status,
            if polling_enabled {
                poll_interval - seconds_since_poll.min(poll_interval)
            } else {
                0
            },
            tunnel_indicator,
            vpn_indicator
        );

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(agent_title))
            .highlight_style(Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED))
            .highlight_symbol("> ");

        f.render_stateful_widget(list, chunks[0], &mut state);

        // Render terminal view using the extracted render function
        let selected_agent = agent_keys_ordered
            .get(selected)
            .and_then(|key| agents.get(key));
        if let Some(agent) = selected_agent {
            render_agent_terminal(agent, chunks[1], f.buffer_mut());
        }

        // Render modal overlays based on mode
        // Returns QrImageState if connection code modal needs Kitty image rendering
        let qr_state: Option<QrImageState> = match mode {
            AppMode::Menu => {
                render_menu_modal(f, &menu_items, menu_selected);
                None
            }
            AppMode::NewAgentSelectWorktree => {
                render_worktree_select_modal(f, &available_worktrees, worktree_selected);
                None
            }
            AppMode::NewAgentCreateWorktree => {
                render_create_worktree_modal(f, &input_buffer);
                None
            }
            AppMode::NewAgentPrompt => {
                render_prompt_modal(f, &input_buffer);
                None
            }
            AppMode::CloseAgentConfirm => {
                render_close_confirm_modal(f);
                None
            }
            AppMode::ConnectionCode => {
                render_connection_code_modal(f, connection_url.as_deref(), bundle_used)
            }
            AppMode::Error => {
                render_error_modal(f, error_message.as_deref());
                None
            }
            AppMode::Normal => None,
        };
        qr_state
    };

    // Capture QR image state from render
    let mut captured_qr_state: Option<QrImageState> = None;

    // Always render to real terminal for local display
    terminal.draw(|f| {
        captured_qr_state = render_ui(f, agents);
    })?;

    // Track whether we wrote a QR image (for preventing re-rendering)
    let mut qr_image_written = false;

    // If Kitty image needs to be rendered, write it after the frame
    // IMPORTANT: Only write if not already displayed to prevent memory leak.
    // Writing 60 images/second causes 150GB+ memory usage in terminal emulators.
    if let Some(qr_state) = captured_qr_state {
        if let Some(escape_seq) = qr_state.kitty_escape {
            if !hub.qr_image_displayed {
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
            render_ui(f, agents);
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

    Ok((ansi_output, out_rows, out_cols, qr_image_written))
}

// === Modal Rendering Helpers ===

fn render_menu_modal(
    f: &mut Frame,
    menu_items: &[crate::hub::MenuItem],
    menu_selected: usize,
) {
    use crate::hub::menu::selectable_count;

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
    let height_percent = ((content_rows * 100) / terminal_height.max(1)).max(30).min(60);

    let area = centered_rect(50, height_percent, f.area());
    f.render_widget(Clear, area);

    let menu = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Menu [↑/↓ navigate | Enter select | Esc cancel] "),
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
                .title(" Select Worktree [↑/↓ navigate | Enter select | Esc cancel] "),
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

fn render_connection_code_modal(f: &mut Frame, connection_url: Option<&str>, bundle_used: bool) -> Option<QrImageState> {
    use crate::tui::qr::{generate_qr_code_lines, generate_qr_kitty_image, QrRenderResult};
    use ratatui::layout::Rect;

    let terminal = f.area();
    let secure_url = connection_url.unwrap_or("Error: No connection URL generated");

    // Footer varies based on whether the bundle has been used
    let footer = if bundle_used {
        "[r] new link  [c] copy  [Esc] close"
    } else {
        "[r] new link  [c] copy  [Esc] close"
    };

    // Try Kitty graphics first (module_size=4 gives good balance of size/quality)
    if let Some(QrRenderResult::KittyImage { escape_sequence, width_cells, height_cells }) =
        generate_qr_kitty_image(secure_url, 4)
    {
        // Kitty image mode: render a compact modal sized to QR + text
        let header = if bundle_used {
            "Link used - [r] to pair new device"
        } else {
            "Scan QR to connect securely"
        };

        // Modal sized to fit QR image + header/footer
        let content_width = width_cells.max(header.len() as u16).max(footer.len() as u16);
        let modal_width = content_width + 4; // +4 for borders and padding
        let modal_height = height_cells + 6; // header, blank, image rows, blank, footer, borders

        // Center the modal
        let x = terminal.x + (terminal.width.saturating_sub(modal_width)) / 2;
        let y = terminal.y + (terminal.height.saturating_sub(modal_height)) / 2;
        let area = Rect::new(x, y, modal_width.min(terminal.width), modal_height.min(terminal.height));

        f.render_widget(Clear, area);

        // Build text with placeholder for image
        let mut text_lines = vec![
            Line::from(Span::styled(header, Style::default().add_modifier(Modifier::BOLD))),
            Line::from(""),
        ];

        // Add empty lines where image will go
        for _ in 0..height_cells {
            text_lines.push(Line::from(""));
        }

        text_lines.push(Line::from(""));
        text_lines.push(Line::from(Span::styled(footer, Style::default().add_modifier(Modifier::DIM))));

        let code_widget = Paragraph::new(text_lines)
            .block(Block::default().borders(Borders::ALL).title(" Secure Connection "))
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

    // Fallback: text-based QR using Unicode half-blocks
    let max_qr_width = terminal.width.saturating_sub(4);
    let max_qr_height = terminal.height.saturating_sub(8);
    let qr_lines = generate_qr_code_lines(secure_url, max_qr_width, max_qr_height);
    let qr_fits = !qr_lines.iter().any(|l| l.contains("Terminal"));

    let qr_width = qr_lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
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
    let area = Rect::new(x, y, modal_width.min(terminal.width), modal_height.min(terminal.height));

    f.render_widget(Clear, area);

    let mut text_lines = vec![
        Line::from(Span::styled(header, Style::default().add_modifier(Modifier::BOLD))),
        Line::from(""),
    ];

    for qr_line in &qr_lines {
        text_lines.push(Line::from(qr_line.clone()));
    }

    if !qr_fits {
        text_lines.push(Line::from(format!(
            "(Terminal: {}x{}, need {}x{})",
            terminal.width, terminal.height, qr_width + 4, qr_height + 6
        )));
    }

    text_lines.push(Line::from(""));
    text_lines.push(Line::from(Span::styled(
        footer,
        if qr_fits { Style::default().add_modifier(Modifier::DIM) }
        else { Style::default().add_modifier(Modifier::BOLD) },
    )));

    let code_widget = Paragraph::new(text_lines)
        .block(Block::default().borders(Borders::ALL).title(" Secure Connection "))
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
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Error "),
        )
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: false });

    f.render_widget(error_widget, area);
}
