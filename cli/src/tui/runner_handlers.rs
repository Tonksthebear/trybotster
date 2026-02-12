//! TUI Runner Handlers - generic UI action processing.
//!
//! This module contains the handler method that processes generic `TuiAction`
//! variants (UI state changes like scroll, list navigation, input).
//!
//! Application-specific logic (agent lifecycle, event handling, navigation)
//! is handled by Lua modules (`actions.lua`, `events.lua`), which return
//! op sequences that Rust executes mechanically via `execute_lua_ops()`.

// Rust guideline compliant 2026-02

use ratatui::backend::Backend;

use super::actions::TuiAction;
use super::runner::TuiRunner;

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
}

/// Parse agent info from a JSON value received via Lua events.
///
/// Converts the JSON agent object (from Lua agent events) into
/// an `AgentInfo` struct. Returns `None` if the `id` field is missing.
// Rust guideline compliant 2026-02
pub(super) fn parse_agent_info(value: &serde_json::Value) -> Option<crate::relay::AgentInfo> {
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
