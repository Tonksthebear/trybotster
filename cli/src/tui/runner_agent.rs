//! TUI Runner Agent Navigation - agent selection and navigation logic.
//!
//! This module contains the methods for navigating between agents in the TUI.
//! Navigation is handled locally using the cached agent list, with TuiClient
//! managing PTY connections through the Client trait.
//!
//! # Navigation Flow
//!
//! 1. User presses Ctrl+J/K (next/previous)
//! 2. TuiRunner computes next agent index from local cache
//! 3. TuiRunner sends `TuiRequest::SelectAgent` to TuiClient
//! 4. TuiClient uses `Client::select_agent()` to connect to PTY and return metadata
//! 5. TuiRunner updates its state with the metadata
//!
//! # Why TuiRequest::SelectAgent (Clean Separation)
//!
//! TuiRunner should only interface with TuiClient through `TuiRequest`, not hold
//! `AgentHandle` directly. The `Client::select_agent()` method:
//! - Handles PTY connection logic in the Client trait
//! - Returns only the metadata TuiRunner needs (agent_id, index, has_server_pty)
//! - Maintains clean separation between TuiRunner (renderer) and Client (I/O layer)

// Rust guideline compliant 2026-01

use ratatui::backend::Backend;
use vt100::Parser;

use crate::agent::PtyView;
use crate::client::{TuiAgentMetadata, TuiRequest};

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

    /// Request to select a specific agent by index via TuiClient.
    ///
    /// Sends a `TuiRequest::SelectAgent` to TuiClient. TuiClient uses
    /// `Client::select_agent()` which:
    /// 1. Looks up the agent via HandleCache
    /// 2. Connects to the agent's CLI PTY
    /// 3. Returns metadata for TuiRunner to update its state
    ///
    /// # Arguments
    ///
    /// * `index` - The display index of the agent to select (0-based)
    pub fn request_select_agent_by_index(&mut self, index: usize) {
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        if self.request_tx.send(TuiRequest::SelectAgent { index, response_tx }).is_err() {
            log::error!("Failed to send select agent request");
            return;
        }

        match response_rx.blocking_recv() {
            Ok(Some(metadata)) => self.apply_agent_metadata(metadata),
            Ok(None) => log::warn!("Agent at index {} not found", index),
            Err(_) => log::error!("Select agent response channel closed"),
        }
    }

    /// Apply agent metadata after selection.
    ///
    /// This method is called after TuiClient processes `TuiRequest::SelectAgent`.
    /// TuiClient has already connected to the agent's CLI PTY via `Client::select_agent()`.
    ///
    /// TuiRunner just needs to:
    /// 1. Reset to CLI view (default when switching agents)
    /// 2. Reset the parser for fresh output
    /// 3. Update local state (agent_id, index, has_server_pty)
    ///
    /// # Scrollback Flow
    ///
    /// TuiClient's `connect_to_pty()` sends scrollback through the output channel
    /// as `TuiOutput::Scrollback`. TuiRunner's event loop receives it via
    /// `poll_pty_events()` and feeds to parser.
    ///
    /// # Arguments
    ///
    /// * `metadata` - Agent metadata returned from TuiClient
    fn apply_agent_metadata(&mut self, metadata: TuiAgentMetadata) {
        // Reset to CLI view when switching agents
        self.active_pty_view = PtyView::Cli;

        // Reset parser FIRST (before loading new scrollback)
        {
            let mut parser = self.vt100_parser.lock().expect("parser lock poisoned");
            let (rows, cols) = self.terminal_dims;
            *parser = Parser::new(rows, cols, DEFAULT_SCROLLBACK);
        }

        // Update TuiRunner state (PTY connection already done by TuiClient)
        self.selected_agent = Some(metadata.agent_id);
        self.current_agent_index = Some(metadata.agent_index);
        self.current_pty_index = Some(0); // CLI PTY
        self.has_server_pty = metadata.has_server_pty;
    }
}
