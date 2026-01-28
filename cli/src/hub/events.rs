//! Hub events for pub/sub broadcasting to clients.
//!
//! This module defines events that the Hub broadcasts to all connected clients.
//! Events are sent via `tokio::sync::broadcast` channels, enabling clients to
//! receive hub-level notifications without polling.
//!
//! # Event Types
//!
//! - [`HubEvent::AgentCreated`] - New agent was created
//! - [`HubEvent::AgentDeleted`] - Agent was deleted
//! - [`HubEvent::AgentStatusChanged`] - Agent status changed
//! - [`HubEvent::Shutdown`] - Hub is shutting down
//!
//! # Usage
//!
//! ```ignore
//! // Hub broadcasts
//! hub.broadcast(HubEvent::AgentCreated { agent_id, info });
//!
//! // Client subscribes and receives
//! let rx = hub.subscribe_events();
//! match rx.recv().await {
//!     Ok(HubEvent::AgentCreated { agent_id, info }) => {
//!         client.on_agent_created(&agent_id, &info);
//!     }
//!     // ...
//! }
//! ```

// Rust guideline compliant 2026-01

use crate::client::ClientId;
use crate::relay::types::AgentInfo;

/// Agent status for status change events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    /// Agent is idle, waiting for input.
    Idle,
    /// Agent is running, processing a task.
    Running,
    /// Agent process exited.
    Exited,
    /// Agent encountered an error.
    Error,
}

impl AgentStatus {
    /// Get a human-readable label for this status.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Running => "Running",
            Self::Exited => "Exited",
            Self::Error => "Error",
        }
    }
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

/// Events broadcast by Hub to all connected clients.
///
/// These events enable decoupled communication between the Hub and clients.
/// Hub emits events without knowing who is subscribed. Each client receives
/// events independently via their own broadcast receiver.
#[derive(Debug, Clone)]
pub enum HubEvent {
    /// A new agent was created.
    ///
    /// Clients should update their agent list UI.
    AgentCreated {
        /// Unique agent identifier (session key).
        agent_id: String,
        /// Full agent information.
        info: AgentInfo,
    },

    /// An agent was deleted.
    ///
    /// Clients viewing this agent should clear their view.
    /// Clients should update their agent list UI.
    AgentDeleted {
        /// Unique agent identifier (session key).
        agent_id: String,
    },

    /// An agent's status changed.
    ///
    /// Clients may update status indicators in their UI.
    AgentStatusChanged {
        /// Unique agent identifier (session key).
        agent_id: String,
        /// New status.
        status: AgentStatus,
    },

    /// Hub is shutting down.
    ///
    /// Clients should disconnect gracefully.
    Shutdown,

    /// Agent creation progress update.
    ///
    /// Sent during agent creation to report progress through stages.
    AgentCreationProgress {
        /// The identifier being created (issue number or branch name).
        identifier: String,
        /// Current creation stage.
        stage: crate::relay::AgentCreationStage,
    },

    /// Error occurred that clients should display.
    Error {
        /// Error message.
        message: String,
    },

    /// A PTY connection was requested for a specific client.
    /// Only the matching client should act on this.
    PtyConnectionRequested {
        /// Which client should connect.
        client_id: ClientId,
        /// Agent index to connect to.
        agent_index: usize,
        /// PTY index within the agent.
        pty_index: usize,
    },

    /// A PTY disconnection was requested for a specific client.
    /// Only the matching client should act on this.
    PtyDisconnectionRequested {
        /// Which client should disconnect.
        client_id: ClientId,
        /// Agent index to disconnect from.
        agent_index: usize,
        /// PTY index within the agent.
        pty_index: usize,
    },
}

impl HubEvent {
    /// Create an agent created event.
    #[must_use]
    pub fn agent_created(agent_id: impl Into<String>, info: AgentInfo) -> Self {
        Self::AgentCreated {
            agent_id: agent_id.into(),
            info,
        }
    }

    /// Create an agent deleted event.
    #[must_use]
    pub fn agent_deleted(agent_id: impl Into<String>) -> Self {
        Self::AgentDeleted {
            agent_id: agent_id.into(),
        }
    }

    /// Create an agent status changed event.
    #[must_use]
    pub fn agent_status_changed(agent_id: impl Into<String>, status: AgentStatus) -> Self {
        Self::AgentStatusChanged {
            agent_id: agent_id.into(),
            status,
        }
    }

    /// Create a shutdown event.
    #[must_use]
    pub fn shutdown() -> Self {
        Self::Shutdown
    }

    /// Check if this is an agent created event.
    #[must_use]
    pub fn is_agent_created(&self) -> bool {
        matches!(self, Self::AgentCreated { .. })
    }

    /// Check if this is an agent deleted event.
    #[must_use]
    pub fn is_agent_deleted(&self) -> bool {
        matches!(self, Self::AgentDeleted { .. })
    }

    /// Check if this is an agent status changed event.
    #[must_use]
    pub fn is_agent_status_changed(&self) -> bool {
        matches!(self, Self::AgentStatusChanged { .. })
    }

    /// Check if this is a shutdown event.
    #[must_use]
    pub fn is_shutdown(&self) -> bool {
        matches!(self, Self::Shutdown)
    }

    /// Get the agent ID if this event pertains to a specific agent.
    #[must_use]
    pub fn agent_id(&self) -> Option<&str> {
        match self {
            Self::AgentCreated { agent_id, .. }
            | Self::AgentDeleted { agent_id }
            | Self::AgentStatusChanged { agent_id, .. } => Some(agent_id),
            Self::Shutdown
            | Self::AgentCreationProgress { .. }
            | Self::Error { .. }
            | Self::PtyConnectionRequested { .. }
            | Self::PtyDisconnectionRequested { .. } => None,
        }
    }

    /// Create an error event.
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_agent_info() -> AgentInfo {
        AgentInfo {
            id: "test-agent-123".to_string(),
            repo: Some("owner/repo".to_string()),
            issue_number: Some(42),
            branch_name: Some("botster-issue-42".to_string()),
            name: None,
            status: Some("Running".to_string()),
            tunnel_port: None,
            server_running: None,
            has_server_pty: None,
            active_pty_view: None,
            scroll_offset: None,
            hub_identifier: None,
        }
    }

    #[test]
    fn test_hub_event_agent_created() {
        let info = test_agent_info();
        let event = HubEvent::agent_created("agent-123", info.clone());

        assert!(event.is_agent_created());
        assert!(!event.is_agent_deleted());
        assert!(!event.is_shutdown());
        assert_eq!(event.agent_id(), Some("agent-123"));

        match event {
            HubEvent::AgentCreated { agent_id, info: i } => {
                assert_eq!(agent_id, "agent-123");
                assert_eq!(i.id, info.id);
            }
            _ => panic!("Expected AgentCreated variant"),
        }
    }

    #[test]
    fn test_hub_event_agent_deleted() {
        let event = HubEvent::agent_deleted("agent-456");

        assert!(event.is_agent_deleted());
        assert!(!event.is_agent_created());
        assert_eq!(event.agent_id(), Some("agent-456"));

        match event {
            HubEvent::AgentDeleted { agent_id } => {
                assert_eq!(agent_id, "agent-456");
            }
            _ => panic!("Expected AgentDeleted variant"),
        }
    }

    #[test]
    fn test_hub_event_agent_status_changed() {
        let event = HubEvent::agent_status_changed("agent-789", AgentStatus::Running);

        assert!(event.is_agent_status_changed());
        assert_eq!(event.agent_id(), Some("agent-789"));

        match event {
            HubEvent::AgentStatusChanged { agent_id, status } => {
                assert_eq!(agent_id, "agent-789");
                assert_eq!(status, AgentStatus::Running);
            }
            _ => panic!("Expected AgentStatusChanged variant"),
        }
    }

    #[test]
    fn test_hub_event_shutdown() {
        let event = HubEvent::shutdown();

        assert!(event.is_shutdown());
        assert!(!event.is_agent_created());
        assert!(event.agent_id().is_none());
    }

    #[test]
    fn test_hub_event_clone() {
        let info = test_agent_info();
        let event = HubEvent::agent_created("agent-123", info);
        let cloned = event.clone();

        assert!(cloned.is_agent_created());
        assert_eq!(cloned.agent_id(), Some("agent-123"));
    }

    #[test]
    fn test_hub_event_debug() {
        let event = HubEvent::agent_deleted("agent-123");
        let debug = format!("{:?}", event);
        assert!(debug.contains("AgentDeleted"));
        assert!(debug.contains("agent-123"));
    }

    #[test]
    fn test_agent_status_label() {
        assert_eq!(AgentStatus::Idle.label(), "Idle");
        assert_eq!(AgentStatus::Running.label(), "Running");
        assert_eq!(AgentStatus::Exited.label(), "Exited");
        assert_eq!(AgentStatus::Error.label(), "Error");
    }

    #[test]
    fn test_agent_status_display() {
        assert_eq!(format!("{}", AgentStatus::Idle), "Idle");
        assert_eq!(format!("{}", AgentStatus::Running), "Running");
    }

    #[test]
    fn test_agent_status_equality() {
        assert_eq!(AgentStatus::Idle, AgentStatus::Idle);
        assert_ne!(AgentStatus::Idle, AgentStatus::Running);
    }
}
