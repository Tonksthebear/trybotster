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
use super::signal::PreKeyBundleData;
use super::types::{BrowserEvent, BrowserResize};
use crate::{AgentInfo, BrowserMode, TerminalMessage, WorktreeInfo, WorktreeManager};

/// Browser event with identity attached.
///
/// Tuple of (event, browser_identity) for client-scoped action routing.
pub type IdentifiedBrowserEvent = (BrowserEvent, String);

/// Browser connection state.
///
/// Consolidates all browser-related fields from Hub into a single struct.
#[derive(Default)]
pub struct BrowserState {
    /// Terminal output sender for encrypted relay.
    pub sender: Option<TerminalOutputSender>,
    /// Browser event receiver (events include browser identity).
    pub event_rx: Option<mpsc::Receiver<IdentifiedBrowserEvent>>,
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
    /// Signal PreKeyBundle data for QR code generation.
    pub signal_bundle: Option<PreKeyBundleData>,
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
    pub fn set_connected(&mut self, sender: TerminalOutputSender, rx: mpsc::Receiver<IdentifiedBrowserEvent>) {
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
    ///
    /// Returns events with their browser identity attached for client-scoped routing.
    pub fn drain_events(&mut self) -> Vec<IdentifiedBrowserEvent> {
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

/// Send agent creating notification to browser.
///
/// Sent immediately when agent creation begins, before blocking operations.
/// Allows browser to show loading state.
pub fn send_agent_creating(ctx: &BrowserSendContext, identifier: &str) {
    let message = TerminalMessage::AgentCreating {
        identifier: identifier.to_string(),
    };
    send_message(ctx, &message);
}

/// Send agent creating notification to a specific browser.
pub fn send_agent_creating_to(ctx: &BrowserSendContext, identity: &str, identifier: &str) {
    let message = TerminalMessage::AgentCreating {
        identifier: identifier.to_string(),
    };
    send_message_to(ctx, identity, &message);
}

/// Send agent creation progress update to all browsers.
pub fn send_agent_progress(
    ctx: &BrowserSendContext,
    identifier: &str,
    stage: super::types::AgentCreationStage,
) {
    let message = TerminalMessage::AgentCreatingProgress {
        identifier: identifier.to_string(),
        stage,
        message: stage.description().to_string(),
    };
    send_message(ctx, &message);
}

/// Send agent creation progress update to a specific browser.
pub fn send_agent_progress_to(
    ctx: &BrowserSendContext,
    identity: &str,
    identifier: &str,
    stage: super::types::AgentCreationStage,
) {
    let message = TerminalMessage::AgentCreatingProgress {
        identifier: identifier.to_string(),
        stage,
        message: stage.description().to_string(),
    };
    send_message_to(ctx, identity, &message);
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

/// Build a scrollback message from buffer lines.
///
/// This is a pure function for testability.
#[must_use]
pub fn build_scrollback_message(lines: Vec<String>) -> TerminalMessage {
    TerminalMessage::Scrollback { lines }
}

/// Send scrollback history to browser.
///
/// Called when an agent is selected so the browser can populate
/// xterm's scrollback buffer with historical output.
pub fn send_scrollback(ctx: &BrowserSendContext, lines: Vec<String>) {
    let message = build_scrollback_message(lines);
    send_message(ctx, &message);
}

/// Send a JSON message to all browsers (broadcast).
fn send_message(ctx: &BrowserSendContext, message: &TerminalMessage) {
    let Ok(json) = serde_json::to_string(message) else {
        return;
    };

    let sender = ctx.sender.clone();
    ctx.runtime.spawn(async move {
        let _ = sender.send(&json).await;
    });
}

/// Send a JSON message to a specific browser (targeted).
fn send_message_to(ctx: &BrowserSendContext, identity: &str, message: &TerminalMessage) {
    let Ok(json) = serde_json::to_string(message) else {
        return;
    };

    let sender = ctx.sender.clone();
    let identity = identity.to_string();
    ctx.runtime.spawn(async move {
        let _ = sender.send_to(&identity, &json).await;
    });
}

// === Targeted send functions (per-client routing) ===

/// Send agent list to a specific browser.
pub fn send_agent_list_to(
    ctx: &BrowserSendContext,
    identity: &str,
    agents: Vec<AgentInfo>,
) {
    let message = TerminalMessage::Agents { agents };
    send_message_to(ctx, identity, &message);
}

/// Send worktree list to a specific browser.
pub fn send_worktree_list_to(
    ctx: &BrowserSendContext,
    identity: &str,
    worktrees: Vec<WorktreeInfo>,
) {
    let repo = WorktreeManager::detect_current_repo()
        .map(|(_, name)| name)
        .ok();

    let message = TerminalMessage::Worktrees { worktrees, repo };
    send_message_to(ctx, identity, &message);
}

/// Send agent selection notification to a specific browser.
pub fn send_agent_selected_to(ctx: &BrowserSendContext, identity: &str, agent_id: &str) {
    let message = TerminalMessage::AgentSelected {
        id: agent_id.to_string(),
    };
    send_message_to(ctx, identity, &message);
}

/// Send scrollback history to a specific browser.
pub fn send_scrollback_to(ctx: &BrowserSendContext, identity: &str, lines: Vec<String>) {
    let message = build_scrollback_message(lines);
    send_message_to(ctx, identity, &message);
}

/// Send agent created confirmation to a specific browser.
pub fn send_agent_created_to(ctx: &BrowserSendContext, identity: &str, agent_id: &str) {
    let message = TerminalMessage::AgentCreated {
        id: agent_id.to_string(),
    };
    send_message_to(ctx, identity, &message);
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

    #[test]
    fn test_build_scrollback_message() {
        let lines = vec![
            "First line".to_string(),
            "Second line".to_string(),
            "Third line with \x1b[32mcolor\x1b[0m".to_string(),
        ];
        let message = build_scrollback_message(lines.clone());

        match message {
            TerminalMessage::Scrollback { lines: msg_lines } => {
                assert_eq!(msg_lines.len(), 3);
                assert_eq!(msg_lines[0], "First line");
                assert_eq!(msg_lines[2], "Third line with \x1b[32mcolor\x1b[0m");
            }
            _ => panic!("Expected Scrollback message"),
        }
    }

    #[test]
    fn test_build_scrollback_message_empty() {
        let message = build_scrollback_message(vec![]);

        match message {
            TerminalMessage::Scrollback { lines } => {
                assert!(lines.is_empty());
            }
            _ => panic!("Expected Scrollback message"),
        }
    }
}
