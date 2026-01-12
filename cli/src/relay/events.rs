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

use crate::hub::HubAction;
use crate::BrowserMode;
use super::types::{BrowserCommand, BrowserEvent, BrowserResize};

/// Convert a BrowserEvent to a HubAction.
///
/// Returns the corresponding HubAction for the browser event, or `None`
/// if the event doesn't map to a Hub action (e.g., list/connection events).
///
/// # Arguments
///
/// * `event` - The browser event to convert
/// * `context` - Additional context for conversion (repo path, etc.)
///
/// # Examples
///
/// ```ignore
/// let event = BrowserEvent::Input("hello".to_string());
/// let action = browser_event_to_hub_action(&event, &context);
/// assert!(matches!(action, Some(HubAction::SendInput(_))));
/// ```
#[must_use]
pub fn browser_event_to_hub_action(
    event: &BrowserEvent,
    context: &BrowserEventContext,
) -> Option<HubAction> {
    match event {
        BrowserEvent::Input(data) => {
            Some(HubAction::SendInput(data.as_bytes().to_vec()))
        }

        BrowserEvent::SelectAgent { id } => {
            Some(HubAction::SelectByKey(id.clone()))
        }

        BrowserEvent::CreateAgent {
            issue_or_branch,
            prompt,
        } => {
            // Parse issue_or_branch to determine if it's an issue number or branch name
            let (issue_number, branch_name) = parse_issue_or_branch(issue_or_branch.as_ref());
            let actual_branch = branch_name.unwrap_or_else(|| {
                issue_number.map_or_else(|| "new-branch".to_string(), |n| format!("botster-issue-{n}"))
            });

            let worktree_path = context
                .worktree_base
                .as_ref().map_or_else(|| PathBuf::from("/tmp").join(&actual_branch), |base| base.join(&actual_branch));

            Some(HubAction::SpawnAgent {
                issue_number,
                branch_name: actual_branch,
                worktree_path,
                repo_path: context.repo_path.clone().unwrap_or_default(),
                repo_name: context.repo_name.clone().unwrap_or_default(),
                prompt: prompt.clone().unwrap_or_default(),
                message_id: None,
                invocation_url: None,
            })
        }

        BrowserEvent::DeleteAgent { id, delete_worktree } => {
            Some(HubAction::CloseAgent {
                session_key: id.clone(),
                delete_worktree: *delete_worktree,
            })
        }

        BrowserEvent::TogglePtyView => {
            Some(HubAction::TogglePtyView)
        }

        BrowserEvent::Scroll { direction, lines } => {
            let line_count = *lines as usize;
            match direction.as_str() {
                "up" => Some(HubAction::ScrollUp(line_count)),
                "down" => Some(HubAction::ScrollDown(line_count)),
                _ => None,
            }
        }

        BrowserEvent::ScrollToBottom => {
            Some(HubAction::ScrollToBottom)
        }

        BrowserEvent::ScrollToTop => {
            Some(HubAction::ScrollToTop)
        }

        BrowserEvent::Resize(resize) => {
            Some(HubAction::Resize {
                rows: resize.rows,
                cols: resize.cols,
            })
        }

        // Events that don't map to Hub actions
        BrowserEvent::Connected { .. }
        | BrowserEvent::Disconnected
        | BrowserEvent::ListAgents
        | BrowserEvent::ListWorktrees
        | BrowserEvent::ReopenWorktree { .. }
        | BrowserEvent::SetMode { .. }
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
    use crate::relay::BrowserResize;

    fn default_context() -> BrowserEventContext {
        BrowserEventContext {
            worktree_base: Some(PathBuf::from("/tmp/worktrees")),
            repo_path: Some(PathBuf::from("/home/user/repo")),
            repo_name: Some("owner/repo".to_string()),
        }
    }

    #[test]
    fn test_input_event_to_action() {
        let event = BrowserEvent::Input("hello".to_string());
        let context = default_context();
        let action = browser_event_to_hub_action(&event, &context);

        assert!(matches!(action, Some(HubAction::SendInput(data)) if data == b"hello"));
    }

    #[test]
    fn test_select_agent_event() {
        let event = BrowserEvent::SelectAgent {
            id: "owner-repo-42".to_string(),
        };
        let context = default_context();
        let action = browser_event_to_hub_action(&event, &context);

        assert!(matches!(action, Some(HubAction::SelectByKey(key)) if key == "owner-repo-42"));
    }

    #[test]
    fn test_delete_agent_event() {
        let event = BrowserEvent::DeleteAgent {
            id: "owner-repo-42".to_string(),
            delete_worktree: true,
        };
        let context = default_context();
        let action = browser_event_to_hub_action(&event, &context);

        match action {
            Some(HubAction::CloseAgent {
                session_key,
                delete_worktree,
            }) => {
                assert_eq!(session_key, "owner-repo-42");
                assert!(delete_worktree);
            }
            _ => panic!("Expected CloseAgent action"),
        }
    }

    #[test]
    fn test_scroll_events() {
        let context = default_context();

        let up = BrowserEvent::Scroll {
            direction: "up".to_string(),
            lines: 5,
        };
        assert!(matches!(
            browser_event_to_hub_action(&up, &context),
            Some(HubAction::ScrollUp(5))
        ));

        let down = BrowserEvent::Scroll {
            direction: "down".to_string(),
            lines: 10,
        };
        assert!(matches!(
            browser_event_to_hub_action(&down, &context),
            Some(HubAction::ScrollDown(10))
        ));

        let to_bottom = BrowserEvent::ScrollToBottom;
        assert!(matches!(
            browser_event_to_hub_action(&to_bottom, &context),
            Some(HubAction::ScrollToBottom)
        ));

        let to_top = BrowserEvent::ScrollToTop;
        assert!(matches!(
            browser_event_to_hub_action(&to_top, &context),
            Some(HubAction::ScrollToTop)
        ));
    }

    #[test]
    fn test_toggle_pty_view() {
        let event = BrowserEvent::TogglePtyView;
        let context = default_context();
        let action = browser_event_to_hub_action(&event, &context);

        assert!(matches!(action, Some(HubAction::TogglePtyView)));
    }

    #[test]
    fn test_resize_event() {
        let event = BrowserEvent::Resize(BrowserResize { rows: 40, cols: 120 });
        let context = default_context();
        let action = browser_event_to_hub_action(&event, &context);

        assert!(matches!(
            action,
            Some(HubAction::Resize { rows: 40, cols: 120 })
        ));
    }

    #[test]
    fn test_connected_event_returns_none() {
        let event = BrowserEvent::Connected {
            public_key: "some-key".to_string(),
            device_name: "test".to_string(),
        };
        let context = default_context();
        let action = browser_event_to_hub_action(&event, &context);

        assert!(action.is_none());
    }

    #[test]
    fn test_list_events_return_none() {
        let context = default_context();

        assert!(browser_event_to_hub_action(&BrowserEvent::ListAgents, &context).is_none());
        assert!(browser_event_to_hub_action(&BrowserEvent::ListWorktrees, &context).is_none());
    }

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
}
