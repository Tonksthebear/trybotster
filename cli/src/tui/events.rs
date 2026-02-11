//! TUI events - events received by TUI for display updates.
//!
//! This module provides TUI-specific event types for the event loop.
//!
//! # Architecture
//!
//! The TUI receives events via the output channel from Hub:
//!
//! ```text
//! Hub ──TuiOutput channel──> TuiRunner (polls in event loop)
//! ```

// Rust guideline compliant 2026-02

use crate::relay::AgentInfo;

/// TUI-specific event wrapper for unified event handling.
///
/// Combines TUI-local events in a single type for easier event loop handling.
#[derive(Debug, Clone)]
pub enum TuiEvent {
    /// Agent list was updated (derived from AgentCreated/Deleted).
    AgentListUpdated {
        /// Current agent list.
        agents: Vec<AgentInfo>,
    },

    /// Connection URL was generated.
    ConnectionUrlUpdated {
        /// The secure connection URL.
        url: String,
    },

    /// An error occurred that should be displayed.
    Error {
        /// Error message to display.
        message: String,
    },

    /// TUI creation progress update.
    CreationProgress {
        /// Identifier being created (issue number or branch name).
        identifier: String,
        /// Current creation stage.
        stage: CreationStage,
    },
}

/// Agent creation stages for progress reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreationStage {
    /// Creating git worktree.
    CreatingWorktree,
    /// Copying configuration files.
    CopyingConfig,
    /// Spawning agent process.
    SpawningAgent,
    /// Agent is ready.
    Ready,
}

impl std::fmt::Display for CreationStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CreatingWorktree => write!(f, "Creating worktree..."),
            Self::CopyingConfig => write!(f, "Copying config..."),
            Self::SpawningAgent => write!(f, "Starting agent..."),
            Self::Ready => write!(f, "Ready"),
        }
    }
}

impl TuiEvent {
    /// Create an agent list updated event.
    #[must_use]
    pub fn agent_list_updated(agents: Vec<AgentInfo>) -> Self {
        Self::AgentListUpdated { agents }
    }

    /// Create a connection URL updated event.
    #[must_use]
    pub fn connection_url_updated(url: impl Into<String>) -> Self {
        Self::ConnectionUrlUpdated { url: url.into() }
    }

    /// Create an error event.
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }

    /// Create a creation progress event.
    #[must_use]
    pub fn creation_progress(identifier: impl Into<String>, stage: CreationStage) -> Self {
        Self::CreationProgress {
            identifier: identifier.into(),
            stage,
        }
    }

    /// Check if this is an error event.
    #[must_use]
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tui_event_error() {
        let event = TuiEvent::error("Something went wrong");

        assert!(event.is_error());

        match event {
            TuiEvent::Error { message } => {
                assert_eq!(message, "Something went wrong");
            }
            _ => panic!("Expected Error variant"),
        }
    }

    #[test]
    fn test_tui_event_agent_list_updated() {
        let event = TuiEvent::agent_list_updated(vec![]);

        match event {
            TuiEvent::AgentListUpdated { agents } => {
                assert!(agents.is_empty());
            }
            _ => panic!("Expected AgentListUpdated variant"),
        }
    }

    #[test]
    fn test_tui_event_connection_url() {
        let event = TuiEvent::connection_url_updated("https://example.com/connect");

        match event {
            TuiEvent::ConnectionUrlUpdated { url } => {
                assert_eq!(url, "https://example.com/connect");
            }
            _ => panic!("Expected ConnectionUrlUpdated variant"),
        }
    }

    #[test]
    fn test_creation_stage_display() {
        assert_eq!(
            format!("{}", CreationStage::CreatingWorktree),
            "Creating worktree..."
        );
        assert_eq!(
            format!("{}", CreationStage::CopyingConfig),
            "Copying config..."
        );
        assert_eq!(
            format!("{}", CreationStage::SpawningAgent),
            "Starting agent..."
        );
        assert_eq!(format!("{}", CreationStage::Ready), "Ready");
    }

    #[test]
    fn test_creation_progress_event() {
        let event = TuiEvent::creation_progress("issue-42", CreationStage::SpawningAgent);

        match event {
            TuiEvent::CreationProgress { identifier, stage } => {
                assert_eq!(identifier, "issue-42");
                assert_eq!(stage, CreationStage::SpawningAgent);
            }
            _ => panic!("Expected CreationProgress variant"),
        }
    }
}
