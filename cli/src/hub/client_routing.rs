//! Client communication helpers for Hub.
//!
//! This module contains methods for client communication:
//!
//! - Error handling: `send_error_to()`
//! - Agent list broadcast: `broadcast_agent_list()`
//!
//! Browser clients receive data via HubEvent subscriptions in their
//! `handle_hub_event()` methods. TUI reads directly from hub state.

// Rust guideline compliant 2026-01

use crate::client::ClientId;
use crate::hub::Hub;

impl Hub {
    /// Send error response to a specific client.
    ///
    /// Currently logs errors. Browser error messaging can be added via relay.
    pub fn send_error_to(&mut self, client_id: &ClientId, message: String) {
        // TODO: Add relay function for browser error messages if needed
        log::error!("Error for client {}: {}", client_id, message);
    }

    /// Broadcast agent list to all connected clients.
    ///
    /// Browser clients receive agent list updates via HubEvent::AgentCreated/AgentDeleted
    /// in their handle_hub_event() methods. TUI reads directly from hub state.
    /// This method is retained for compatibility but is effectively a no-op
    /// now that browser updates are handled via WebRTC in server_comms.rs.
    pub fn broadcast_agent_list(&mut self) {
        // Browser clients react to HubEvent broadcasts (AgentCreated, AgentDeleted)
        // TUI reads directly from hub state
        log::debug!("broadcast_agent_list: clients receive via HubEvent subscription");
    }
}
