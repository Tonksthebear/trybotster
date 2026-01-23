//! Client routing and selection helpers for Hub.
//!
//! This module contains methods for managing client-agent associations,
//! including TUI selection state, agent navigation, and client communication.
//!
//! # Client Model
//!
//! Each client (TUI, browser) maintains its own selected agent. The registry
//! tracks these selections, allowing independent navigation per client.
//!
//! # Key Functions
//!
//! - Selection: `get_tui_selected_agent_key()`, `with_selected_agent()`
//! - Navigation: `get_next_agent_key()`, `get_previous_agent_key()`
//! - Communication: `send_agent_list_to()`, `broadcast_agent_list()`

// Rust guideline compliant 2025-01

use crate::client::ClientId;
use crate::hub::actions::{self, HubAction};
use crate::hub::Hub;
use crate::relay::AgentInfo;

impl Hub {
    /// Execute a closure with a reference to the currently selected agent for TUI.
    ///
    /// This uses `TuiClient.state().selected_agent` as the source of truth,
    /// NOT `HubState.selected`. This is part of the client abstraction.
    ///
    /// Returns `None` if no agent is selected or the agent doesn't exist.
    pub fn with_selected_agent<T, F>(&self, f: F) -> Option<T>
    where
        F: FnOnce(&crate::agent::Agent) -> T,
    {
        let key = self.get_tui_selected_agent_key()?;
        let state = self.state.read().unwrap();
        state.agents.get(&key).map(f)
    }

    /// Execute a closure with a mutable reference to the currently selected agent for TUI.
    ///
    /// Returns `None` if no agent is selected or the agent doesn't exist.
    pub fn with_selected_agent_mut<T, F>(&mut self, f: F) -> Option<T>
    where
        F: FnOnce(&mut crate::agent::Agent) -> T,
    {
        let key = self.get_tui_selected_agent_key()?;
        let mut state = self.state.write().unwrap();
        state.agents.get_mut(&key).map(f)
    }

    /// Check if TUI has a selected agent.
    #[must_use]
    pub fn has_selected_agent(&self) -> bool {
        if let Some(key) = self.get_tui_selected_agent_key() {
            self.state.read().unwrap().agents.contains_key(&key)
        } else {
            false
        }
    }

    /// Get the selected agent key from TuiClient.
    ///
    /// Get the TUI client's selected agent key from the registry.
    /// This replaces the old `hub.state.selected` index-based approach.
    #[must_use]
    pub fn get_tui_selected_agent_key(&self) -> Option<String> {
        self.clients.selected_agent(&ClientId::Tui).map(String::from)
    }

    /// Ensure TUI has a valid selection if agents exist.
    ///
    /// This prevents the visual fallback mismatch where render.rs shows
    /// the first agent (index 0) but TuiClient.selected_agent is None,
    /// causing input to not route anywhere.
    ///
    /// Called from tick() to sync TUI selection with visual display.
    pub(crate) fn ensure_tui_selection(&mut self) {
        let state = self.state.read().unwrap();

        // If no agents exist, nothing to select
        if state.agent_keys_ordered.is_empty() {
            return;
        }

        // Check current TUI selection
        let current_selection = self.get_tui_selected_agent_key();

        // If no selection, select the first agent
        if current_selection.is_none() {
            if let Some(first_key) = state.agent_keys_ordered.first().cloned() {
                drop(state); // Release lock before dispatch
                log::debug!(
                    "Auto-selecting first agent {} for TUI (was None)",
                    first_key
                );
                actions::dispatch(
                    self,
                    HubAction::SelectAgentForClient {
                        client_id: ClientId::Tui,
                        agent_key: first_key,
                    },
                );
            }
            return;
        }

        // If selection exists but agent doesn't (deleted), select first agent
        let selection = current_selection.unwrap();
        if !state.agents.contains_key(&selection) {
            if let Some(first_key) = state.agent_keys_ordered.first().cloned() {
                drop(state); // Release lock before dispatch
                log::debug!(
                    "Auto-selecting first agent {} for TUI (previous {} no longer exists)",
                    first_key,
                    selection
                );
                actions::dispatch(
                    self,
                    HubAction::SelectAgentForClient {
                        client_id: ClientId::Tui,
                        agent_key: first_key,
                    },
                );
            }
        }
    }

    /// Get the next agent key for a client's navigation.
    ///
    /// Returns the next agent in the ordered list, wrapping around.
    /// If no agent is selected, returns the first agent.
    #[must_use]
    pub fn get_next_agent_key(&self, client_id: &ClientId) -> Option<String> {
        let state = self.state.read().unwrap();
        if state.agent_keys_ordered.is_empty() {
            return None;
        }

        let current = self.clients.selected_agent(client_id);

        match current {
            Some(key) => {
                let idx = state
                    .agent_keys_ordered
                    .iter()
                    .position(|k| k == key)
                    .unwrap_or(0);
                let next_idx = (idx + 1) % state.agent_keys_ordered.len();
                Some(state.agent_keys_ordered[next_idx].clone())
            }
            None => Some(state.agent_keys_ordered[0].clone()),
        }
    }

    /// Get the previous agent key for a client's navigation.
    ///
    /// Returns the previous agent in the ordered list, wrapping around.
    /// If no agent is selected, returns the last agent.
    #[must_use]
    pub fn get_previous_agent_key(&self, client_id: &ClientId) -> Option<String> {
        let state = self.state.read().unwrap();
        if state.agent_keys_ordered.is_empty() {
            return None;
        }

        let current = self.clients.selected_agent(client_id);

        match current {
            Some(key) => {
                let idx = state
                    .agent_keys_ordered
                    .iter()
                    .position(|k| k == key)
                    .unwrap_or(0);
                let prev_idx = if idx == 0 {
                    state.agent_keys_ordered.len() - 1
                } else {
                    idx - 1
                };
                Some(state.agent_keys_ordered[prev_idx].clone())
            }
            None => Some(state.agent_keys_ordered.last()?.clone()),
        }
    }

    /// Build the agent list for sending to clients.
    pub(crate) fn build_agent_list(&self) -> Vec<AgentInfo> {
        let state = self.state.read().unwrap();
        state
            .agents
            .iter()
            .map(|(key, agent)| AgentInfo {
                id: key.clone(),
                repo: Some(agent.repo.clone()),
                issue_number: agent.issue_number.map(u64::from),
                branch_name: Some(agent.branch_name.clone()),
                name: None, // Agent doesn't have a separate name field
                status: Some(format!("{:?}", agent.status)),
                tunnel_port: agent.tunnel_port,
                server_running: Some(agent.server_pty.is_some()),
                has_server_pty: Some(agent.server_pty.is_some()),
                active_pty_view: None, // Not tracked at Agent level
                scroll_offset: None,   // Not tracked at Agent level
                hub_identifier: Some(self.hub_identifier.clone()),
            })
            .collect()
    }

    /// Send agent list to a specific client.
    ///
    /// For browser clients, this sends via relay. For TUI, the data is available
    /// through hub state (TuiClient reads directly from hub.state).
    pub fn send_agent_list_to(&mut self, client_id: &ClientId) {
        // Browser clients receive data via relay module
        if let ClientId::Browser(ref identity) = client_id {
            if let Some(ref sender) = self.browser.sender {
                let ctx = crate::relay::BrowserSendContext {
                    sender,
                    runtime: &self.tokio_runtime,
                };
                let agents = self.build_agent_list();
                crate::relay::send_agent_list_to(&ctx, identity, agents);
            }
        }
        // TUI reads agent list directly from hub state
    }

    /// Send worktree list to a specific client.
    ///
    /// For browser clients, this sends via relay. For TUI, data is available
    /// through hub state.
    pub fn send_worktree_list_to(&mut self, client_id: &ClientId) {
        // Browser clients receive data via relay module
        if let ClientId::Browser(ref identity) = client_id {
            if let Some(ref sender) = self.browser.sender {
                let ctx = crate::relay::BrowserSendContext {
                    sender,
                    runtime: &self.tokio_runtime,
                };
                let worktrees: Vec<crate::relay::WorktreeInfo> = self
                    .state
                    .read()
                    .unwrap()
                    .available_worktrees
                    .iter()
                    .map(|(path, branch)| crate::relay::WorktreeInfo {
                        path: path.clone(),
                        branch: branch.clone(),
                        issue_number: None,
                    })
                    .collect();
                crate::relay::send_worktree_list_to(&ctx, identity, worktrees);
            }
        }
        // TUI reads worktree list directly from hub state
    }

    /// Send error response to a specific client.
    ///
    /// Currently logs errors. Browser error messaging can be added via relay.
    pub fn send_error_to(&mut self, client_id: &ClientId, message: String) {
        // TODO: Add relay function for browser error messages if needed
        log::error!("Error for client {}: {}", client_id, message);
    }

    /// Broadcast agent list to all connected clients.
    ///
    /// Sends via relay for browser clients.
    pub fn broadcast_agent_list(&mut self) {
        // Send to all browsers via relay
        crate::relay::browser::send_agent_list(self);
        // TUI reads directly from hub state
    }
}
