//! Browser event to Hub action conversion.
//!
//! This module provides functions for converting browser events received
//! via the WebSocket relay into [`HubAction`]s that the Hub can process.
//!
//! # Event Flow
//!
//! ```text
//! Browser ──► WebSocket ──► BrowserEvent ──► HubAction ──► Hub
//! ```
//!
//! # Resize Handling
//!
//! Browser resize events are tracked for dimension changes. The resize
//! handler returns actions to apply (agent resize dimensions) based on
//! browser mode (TUI vs GUI) and connection state.

// Rust guideline compliant 2025-01

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::client::{ClientId, CreateAgentRequest, DeleteAgentRequest};
use crate::hub::{HubAction, ScrollDirection};
use crate::BrowserMode;
use super::types::{BrowserCommand, BrowserEvent, BrowserResize};

/// Convert a BrowserEvent to a client-scoped HubAction.
///
/// This function wraps browser events with the browser's identity, enabling
/// independent selection and routing per browser client.
///
/// # Arguments
///
/// * `event` - The browser event to convert
/// * `browser_identity` - Signal identity key of the browser client
///
/// # Returns
///
/// Optional `HubAction` with client_id scoped to this browser.
#[must_use]
pub fn browser_event_to_client_action(
    event: &BrowserEvent,
    browser_identity: &str,
) -> Option<HubAction> {
    let client_id = ClientId::Browser(browser_identity.to_string());

    match event {
        BrowserEvent::Input(data) => {
            Some(HubAction::SendInputForClient {
                client_id,
                data: data.as_bytes().to_vec(),
            })
        }

        BrowserEvent::SelectAgent { id } => {
            Some(HubAction::SelectAgentForClient {
                client_id,
                agent_key: id.clone(),
            })
        }

        BrowserEvent::CreateAgent { issue_or_branch, prompt } => {
            Some(HubAction::CreateAgentForClient {
                client_id,
                request: CreateAgentRequest {
                    issue_or_branch: issue_or_branch.clone().unwrap_or_default(),
                    prompt: prompt.clone(),
                    from_worktree: None,
                },
            })
        }

        BrowserEvent::DeleteAgent { id, delete_worktree } => {
            Some(HubAction::DeleteAgentForClient {
                client_id,
                request: DeleteAgentRequest {
                    agent_key: id.clone(),
                    delete_worktree: *delete_worktree,
                },
            })
        }

        BrowserEvent::Resize(resize) => {
            Some(HubAction::ResizeForClient {
                client_id,
                rows: resize.rows,
                cols: resize.cols,
            })
        }

        BrowserEvent::ListAgents => {
            Some(HubAction::RequestAgentList { client_id })
        }

        BrowserEvent::ListWorktrees => {
            Some(HubAction::RequestWorktreeList { client_id })
        }

        BrowserEvent::Connected { .. } => {
            Some(HubAction::ClientConnected { client_id })
        }

        BrowserEvent::Disconnected => {
            Some(HubAction::ClientDisconnected { client_id })
        }

        // Client-scoped scroll and toggle actions
        BrowserEvent::TogglePtyView => Some(HubAction::TogglePtyViewForClient { client_id }),

        BrowserEvent::Scroll { direction, lines } => {
            let scroll = match direction.as_str() {
                "up" => ScrollDirection::Up(*lines as usize),
                "down" => ScrollDirection::Down(*lines as usize),
                _ => return None,
            };
            Some(HubAction::ScrollForClient { client_id, scroll })
        }

        BrowserEvent::ScrollToBottom => Some(HubAction::ScrollForClient {
            client_id,
            scroll: ScrollDirection::ToBottom,
        }),

        BrowserEvent::ScrollToTop => Some(HubAction::ScrollForClient {
            client_id,
            scroll: ScrollDirection::ToTop,
        }),

        BrowserEvent::ReopenWorktree { path, branch, prompt } => {
            Some(HubAction::CreateAgentForClient {
                client_id,
                request: CreateAgentRequest {
                    issue_or_branch: branch.clone(),
                    prompt: prompt.clone(),
                    from_worktree: Some(PathBuf::from(path)),
                },
            })
        }

        // Events with no Hub action mapping
        BrowserEvent::SetMode { .. }
        | BrowserEvent::GenerateInvite => None,
    }
}

/// Context needed for browser event conversion.
#[derive(Debug, Clone, Default)]
pub struct BrowserEventContext {
    /// Base path for worktrees.
    pub worktree_base: Option<PathBuf>,
    /// Path to the main repository.
    pub repo_path: Option<PathBuf>,
    /// Repository name (owner/repo format).
    pub repo_name: Option<String>,
}

/// Parse an issue_or_branch string into issue number and branch name.
///
/// Note: This function is only used in tests after Phase 4A removed
/// browser_event_to_hub_action. Kept for test coverage.
#[cfg(test)]
fn parse_issue_or_branch(value: Option<&String>) -> (Option<u32>, Option<String>) {
    let Some(v) = value else {
        return (None, None);
    };

    // Try to parse as issue number
    if let Ok(num) = v.parse::<u32>() {
        return (Some(num), None);
    }

    // Otherwise treat as branch name
    (None, Some(v.clone()))
}

/// Convert a BrowserCommand to a BrowserEvent.
///
/// This provides a standard conversion from the lower-level command
/// types to the higher-level event types.
#[must_use]
pub fn command_to_event(cmd: &BrowserCommand) -> BrowserEvent {
    match cmd {
        BrowserCommand::Handshake { device_name, .. } => {
            // Handshake is handled specially in connection code where the
            // identity key is extracted from the Signal envelope.
            BrowserEvent::Connected {
                public_key: String::new(), // Filled from envelope in connection handler
                device_name: device_name.clone(),
            }
        }
        BrowserCommand::Input { data } => BrowserEvent::Input(data.clone()),
        BrowserCommand::SetMode { mode } => BrowserEvent::SetMode { mode: mode.clone() },
        BrowserCommand::ListAgents => BrowserEvent::ListAgents,
        BrowserCommand::ListWorktrees => BrowserEvent::ListWorktrees,
        BrowserCommand::SelectAgent { id } => BrowserEvent::SelectAgent { id: id.clone() },
        BrowserCommand::CreateAgent {
            issue_or_branch,
            prompt,
        } => BrowserEvent::CreateAgent {
            issue_or_branch: issue_or_branch.clone(),
            prompt: prompt.clone(),
        },
        BrowserCommand::ReopenWorktree {
            path,
            branch,
            prompt,
        } => BrowserEvent::ReopenWorktree {
            path: path.clone(),
            branch: branch.clone(),
            prompt: prompt.clone(),
        },
        BrowserCommand::DeleteAgent { id, delete_worktree } => BrowserEvent::DeleteAgent {
            id: id.clone(),
            delete_worktree: delete_worktree.unwrap_or(false),
        },
        BrowserCommand::TogglePtyView => BrowserEvent::TogglePtyView,
        BrowserCommand::Scroll { direction, lines } => BrowserEvent::Scroll {
            direction: direction.clone(),
            lines: lines.unwrap_or(10),
        },
        BrowserCommand::ScrollToBottom => BrowserEvent::ScrollToBottom,
        BrowserCommand::ScrollToTop => BrowserEvent::ScrollToTop,
        BrowserCommand::Resize { cols, rows } => BrowserEvent::Resize(BrowserResize {
            cols: *cols,
            rows: *rows,
        }),
        BrowserCommand::GenerateInvite => BrowserEvent::GenerateInvite,
    }
}

/// Result of checking browser resize state.
#[derive(Debug, Clone)]
pub enum ResizeAction {
    /// No action needed (dimensions unchanged or browser disconnected).
    None,
    /// Resize agents to these dimensions.
    ResizeAgents {
        /// Terminal height in rows.
        rows: u16,
        /// Terminal width in columns.
        cols: u16,
    },
    /// Browser disconnected - reset to local terminal dimensions.
    ResetToLocal {
        /// Terminal height in rows.
        rows: u16,
        /// Terminal width in columns.
        cols: u16,
    },
}

/// Check if browser dimensions have changed and return resize action.
///
/// This function tracks dimension state across calls using atomic variables.
/// It handles:
/// - Browser mode changes (GUI uses full dims, TUI uses 70% width)
/// - Connection/disconnection transitions
/// - Dimension validation (min 20 cols, 5 rows)
///
/// # Arguments
///
/// * `browser_dims` - Current browser dimensions, or None if disconnected
/// * `local_dims` - Local terminal dimensions (rows, cols) for fallback
///
/// # Returns
///
/// A `ResizeAction` indicating what should be done.
pub fn check_browser_resize(
    browser_dims: Option<(u16, u16, BrowserMode)>,
    local_dims: (u16, u16),
) -> ResizeAction {
    static LAST_DIMS: AtomicU32 = AtomicU32::new(0);
    static WAS_CONNECTED: AtomicBool = AtomicBool::new(false);

    let is_connected = browser_dims.is_some();
    let was_connected = WAS_CONNECTED.swap(is_connected, Ordering::Relaxed);

    if let Some((rows, cols, mode)) = browser_dims {
        if cols >= 20 && rows >= 5 {
            let mode_bit = if mode == BrowserMode::Gui { 1u32 << 31 } else { 0 };
            let combined = mode_bit | (u32::from(cols) << 16) | u32::from(rows);
            let last = LAST_DIMS.swap(combined, Ordering::Relaxed);

            if last != combined {
                let (agent_cols, agent_rows) = match mode {
                    BrowserMode::Gui => {
                        log::info!("GUI mode - using full browser dimensions: {cols}x{rows}");
                        (cols, rows)
                    }
                    BrowserMode::Tui => {
                        let tui_cols = (cols * 70 / 100).saturating_sub(2);
                        let tui_rows = rows.saturating_sub(2);
                        log::info!("TUI mode - using 70% width: {tui_cols}x{tui_rows} (from {cols}x{rows})");
                        (tui_cols, tui_rows)
                    }
                };
                return ResizeAction::ResizeAgents {
                    rows: agent_rows,
                    cols: agent_cols,
                };
            }
        }
        ResizeAction::None
    } else if was_connected {
        log::info!("Browser disconnected, resetting agents to local terminal size");
        LAST_DIMS.store(0, Ordering::Relaxed);
        let (local_rows, local_cols) = local_dims;
        let terminal_cols = (local_cols * 70 / 100).saturating_sub(2);
        let terminal_rows = local_rows.saturating_sub(2);
        ResizeAction::ResetToLocal {
            rows: terminal_rows,
            cols: terminal_cols,
        }
    } else {
        ResizeAction::None
    }
}

/// Process a single browser event and return actions to take.
///
/// Returns a tuple of:
/// - Optional `HubAction` to dispatch
/// - Optional resize dimensions (rows, cols) for agent resizing
/// - Whether the screen should be invalidated
#[derive(Debug)]
pub struct BrowserEventResult {
    /// Hub action to dispatch, if any.
    pub action: Option<HubAction>,
    /// Agent resize dimensions, if needed.
    pub resize: Option<(u16, u16)>,
    /// Whether screen cache should be invalidated.
    pub invalidate_screen: bool,
    /// Response to send back to browser.
    pub response: BrowserResponse,
}

/// Response to send back to browser after processing an event.
#[derive(Debug, Clone, PartialEq)]
pub enum BrowserResponse {
    /// No response needed.
    None,
    /// Send agent list to browser.
    SendAgentList,
    /// Send worktree list to browser.
    SendWorktreeList,
    /// Send agent selected notification.
    SendAgentSelected(String),
}

impl Default for BrowserEventResult {
    fn default() -> Self {
        Self {
            action: None,
            resize: None,
            invalidate_screen: false,
            response: BrowserResponse::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: Old tests for browser_event_to_hub_action removed (Phase 4A).
    // All browser events now use browser_event_to_client_action for client-scoped routing.

    #[test]
    fn test_parse_issue_or_branch_number() {
        let value = Some("42".to_string());
        let (issue, branch) = parse_issue_or_branch(value.as_ref());
        assert_eq!(issue, Some(42));
        assert!(branch.is_none());
    }

    #[test]
    fn test_parse_issue_or_branch_string() {
        let value = Some("feature-branch".to_string());
        let (issue, branch) = parse_issue_or_branch(value.as_ref());
        assert!(issue.is_none());
        assert_eq!(branch, Some("feature-branch".to_string()));
    }

    #[test]
    fn test_parse_issue_or_branch_none() {
        let (issue, branch) = parse_issue_or_branch(None);
        assert!(issue.is_none());
        assert!(branch.is_none());
    }

    #[test]
    fn test_command_to_event_input() {
        let cmd = BrowserCommand::Input {
            data: "test".to_string(),
        };
        let event = command_to_event(&cmd);
        assert!(matches!(event, BrowserEvent::Input(data) if data == "test"));
    }

    #[test]
    fn test_command_to_event_scroll() {
        let cmd = BrowserCommand::Scroll {
            direction: "up".to_string(),
            lines: Some(5),
        };
        let event = command_to_event(&cmd);
        assert!(matches!(
            event,
            BrowserEvent::Scroll { direction, lines } if direction == "up" && lines == 5
        ));
    }

    #[test]
    fn test_command_to_event_scroll_default_lines() {
        let cmd = BrowserCommand::Scroll {
            direction: "down".to_string(),
            lines: None,
        };
        let event = command_to_event(&cmd);
        assert!(matches!(
            event,
            BrowserEvent::Scroll { direction, lines } if direction == "down" && lines == 10
        ));
    }

    // === Client-scoped action conversion tests ===

    #[test]
    fn test_client_input_event() {
        let event = BrowserEvent::Input("hello".to_string());
        let browser_identity = "browser-abc123";
        let action = browser_event_to_client_action(&event, browser_identity);

        match action {
            Some(HubAction::SendInputForClient { client_id, data }) => {
                assert_eq!(client_id, ClientId::Browser("browser-abc123".to_string()));
                assert_eq!(data, b"hello");
            }
            _ => panic!("Expected SendInputForClient action"),
        }
    }

    #[test]
    fn test_client_select_agent_event() {
        let event = BrowserEvent::SelectAgent {
            id: "owner-repo-42".to_string(),
        };
        let browser_identity = "browser-xyz";
        let action = browser_event_to_client_action(&event, browser_identity);

        match action {
            Some(HubAction::SelectAgentForClient { client_id, agent_key }) => {
                assert_eq!(client_id, ClientId::Browser("browser-xyz".to_string()));
                assert_eq!(agent_key, "owner-repo-42");
            }
            _ => panic!("Expected SelectAgentForClient action"),
        }
    }

    #[test]
    fn test_client_create_agent_event() {
        let event = BrowserEvent::CreateAgent {
            issue_or_branch: Some("feature/new-ui".to_string()),
            prompt: Some("Build the UI".to_string()),
        };
        let browser_identity = "browser-123";
        let action = browser_event_to_client_action(&event, browser_identity);

        match action {
            Some(HubAction::CreateAgentForClient { client_id, request }) => {
                assert_eq!(client_id, ClientId::Browser("browser-123".to_string()));
                // issue_or_branch is converted via unwrap_or_default()
                assert_eq!(request.issue_or_branch, "feature/new-ui");
                assert_eq!(request.prompt, Some("Build the UI".to_string()));
                assert!(request.from_worktree.is_none());
            }
            _ => panic!("Expected CreateAgentForClient action"),
        }
    }

    #[test]
    fn test_client_reopen_worktree_event() {
        let event = BrowserEvent::ReopenWorktree {
            path: "/tmp/worktrees/my-branch".to_string(),
            branch: "my-branch".to_string(),
            prompt: Some("Continue working".to_string()),
        };
        let browser_identity = "browser-456";
        let action = browser_event_to_client_action(&event, browser_identity);

        match action {
            Some(HubAction::CreateAgentForClient { client_id, request }) => {
                assert_eq!(client_id, ClientId::Browser("browser-456".to_string()));
                // branch becomes issue_or_branch directly (not Option)
                assert_eq!(request.issue_or_branch, "my-branch");
                assert_eq!(request.prompt, Some("Continue working".to_string()));
                assert_eq!(request.from_worktree, Some(PathBuf::from("/tmp/worktrees/my-branch")));
            }
            _ => panic!("Expected CreateAgentForClient action with from_worktree"),
        }
    }

    #[test]
    fn test_client_delete_agent_event() {
        let event = BrowserEvent::DeleteAgent {
            id: "owner-repo-99".to_string(),
            delete_worktree: true,
        };
        let browser_identity = "browser-del";
        let action = browser_event_to_client_action(&event, browser_identity);

        match action {
            Some(HubAction::DeleteAgentForClient { client_id, request }) => {
                assert_eq!(client_id, ClientId::Browser("browser-del".to_string()));
                assert_eq!(request.agent_key, "owner-repo-99");
                assert!(request.delete_worktree);
            }
            _ => panic!("Expected DeleteAgentForClient action"),
        }
    }

    #[test]
    fn test_client_resize_event() {
        let event = BrowserEvent::Resize(BrowserResize { rows: 50, cols: 120 });
        let browser_identity = "browser-resize";
        let action = browser_event_to_client_action(&event, browser_identity);

        match action {
            Some(HubAction::ResizeForClient { client_id, rows, cols }) => {
                assert_eq!(client_id, ClientId::Browser("browser-resize".to_string()));
                assert_eq!(rows, 50);
                assert_eq!(cols, 120);
            }
            _ => panic!("Expected ResizeForClient action"),
        }
    }

    #[test]
    fn test_client_list_agents_event() {
        let event = BrowserEvent::ListAgents;
        let browser_identity = "browser-list";
        let action = browser_event_to_client_action(&event, browser_identity);

        match action {
            Some(HubAction::RequestAgentList { client_id }) => {
                assert_eq!(client_id, ClientId::Browser("browser-list".to_string()));
            }
            _ => panic!("Expected RequestAgentList action"),
        }
    }

    #[test]
    fn test_client_list_worktrees_event() {
        let event = BrowserEvent::ListWorktrees;
        let browser_identity = "browser-wt";
        let action = browser_event_to_client_action(&event, browser_identity);

        match action {
            Some(HubAction::RequestWorktreeList { client_id }) => {
                assert_eq!(client_id, ClientId::Browser("browser-wt".to_string()));
            }
            _ => panic!("Expected RequestWorktreeList action"),
        }
    }

    #[test]
    fn test_client_connected_event() {
        let event = BrowserEvent::Connected {
            public_key: "pk123".to_string(),
            device_name: "Chrome".to_string(),
        };
        let browser_identity = "browser-new";
        let action = browser_event_to_client_action(&event, browser_identity);

        match action {
            Some(HubAction::ClientConnected { client_id }) => {
                assert_eq!(client_id, ClientId::Browser("browser-new".to_string()));
            }
            _ => panic!("Expected ClientConnected action"),
        }
    }

    #[test]
    fn test_client_disconnected_event() {
        let event = BrowserEvent::Disconnected;
        let browser_identity = "browser-bye";
        let action = browser_event_to_client_action(&event, browser_identity);

        match action {
            Some(HubAction::ClientDisconnected { client_id }) => {
                assert_eq!(client_id, ClientId::Browser("browser-bye".to_string()));
            }
            _ => panic!("Expected ClientDisconnected action"),
        }
    }

    #[test]
    fn test_client_set_mode_returns_none() {
        // SetMode is handled directly in browser.rs, not via action dispatch
        let event = BrowserEvent::SetMode {
            mode: "gui".to_string(),
        };
        let browser_identity = "browser-mode";
        let action = browser_event_to_client_action(&event, browser_identity);

        assert!(action.is_none(), "SetMode should return None (handled directly)");
    }

    #[test]
    fn test_client_generate_invite_returns_none() {
        // GenerateInvite is handled in connection.rs, not via action dispatch
        let event = BrowserEvent::GenerateInvite;
        let browser_identity = "browser-invite";
        let action = browser_event_to_client_action(&event, browser_identity);

        assert!(action.is_none(), "GenerateInvite should return None (handled in relay)");
    }

    /// TEST: Scroll events are now CLIENT-SCOPED, not global.
    ///
    /// This ensures browser scroll commands affect only the browser's selected agent,
    /// not the TUI's selection.
    #[test]
    fn test_scroll_events_are_client_scoped() {
        let browser_identity = "browser-scroll";
        let expected_client_id = ClientId::Browser(browser_identity.to_string());

        // Scroll up
        let scroll_up = BrowserEvent::Scroll {
            direction: "up".to_string(),
            lines: 5,
        };
        let action = browser_event_to_client_action(&scroll_up, browser_identity);
        match action {
            Some(HubAction::ScrollForClient { client_id, scroll }) => {
                assert_eq!(client_id, expected_client_id);
                assert_eq!(scroll, ScrollDirection::Up(5));
            }
            _ => panic!("Expected ScrollForClient, got {:?}", action),
        }

        // Scroll down
        let scroll_down = BrowserEvent::Scroll {
            direction: "down".to_string(),
            lines: 10,
        };
        let action = browser_event_to_client_action(&scroll_down, browser_identity);
        match action {
            Some(HubAction::ScrollForClient { client_id, scroll }) => {
                assert_eq!(client_id, expected_client_id);
                assert_eq!(scroll, ScrollDirection::Down(10));
            }
            _ => panic!("Expected ScrollForClient, got {:?}", action),
        }

        // Scroll to top
        let scroll_top = BrowserEvent::ScrollToTop;
        let action = browser_event_to_client_action(&scroll_top, browser_identity);
        match action {
            Some(HubAction::ScrollForClient { client_id, scroll }) => {
                assert_eq!(client_id, expected_client_id);
                assert_eq!(scroll, ScrollDirection::ToTop);
            }
            _ => panic!("Expected ScrollForClient, got {:?}", action),
        }

        // Scroll to bottom
        let scroll_bottom = BrowserEvent::ScrollToBottom;
        let action = browser_event_to_client_action(&scroll_bottom, browser_identity);
        match action {
            Some(HubAction::ScrollForClient { client_id, scroll }) => {
                assert_eq!(client_id, expected_client_id);
                assert_eq!(scroll, ScrollDirection::ToBottom);
            }
            _ => panic!("Expected ScrollForClient, got {:?}", action),
        }
    }

    /// TEST: TogglePtyView is now CLIENT-SCOPED, not global.
    #[test]
    fn test_toggle_pty_is_client_scoped() {
        let event = BrowserEvent::TogglePtyView;
        let browser_identity = "browser-toggle";
        let action = browser_event_to_client_action(&event, browser_identity);

        match action {
            Some(HubAction::TogglePtyViewForClient { client_id }) => {
                assert_eq!(client_id, ClientId::Browser(browser_identity.to_string()));
            }
            _ => panic!("Expected TogglePtyViewForClient, got {:?}", action),
        }
    }
}
