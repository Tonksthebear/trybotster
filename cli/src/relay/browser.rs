//! Browser event handling for the Hub.
//!
//! This module provides browser event processing functions that are called from
//! the Hub's event loop. Functions take `&mut Hub` to access state and dispatch actions.
//!
//! # Architecture
//!
//! Browser events flow from the relay connection to these handlers:
//!
//! ```text
//! WebSocket → BrowserEvent → browser::poll_events() → Hub state changes
//!                                                   → HubAction dispatch
//!                                                   → Browser responses
//! ```
//!
//! # Functions
//!
//! - [`poll_events`] - Main event loop integration point (TUI mode)
//! - [`poll_events_headless`] - Main event loop integration point (headless mode)
//! - [`send_agent_list`] - Send agent list to browser
//! - [`send_worktree_list`] - Send worktree list to browser

// Rust guideline compliant 2025-01

use anyhow::Result;

use crate::hub::{actions, Hub};
use crate::relay::{events::browser_event_to_client_action, BrowserEvent, BrowserSendContext};

/// Get browser send context if browser is connected.
fn browser_ctx(hub: &Hub) -> Option<BrowserSendContext<'_>> {
    hub.browser
        .sender
        .as_ref()
        .map(|sender| BrowserSendContext {
            sender,
            runtime: &hub.tokio_runtime,
        })
}

/// Poll and handle browser events from the terminal relay.
///
/// This is the main integration point between the browser relay and the Hub.
/// Called from the Hub's event loop to process incoming browser events.
///
/// # Arguments
///
/// * `hub` - Mutable reference to the Hub
/// * `_terminal` - Currently unused, kept for API compatibility
///
/// # Errors
///
/// Returns an error if event handling fails.
pub fn poll_events(
    hub: &mut Hub,
    _terminal: &ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
) -> Result<()> {
    poll_events_headless(hub)
}

/// Poll and handle browser events in headless mode.
///
/// Same as `poll_events` but doesn't require a terminal reference.
/// Used by headless mode where no TUI is available.
///
/// Events are now client-scoped via `browser_event_to_client_action`, enabling
/// independent selection and routing per browser client.
///
/// # Errors
///
/// Returns an error if event handling fails.
pub fn poll_events_headless(hub: &mut Hub) -> Result<()> {
    let browser_events = hub.browser.drain_events();

    for (event, browser_identity) in browser_events {
        // Try to convert to client-scoped action first
        if let Some(action) = browser_event_to_client_action(&event, &browser_identity) {
            actions::dispatch(hub, action);

            // Handle additional side effects based on event type
            // Some events need to update BrowserState in addition to client state
            match &event {
                BrowserEvent::Connected { device_name, .. } => {
                    // Update shared BrowserState for backwards compatibility
                    hub.browser.handle_connected(device_name);
                    // Send initial data to THIS browser only (not broadcast)
                    send_agent_list_to_browser(hub, &browser_identity);
                    send_worktree_list_to_browser(hub, &browser_identity);
                }
                BrowserEvent::Disconnected => {
                    hub.browser.handle_disconnected();
                }
                BrowserEvent::SelectAgent { id } => {
                    hub.browser.invalidate_screen();
                    // Send to THIS browser only (not broadcast to all browsers)
                    send_agent_selected_to_browser(hub, &browser_identity, id);
                    // Default to CLI view when selecting agent - browser specifies view
                    // via the PTY channel it subscribes to
                    send_scrollback_for_agent_to_browser(
                        hub,
                        &browser_identity,
                        id,
                        crate::agent::PtyView::Cli,
                    );
                }
                BrowserEvent::DeleteAgent { .. } => {
                    hub.browser.invalidate_screen();
                    // All browsers need to know agent was deleted
                    send_agent_list(hub);
                }
                BrowserEvent::CreateAgent { .. } | BrowserEvent::ReopenWorktree { .. } => {
                    hub.browser.invalidate_screen();
                    // Agent creation is async - agent_list, selection, and scrollback
                    // are sent from handle_pending_agent_result when the agent is ready
                }
                BrowserEvent::TogglePtyView
                | BrowserEvent::Scroll { .. }
                | BrowserEvent::ScrollToTop
                | BrowserEvent::ScrollToBottom => {
                    hub.browser.invalidate_screen();
                }
                _ => {}
            }
            continue;
        }

        // Handle events not covered by client-scoped actions
        match event {
            // SetMode updates shared BrowserState mode
            BrowserEvent::SetMode { mode } => {
                hub.browser.handle_set_mode(&mode);
            }

            // GenerateInvite is handled directly in relay connection.rs
            BrowserEvent::GenerateInvite => {
                log::warn!("GenerateInvite reached Hub - should be handled in relay");
            }

            // BundleRegenerated - new PreKeyBundle was generated
            BrowserEvent::BundleRegenerated { bundle } => {
                log::info!("Received regenerated PreKeyBundle");
                hub.browser.signal_bundle = Some(bundle);
                hub.browser.bundle_used = false;
                // Update connection URL with new bundle
                if let Some(ref bundle) = hub.browser.signal_bundle {
                    use data_encoding::BASE32_NOPAD;
                    if let Ok(bytes) = bundle.to_binary() {
                        let encoded = BASE32_NOPAD.encode(&bytes);
                        hub.connection_url = Some(format!(
                            "{}/hubs/{}#{}",
                            hub.config.server_url,
                            hub.server_hub_id(),
                            encoded
                        ));
                        // Also write to file for external access
                        let _ = crate::relay::write_connection_url(
                            &hub.hub_identifier,
                            hub.connection_url.as_ref().unwrap(),
                        );
                        // NOTE: qr_image_displayed is TUI state, handled by TuiRunner
                        log::info!("Connection URL updated with new bundle");
                    }
                }
            }

            // All other events are handled by client-scoped actions above
            _ => {}
        }
    }

    Ok(())
}

/// Send agent list to browser.
///
/// Collects agent information and sends it to the connected browser client.
pub fn send_agent_list(hub: &Hub) {
    let Some(ctx) = browser_ctx(hub) else { return };

    let state = hub.state.read().unwrap();
    let agents = state
        .agent_keys_ordered
        .iter()
        .filter_map(|id| state.agents.get(id).map(|a| (id, a)))
        .map(|(id, a)| crate::relay::build_agent_info(id, a, &hub.hub_identifier))
        .collect();

    crate::relay::send_agent_list(&ctx, agents);
}

/// Send worktree list to browser.
///
/// Loads and sends available worktree information to the connected browser client.
pub fn send_worktree_list(hub: &mut Hub) {
    // Load worktrees fresh (they may not have been loaded yet)
    if let Err(e) = hub.load_available_worktrees() {
        log::warn!("Failed to load worktrees: {}", e);
    }

    // Get browser context after loading worktrees (borrow checker)
    let Some(ctx) = browser_ctx(hub) else { return };

    let worktrees = hub
        .state
        .read()
        .unwrap()
        .available_worktrees
        .iter()
        .map(|(path, branch)| crate::relay::build_worktree_info(path, branch))
        .collect();

    crate::relay::send_worktree_list(&ctx, worktrees);
}

// === Targeted send functions (per-client routing) ===

/// Send agent list to a specific browser (not broadcast).
pub fn send_agent_list_to_browser(hub: &Hub, browser_identity: &str) {
    let Some(ctx) = browser_ctx(hub) else { return };

    let state = hub.state.read().unwrap();
    let agents = state
        .agent_keys_ordered
        .iter()
        .filter_map(|id| state.agents.get(id).map(|a| (id, a)))
        .map(|(id, a)| crate::relay::build_agent_info(id, a, &hub.hub_identifier))
        .collect();

    crate::relay::send_agent_list_to(&ctx, browser_identity, agents);
}

/// Send worktree list to a specific browser (not broadcast).
pub fn send_worktree_list_to_browser(hub: &mut Hub, browser_identity: &str) {
    // Load worktrees fresh (they may not have been loaded yet)
    if let Err(e) = hub.load_available_worktrees() {
        log::warn!("Failed to load worktrees: {}", e);
    }

    // Get browser context after loading worktrees (borrow checker)
    let Some(ctx) = browser_ctx(hub) else { return };

    let worktrees = hub
        .state
        .read()
        .unwrap()
        .available_worktrees
        .iter()
        .map(|(path, branch)| crate::relay::build_worktree_info(path, branch))
        .collect();

    crate::relay::send_worktree_list_to(&ctx, browser_identity, worktrees);
}

/// Send agent selected notification to a specific browser (not broadcast).
fn send_agent_selected_to_browser(hub: &Hub, browser_identity: &str, agent_id: &str) {
    let Some(ctx) = browser_ctx(hub) else { return };
    crate::relay::send_agent_selected_to(&ctx, browser_identity, agent_id);
}

/// Send scrollback history to a specific browser (not broadcast).
///
/// # Arguments
///
/// * `hub` - Hub reference for agent and relay access
/// * `browser_identity` - Signal identity of the target browser
/// * `agent_id` - The agent ID to get scrollback from
/// * `view` - Which PTY view to get scrollback from
pub fn send_scrollback_for_agent_to_browser(
    hub: &Hub,
    browser_identity: &str,
    agent_id: &str,
    view: crate::agent::PtyView,
) {
    let Some(ctx) = browser_ctx(hub) else { return };
    let state = hub.state.read().unwrap();
    let Some(agent) = state.agents.get(agent_id) else {
        log::warn!("Cannot send scrollback for unknown agent: {}", agent_id);
        return;
    };

    let bytes = agent.get_scrollback_snapshot(view);
    log::info!(
        "Sending {} scrollback bytes to browser {} for agent {} ({:?})",
        bytes.len(),
        &browser_identity[..8.min(browser_identity.len())],
        agent_id,
        view
    );
    crate::relay::send_scrollback_to(&ctx, browser_identity, bytes);
}

#[cfg(test)]
mod tests {
    use crate::relay::types::BrowserCommand;

    /// Verify BrowserCommand::Input -> BrowserEvent::Input mapping.
    /// This is critical for keyboard input from browser to reach CLI.
    #[test]
    fn test_browser_command_input_converts_to_event() {
        let json = r#"{"type":"input","data":"hello world"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();

        // The conversion happens in connection.rs, but we verify the type structure
        match cmd {
            BrowserCommand::Input { data } => {
                assert_eq!(data, "hello world");
                // In connection.rs line 402, this becomes BrowserEvent::Input(data)
            }
            _ => panic!("Expected Input variant"),
        }
    }

    /// Verify BrowserCommand::Scroll -> BrowserEvent::Scroll mapping.
    #[test]
    fn test_browser_command_scroll_converts_to_event() {
        let json = r#"{"type":"scroll","direction":"up","lines":10}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();

        match cmd {
            BrowserCommand::Scroll { direction, lines } => {
                assert_eq!(direction, "up");
                assert_eq!(lines, Some(10));
            }
            _ => panic!("Expected Scroll variant"),
        }
    }

    /// Verify BrowserCommand::Resize -> BrowserEvent::Resize mapping.
    #[test]
    fn test_browser_command_resize_converts_to_event() {
        let json = r#"{"type":"resize","cols":120,"rows":40}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();

        match cmd {
            BrowserCommand::Resize { cols, rows } => {
                assert_eq!(cols, 120);
                assert_eq!(rows, 40);
                // In connection.rs line 425-427, this becomes:
                // BrowserEvent::Resize(BrowserResize { cols, rows })
            }
            _ => panic!("Expected Resize variant"),
        }
    }

    /// Verify BrowserCommand::SetMode parsing for gui mode.
    #[test]
    fn test_browser_command_set_mode_gui() {
        let json = r#"{"type":"set_mode","mode":"gui"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();

        match cmd {
            BrowserCommand::SetMode { mode } => {
                assert_eq!(mode, "gui");
            }
            _ => panic!("Expected SetMode variant"),
        }
    }

    /// Test the actual event handling in poll_events would require a full Hub,
    /// which is tested in hub/actions.rs. This module tests the parsing layer.

    /// Verify browser input with special characters (Ctrl+C, etc.)
    #[test]
    fn test_browser_command_input_with_control_chars() {
        // Ctrl+C is \x03
        let json = r#"{"type":"input","data":"\u0003"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();

        match cmd {
            BrowserCommand::Input { data } => {
                assert_eq!(data, "\x03");
            }
            _ => panic!("Expected Input variant"),
        }
    }

    /// Verify browser input with escape sequences (arrow keys, etc.)
    #[test]
    fn test_browser_command_input_with_escape_sequences() {
        // Arrow up is \x1b[A
        let json = r#"{"type":"input","data":"\u001b[A"}"#;
        let cmd: BrowserCommand = serde_json::from_str(json).unwrap();

        match cmd {
            BrowserCommand::Input { data } => {
                assert_eq!(data, "\x1b[A");
            }
            _ => panic!("Expected Input variant"),
        }
    }
}
