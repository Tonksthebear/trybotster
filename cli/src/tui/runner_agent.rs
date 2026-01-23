//! TUI Runner Agent Navigation - agent selection and navigation logic.
//!
//! This module contains the methods for navigating between agents in the TUI.
//! Navigation is handled locally using the cached agent list, with the Hub
//! providing agent handles when an agent is selected.
//!
//! # Navigation Flow
//!
//! 1. User presses Ctrl+J/K (next/previous)
//! 2. TuiRunner computes next agent key from local cache
//! 3. TuiRunner requests agent handle from Hub via `GetAgent` command
//! 4. Hub returns `AgentHandle` with PTY channels
//! 5. TuiRunner connects to the PTY and updates selection

// Rust guideline compliant 2026-01

use ratatui::backend::Backend;
use vt100::Parser;

use crate::agent::PtyView;
use crate::hub::{AgentHandle, HubCommand};

use super::runner::{TuiRunner, DEFAULT_SCROLLBACK};

impl<B> TuiRunner<B>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    /// Request to select the next agent.
    ///
    /// Navigation is handled locally using the agent list. We compute the next
    /// agent key and then connect to it via the Hub.
    ///
    /// The selection wraps around: after the last agent, it goes back to the first.
    pub fn request_select_next(&mut self) {
        if self.agents.is_empty() {
            return;
        }

        let next_key = match self.client.selected_agent() {
            Some(current) => {
                // Find current index and select next
                let current_idx = self.agents.iter().position(|a| a.id == current);
                let next_idx = match current_idx {
                    Some(idx) => (idx + 1) % self.agents.len(),
                    None => 0,
                };
                self.agents[next_idx].id.clone()
            }
            None => self.agents[0].id.clone(),
        };

        self.request_select_agent(&next_key);
    }

    /// Request to select the previous agent.
    ///
    /// Navigation is handled locally using the agent list.
    /// The selection wraps around: before the first agent, it goes to the last.
    pub fn request_select_previous(&mut self) {
        if self.agents.is_empty() {
            return;
        }

        let prev_key = match self.client.selected_agent() {
            Some(current) => {
                // Find current index and select previous
                let current_idx = self.agents.iter().position(|a| a.id == current);
                let prev_idx = match current_idx {
                    Some(idx) if idx > 0 => idx - 1,
                    Some(_) => self.agents.len() - 1,
                    None => 0,
                };
                self.agents[prev_idx].id.clone()
            }
            None => self.agents.last().map(|a| a.id.clone()).unwrap_or_default(),
        };

        if !prev_key.is_empty() {
            self.request_select_agent(&prev_key);
        }
    }

    /// Request to select a specific agent via Hub.
    ///
    /// Sends a `GetAgent` command to the Hub and waits for the response.
    /// If successful, applies the agent handle to connect to the PTY.
    ///
    /// # Arguments
    ///
    /// * `agent_id` - The ID of the agent to select
    pub fn request_select_agent(&mut self, agent_id: &str) {
        let (cmd, rx) = HubCommand::get_agent(agent_id);
        if self.command_tx.inner().blocking_send(cmd).is_err() {
            log::error!("Failed to send GetAgent command");
            return;
        }
        match rx.blocking_recv() {
            Ok(Ok(handle)) => self.apply_agent_handle(handle),
            Ok(Err(e)) => log::error!("Failed to get agent: {}", e),
            Err(_) => log::error!("Failed to receive GetAgent response"),
        }
    }

    /// Apply an agent handle - subscribe to PTY and update selection.
    ///
    /// This method:
    /// 1. Resets to CLI view (default when switching agents)
    /// 2. Connects the client to the agent's CLI PTY
    /// 3. Updates the selected agent
    /// 4. Stores the full handle for PTY view toggling
    /// 5. Clears and resets the parser for fresh output
    ///
    /// # Arguments
    ///
    /// * `handle` - The agent handle from the Hub containing PTY channels
    pub fn apply_agent_handle(&mut self, handle: AgentHandle) {
        // Reset to CLI view when switching agents
        self.client.set_active_pty_view(PtyView::Cli);

        // Connect client to the CLI PTY
        let agent_id = handle.agent_id().to_string();
        self.client
            .connect_to_pty(&agent_id, handle.cli_pty().clone());
        self.client.set_selected_agent(Some(&agent_id));

        // Store full handle for PTY view toggling (TuiRunner-specific)
        self.agent_handle = Some(handle);

        // Clear and reset parser for new agent
        let mut parser = self.vt100_parser.lock().expect("parser lock poisoned");
        let (rows, cols) = self.terminal_dims;
        *parser = Parser::new(rows, cols, DEFAULT_SCROLLBACK);
    }
}
