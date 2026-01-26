//! Client communication helpers for Hub.
//!
//! This module contains methods for client communication,
//! including building and sending agent/worktree lists and error messages.
//!
//! # Key Functions
//!
//! - Communication: `send_agent_list_to()`, `broadcast_agent_list()`
//! - Data: `build_agent_list()`
//! - Error handling: `send_error_to()`

// Rust guideline compliant 2026-01

use crate::client::ClientId;
use crate::hub::Hub;
use crate::relay::AgentInfo;

impl Hub {
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
