//! Browser communication and state management.
//!
//! Handles the WebSocket relay connection to browsers, including:
//! - Connection state tracking
//! - Event handling and dispatch
//! - Output streaming (TUI or GUI mode)
//! - Agent/worktree list synchronization
//!
//! # Architecture
//!
//! `BrowserState` consolidates all browser-related state that was previously
//! scattered across Hub fields. Event handling is split between:
//!
//! - State changes (Connected, Disconnected, Resize, SetMode) - handled by `BrowserState` methods
//! - Actions (Input, Scroll, Select) - converted to `HubAction` via `relay::events`
//! - Responses (ListAgents, ListWorktrees) - handled by Hub with helpers here

// Rust guideline compliant 2025-01

use std::collections::HashMap;
use tokio::sync::mpsc;

use super::connection::TerminalOutputSender;
use super::types::{BrowserEvent, BrowserResize};
use crate::{AgentInfo, BrowserMode, TerminalMessage, WorktreeInfo, WorktreeManager};

/// Browser connection state.
///
/// Consolidates all browser-related fields from Hub into a single struct.
#[derive(Default)]
pub struct BrowserState {
    /// Terminal output sender for encrypted relay.
    pub sender: Option<TerminalOutputSender>,
    /// Browser event receiver.
    pub event_rx: Option<mpsc::Receiver<BrowserEvent>>,
    /// Whether a browser is currently connected.
    pub connected: bool,
    /// Browser terminal dimensions.
    pub dims: Option<BrowserResize>,
    /// Browser display mode (TUI or GUI).
    pub mode: Option<BrowserMode>,
    /// Last screen hash per agent (bandwidth optimization).
    pub agent_screen_hashes: HashMap<String, u64>,
    /// Last screen hash sent to browser.
    pub last_screen_hash: Option<u64>,
}

impl std::fmt::Debug for BrowserState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserState")
            .field("connected", &self.connected)
            .field("dims", &self.dims)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl BrowserState {
    /// Check if browser is connected and ready.
    pub fn is_connected(&self) -> bool {
        self.connected && self.sender.is_some()
    }

    /// Set connection established with sender and receiver.
    pub fn set_connected(&mut self, sender: TerminalOutputSender, rx: mpsc::Receiver<BrowserEvent>) {
        self.sender = Some(sender);
        self.event_rx = Some(rx);
        self.connected = false; // Will be true after Connected event
    }

    /// Handle browser connected event.
    ///
    /// Sets the connected flag and default mode.
    pub fn handle_connected(&mut self, device_name: &str) {
        log::info!("Browser connected: {device_name} - E2E encryption active");
        self.connected = true;
        self.mode = Some(BrowserMode::Gui);
    }

    /// Handle browser disconnected event.
    pub fn handle_disconnected(&mut self) {
        log::info!("Browser disconnected");
        self.connected = false;
        self.dims = None;
        self.last_screen_hash = None;
    }

    /// Handle browser resize event.
    ///
    /// Returns the new dimensions for agent resizing.
    pub fn handle_resize(&mut self, resize: BrowserResize) -> (u16, u16) {
        log::info!("Browser resize: {}x{}", resize.cols, resize.rows);
        let dims = (resize.rows, resize.cols);
        self.dims = Some(resize);
        self.last_screen_hash = None;
        dims
    }

    /// Handle browser mode change.
    pub fn handle_set_mode(&mut self, mode: &str) {
        log::info!("Browser set mode: {mode}");
        self.mode = if mode == "gui" {
            Some(BrowserMode::Gui)
        } else {
            Some(BrowserMode::Tui)
        };
        self.last_screen_hash = None;
    }

    /// Handle disconnect (legacy method name).
    pub fn disconnect(&mut self) {
        self.handle_disconnected();
    }

    /// Invalidate screen hash (forces re-send).
    pub fn invalidate_screen(&mut self) {
        self.last_screen_hash = None;
    }

    /// Drain pending events from receiver.
    pub fn drain_events(&mut self) -> Vec<BrowserEvent> {
        let Some(ref mut rx) = self.event_rx else {
            return Vec::new();
        };

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }
}

/// Context needed for sending browser messages.
pub struct BrowserSendContext<'a> {
    /// Terminal output sender for encrypted relay.
    pub sender: &'a TerminalOutputSender,
    /// Async runtime for spawning send tasks.
    pub runtime: &'a tokio::runtime::Runtime,
}

impl std::fmt::Debug for BrowserSendContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserSendContext").finish_non_exhaustive()
    }
}

/// Build AgentInfo from agent data.
///
/// This is a helper to convert agent data into the format expected by browsers.
#[must_use]
pub fn build_agent_info(
    id: &str,
    agent: &crate::Agent,
    hub_identifier: &str,
) -> AgentInfo {
    AgentInfo {
        id: id.to_string(),
        repo: Some(agent.repo.clone()),
        issue_number: agent.issue_number.map(u64::from),
        branch_name: Some(agent.branch_name.clone()),
        name: None,
        status: Some(format!("{:?}", agent.status)),
        tunnel_port: agent.tunnel_port,
        server_running: Some(agent.is_server_running()),
        has_server_pty: Some(agent.has_server_pty()),
        active_pty_view: Some(format!("{:?}", agent.active_pty).to_lowercase()),
        scroll_offset: Some(agent.get_scroll_offset() as u32),
        hub_identifier: Some(hub_identifier.to_string()),
    }
}

/// Build WorktreeInfo from worktree data.
#[must_use]
pub fn build_worktree_info(path: &str, branch: &str) -> WorktreeInfo {
    let issue_number = branch
        .strip_prefix("botster-issue-")
        .and_then(|s| s.parse::<u64>().ok());
    WorktreeInfo {
        path: path.to_string(),
        branch: branch.to_string(),
        issue_number,
    }
}

/// Send agent list to connected browser.
pub fn send_agent_list(
    ctx: &BrowserSendContext,
    agents: Vec<AgentInfo>,
) {
    let message = TerminalMessage::Agents { agents };
    send_message(ctx, &message);
}

/// Send worktree list to connected browser.
pub fn send_worktree_list(
    ctx: &BrowserSendContext,
    worktrees: Vec<WorktreeInfo>,
) {
    let repo = WorktreeManager::detect_current_repo()
        .map(|(_, name)| name)
        .ok();

    let message = TerminalMessage::Worktrees { worktrees, repo };
    send_message(ctx, &message);
}

/// Send agent selection notification to browser.
pub fn send_agent_selected(ctx: &BrowserSendContext, agent_id: &str) {
    let message = TerminalMessage::AgentSelected {
        id: agent_id.to_string(),
    };
    send_message(ctx, &message);
}

/// Send terminal output to browser.
pub fn send_output(ctx: &BrowserSendContext, output: &str) {
    let sender = ctx.sender.clone();
    let output = output.to_string();
    ctx.runtime.spawn(async move {
        if let Err(e) = sender.send(&output).await {
            log::warn!("Failed to send output to browser: {e}");
        }
    });
}

/// Send a JSON message to browser.
fn send_message(ctx: &BrowserSendContext, message: &TerminalMessage) {
    let Ok(json) = serde_json::to_string(message) else {
        return;
    };

    let sender = ctx.sender.clone();
    ctx.runtime.spawn(async move {
        let _ = sender.send(&json).await;
    });
}

/// Calculate agent dimensions based on browser mode.
pub fn calculate_agent_dims(dims: &BrowserResize, mode: BrowserMode) -> (u16, u16) {
    match mode {
        BrowserMode::Gui => {
            log::info!(
                "GUI mode - using full browser dimensions: {}x{}",
                dims.cols,
                dims.rows
            );
            (dims.cols, dims.rows)
        }
        BrowserMode::Tui => {
            let tui_cols = (dims.cols * 70 / 100).saturating_sub(2);
            let tui_rows = dims.rows.saturating_sub(2);
            log::info!(
                "TUI mode - using 70% width: {}x{} (from {}x{})",
                tui_cols,
                tui_rows,
                dims.cols,
                dims.rows
            );
            (tui_cols, tui_rows)
        }
    }
}

/// Get output to send based on browser mode.
pub fn get_output_for_mode(
    mode: Option<BrowserMode>,
    tui_output: &str,
    agent_output: Option<String>,
) -> String {
    match mode {
        Some(BrowserMode::Gui) => {
            agent_output.unwrap_or_else(|| "\x1b[2J\x1b[HNo agent selected".to_string())
        }
        Some(BrowserMode::Tui) | None => tui_output.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_browser_state_default() {
        let state = BrowserState::default();
        assert!(!state.is_connected());
        assert!(state.dims.is_none());
    }

    #[test]
    fn test_browser_state_disconnect() {
        let mut state = BrowserState::default();
        state.connected = true;
        state.dims = Some(BrowserResize { rows: 24, cols: 80 });
        state.last_screen_hash = Some(12345);

        state.disconnect();

        assert!(!state.connected);
        assert!(state.dims.is_none());
        assert!(state.last_screen_hash.is_none());
    }

    #[test]
    fn test_handle_connected() {
        let mut state = BrowserState::default();
        state.handle_connected("Test Device");

        assert!(state.connected);
        assert_eq!(state.mode, Some(BrowserMode::Gui));
    }

    #[test]
    fn test_handle_resize() {
        let mut state = BrowserState::default();
        state.last_screen_hash = Some(12345);

        let (rows, cols) = state.handle_resize(BrowserResize { rows: 40, cols: 120 });

        assert_eq!(rows, 40);
        assert_eq!(cols, 120);
        assert!(state.dims.is_some());
        assert!(state.last_screen_hash.is_none()); // Invalidated
    }

    #[test]
    fn test_handle_set_mode_gui() {
        let mut state = BrowserState::default();
        state.handle_set_mode("gui");
        assert_eq!(state.mode, Some(BrowserMode::Gui));
    }

    #[test]
    fn test_handle_set_mode_tui() {
        let mut state = BrowserState::default();
        state.handle_set_mode("tui");
        assert_eq!(state.mode, Some(BrowserMode::Tui));
    }

    #[test]
    fn test_calculate_agent_dims_gui() {
        let dims = BrowserResize { rows: 40, cols: 120 };
        let (cols, rows) = calculate_agent_dims(&dims, BrowserMode::Gui);
        assert_eq!(cols, 120);
        assert_eq!(rows, 40);
    }

    #[test]
    fn test_calculate_agent_dims_tui() {
        let dims = BrowserResize { rows: 40, cols: 100 };
        let (cols, rows) = calculate_agent_dims(&dims, BrowserMode::Tui);
        // 70% of 100 = 70, minus 2 = 68
        assert_eq!(cols, 68);
        // 40 minus 2 = 38
        assert_eq!(rows, 38);
    }

    #[test]
    fn test_get_output_for_mode_gui() {
        let output = get_output_for_mode(
            Some(BrowserMode::Gui),
            "tui stuff",
            Some("agent output".to_string()),
        );
        assert_eq!(output, "agent output");
    }

    #[test]
    fn test_get_output_for_mode_tui() {
        let output = get_output_for_mode(
            Some(BrowserMode::Tui),
            "tui stuff",
            Some("agent output".to_string()),
        );
        assert_eq!(output, "tui stuff");
    }
}
