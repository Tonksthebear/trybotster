//! TUI Runner Agent Navigation - agent selection and navigation logic.
//!
//! This module contains the methods for navigating between agents in the TUI.
//! Navigation is handled locally using the cached agent list, with terminal
//! connections managed through the Lua subscribe/unsubscribe protocol (same
//! path as browser clients).
//!
//! Agent selection eagerly subscribes to the focused PTY for immediate
//! responsiveness. `sync_subscriptions()` in the render loop handles
//! additional bindings (e.g., multi-PTY layouts) and cleans up stale ones.
//!
//! # Navigation Flow
//!
//! 1. User presses Ctrl+J/K (next/previous)
//! 2. TuiRunner computes next agent index from local cache
//! 3. TuiRunner sends `unsubscribe` for current PTY, `subscribe` for new PTY
//! 4. TuiRunner updates local state (indices, parser pointer, sub ID)
//! 5. Next render: `sync_subscriptions()` reconciles any additional bindings

// Rust guideline compliant 2026-02

use std::sync::{Arc, Mutex};

use ratatui::backend::Backend;
use vt100::Parser;

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

    /// Select a specific agent by index via Lua subscription protocol.
    ///
    /// Eagerly subscribes to the new agent's CLI PTY for immediate
    /// responsiveness (keyboard input, PTY output). Updates `active_subscriptions`
    /// so `sync_subscriptions()` stays consistent.
    ///
    /// # Arguments
    ///
    /// * `index` - The display index of the agent to select (0-based)
    pub fn request_select_agent_by_index(&mut self, index: usize) {
        let Some(agent_info) = self.agents.get(index) else {
            log::warn!("Agent at index {} not found in local cache", index);
            return;
        };

        let agent_id = agent_info.id.clone();

        // Unsubscribe from current focused PTY (if any)
        if let Some(ref sub_id) = self.current_terminal_sub_id {
            self.send_msg(serde_json::json!({
                "type": "unsubscribe",
                "subscriptionId": sub_id,
            }));
            // Remove from active set (sync_subscriptions will re-add if still in tree)
            if let (Some(ai), Some(pi)) = (self.current_agent_index, self.current_pty_index) {
                self.active_subscriptions.remove(&(ai, pi));
            }
        }

        // Reset to CLI view when switching agents
        self.active_pty_index = 0;

        // Point vt100_parser at the pool entry for the new agent's CLI PTY.
        let (rows, cols) = self.terminal_dims;
        let parser = self.parser_pool
            .entry((index, 0))
            .or_insert_with(|| Arc::new(Mutex::new(Parser::new(rows, cols, DEFAULT_SCROLLBACK))))
            .clone();
        self.vt100_parser = parser;

        // Eagerly subscribe to new PTY via Lua protocol
        let sub_id = format!("tui:{}:{}", index, 0);
        self.send_msg(serde_json::json!({
            "type": "subscribe",
            "channel": "terminal",
            "subscriptionId": sub_id,
            "params": {
                "agent_index": index,
                "pty_index": 0,
            }
        }));

        // Update local state and active subscriptions
        self.current_terminal_sub_id = Some(sub_id);
        self.selected_agent = Some(agent_id);
        self.current_agent_index = Some(index);
        self.current_pty_index = Some(0);
        self.active_subscriptions.insert((index, 0));
    }
}
