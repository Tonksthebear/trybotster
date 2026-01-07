//! Hub event loop implementation.
//!
//! Contains the main run loop that coordinates:
//! - Terminal input handling
//! - Browser event processing
//! - Periodic polling and heartbeats
//! - TUI rendering
//!
//! The event loop is the central orchestrator that ties together
//! all Hub components.

// Rust guideline compliant 2025-01

use std::io::Stdout;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use crossterm::event;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::hub::Hub;
use crate::relay::{browser, check_browser_resize, ResizeAction};
use crate::{constants, tui, BrowserDimensions, BrowserMode};

/// Run the Hub event loop.
///
/// This is the main entry point for the Hub. It handles:
/// 1. Keyboard/mouse input → HubActions
/// 2. Browser events → HubActions
/// 3. Rendering via tui::render()
/// 4. Periodic tasks via tick()
///
/// # Arguments
///
/// * `hub` - The Hub instance to run
/// * `terminal` - The ratatui terminal for rendering
/// * `shutdown_flag` - Atomic flag for external shutdown requests (signals)
///
/// # Errors
///
/// Returns an error if terminal operations fail.
pub fn run_event_loop(
    hub: &mut Hub,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    shutdown_flag: &AtomicBool,
) -> Result<()> {
    log::info!("Hub event loop starting");

    while !hub.quit && !shutdown_flag.load(Ordering::SeqCst) {
        // 1. Handle keyboard/mouse input
        if event::poll(Duration::from_millis(10))? {
            let ev = event::read()?;
            let context = tui::InputContext {
                terminal_rows: terminal.size()?.height,
                menu_selected: hub.menu_selected,
                menu_count: constants::MENU_ITEMS.len(),
                worktree_selected: hub.worktree_selected,
                worktree_count: hub.state.available_worktrees.len(),
            };
            if let Some(action) = tui::event_to_hub_action(&ev, &hub.mode, &context) {
                hub.handle_action(action);
            }
        }

        // Check quit after input handling
        if hub.quit || shutdown_flag.load(Ordering::SeqCst) {
            break;
        }

        // 2. Get browser dimensions for rendering
        let browser_dims: Option<BrowserDimensions> = hub.browser.dims.as_ref().map(|dims| {
            BrowserDimensions {
                cols: dims.cols,
                rows: dims.rows,
                mode: hub.browser.mode.unwrap_or(BrowserMode::Tui),
            }
        });

        // 3. Handle browser resize
        handle_browser_resize_action(hub, &browser_dims, terminal);

        // 4. Render using tui::render()
        let (ansi_output, _rows, _cols) = tui::render(terminal, hub, browser_dims.clone())?;

        // 5. Poll and handle browser events
        browser::poll_events(hub, terminal)?;

        // 6. Send output to browser
        browser::send_output(hub, &ansi_output);

        // 7. Periodic tasks (polling, heartbeat, notifications)
        hub.tick();

        // Small sleep to prevent CPU spinning (60 FPS max)
        std::thread::sleep(Duration::from_millis(16));
    }

    log::info!("Hub event loop exiting");
    Ok(())
}

/// Handle browser resize by applying dimension changes to agents.
fn handle_browser_resize_action(
    hub: &mut Hub,
    browser_dims: &Option<BrowserDimensions>,
    terminal: &Terminal<CrosstermBackend<Stdout>>,
) {
    let dims_tuple = browser_dims.as_ref().map(|d| (d.rows, d.cols, d.mode));
    let terminal_size = terminal.size().unwrap_or_default();
    let local_dims = (terminal_size.height, terminal_size.width);

    match check_browser_resize(dims_tuple, local_dims) {
        ResizeAction::ResizeAgents { rows, cols } | ResizeAction::ResetToLocal { rows, cols } => {
            for agent in hub.state.agents.values() {
                agent.resize(rows, cols);
            }
        }
        ResizeAction::None => {}
    }
}
