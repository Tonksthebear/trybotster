//! TUI Runner Handlers - action handlers and Lua message processing.
//!
//! This module contains the handler methods that process TUI actions and
//! incoming Lua messages. These are extracted from `TuiRunner` to keep the
//! main module focused on the event loop and state management.
//!
//! # Handler Categories
//!
//! - [`handle_tui_action`] - Processes generic `TuiAction` variants (UI state changes)
//! - [`handle_pty_view_toggle`] - Toggles between PTY sessions
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
    /// TUI actions are generic UI state changes. Application-specific workflow
    /// logic is handled by Lua compound actions (`actions.lua`), which return
    /// sequences of these generic operations.
    pub fn handle_tui_action(&mut self, action: TuiAction) {
        match action {
            TuiAction::Quit => {
                self.quit = true;
            }

            TuiAction::SetMode(mode) => {
                self.mode = mode;
                self.overlay_list_selected = 0;
                self.input_buffer.clear();
            }

            TuiAction::ListUp => {
                if self.overlay_list_selected > 0 {
                    self.overlay_list_selected -= 1;
                }
            }

            TuiAction::ListDown => {
                let max_idx = self.overlay_list_actions.len().saturating_sub(1);
                self.overlay_list_selected = (self.overlay_list_selected + 1).min(max_idx);
            }

            TuiAction::ListSelect(_) => {
                // ListSelect is handled by Lua compound actions via execute_lua_ops.
                // If it reaches here, it means Lua didn't handle it — log and ignore.
                log::warn!("ListSelect reached generic handler (should be handled by Lua)");
            }

            TuiAction::InputChar(c) => {
                self.input_buffer.push(c);
            }

            TuiAction::InputBackspace => {
                self.input_buffer.pop();
            }

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

            TuiAction::SendMessage(msg) => {
                self.send_msg(msg);
            }

            TuiAction::StoreField { key, value } => {
                self.pending_fields.insert(key, value);
            }

            TuiAction::ClearField(key) => {
                self.pending_fields.remove(&key);
            }

            TuiAction::ClearInput => {
                self.input_buffer.clear();
            }

            TuiAction::ResetList => {
                self.overlay_list_selected = 0;
            }

            TuiAction::None => {}
        }
    }

    /// Switch to a specific PTY session by index.
    ///
    /// Unsubscribes from the current PTY, points the parser at the target
    /// session, and subscribes to it. Updates `active_subscriptions` so
    /// `sync_subscriptions()` stays consistent.
    ///
    /// No-op if no agent is selected or the target index matches the current.
    pub(super) fn switch_to_pty(&mut self, target_index: usize) {
        let Some(agent_index) = self.current_agent_index else {
            log::debug!("Cannot switch PTY - no agent selected");
            return;
        };

        if self.current_pty_index == Some(target_index) {
            log::debug!("Already on PTY index {target_index}, skipping switch");
            return;
        }

        log::debug!(
            "Switching PTY to index {} for agent index {}",
            target_index,
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
            .entry((agent_index, target_index))
            .or_insert_with(|| Arc::new(Mutex::new(Parser::new(rows, cols, DEFAULT_SCROLLBACK))))
            .clone();
        self.vt100_parser = parser;

        // Eagerly subscribe to new PTY via Lua protocol
        let sub_id = format!("tui:{}:{}", agent_index, target_index);
        self.send_msg(serde_json::json!({
            "type": "subscribe",
            "channel": "terminal",
            "subscriptionId": sub_id,
            "params": {
                "agent_index": agent_index,
                "pty_index": target_index,
            }
        }));

        self.active_pty_index = target_index;
        self.current_pty_index = Some(target_index);
        self.current_terminal_sub_id = Some(sub_id);
        self.active_subscriptions.insert((agent_index, target_index));
    }

    /// Cycle to the next PTY session for the current agent.
    ///
    /// Wraps around: after the last session, returns to session 0.
    pub(super) fn handle_pty_view_toggle(&mut self) {
        let session_count = self
            .selected_agent
            .as_ref()
            .and_then(|key| self.agents.iter().find(|a| a.id == *key))
            .and_then(|a| a.sessions.as_ref())
            .map_or(1, |s| s.len().max(1));

        let new_index = (self.active_pty_index + 1) % session_count;
        self.switch_to_pty(new_index);
    }

    /// Handle a JSON message from the Lua event system.
    ///
    /// These messages arrive via `tui.send()` in Lua and carry agent lifecycle
    /// events broadcast by `broadcast_hub_event()` to all hub-subscribed clients,
    /// plus responses to explicit requests (connection_code, agent_list, etc.).
    pub(super) fn handle_lua_message(&mut self, msg: serde_json::Value) {
        let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match msg_type {
            "agent_created" => {
                // Clear the "creating" indicator
                self.pending_fields.remove("creating_agent_id");
                self.pending_fields.remove("creating_agent_stage");

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
                            self.pending_fields.insert(
                                "creating_agent_id".to_string(),
                                agent_id.to_string(),
                            );
                            self.pending_fields.insert(
                                "creating_agent_stage".to_string(),
                                "creating_worktree".to_string(),
                            );
                        }
                        "spawning_ptys" => {
                            self.pending_fields.insert(
                                "creating_agent_id".to_string(),
                                agent_id.to_string(),
                            );
                            self.pending_fields.insert(
                                "creating_agent_stage".to_string(),
                                "spawning_agent".to_string(),
                            );
                        }
                        "running" | "failed" => {
                            self.pending_fields.remove("creating_agent_id");
                            self.pending_fields.remove("creating_agent_stage");
                        }
                        "stopping" | "removing_worktree" | "deleted" => {
                            if self.pending_fields.get("creating_agent_id").map(|s| s.as_str()) == Some(agent_id) {
                                self.pending_fields.remove("creating_agent_id");
                                self.pending_fields.remove("creating_agent_stage");
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
