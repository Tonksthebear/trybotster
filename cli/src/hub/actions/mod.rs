//! Hub actions - commands that can be dispatched to modify hub state.
//!
//! Actions represent user intent from any input source (TUI, browser, server).
//! The Hub processes actions uniformly regardless of their origin.
//!
//! # Module Structure
//!
//! - `mod.rs` - HubAction enum and dispatch() routing
//! - `agent_handlers.rs` - Agent lifecycle: close (server-initiated cleanup)
//! - `connection_handlers.rs` - Connection URL copy and regeneration
//!
//! # Agent Lifecycle Ownership
//!
//! Agent creation is fully owned by Lua (`handlers/agents.lua` + `lib/agent.lua`).
//! Rust retains only `CloseAgent` for server-initiated cleanup messages
//! (`agent_cleanup` events from command channel).
//!
//! # Dispatch
//!
//! The `dispatch()` function is the central handler for all actions. It pattern
//! matches on the action type and routes to the appropriate handler module.

mod agent_handlers;
mod connection_handlers;
#[cfg(test)]
mod tests;

use super::Hub;

/// Actions that can be dispatched to the Hub.
///
/// These represent high-level user intentions that modify hub state.
/// The Hub's `handle_action()` method processes these uniformly,
/// regardless of whether they came from keyboard input, browser events,
/// or server messages.
///
#[derive(Debug, Clone, PartialEq)]
pub enum HubAction {
    // === Agent Lifecycle ===
    /// Close an agent and optionally delete its worktree.
    ///
    /// Triggered by server `agent_cleanup` messages when an issue/PR is closed.
    CloseAgent {
        /// Session key of the agent to close.
        session_key: String,
        /// Whether to delete the worktree.
        delete_worktree: bool,
    },

    // === Connection ===
    /// Regenerate the connection QR code with a fresh PreKey.
    RegenerateConnectionCode,

    /// Copy connection URL to clipboard.
    CopyConnectionUrl,

    // === Application Control ===
    /// Request application shutdown.
    Quit,
}


/// Dispatch a hub action, modifying hub state accordingly.
///
/// This is the central dispatch point for all actions. TUI input,
/// browser events, and server messages all eventually become actions
/// that are processed here.
pub fn dispatch(hub: &mut Hub, action: HubAction) {
    match action {
        // === Simple Inline Handlers (1-3 lines) ===
        HubAction::Quit => {
            hub.quit = true;
        }

        // === Agent Lifecycle ===
        HubAction::CloseAgent {
            session_key,
            delete_worktree,
        } => {
            agent_handlers::handle_close_agent(hub, &session_key, delete_worktree);
        }

        // === Connection ===
        HubAction::CopyConnectionUrl => {
            connection_handlers::handle_copy_connection_url(hub);
        }

        HubAction::RegenerateConnectionCode => {
            connection_handlers::handle_regenerate_connection_code(hub);
        }
    }
}
