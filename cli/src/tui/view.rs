//! TUI view rendering state and helpers.
//!
//! This module provides the state and helper types needed for rendering
//! the TUI. It bridges between Hub state and ratatui's rendering system.
//!
//! # Architecture
//!
//! The view module follows the pattern of extracting view state from
//! the Hub and passing it to render functions:
//!
//! ```text
//! HubState ──► ViewState::from_hub() ──► render functions
//! ```
//!
//! # Client-Owned View State
//!
//! Note that `active_view` (which PTY is being viewed) and scroll position
//! are per-client state. The caller (TuiRunner/TuiClient) tracks which view
//! is active and passes it as a parameter when needed.

// Rust guideline compliant 2026-01

use crate::agent::PtyView;
use crate::app::AppMode;
use crate::hub::HubState;
use crate::tunnel::TunnelStatus;

/// Snapshot of state needed for TUI rendering.
///
/// This struct captures all the state needed to render a single frame,
/// allowing the view to be rendered without borrowing the Hub.
#[derive(Debug, Clone)]
pub struct ViewState {
    /// Number of active agents.
    pub agent_count: usize,
    /// Ordered list of agent IDs.
    pub agent_ids: Vec<String>,
    /// Currently selected agent ID (from TUI client).
    pub selected_agent_id: Option<String>,
    /// Current application mode.
    pub mode: AppMode,
    /// Seconds since last poll.
    pub seconds_since_poll: u64,
    /// Poll interval in seconds.
    pub poll_interval: u64,
    /// Currently selected menu item.
    pub menu_selected: usize,
    /// Available worktrees for selection.
    pub available_worktrees: Vec<(String, String)>,
    /// Currently selected worktree index.
    pub worktree_selected: usize,
    /// Current text input buffer.
    pub input_buffer: String,
    /// Tunnel connection status.
    pub tunnel_status: TunnelStatus,
    /// Connection URL for QR code display.
    pub connection_url: Option<String>,
    /// Error message for Error mode.
    pub error_message: Option<String>,
    /// Whether the current bundle's PreKey has been used (needs regeneration for new devices).
    pub bundle_used: bool,
}

impl ViewState {
    /// Create a new ViewState from HubState and additional context.
    ///
    /// # Arguments
    ///
    /// * `hub_state` - Reference to the Hub's state
    /// * `context` - Additional view context from the application (includes TUI selection)
    #[must_use]
    pub fn from_hub(hub_state: &HubState, context: ViewContext) -> Self {
        Self {
            agent_count: hub_state.agent_count(),
            agent_ids: hub_state.agent_keys_ordered.clone(),
            selected_agent_id: context.selected_key,
            mode: context.mode,
            seconds_since_poll: context.seconds_since_poll,
            poll_interval: context.poll_interval,
            menu_selected: context.menu_selected,
            available_worktrees: hub_state.available_worktrees.clone(),
            worktree_selected: context.worktree_selected,
            input_buffer: context.input_buffer,
            tunnel_status: context.tunnel_status,
            connection_url: context.connection_url,
            error_message: context.error_message,
            bundle_used: context.bundle_used,
        }
    }

    /// Get the agent ID of the currently selected agent.
    #[must_use]
    pub fn selected_key(&self) -> Option<&str> {
        self.selected_agent_id.as_deref()
    }

    /// Check if there are any active agents.
    #[must_use]
    pub fn has_agents(&self) -> bool {
        self.agent_count > 0
    }

    /// Check if currently in a modal state.
    #[must_use]
    pub fn is_modal(&self) -> bool {
        !matches!(self.mode, AppMode::Normal)
    }
}

/// Additional context needed for view rendering.
///
/// This contains state that lives outside HubState but is needed
/// for rendering. Selection is provided by the TUI client.
#[derive(Debug, Clone)]
pub struct ViewContext {
    /// Currently selected agent key (from TUI client).
    pub selected_key: Option<String>,
    /// Current application mode.
    pub mode: AppMode,
    /// Seconds since last poll.
    pub seconds_since_poll: u64,
    /// Poll interval configuration.
    pub poll_interval: u64,
    /// Currently selected menu item.
    pub menu_selected: usize,
    /// Currently selected worktree.
    pub worktree_selected: usize,
    /// Text input buffer.
    pub input_buffer: String,
    /// Tunnel status.
    pub tunnel_status: TunnelStatus,
    /// Connection URL for QR code.
    pub connection_url: Option<String>,
    /// Error message for Error mode.
    pub error_message: Option<String>,
    /// Whether the current bundle's PreKey has been used.
    pub bundle_used: bool,
}

impl Default for ViewContext {
    fn default() -> Self {
        Self {
            selected_key: None,
            mode: AppMode::Normal,
            seconds_since_poll: 0,
            poll_interval: 10,
            menu_selected: 0,
            worktree_selected: 0,
            input_buffer: String::new(),
            tunnel_status: TunnelStatus::Disconnected,
            connection_url: None,
            error_message: None,
            bundle_used: false,
        }
    }
}

/// Information about an agent for display.
///
/// Note: `active_view` and `is_scrolled` are per-client state.
/// The caller provides these when constructing the display info.
#[derive(Debug, Clone)]
pub struct AgentDisplayInfo {
    /// Display label for the agent.
    pub label: String,
    /// Tunnel port if assigned.
    pub tunnel_port: Option<u16>,
    /// Whether the server is running.
    pub server_running: bool,
    /// Which PTY view is being displayed (client's current view).
    pub active_view: PtyView,
    /// Whether the view is scrolled up (client's scroll state).
    pub is_scrolled: bool,
}

impl AgentDisplayInfo {
    /// Create display info from an agent for a specific view.
    ///
    /// # Arguments
    ///
    /// * `agent` - The agent to extract info from
    /// * `view` - Which PTY view the client is currently viewing
    /// * `is_scrolled` - Whether the client's parser is scrolled (caller tracks this)
    ///
    /// The `view` and `is_scrolled` parameters are required because they are per-client state,
    /// not agent state. The caller (TuiRunner/TuiClient) knows its own view and scroll state.
    #[must_use]
    pub fn from_agent(agent: &crate::agent::Agent, view: PtyView, is_scrolled: bool) -> Self {
        let label = if let Some(issue_num) = agent.issue_number {
            format!("{}#{}", agent.repo, issue_num)
        } else {
            format!("{}/{}", agent.repo, agent.branch_name)
        };

        Self {
            label,
            tunnel_port: agent.tunnel_port,
            server_running: agent.is_server_running(),
            active_view: view,
            is_scrolled,
        }
    }

    /// Format the full display string with server status.
    #[must_use]
    pub fn display_string(&self) -> String {
        if let Some(port) = self.tunnel_port {
            let server_icon = if self.server_running { "▶" } else { "○" };
            format!("{} {}:{}", self.label, server_icon, port)
        } else {
            self.label.clone()
        }
    }
}

/// Format the poll status indicator.
#[must_use]
pub fn format_poll_status(seconds_since_poll: u64) -> &'static str {
    if seconds_since_poll < 1 {
        "●"
    } else {
        "○"
    }
}

/// Format the tunnel status indicator.
#[must_use]
pub fn format_tunnel_status(status: TunnelStatus) -> &'static str {
    match status {
        TunnelStatus::Connected => "⬤",
        TunnelStatus::Connecting => "◐",
        TunnelStatus::Disconnected => "○",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;
    use std::path::PathBuf;

    #[test]
    fn test_view_state_from_hub() {
        let hub_state = HubState::new(PathBuf::from("/tmp/worktrees"));
        let context = ViewContext::default();

        let view_state = ViewState::from_hub(&hub_state, context);

        assert_eq!(view_state.agent_count, 0);
        assert!(view_state.agent_ids.is_empty());
        assert!(!view_state.has_agents());
    }

    #[test]
    fn test_view_state_selected_key() {
        let mut hub_state = HubState::new(PathBuf::from("/tmp/worktrees"));
        hub_state.agent_keys_ordered.push("test-key".to_string());

        // Selection comes from ViewContext (TUI client's selection)
        let context = ViewContext {
            selected_key: Some("test-key".to_string()),
            ..Default::default()
        };
        let view_state = ViewState::from_hub(&hub_state, context);

        assert_eq!(view_state.selected_key(), Some("test-key"));
    }

    #[test]
    fn test_view_state_is_modal() {
        let hub_state = HubState::new(PathBuf::from("/tmp/worktrees"));

        let normal_context = ViewContext {
            mode: AppMode::Normal,
            ..Default::default()
        };
        let view_state = ViewState::from_hub(&hub_state, normal_context);
        assert!(!view_state.is_modal());

        let menu_context = ViewContext {
            mode: AppMode::Menu,
            ..Default::default()
        };
        let view_state = ViewState::from_hub(&hub_state, menu_context);
        assert!(view_state.is_modal());
    }

    #[test]
    fn test_agent_display_info_with_issue() {
        let agent = Agent::new(
            uuid::Uuid::new_v4(),
            "owner/repo".to_string(),
            Some(42),
            "issue-42".to_string(),
            PathBuf::from("/tmp/worktree"),
        );

        // Pass view and is_scrolled as parameters - caller tracks these
        let info = AgentDisplayInfo::from_agent(&agent, PtyView::Cli, false);
        assert_eq!(info.label, "owner/repo#42");
        assert!(info.tunnel_port.is_none());
        assert!(!info.server_running);
        assert_eq!(info.active_view, PtyView::Cli);
        assert!(!info.is_scrolled);
    }

    #[test]
    fn test_agent_display_info_without_issue() {
        let agent = Agent::new(
            uuid::Uuid::new_v4(),
            "owner/repo".to_string(),
            None,
            "feature-branch".to_string(),
            PathBuf::from("/tmp/worktree"),
        );

        let info = AgentDisplayInfo::from_agent(&agent, PtyView::Cli, false);
        assert_eq!(info.label, "owner/repo/feature-branch");
    }

    #[test]
    fn test_agent_display_info_server_view() {
        let agent = Agent::new(
            uuid::Uuid::new_v4(),
            "owner/repo".to_string(),
            Some(42),
            "issue-42".to_string(),
            PathBuf::from("/tmp/worktree"),
        );

        // Same agent, different view - demonstrates per-client view state
        let info = AgentDisplayInfo::from_agent(&agent, PtyView::Server, true);
        assert_eq!(info.active_view, PtyView::Server);
        assert!(info.is_scrolled);
    }

    #[test]
    fn test_agent_display_string() {
        let info = AgentDisplayInfo {
            label: "owner/repo#42".to_string(),
            tunnel_port: Some(3000),
            server_running: true,
            active_view: PtyView::Cli,
            is_scrolled: false,
        };

        assert_eq!(info.display_string(), "owner/repo#42 ▶:3000");

        let info_no_port = AgentDisplayInfo {
            label: "owner/repo#42".to_string(),
            tunnel_port: None,
            server_running: false,
            active_view: PtyView::Cli,
            is_scrolled: false,
        };

        assert_eq!(info_no_port.display_string(), "owner/repo#42");
    }

    #[test]
    fn test_format_poll_status() {
        assert_eq!(format_poll_status(0), "●");
        assert_eq!(format_poll_status(5), "○");
    }

    #[test]
    fn test_format_tunnel_status() {
        assert_eq!(format_tunnel_status(TunnelStatus::Connected), "⬤");
        assert_eq!(format_tunnel_status(TunnelStatus::Connecting), "◐");
        assert_eq!(format_tunnel_status(TunnelStatus::Disconnected), "○");
    }
}
