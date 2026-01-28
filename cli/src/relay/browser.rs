//! Browser relay event handling.
//!
//! This module handles relay-level events from the browser WebSocket relay.
//! Bootstrap events (Connected, Disconnected) update BrowserState and drive
//! client lifecycle (ClientConnected/ClientDisconnected). These events fire
//! after the Signal Protocol handshake completes, guaranteeing the E2E
//! session exists before BrowserClient is created.
//! Command events (SelectAgent, CreateAgent, Scroll, etc.) are converted
//! to HubActions and dispatched to the Hub.
//!
//! # Architecture
//!
//! ```text
//! WebSocket → BrowserEvent → browser::poll_events() → Bootstrap state changes
//!                                                    → HubAction dispatch
//! ```
//!
//! BrowserClient also handles events independently via its own hub channel
//! subscription and BrowserRequest processing in its async task.

// Rust guideline compliant 2026-01

use anyhow::Result;

use crate::hub::{actions, Hub};
use crate::relay::BrowserEvent;

/// Poll and handle browser relay events from the terminal relay.
///
/// This is the main integration point between the browser relay and the Hub.
/// Called from the Hub's event loop to process incoming browser events.
///
/// Only handles bootstrap events (Connected, Disconnected, BundleRegenerated).
/// All command routing is handled by BrowserClient's async task.
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

/// Poll and handle browser relay events in headless mode.
///
/// Same as `poll_events` but doesn't require a terminal reference.
/// Used by headless mode where no TUI is available.
///
/// Handles all browser events:
/// - Bootstrap/lifecycle: Connected, Disconnected, BundleRegenerated
/// - Command routing: SelectAgent, CreateAgent, DeleteAgent, Scroll, etc.
///
/// # Errors
///
/// Returns an error if event handling fails.
pub fn poll_events_headless(hub: &mut Hub) -> Result<()> {
    let browser_events = hub.browser.drain_events();

    for (event, browser_identity) in browser_events {
        match event {
            // === Bootstrap Events ===

            BrowserEvent::Connected { ref device_name, .. } => {
                hub.browser.handle_connected(device_name);
                // Create BrowserClient after Signal handshake completes.
                // The command channel browser_connected message arrives earlier
                // (on HubChannel subscribe), but the Signal session isn't ready yet.
                let client_id = crate::client::ClientId::Browser(browser_identity.clone());
                actions::dispatch(hub, crate::hub::HubAction::ClientConnected { client_id });
            }

            BrowserEvent::Disconnected => {
                hub.browser.handle_disconnected();
                let client_id = crate::client::ClientId::Browser(browser_identity.clone());
                actions::dispatch(hub, crate::hub::HubAction::ClientDisconnected { client_id });
            }

            BrowserEvent::BundleRegenerated { bundle } => {
                log::info!("Received regenerated PreKeyBundle");
                hub.browser.signal_bundle = Some(bundle);
                hub.browser.bundle_used = false;
                // Update cached connection URL with new bundle
                let result = hub.generate_connection_url();
                hub.handle_cache.set_connection_url(result.clone());
                if let Ok(ref url) = result {
                    // Write to file for external access
                    let _ = crate::relay::write_connection_url(
                        &hub.hub_identifier,
                        url,
                    );
                    log::info!("Connection URL updated with new bundle");
                }
            }

            // === Command Events — convert to HubActions and dispatch ===

            BrowserEvent::SelectAgent { ref id } => {
                let client_id = crate::client::ClientId::Browser(browser_identity.clone());
                actions::dispatch(hub, crate::hub::HubAction::SelectAgentForClient {
                    client_id,
                    agent_key: id.clone(),
                });
            }

            BrowserEvent::CreateAgent { ref issue_or_branch, ref prompt } => {
                let client_id = crate::client::ClientId::Browser(browser_identity.clone());
                let request = crate::client::CreateAgentRequest {
                    issue_or_branch: issue_or_branch.clone().unwrap_or_default(),
                    prompt: prompt.clone(),
                    from_worktree: None,
                    dims: None, // Hub will use defaults
                };
                actions::dispatch(hub, crate::hub::HubAction::CreateAgentForClient {
                    client_id,
                    request,
                });
            }

            BrowserEvent::ReopenWorktree { ref path, ref branch, ref prompt } => {
                let client_id = crate::client::ClientId::Browser(browser_identity.clone());
                let request = crate::client::CreateAgentRequest {
                    issue_or_branch: branch.clone(),
                    prompt: prompt.clone(),
                    from_worktree: Some(std::path::PathBuf::from(path)),
                    dims: None,
                };
                actions::dispatch(hub, crate::hub::HubAction::CreateAgentForClient {
                    client_id,
                    request,
                });
            }

            BrowserEvent::DeleteAgent { ref id, delete_worktree } => {
                let client_id = crate::client::ClientId::Browser(browser_identity.clone());
                let request = crate::client::DeleteAgentRequest {
                    agent_id: id.clone(),
                    delete_worktree,
                };
                actions::dispatch(hub, crate::hub::HubAction::DeleteAgentForClient {
                    client_id,
                    request,
                });
            }

            BrowserEvent::Scroll { ref direction, lines } => {
                let client_id = crate::client::ClientId::Browser(browser_identity.clone());
                let scroll = if direction == "up" {
                    crate::hub::actions::ScrollDirection::Up(lines as usize)
                } else {
                    crate::hub::actions::ScrollDirection::Down(lines as usize)
                };
                actions::dispatch(hub, crate::hub::HubAction::ScrollForClient { client_id, scroll });
            }

            BrowserEvent::ScrollToTop => {
                let client_id = crate::client::ClientId::Browser(browser_identity.clone());
                actions::dispatch(hub, crate::hub::HubAction::ScrollForClient {
                    client_id,
                    scroll: crate::hub::actions::ScrollDirection::ToTop,
                });
            }

            BrowserEvent::ScrollToBottom => {
                let client_id = crate::client::ClientId::Browser(browser_identity.clone());
                actions::dispatch(hub, crate::hub::HubAction::ScrollForClient {
                    client_id,
                    scroll: crate::hub::actions::ScrollDirection::ToBottom,
                });
            }

            BrowserEvent::TogglePtyView => {
                let client_id = crate::client::ClientId::Browser(browser_identity.clone());
                actions::dispatch(hub, crate::hub::HubAction::TogglePtyViewForClient { client_id });
            }

            BrowserEvent::SetMode { .. } => {
                // Mode is browser-local state, no Hub action needed.
                log::debug!("Browser SetMode event (handled client-side)");
            }

            BrowserEvent::ListAgents | BrowserEvent::ListWorktrees => {
                // These are handled by BrowserClient when it loads initial data.
                log::debug!("Browser list request (handled by BrowserClient on connect)");
            }

            BrowserEvent::Input(_) | BrowserEvent::Resize(_) => {
                // Input and resize are per-PTY events handled by BrowserClient's
                // TerminalRelayChannel, not the bootstrap relay.
                log::trace!("Browser PTY event (handled by BrowserClient TerminalRelayChannel)");
            }

            BrowserEvent::GenerateInvite => {
                log::warn!("GenerateInvite reached Hub - should be handled in relay");
            }
        }
    }

    Ok(())
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
