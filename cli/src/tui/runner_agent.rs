//! TUI Runner Agent Navigation - agent selection and navigation logic.
//!
//! This module contains the methods for navigating between agents in the TUI.
//! Navigation is handled locally using the cached agent list, with terminal
//! connections managed through the Lua subscribe/unsubscribe protocol (same
//! path as browser clients).
//!
//! # Navigation Flow
//!
//! 1. User presses Ctrl+J/K (next/previous)
//! 2. TuiRunner computes next agent index from local cache
//! 3. TuiRunner sends `unsubscribe` for current terminal (if any)
//! 4. TuiRunner sends `subscribe` for new terminal
//! 5. Lua `Client:on_message()` handles subscription, creates PTY forwarder
//! 6. TuiRunner updates local state from cached agent info

// Rust guideline compliant 2026-02

use ratatui::backend::Backend;
use vt100::Parser;

use crate::agent::PtyView;

use super::runner::{TuiRunner, DEFAULT_SCROLLBACK};

impl<B> TuiRunner<B>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    /// Request to select the next agent.
    ///
    /// Navigation is handled locally using the agent list. We compute the next
    /// agent index and then subscribe to its terminal via Lua.
    ///
    /// The selection wraps around: after the last agent, it goes back to the first.
    pub fn request_select_next(&mut self) {
        if self.agents.is_empty() {
            return;
        }

        let next_idx = match &self.selected_agent {
            Some(current) => {
                // Find current index and select next
                let current_idx = self.agents.iter().position(|a| a.id == *current);
                match current_idx {
                    Some(idx) => (idx + 1) % self.agents.len(),
                    None => 0,
                }
            }
            None => 0,
        };

        self.request_select_agent_by_index(next_idx);
    }

    /// Request to select the previous agent.
    ///
    /// Navigation is handled locally using the agent list.
    /// The selection wraps around: before the first agent, it goes to the last.
    pub fn request_select_previous(&mut self) {
        if self.agents.is_empty() {
            return;
        }

        let prev_idx = match &self.selected_agent {
            Some(current) => {
                // Find current index and select previous
                let current_idx = self.agents.iter().position(|a| a.id == *current);
                match current_idx {
                    Some(idx) if idx > 0 => idx - 1,
                    Some(_) => self.agents.len() - 1,
                    None => 0,
                }
            }
            None => self.agents.len().saturating_sub(1),
        };

        self.request_select_agent_by_index(prev_idx);
    }

    /// Request to select a specific agent by ID.
    ///
    /// Looks up the agent index in the local cache and delegates to
    /// `request_select_agent_by_index`.
    ///
    /// # Arguments
    ///
    /// * `agent_id` - The ID of the agent to select
    pub fn request_select_agent(&mut self, agent_id: &str) {
        let Some(index) = self.agents.iter().position(|a| a.id == agent_id) else {
            log::warn!("Agent not found in local cache: {}", agent_id);
            return;
        };
        self.request_select_agent_by_index(index);
    }

    /// Request to select a specific agent by index via Lua subscription protocol.
    ///
    /// Uses the same subscribe/unsubscribe protocol as browser clients:
    /// 1. Looks up agent metadata from local cache
    /// 2. Unsubscribes from current terminal (if any)
    /// 3. Subscribes to new agent's terminal
    /// 4. Updates local state from cached agent info
    ///
    /// # Arguments
    ///
    /// * `index` - The display index of the agent to select (0-based)
    pub fn request_select_agent_by_index(&mut self, index: usize) {
        // Look up agent from local cache
        let Some(agent_info) = self.agents.get(index) else {
            log::warn!("Agent at index {} not found in local cache", index);
            return;
        };

        let agent_id = agent_info.id.clone();

        // Unsubscribe from current terminal (if any)
        if let Some(ref sub_id) = self.current_terminal_sub_id {
            self.send_msg(serde_json::json!({
                "type": "unsubscribe",
                "subscriptionId": sub_id,
            }));
        }

        // Reset to CLI view when switching agents
        self.active_pty_view = PtyView::Cli;

        // Reset parser for fresh output
        {
            let mut parser = self.vt100_parser.lock().expect("parser lock poisoned");
            let (rows, cols) = self.terminal_dims;
            *parser = Parser::new(rows, cols, DEFAULT_SCROLLBACK);
        }

        // Subscribe to new terminal via Lua protocol
        let sub_id = "tui_term".to_string();
        self.send_msg(serde_json::json!({
            "type": "subscribe",
            "channel": "terminal",
            "subscriptionId": sub_id,
            "params": {
                "agent_index": index,
                "pty_index": 0,
            }
        }));

        // Update local state from cached agent info
        self.current_terminal_sub_id = Some(sub_id);
        self.selected_agent = Some(agent_id);
        self.current_agent_index = Some(index);
        self.current_pty_index = Some(0);
    }
}
