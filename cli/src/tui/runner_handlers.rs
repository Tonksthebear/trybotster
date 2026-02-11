//! TUI Runner Handlers - action handlers and Lua message processing.
//!
//! This module contains the handler methods that process TUI actions and
//! incoming Lua messages. These are extracted from `TuiRunner` to keep the
//! main module focused on the event loop and state management.
//!
//! # Handler Categories
//!
//! - [`handle_tui_action`] - Processes `TuiAction` variants (UI state changes)
//! - [`handle_pty_view_toggle`] - Toggles between CLI and Server PTY views
//! - [`handle_lua_message`] - Processes JSON messages from Lua event system

// Rust guideline compliant 2026-02

use std::sync::{Arc, Mutex};

use ratatui::backend::Backend;
use vt100::Parser;

use super::actions::TuiAction;
use super::runner::{TuiRunner, DEFAULT_SCROLLBACK};

impl<B> TuiRunner<B>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    /// Handle a TUI action generated from input.
    ///
    /// TUI actions are handled locally (UI state changes). This is the main
    /// dispatch function for all user interactions converted to actions.
    ///
    /// # Action Categories
    ///
    /// - Application Control: `Quit`
    /// - Modal State: `OpenMenu`, `CloseModal`
    /// - Menu Navigation: `MenuUp`, `MenuDown`, `MenuSelect`
    /// - Worktree Selection: `WorktreeUp`, `WorktreeDown`, `WorktreeSelect`
    /// - Text Input: `InputChar`, `InputBackspace`, `InputSubmit`
    /// - Connection Code: `ShowConnectionCode`, `RegenerateConnectionCode`, `CopyConnectionUrl`
    /// - Agent Close: `ConfirmCloseAgent`, `ConfirmCloseAgentDeleteWorktree`
    /// - Scrolling: `ScrollUp`, `ScrollDown`, `ScrollToTop`, `ScrollToBottom`
    /// - Agent Navigation: `SelectNext`, `SelectPrevious`
    /// - PTY View: `TogglePtyView`
    pub fn handle_tui_action(&mut self, action: TuiAction) {
        use crate::app::AppMode;

        match action {
            // === Application Control ===
            TuiAction::Quit => {
                self.quit = true;
                self.send_msg(serde_json::json!({
                    "subscriptionId": "tui_hub",
                    "data": { "type": "quit" }
                }));
            }

            // === Modal State ===
            TuiAction::OpenMenu => {
                self.mode = AppMode::Menu;
                self.menu_selected = 0;
            }

            TuiAction::CloseModal => {
                // Delete Kitty graphics images if closing ConnectionCode modal
                self.mode = AppMode::Normal;
                self.input_buffer.clear();
            }

            // === Menu Navigation ===
            TuiAction::MenuUp => {
                if self.menu_selected > 0 {
                    self.menu_selected -= 1;
                }
            }

            TuiAction::MenuDown => {
                // Use dynamic menu's selectable item count, not static constant
                let menu_context = self.build_menu_context();
                let menu_items = crate::tui::menu::build_menu(&menu_context);
                let max_idx = crate::tui::menu::selectable_count(&menu_items).saturating_sub(1);
                self.menu_selected = (self.menu_selected + 1).min(max_idx);
            }

            TuiAction::MenuSelect(idx) => {
                self.handle_menu_select(idx);
            }

            // === Worktree Selection ===
            TuiAction::WorktreeUp => {
                if self.worktree_selected > 0 {
                    self.worktree_selected -= 1;
                }
            }

            TuiAction::WorktreeDown => {
                let max = self.available_worktrees.len();
                self.worktree_selected = (self.worktree_selected + 1).min(max);
            }

            TuiAction::WorktreeSelect(idx) => {
                self.handle_worktree_select(idx);
            }

            // === Text Input ===
            TuiAction::InputChar(c) => {
                self.input_buffer.push(c);
            }

            TuiAction::InputBackspace => {
                self.input_buffer.pop();
            }

            TuiAction::InputSubmit => {
                self.handle_input_submit();
            }

            // === Connection Code ===
            TuiAction::ShowConnectionCode => {
                self.mode = AppMode::ConnectionCode;
                // Request connection code via Lua protocol
                self.send_msg(serde_json::json!({
                    "subscriptionId": "tui_hub",
                    "data": { "type": "get_connection_code" }
                }));
            }

            TuiAction::RegenerateConnectionCode => {
                // Send via Lua client protocol (same path as browser).
                self.send_msg(serde_json::json!({
                    "subscriptionId": "tui_hub",
                    "data": {
                        "type": "regenerate_connection_code",
                    }
                }));
                // Clear cache and request fresh code
                self.connection_code = None;
                self.send_msg(serde_json::json!({
                    "subscriptionId": "tui_hub",
                    "data": { "type": "get_connection_code" }
                }));
            }

            TuiAction::CopyConnectionUrl => {
                self.send_msg(serde_json::json!({
                    "subscriptionId": "tui_hub",
                    "data": { "type": "copy_connection_url" }
                }));
            }

            // === Agent Close Confirmation ===
            TuiAction::ConfirmCloseAgent => {
                self.handle_confirm_close_agent(false);
            }

            TuiAction::ConfirmCloseAgentDeleteWorktree => {
                self.handle_confirm_close_agent(true);
            }

            // === Scrolling (local to TUI parser) ===
            TuiAction::ScrollUp(lines) => {
                crate::tui::scroll::up_parser(&self.vt100_parser, lines);
            }

            TuiAction::ScrollDown(lines) => {
                crate::tui::scroll::down_parser(&self.vt100_parser, lines);
            }

            TuiAction::ScrollToTop => {
                crate::tui::scroll::to_top_parser(&self.vt100_parser);
            }

            TuiAction::ScrollToBottom => {
                crate::tui::scroll::to_bottom_parser(&self.vt100_parser);
            }

            // === Agent Navigation (request from Hub) ===
            TuiAction::SelectNext => {
                self.request_select_next();
            }

            TuiAction::SelectPrevious => {
                self.request_select_previous();
            }

            // === PTY View Toggle ===
            TuiAction::TogglePtyView => {
                self.handle_pty_view_toggle();
            }

            TuiAction::None => {}
        }
    }

    /// Handle PTY view toggle action.
    ///
    /// Cycles through available PTY sessions for the current agent using the
    /// Lua subscribe/unsubscribe protocol. Eagerly subscribes to the new PTY
    /// for immediate responsiveness, and updates `active_subscriptions` so
    /// `sync_subscriptions()` stays consistent.
    /// Wraps around: after the last session, returns to session 0.
    pub(super) fn handle_pty_view_toggle(&mut self) {
        let Some(agent_index) = self.current_agent_index else {
            log::debug!("Cannot toggle PTY view - no agent selected");
            return;
        };

        // Determine session count from the selected agent's info
        let session_count = self
            .selected_agent
            .as_ref()
            .and_then(|key| self.agents.iter().find(|a| a.id == *key))
            .and_then(|a| a.sessions.as_ref())
            .map_or(1, |s| s.len().max(1));

        // Cycle to next session (wrap around)
        let new_index = (self.active_pty_index + 1) % session_count;

        log::debug!(
            "Cycling PTY view to index {} (of {}) for agent index {}",
            new_index,
            session_count,
            agent_index
        );

        // Unsubscribe from current focused PTY
        if let Some(ref sub_id) = self.current_terminal_sub_id {
            self.send_msg(serde_json::json!({
                "type": "unsubscribe",
                "subscriptionId": sub_id,
            }));
            if let Some(pi) = self.current_pty_index {
                self.active_subscriptions.remove(&(agent_index, pi));
            }
        }

        // Point vt100_parser at the pool entry for the new PTY.
        let (rows, cols) = self.terminal_dims;
        let parser = self.parser_pool
            .entry((agent_index, new_index))
            .or_insert_with(|| Arc::new(Mutex::new(Parser::new(rows, cols, DEFAULT_SCROLLBACK))))
            .clone();
        self.vt100_parser = parser;

        // Eagerly subscribe to new PTY via Lua protocol
        let sub_id = format!("tui:{}:{}", agent_index, new_index);
        self.send_msg(serde_json::json!({
            "type": "subscribe",
            "channel": "terminal",
            "subscriptionId": sub_id,
            "params": {
                "agent_index": agent_index,
                "pty_index": new_index,
            }
        }));

        self.active_pty_index = new_index;
        self.current_pty_index = Some(new_index);
        self.current_terminal_sub_id = Some(sub_id);
        self.active_subscriptions.insert((agent_index, new_index));
    }

    /// Handle a JSON message from the Lua event system.
    ///
    /// These messages arrive via `tui.send()` in Lua and carry agent lifecycle
    /// events broadcast by `broadcast_hub_event()` to all hub-subscribed clients,
    /// plus responses to explicit requests (connection_code, agent_list, etc.).
    ///
    /// # Message Types
    ///
    /// - `agent_created` -- Add agent to cache, auto-select
    /// - `agent_deleted` -- Remove agent from cache, clear selection if active
    /// - `agent_status_changed` -- Update cached agent status in-place
    /// - `agent_list` -- Full agent list refresh (initial subscription, explicit request)
    /// - `worktree_list` -- Update cached worktree list
    /// - `connection_code` -- Cache connection URL and generate QR PNG locally
    /// - `connection_code_error` -- Clear cached connection code
    /// - `subscribed` -- Subscription confirmation (logged, no action needed)
    /// - `error` -- Generic Lua protocol error (logged)
    // Rust guideline compliant 2026-02
    pub(super) fn handle_lua_message(&mut self, msg: serde_json::Value) {
        let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match msg_type {
            "agent_created" => {
                // Clear the "creating" indicator
                self.creating_agent = None;

                // Add/update agent in local cache from event data
                if let Some(agent) = msg.get("agent") {
                    if let Some(info) = parse_agent_info(agent) {
                        // Remove existing entry if present (in case of duplicate events)
                        self.agents.retain(|a| a.id != info.id);
                        self.agents.push(info);
                    }
                    // Auto-select the new agent
                    if let Some(agent_id) = agent.get("id").and_then(|v| v.as_str()) {
                        self.request_select_agent(agent_id);
                    }
                }
            }
            "agent_deleted" => {
                if let Some(agent_id) = msg.get("agent_id").and_then(|v| v.as_str()) {
                    // Clear selection if the deleted agent was active
                    if self.selected_agent.as_deref() == Some(agent_id) {
                        self.selected_agent = None;
                        self.current_agent_index = None;
                        self.current_pty_index = None;
                    }
                    // Remove from cached list
                    self.agents.retain(|a| a.id != agent_id);
                }
            }
            "agent_status_changed" => {
                if let (Some(agent_id), Some(status)) = (
                    msg.get("agent_id").and_then(|v| v.as_str()),
                    msg.get("status").and_then(|v| v.as_str()),
                ) {
                    // Update creation progress display based on lifecycle status
                    match status {
                        "creating_worktree" => {
                            self.creating_agent = Some((
                                agent_id.to_string(),
                                crate::tui::events::CreationStage::CreatingWorktree,
                            ));
                        }
                        "spawning_ptys" => {
                            self.creating_agent = Some((
                                agent_id.to_string(),
                                crate::tui::events::CreationStage::SpawningAgent,
                            ));
                        }
                        "running" | "failed" => {
                            // Clear creation progress on completion or failure
                            self.creating_agent = None;
                        }
                        "stopping" | "removing_worktree" | "deleted" => {
                            // Clear creation progress if somehow still showing
                            if self.creating_agent.as_ref().map(|(id, _)| id.as_str()) == Some(agent_id) {
                                self.creating_agent = None;
                            }
                        }
                        _ => {}
                    }

                    // Update existing agent status if present in list
                    if let Some(agent) = self.agents.iter_mut().find(|a| a.id == agent_id) {
                        agent.status = Some(status.to_string());
                    }
                }
            }
            "agent_list" => {
                if let Some(agents) = msg.get("agents").and_then(|v| v.as_array()) {
                    self.agents = agents
                        .iter()
                        .filter_map(|a| parse_agent_info(a))
                        .collect();
                    log::debug!("Updated agent list: {} agents", self.agents.len());
                }
            }
            "worktree_list" => {
                if let Some(worktrees) = msg.get("worktrees").and_then(|v| v.as_array()) {
                    self.available_worktrees = worktrees
                        .iter()
                        .filter_map(|w| {
                            let path = w.get("path").and_then(|v| v.as_str())?;
                            let branch = w.get("branch").and_then(|v| v.as_str())?;
                            Some((path.to_string(), branch.to_string()))
                        })
                        .collect();
                }
            }
            "connection_code" => {
                let url = msg.get("url").and_then(|v| v.as_str());
                let qr_ascii = msg.get("qr_ascii").and_then(|v| v.as_array());

                if let (Some(url), Some(qr_array)) = (url, qr_ascii) {
                    let qr_lines: Vec<String> = qr_array
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();

                    let qr_width = qr_lines.first().map(|l| l.chars().count() as u16).unwrap_or(0);
                    let qr_height = qr_lines.len() as u16;
                    self.connection_code = Some(crate::tui::ConnectionCodeData {
                        url: url.to_string(),
                        qr_ascii: qr_lines,
                        qr_width,
                        qr_height,
                    });
                } else {
                    log::warn!("connection_code message missing url or qr_ascii");
                    self.connection_code = None;
                }
            }
            "connection_code_error" => {
                log::warn!(
                    "Connection code error: {}",
                    msg.get("error").and_then(|v| v.as_str()).unwrap_or("unknown")
                );
                self.connection_code = None;
            }
            "subscribed" => {
                // Subscription confirmation from client.lua. Browser clients use
                // this to gate input; TUI doesn't need to gate but we log for
                // protocol traceability.
                log::debug!(
                    "Subscription confirmed: {}",
                    msg.get("subscriptionId").and_then(|v| v.as_str()).unwrap_or("?")
                );
            }
            "error" => {
                let error = msg.get("error").and_then(|v| v.as_str()).unwrap_or("unknown");
                log::error!("Lua protocol error: {}", error);
            }
            _ => {
                log::trace!("Unhandled Lua message type: {}", msg_type);
            }
        }
    }
}

/// Parse agent info from a JSON value received via Lua events.
///
/// Converts the JSON agent object (from Lua agent events) into
/// an `AgentInfo` struct. Returns `None` if the `id` field is missing.
// Rust guideline compliant 2026-02
fn parse_agent_info(value: &serde_json::Value) -> Option<crate::relay::AgentInfo> {
    let id = value.get("id").and_then(|v| v.as_str())?.to_string();

    // Parse sessions array if present
    let sessions = value.get("sessions").and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|s| {
                let name = s.get("name").and_then(|v| v.as_str())?.to_string();
                let port_forward = s.get("port_forward").and_then(|v| v.as_bool()).unwrap_or(false);
                let port = s.get("port").and_then(|v| v.as_u64()).and_then(|p| u16::try_from(p).ok());
                Some(crate::relay::SessionInfo { name, port_forward, port })
            })
            .collect()
    });

    Some(crate::relay::AgentInfo {
        id,
        repo: value.get("repo").and_then(|v| v.as_str()).map(String::from),
        issue_number: value.get("issue_number").and_then(|v| v.as_u64()),
        branch_name: value.get("branch_name").and_then(|v| v.as_str()).map(String::from),
        name: value.get("display_name").and_then(|v| v.as_str())
            .or_else(|| value.get("name").and_then(|v| v.as_str()))
            .map(String::from),
        status: value.get("status").and_then(|v| v.as_str()).map(String::from),
        sessions,
        port: value.get("port").and_then(|v| v.as_u64()).and_then(|p| u16::try_from(p).ok()),
        server_running: value.get("server_running").and_then(|v| v.as_bool()),
        has_server_pty: value.get("has_server_pty").and_then(|v| v.as_bool()),
        scroll_offset: value.get("scroll_offset").and_then(|v| v.as_u64()).map(|s| s as u32),
        hub_identifier: value.get("hub_identifier").and_then(|v| v.as_str()).map(String::from),
    })
}
