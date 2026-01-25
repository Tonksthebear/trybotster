//! TUI Runner Agent Navigation - agent selection and navigation logic.
//!
//! This module contains the methods for navigating between agents in the TUI.
//! Navigation is handled locally using the cached agent list, with the Hub
//! providing agent handles when an agent is selected.
//!
//! # Navigation Flow
//!
//! 1. User presses Ctrl+J/K (next/previous)
//! 2. TuiRunner computes next agent index from local cache
//! 3. TuiRunner requests agent handle from Hub via `GetAgentByIndex` command
//! 4. Hub returns `AgentHandle` with PTY channels
//! 5. TuiRunner connects to the PTY and updates selection

// Rust guideline compliant 2026-01

use ratatui::backend::Backend;
use vt100::Parser;

use crate::agent::PtyView;
use crate::client::{Client, ClientId};
use crate::hub::{AgentHandle, HubAction, HubCommand};

use super::runner::{TuiRunner, DEFAULT_SCROLLBACK};

impl<B> TuiRunner<B>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    /// Request to select the next agent.
    ///
    /// Navigation is handled locally using the agent list. We compute the next
    /// agent index and then connect to it via the Hub.
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

    /// Request to select a specific agent by index via Hub.
    ///
    /// Sends a `GetAgentByIndex` command to the Hub and waits for the response.
    /// If successful, applies the agent handle to connect to the PTY.
    ///
    /// # Arguments
    ///
    /// * `index` - The display index of the agent to select (0-based)
    pub fn request_select_agent_by_index(&mut self, index: usize) {
        let (cmd, rx) = HubCommand::get_agent_by_index(index);
        if self.command_tx.inner().blocking_send(cmd).is_err() {
            log::error!("Failed to send GetAgentByIndex command");
            return;
        }
        match rx.blocking_recv() {
            Ok(Some(handle)) => self.apply_agent_handle(handle),
            Ok(None) => log::warn!("Agent at index {} not found", index),
            Err(_) => log::error!("Failed to receive GetAgentByIndex response"),
        }
    }

    /// Apply an agent handle - subscribe to PTY and update selection.
    ///
    /// This method:
    /// 1. Resets to CLI view (default when switching agents)
    /// 2. Connects the client to the agent's CLI PTY
    /// 3. Updates the selected agent (TuiRunner state) and notifies Hub registry
    /// 4. Stores the full handle for PTY view toggling
    /// 5. Clears and resets the parser for fresh output
    ///
    /// # Arguments
    ///
    /// * `handle` - The agent handle from the Hub containing PTY channels
    pub fn apply_agent_handle(&mut self, handle: AgentHandle) {
        // Reset to CLI view when switching agents
        self.active_pty_view = PtyView::Cli;

        // Get agent info from handle
        let agent_id = handle.agent_id().to_string();
        let agent_index = handle.agent_index();

        // CLI PTY is always at index 0
        if let Err(e) = self.client.connect_to_pty(agent_index, 0) {
            log::warn!("Failed to connect to PTY: {}", e);
        }

        // Update TuiRunner state
        self.selected_agent = Some(agent_id.clone());
        self.current_agent_index = Some(agent_index);
        self.current_pty_index = Some(0); // CLI PTY

        // Notify Hub of selection change to keep registry in sync.
        // The registry tracks client->agent mappings for input routing and viewer management.
        let action = HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: agent_id,
        };
        if let Err(e) = self.command_tx.dispatch_action_blocking(action) {
            log::warn!("Failed to notify Hub of TUI selection change: {}", e);
        }

        // Store full handle for PTY view toggling (TuiRunner-specific)
        self.agent_handle = Some(handle);

        // Clear and reset parser for new agent
        let mut parser = self.vt100_parser.lock().expect("parser lock poisoned");
        let (rows, cols) = self.terminal_dims;
        *parser = Parser::new(rows, cols, DEFAULT_SCROLLBACK);
    }
}
