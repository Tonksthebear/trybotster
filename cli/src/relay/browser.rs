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
//! - [`drain_and_route_pty_output`] - Route PTY output to viewing clients

// Rust guideline compliant 2025-01

use anyhow::Result;

use crate::client::ClientId;
use crate::hub::{actions, Hub};
use crate::relay::{
    BrowserCommand, BrowserEvent, BrowserSendContext,
    events::browser_event_to_client_action,
};

/// Get browser send context if browser is connected.
fn browser_ctx(hub: &Hub) -> Option<BrowserSendContext<'_>> {
    hub.browser.sender.as_ref().map(|sender| BrowserSendContext {
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
                // Note: BrowserEvent::Resize is intentionally not handled here.
                // Resize is now per-client via terminal channel (drain_and_route_browser_input).
                BrowserEvent::SelectAgent { id } => {
                    hub.browser.invalidate_screen();
                    // Send to THIS browser only (not broadcast to all browsers)
                    send_agent_selected_to_browser(hub, &browser_identity, id);
                    send_scrollback_for_agent_to_browser(hub, &browser_identity, id);
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
                            hub.connection_url.as_ref().unwrap()
                        );
                        // Reset QR image flag so it renders with new URL
                        hub.qr_image_displayed = false;
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

    let agents = hub.state.agent_keys_ordered.iter()
        .filter_map(|key| hub.state.agents.get(key).map(|a| (key, a)))
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

    let worktrees = hub.state.available_worktrees.iter()
        .map(|(path, branch)| crate::relay::build_worktree_info(path, branch))
        .collect();

    crate::relay::send_worktree_list(&ctx, worktrees);
}

/// Send selected agent notification to browser.
///
/// Notifies the browser that an agent has been selected.
pub fn send_agent_selected(hub: &Hub, agent_id: &str) {
    let Some(ctx) = browser_ctx(hub) else { return };
    crate::relay::send_agent_selected(&ctx, agent_id);
}

/// Send scrollback history for a specific agent to browser (broadcast).
///
/// Called when an agent is selected so the browser can populate
/// xterm's scrollback buffer with historical output.
///
/// # Arguments
///
/// * `hub` - Hub reference for agent and relay access
/// * `agent_key` - The agent key to get scrollback from (the browser's selection)
pub fn send_scrollback_for_agent(hub: &Hub, agent_key: &str) {
    let Some(ctx) = browser_ctx(hub) else { return };
    let Some(agent) = hub.state.agents.get(agent_key) else {
        log::warn!("Cannot send scrollback for unknown agent: {}", agent_key);
        return;
    };

    let bytes = agent.get_scrollback_snapshot();
    log::info!("Sending {} scrollback bytes to browser for agent {}", bytes.len(), agent_key);
    crate::relay::send_scrollback(&ctx, bytes);
}

// === Targeted send functions (per-client routing) ===

/// Send agent list to a specific browser (not broadcast).
pub fn send_agent_list_to_browser(hub: &Hub, browser_identity: &str) {
    let Some(ctx) = browser_ctx(hub) else { return };

    let agents = hub.state.agent_keys_ordered.iter()
        .filter_map(|key| hub.state.agents.get(key).map(|a| (key, a)))
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

    let worktrees = hub.state.available_worktrees.iter()
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
pub fn send_scrollback_for_agent_to_browser(hub: &Hub, browser_identity: &str, agent_key: &str) {
    let Some(ctx) = browser_ctx(hub) else { return };
    let Some(agent) = hub.state.agents.get(agent_key) else {
        log::warn!("Cannot send scrollback for unknown agent: {}", agent_key);
        return;
    };

    let bytes = agent.get_scrollback_snapshot();
    log::info!("Sending {} scrollback bytes to browser {} for agent {}",
        bytes.len(), &browser_identity[..8.min(browser_identity.len())], agent_key);
    crate::relay::send_scrollback_to(&ctx, browser_identity, bytes);
}


/// Drain browser input from PTY channels and route to PTY.
///
/// Each PTY session owns its channel. Browsers send input to the PTY's
/// channel stream. This function drains that input and writes to the PTY.
///
/// Call this each event loop iteration to process browser input for all agents.
pub fn drain_and_route_browser_input(hub: &mut crate::hub::Hub) {
    // Collect all agent keys
    let agent_keys: Vec<String> = hub.state.agents.keys().cloned().collect();

    // Collect resize operations to apply client dims after agent borrow is released
    // (peer_id, cols, rows)
    let mut client_resizes: Vec<(String, u16, u16)> = Vec::new();

    for agent_key in agent_keys {
        // Drain CLI PTY input for this agent
        let inputs = {
            let Some(agent) = hub.state.agents.get_mut(&agent_key) else {
                continue;
            };
            agent.drain_cli_input()
        };

        // Parse and route each command to the agent's PTY
        if !inputs.is_empty() {
            let Some(agent) = hub.state.agents.get_mut(&agent_key) else {
                continue;
            };
            for (data, peer_id) in inputs {
                // Parse as BrowserCommand to extract the actual input data
                match serde_json::from_slice::<BrowserCommand>(&data) {
                    Ok(BrowserCommand::Input { data: input_data }) => {
                        log::debug!(
                            "Routing input from {} to agent {}: {} bytes",
                            &peer_id.0[..8.min(peer_id.0.len())],
                            agent_key,
                            input_data.len()
                        );
                        if let Err(e) = agent.write_input(input_data.as_bytes()) {
                            log::error!(
                                "Failed to write input from {} to agent {}: {}",
                                &peer_id.0[..8.min(peer_id.0.len())],
                                agent_key,
                                e
                            );
                        }
                    }
                    Ok(BrowserCommand::Resize { cols, rows }) => {
                        log::debug!(
                            "Resize from {} for agent {}: {}x{}",
                            &peer_id.0[..8.min(peer_id.0.len())],
                            agent_key,
                            cols,
                            rows
                        );
                        // Collect resize for later dispatch (avoid borrow conflict)
                        // Don't resize agent directly - let action dispatch handle it
                        // with proper size_owner tracking
                        client_resizes.push((peer_id.0.clone(), cols, rows));
                    }
                    Ok(other) => {
                        log::warn!(
                            "Unexpected command on terminal channel from {}: {:?}",
                            &peer_id.0[..8.min(peer_id.0.len())],
                            other
                        );
                    }
                    Err(e) => {
                        log::warn!(
                            "Failed to parse terminal command from {}: {}",
                            &peer_id.0[..8.min(peer_id.0.len())],
                            e
                        );
                    }
                }
            }
        }
    }

    // Dispatch resize actions after agent borrows are released.
    // Action dispatch handles size_owner tracking.
    for (peer_id, cols, rows) in client_resizes {
        use crate::hub::HubAction;
        actions::dispatch(
            hub,
            HubAction::ResizeForClient {
                client_id: ClientId::browser(&peer_id),
                cols,
                rows,
            },
        );
    }
}

/// Drain PTY output from all agents and route to viewing clients.
///
/// Each agent owns its terminal_channel (CLI PTY) and optionally server_terminal_channel
/// (Server PTY). Output is sent via the appropriate channel to browsers subscribed
/// to that agent's stream.
///
/// Call this each event loop iteration to stream PTY output to clients.
pub fn drain_and_route_pty_output(hub: &mut crate::hub::Hub) {
    // Collect agent keys and CLI PTY output (access PtySession directly)
    let cli_outputs: Vec<(String, Vec<u8>)> = hub
        .state
        .agents
        .iter()
        .map(|(key, agent)| (key.clone(), agent.cli_pty.drain_raw_output()))
        .filter(|(_, bytes)| !bytes.is_empty())
        .collect();

    // Collect agent keys and Server PTY output (access PtySession directly)
    let server_outputs: Vec<(String, Vec<u8>)> = hub
        .state
        .agents
        .iter()
        .filter_map(|(key, agent)| {
            agent
                .server_pty
                .as_ref()
                .map(|pty| (key.clone(), pty.drain_raw_output()))
        })
        .filter(|(_, bytes)| !bytes.is_empty())
        .collect();

    // Route CLI PTY output via cli_pty.channel (pty_index=0)
    // Browsers subscribed to this stream will receive it
    for (agent_key, data) in &cli_outputs {
        if let Some(agent) = hub.state.agents.get(agent_key) {
            if agent.cli_pty.has_channel() {
                send_via_pty_channel(hub, agent_key, &data, PtyType::Cli);
            } else {
                log::debug!(
                    "CLI output: {} bytes from agent {} but no channel",
                    data.len(),
                    &agent_key[..8.min(agent_key.len())]
                );
            }
        }
    }

    // Route Server PTY output via server_pty.channel (pty_index=1)
    // Browsers subscribed to this stream will receive it
    for (agent_key, data) in &server_outputs {
        if let Some(agent) = hub.state.agents.get(agent_key) {
            if agent.has_server_terminal_channel() {
                send_via_pty_channel(hub, agent_key, &data, PtyType::Server);
            } else {
                log::debug!(
                    "Server output: {} bytes from agent {} but no channel",
                    data.len(),
                    &agent_key[..8.min(agent_key.len())]
                );
            }
        }
    }
}

/// Which PTY type to send output through.
enum PtyType {
    /// CLI PTY (index 0) - uses cli_pty.channel.
    Cli,
    /// Server PTY (index 1) - uses server_pty.channel.
    Server,
}

/// Send output via a PTY's terminal channel.
///
/// Broadcasts output through the specified PTY's channel. The channel handles
/// encryption and delivery to all connected browsers subscribed to that stream.
fn send_via_pty_channel(hub: &crate::hub::Hub, agent_key: &str, data: &[u8], pty_type: PtyType) {
    let Some(agent) = hub.state.agents.get(agent_key) else {
        log::warn!("Agent {} not found for output routing", agent_key);
        return;
    };

    // Get the sender handle from the appropriate PTY's channel
    let (sender_handle, label) = match pty_type {
        PtyType::Cli => (agent.cli_pty.get_channel_sender(), "CLI"),
        PtyType::Server => (
            agent
                .server_pty
                .as_ref()
                .and_then(|pty| pty.get_channel_sender()),
            "Server",
        ),
    };

    let Some(sender_handle) = sender_handle else {
        log::warn!(
            "Agent {} {} PTY has no channel sender, dropping {} bytes",
            &agent_key[..8.min(agent_key.len())],
            label,
            data.len()
        );
        return;
    };

    // Clone what we need for the async task
    let data = data.to_vec();
    let agent_key = agent_key.to_string();
    let label = label.to_string();

    hub.tokio_runtime.spawn(async move {
        log::debug!(
            "Broadcasting {} bytes via agent {} {} channel",
            data.len(),
            &agent_key[..8.min(agent_key.len())],
            label
        );

        // Wrap output in JSON structure for browser deserialization
        // Browser's reliable_channel.js expects JSON payloads
        let output_msg = serde_json::json!({
            "type": "output",
            "data": data_encoding::BASE64.encode(&data),
        });
        let msg_bytes = serde_json::to_vec(&output_msg).expect("JSON serialization");

        // Broadcast to all peers on this channel
        // The Rails channel routes to browsers subscribed to this pty_index stream
        if let Err(e) = sender_handle.send(&msg_bytes).await {
            log::error!(
                "Failed to broadcast {} output for agent {}: {}",
                label,
                &agent_key[..8.min(agent_key.len())],
                e
            );
        } else {
            log::debug!(
                "Broadcast {} bytes via {} channel",
                data.len(),
                label
            );
        }
    });
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
