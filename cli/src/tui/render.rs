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
/// Returns `(ansi_string, rows, cols)` for sending to connected browsers.
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
/// A tuple of (ANSI output string, rows, cols) for browser streaming.
/// If no browser is connected, returns empty string with 0 dimensions.
pub fn render(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    hub: &Hub,
    browser_dims: Option<BrowserDimensions>,
) -> Result<(String, u16, u16)> {
    // Collect all state needed for rendering
    let agent_keys_ordered = hub.state.agent_keys_ordered.clone();
    let agents = &hub.state.agents;
    let selected = hub.state.selected;
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
        let items: Vec<ListItem> = agent_keys_ordered
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
            })
            .collect();

        let mut state = ListState::default();
        state.select(Some(
            selected.min(agent_keys_ordered.len().saturating_sub(1)),
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
        match mode {
            AppMode::Menu => {
                render_menu_modal(f, &menu_items, menu_selected);
            }
            AppMode::NewAgentSelectWorktree => {
                render_worktree_select_modal(f, &available_worktrees, worktree_selected);
            }
            AppMode::NewAgentCreateWorktree => {
                render_create_worktree_modal(f, &input_buffer);
            }
            AppMode::NewAgentPrompt => {
                render_prompt_modal(f, &input_buffer);
            }
            AppMode::CloseAgentConfirm => {
                render_close_confirm_modal(f);
            }
            AppMode::ConnectionCode => {
                render_connection_code_modal(f, connection_url.as_deref());
            }
            AppMode::Normal => {}
        }
    };

    // Always render to real terminal for local display
    terminal.draw(|f| render_ui(f, agents))?;

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

    Ok((ansi_output, out_rows, out_cols))
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
            // Section headers are dimmed and not selectable
            lines.push(Line::from(Span::styled(
                item.label.clone(),
                Style::default()
                    .fg(ratatui::style::Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )));
        } else {
            // Selectable items with cursor indicator
            let is_selected = selectable_idx == menu_selected;
            let cursor = if is_selected { ">" } else { " " };
            let style = if is_selected {
                Style::default()
                    .fg(ratatui::style::Color::Black)
                    .bg(ratatui::style::Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                // Explicit white foreground for xterm.js compatibility
                Style::default().fg(ratatui::style::Color::White)
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

fn render_connection_code_modal(f: &mut Frame, connection_url: Option<&str>) {
    use crate::tui::generate_qr_code_lines;

    // Use full terminal for the QR code modal (QR codes with Kyber keys are large)
    let area = centered_rect(98, 98, f.area());
    f.render_widget(Clear, area);

    // Use the pre-generated connection URL
    let secure_url = connection_url.unwrap_or("Error: No connection URL generated");

    // Calculate available space for QR code (minimal overhead)
    let header_lines = 2u16; // Title + instruction
    let footer_lines = 2u16; // Copy hint + close hint
    let border_overhead = 2u16;
    let available_height = area
        .height
        .saturating_sub(header_lines + footer_lines + border_overhead);
    let available_width = area.width.saturating_sub(2);

    let qr_lines = generate_qr_code_lines(secure_url, available_width, available_height);
    let qr_fits = !qr_lines.iter().any(|l| l.contains("too large"));

    let mut text_lines = vec![
        Line::from(Span::styled(
            "Scan QR to connect securely",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    // Add QR code lines
    for qr_line in qr_lines {
        text_lines.push(Line::from(qr_line));
    }

    text_lines.push(Line::from(""));
    if qr_fits {
        text_lines.push(Line::from(Span::styled(
            "[c] copy URL  [Esc/q/Enter] close",
            Style::default().fg(ratatui::style::Color::DarkGray),
        )));
    } else {
        // If QR doesn't fit, show more helpful message
        text_lines.push(Line::from(Span::styled(
            "[c] copy URL to clipboard  [Esc] close",
            Style::default().fg(ratatui::style::Color::Yellow),
        )));
    }

    let code_widget = Paragraph::new(text_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Secure Connection "),
        )
        .alignment(Alignment::Center);

    f.render_widget(code_widget, area);
}
