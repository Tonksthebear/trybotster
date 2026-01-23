//! Client-scoped action handlers.
//!
//! Handlers for actions that operate on a specific client's view,
//! including selection, input routing, resize, and agent management.

use std::path::PathBuf;
use std::sync::Arc;

use crate::client::{BrowserClient, ClientId, CreateAgentRequest, DeleteAgentRequest};
use crate::hub::{lifecycle, Hub};

/// Handle selecting an agent for a specific client.
pub fn handle_select_agent_for_client(hub: &mut Hub, client_id: ClientId, agent_key: String) {
    log::info!(
        "handle_select_agent_for_client: client={}, agent={}",
        client_id,
        &agent_key[..8.min(agent_key.len())]
    );

    // Validate agent exists
    if !hub.state.read().unwrap().agents.contains_key(&agent_key) {
        hub.send_error_to(&client_id, "Agent not found".to_string());
        return;
    }

    // Connect agent's channels if not already connected (lazy connection)
    // This handles agents created before a browser connected (no crypto service yet)
    // Terminal channels are managed by BrowserClient, not Agent - just ensure connection
    let agent_index = hub
        .state
        .read()
        .unwrap()
        .agents
        .keys()
        .position(|k| k == &agent_key);

    if let Some(idx) = agent_index {
        log::debug!(
            "Ensuring terminal channel connected for agent {}",
            &agent_key[..8.min(agent_key.len())]
        );
        hub.connect_agent_channels(&agent_key, idx);
    }

    // Get client dims BEFORE updating state (for resize after selection)
    let client_dims = hub.clients.get(&client_id).map(|c| c.dims());

    // Update selection in registry (this updates both forward and reverse indices)
    hub.clients.select_agent(&client_id, Some(&agent_key));

    // Note: Scrollback is sent via browser.rs event handler (send_scrollback_for_agent_to_browser)
    // when BrowserEvent::SelectAgent is processed. This action handler is generic for all clients.
    // TUI doesn't need scrollback pushed - it reads directly from vt100 parser.
    // Response delivery is handled via browser relay channels, not here.

    // Resize agent to client's dimensions if available
    // Note: Size ownership tracking removed - PTY sessions manage their own viewers
    if let Some((cols, rows)) = client_dims {
        if let Some(agent) = hub.state.write().unwrap().agents.get_mut(&agent_key) {
            agent.resize(rows, cols);
            log::debug!(
                "Resized agent {} to {}x{} for client {}",
                &agent_key[..8.min(agent_key.len())],
                cols,
                rows,
                client_id
            );
        }
    }

    log::debug!(
        "Client {} selected agent {}",
        client_id,
        &agent_key[..8.min(agent_key.len())]
    );
}

/// Handle sending input for a specific client.
pub fn handle_send_input_for_client(hub: &mut Hub, client_id: ClientId, data: Vec<u8>) {
    // Get client's selected agent from registry
    let agent_key = hub.clients.selected_agent(&client_id).map(String::from);

    // Route input to agent's CLI PTY
    if let Some(key) = agent_key {
        if let Some(agent) = hub.state.write().unwrap().agents.get_mut(&key) {
            if let Err(e) = agent.write_input_to_cli(&data) {
                log::error!("Failed to send input to agent {}: {}", key, e);
            }
        }
    } else {
        log::debug!("Client {} sent input but no agent selected", client_id);
    }
}

/// Handle resize for a specific client.
///
/// Resizes the client's stored dimensions and the agent the client is currently viewing.
/// Note: Size ownership tracking removed - all resize requests are applied.
pub fn handle_resize_for_client(hub: &mut Hub, client_id: ClientId, cols: u16, rows: u16) {
    // Also update hub terminal_dims for TUI (used for new agent spawns)
    if client_id.is_tui() {
        hub.terminal_dims = (rows, cols);
    }

    // Update the client's stored dimensions
    if let Some(client) = hub.clients.get_mut(&client_id) {
        client.on_resized(rows, cols);
    }

    // Get the agent this client is viewing from registry
    let agent_key = hub.clients.selected_agent(&client_id).map(String::from);

    // Resize the agent the client is viewing
    if let Some(key) = agent_key {
        if let Some(agent) = hub.state.write().unwrap().agents.get_mut(&key) {
            agent.resize(rows, cols);
            log::debug!(
                "Resized agent {} to {}x{} for client {}",
                &key[..8.min(key.len())],
                cols,
                rows,
                client_id
            );
        }
    }
}

/// Handle creating an agent for a specific client.
///
/// This spawns the heavy git/file operations to a background thread to avoid
/// blocking the main event loop. The main loop polls for completion and finishes
/// the spawn (PTY creation) on the main thread.
pub fn handle_create_agent_for_client(
    hub: &mut Hub,
    client_id: ClientId,
    request: CreateAgentRequest,
) {
    // Send immediate "creating" notification to browser clients
    if let ClientId::Browser(ref identity) = client_id {
        if let Some(ref sender) = hub.browser.sender {
            let ctx = crate::relay::BrowserSendContext {
                sender,
                runtime: &hub.tokio_runtime,
            };
            crate::relay::send_agent_creating_to(&ctx, identity, &request.issue_or_branch);
        }
    }

    // Parse issue number or branch name
    let (issue_number, actual_branch_name) = if let Ok(num) = request.issue_or_branch.parse::<u32>()
    {
        (Some(num), format!("botster-issue-{num}"))
    } else {
        (None, request.issue_or_branch.clone())
    };

    // If worktree already provided, spawn agent synchronously (fast path)
    if let Some(worktree_path) = request.from_worktree {
        spawn_agent_sync(
            hub,
            client_id,
            issue_number,
            actual_branch_name,
            worktree_path,
            request.prompt,
        );
        return;
    }

    // Get repo info before spawning (needed for config)
    let (repo_path, repo_name) = match crate::git::WorktreeManager::detect_current_repo() {
        Ok(info) => info,
        Err(e) => {
            hub.send_error_to(&client_id, format!("Failed to detect repo: {}", e));
            return;
        }
    };

    // Clone what we need for the background thread
    let worktree_base = hub.config.worktree_base.clone();
    let branch_name = actual_branch_name.clone();
    let result_tx = hub.pending_agent_tx.clone();
    let progress_tx = hub.progress_tx.clone();
    let identifier = request.issue_or_branch.clone();
    let prompt = request.prompt.unwrap_or_else(|| {
        issue_number.map_or_else(
            || format!("Work on {branch_name}"),
            |n| format!("Work on issue #{n}"),
        )
    });

    // Spawn heavy git/file operations to background thread
    log::info!(
        "Spawning background agent creation for branch: {}",
        branch_name
    );

    std::thread::spawn(move || {
        use crate::relay::AgentCreationStage;

        // Helper to send progress updates
        let send_progress = |stage: AgentCreationStage| {
            let _ = progress_tx.send(crate::hub::AgentProgressEvent {
                client_id: client_id.clone(),
                identifier: identifier.clone(),
                stage,
            });
        };

        // Stage 1: Creating worktree
        send_progress(AgentCreationStage::CreatingWorktree);

        // Create worktree manager for this thread
        let git_manager = crate::git::WorktreeManager::new(worktree_base);

        // SLOW: Create git worktree
        let worktree_path = match git_manager.create_worktree_with_branch(&branch_name) {
            Ok(path) => path,
            Err(e) => {
                let _ = result_tx.send(crate::hub::PendingAgentResult {
                    client_id,
                    result: Err(format!("Failed to create worktree: {}", e)),
                    config: crate::agents::AgentSpawnConfig {
                        issue_number,
                        branch_name,
                        worktree_path: PathBuf::new(),
                        repo_path,
                        repo_name,
                        prompt,
                        message_id: None,
                        invocation_url: None,
                    },
                });
                return;
            }
        };

        // Stage 2: Copying config files (done inside create_worktree_with_branch)
        send_progress(AgentCreationStage::CopyingConfig);

        // Build config for spawn (will be completed on main thread)
        let config = crate::agents::AgentSpawnConfig {
            issue_number,
            branch_name,
            worktree_path: worktree_path.clone(),
            repo_path,
            repo_name,
            prompt,
            message_id: None,
            invocation_url: None,
        };

        log::info!("Background worktree creation complete: {:?}", worktree_path);

        // Stage 3: Spawning agent (main thread will handle this, but signal we're ready)
        send_progress(AgentCreationStage::SpawningAgent);

        // Send config back to main thread for PTY spawn
        // The main thread will call spawn_agent() which is fast
        let _ = result_tx.send(crate::hub::PendingAgentResult {
            client_id,
            result: Ok(crate::hub::SpawnResult {
                // Placeholder - actual spawn happens on main thread
                agent_id: String::new(),
                tunnel_port: None,
                has_server_pty: false,
            }),
            config,
        });
    });
}

/// Synchronous agent spawn (fast path when worktree already exists).
fn spawn_agent_sync(
    hub: &mut Hub,
    client_id: ClientId,
    issue_number: Option<u32>,
    branch_name: String,
    worktree_path: std::path::PathBuf,
    prompt: Option<String>,
) {
    let (repo_path, repo_name) = match crate::git::WorktreeManager::detect_current_repo() {
        Ok(info) => info,
        Err(e) => {
            hub.send_error_to(&client_id, format!("Failed to detect repo: {}", e));
            return;
        }
    };

    let prompt = prompt.unwrap_or_else(|| {
        issue_number.map_or_else(
            || format!("Work on {branch_name}"),
            |n| format!("Work on issue #{n}"),
        )
    });

    let config = crate::agents::AgentSpawnConfig {
        issue_number,
        branch_name,
        worktree_path,
        repo_path,
        repo_name,
        prompt,
        message_id: None,
        invocation_url: None,
    };

    let dims = hub
        .clients
        .get(&client_id)
        .map(|c| c.dims())
        .unwrap_or(hub.terminal_dims);

    // Spawn agent - release lock before continuing
    let spawn_result = {
        let mut state = hub.state.write().unwrap();
        lifecycle::spawn_agent(&mut state, &config, dims)
    };

    match spawn_result {
        Ok(result) => {
            log::info!("Client {} created agent: {}", client_id, result.agent_id);

            // Register tunnel for HTTP forwarding if tunnel port allocated
            if let Some(port) = result.tunnel_port {
                let tm = Arc::clone(&hub.tunnel_manager);
                let key = result.agent_id.clone();
                hub.tokio_runtime.spawn(async move {
                    tm.register_agent(key, port).await;
                });
            }

            // Connect agent's channels (terminal + preview if tunnel exists)
            let agent_index = hub
                .state
                .read()
                .unwrap()
                .agents
                .keys()
                .position(|k| k == &result.agent_id);

            if let Some(idx) = agent_index {
                hub.connect_agent_channels(&result.agent_id, idx);
            }

            // Response delivery is handled via browser relay channels
            let agent_id = result.agent_id;

            // Send agent_created to browser clients via relay
            if let ClientId::Browser(ref identity) = client_id {
                if let Some(ref sender) = hub.browser.sender {
                    let ctx = crate::relay::BrowserSendContext {
                        sender,
                        runtime: &hub.tokio_runtime,
                    };
                    crate::relay::send_agent_created_to(&ctx, identity, &agent_id);
                }
            }

            hub.broadcast_agent_list();
            handle_select_agent_for_client(hub, client_id, agent_id);
        }
        Err(e) => {
            hub.send_error_to(&client_id, format!("Failed to spawn agent: {}", e));
        }
    }
}

/// Handle deleting an agent for a specific client.
pub fn handle_delete_agent_for_client(
    hub: &mut Hub,
    client_id: ClientId,
    request: DeleteAgentRequest,
) {
    // Collect viewers before modifying (to avoid borrow issues)
    let viewers: Vec<ClientId> = hub.clients.viewers_of(&request.agent_id).cloned().collect();

    // Clear selection for each viewer
    for viewer_id in &viewers {
        hub.clients.clear_selection(viewer_id);
    }

    // Remove from viewer index
    hub.clients.remove_agent_viewers(&request.agent_id);

    // Delete the agent - release lock before continuing
    let close_result = {
        let mut state = hub.state.write().unwrap();
        lifecycle::close_agent(&mut state, &request.agent_id, request.delete_worktree)
    };

    match close_result {
        Ok(_was_deleted) => {
            log::info!("Client {} deleted agent: {}", client_id, request.agent_id);

            // Response delivery is handled via browser relay channels

            hub.broadcast_agent_list();
        }
        Err(e) => {
            hub.send_error_to(&client_id, format!("Failed to delete agent: {}", e));
        }
    }
}

/// Handle client connected event.
pub fn handle_client_connected(hub: &mut Hub, client_id: ClientId) {
    log::info!("Client connected: {}", client_id);

    // For browser clients, create and register BrowserClient
    if let ClientId::Browser(ref identity) = client_id {
        let hub_handle = hub.handle();
        let browser_client = BrowserClient::new(hub_handle, identity.clone());
        hub.clients.register(Box::new(browser_client));
        log::info!("Registered BrowserClient for {}", identity);
    }
}

/// Handle client disconnected event.
///
/// When a client disconnects, we check if they were viewing an agent and if so,
/// resize that agent to match any remaining viewer's dimensions.
pub fn handle_client_disconnected(hub: &mut Hub, client_id: ClientId) {
    // Get the agent this client was viewing BEFORE unregistering via registry
    let agent_key = hub.clients.selected_agent(&client_id).map(String::from);

    // Unregister the client
    hub.clients.unregister(&client_id);
    log::info!("Client disconnected: {}", client_id);

    // If client was viewing an agent and was the size owner, transfer ownership
    if let Some(key) = agent_key {
        resize_agent_for_remaining_viewers(hub, &key, &client_id);
    }
}

/// Resize agent for remaining viewers when a client disconnects.
///
/// Finds the first remaining viewer and resizes the agent to their dimensions.
/// Note: Size ownership tracking removed - just resize to first viewer's dims.
fn resize_agent_for_remaining_viewers(hub: &mut Hub, agent_key: &str, _disconnected_id: &ClientId) {
    // Find first remaining viewer with dimensions
    let new_viewer: Option<(ClientId, u16, u16)> = hub
        .clients
        .viewers_of(agent_key)
        .filter_map(|id| {
            hub.clients.get(id).map(|c| {
                let (cols, rows) = c.dims();
                (id.clone(), cols, rows)
            })
        })
        .next();

    if let Some((viewer_id, cols, rows)) = new_viewer {
        if let Some(agent) = hub.state.write().unwrap().agents.get_mut(agent_key) {
            agent.resize(rows, cols);
            log::info!(
                "Resized agent {} to {}x{} for remaining viewer {}",
                &agent_key[..8.min(agent_key.len())],
                cols,
                rows,
                viewer_id
            );
        }
    } else {
        log::debug!(
            "Agent {} has no remaining viewers",
            &agent_key[..8.min(agent_key.len())]
        );
    }
}
