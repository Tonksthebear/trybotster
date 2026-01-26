//! Client-scoped action handlers.
//!
//! Handlers for actions that operate on a specific client's view,
//! including selection, input routing, resize, and agent management.
//!
//! # Architecture
//!
//! BrowserClient owns its PTY channels internally (see `client/browser.rs`).
//! This module handles high-level client actions:
//!
//! - Agent selection: `handle_select_agent_for_client()`
//! - Agent creation/deletion: `handle_create_agent_for_client()`, `handle_delete_agent_for_client()`
//! - Client lifecycle: `handle_client_connected()`, `handle_client_disconnected()`
//! - Input/resize: `handle_send_input_for_client()`, `handle_resize_for_client()`
//!
//! PTY I/O routing (output forwarder, input receiver) is handled by BrowserClient directly.

// Rust guideline compliant 2026-01

use std::path::PathBuf;
use std::sync::Arc;

use crate::client::{BrowserClient, ClientId, CreateAgentRequest, DeleteAgentRequest};
use crate::client::browser::BrowserClientConfig;
use crate::hub::{lifecycle, Hub};

/// Handle selecting an agent for a specific client.
///
/// When a client selects an agent:
/// 1. Validates agent exists
/// 2. Ensures agent's channels are connected (lazy connection)
/// 3. Connects the client to the agent's PTY via Client trait
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

    // Track TUI selection in Hub for get_tui_selected_agent_key()
    if client_id.is_tui() {
        hub.tui_selected_agent = Some(agent_key.clone());
    }

    // NOTE: PTY connection is NOT handled here.
    //
    // SelectAgentForClient is about SELECTION TRACKING, not PTY I/O setup.
    // - TuiRunner manages its own PTY subscription (has direct state access)
    // - BrowserClient creates PTY channels when browser actually requests output
    //
    // PTY connection is handled separately by each client type:
    // - TUI: TuiRequest::SelectAgent -> TuiClient.handle_request() -> connect_to_pty
    // - Browser: BrowserEvent::SelectAgent handler in relay/browser.rs

    // Note: Scrollback is sent via browser.rs event handler (send_scrollback_for_agent_to_browser)
    // when BrowserEvent::SelectAgent is processed. This action handler is generic for all clients.
    // TUI doesn't need scrollback pushed - it reads directly from vt100 parser.

    log::debug!(
        "Client {} selected agent {}",
        client_id,
        &agent_key[..8.min(agent_key.len())]
    );
}

/// Handle sending input for a specific client.
///
/// Uses direct state access to write input to the selected agent's PTY.
/// This avoids blocking on `hub_handle` which would deadlock in tests.
pub fn handle_send_input_for_client(hub: &mut Hub, client_id: ClientId, data: Vec<u8>) {
    use crate::agent::PtyView;

    // Get selected agent key based on client type
    let agent_key = match &client_id {
        ClientId::Tui => hub.tui_selected_agent.clone(),
        ClientId::Browser(_) => {
            // Browser selection not tracked in hub yet - browser input goes through
            // the PTY input receiver spawned by BrowserClient, not this handler.
            log::debug!("Browser input routing not implemented via this handler");
            return;
        }
    };

    // Direct state access - no hub_handle blocking
    if let Some(key) = agent_key {
        if let Some(agent) = hub.state.write().unwrap().agents.get_mut(&key) {
            // Default to CLI PTY for input
            if let Err(e) = agent.write_input(PtyView::Cli, &data) {
                log::debug!("Failed to write to agent PTY: {}", e);
            }
        } else {
            log::debug!("Agent {} not found for input", key);
        }
    } else {
        log::debug!("Client {} has no agent selected", client_id);
    }
}

/// Handle resize for a specific client.
///
/// For TUI, updates hub.terminal_dims (used for new agent spawns).
/// Updates client's internal dimensions and resizes connected PTYs directly
/// (avoiding the deadlock that would occur if client.set_dims called hub_handle).
pub fn handle_resize_for_client(hub: &mut Hub, client_id: ClientId, cols: u16, rows: u16) {

    // Update hub terminal_dims for TUI (used for new agent spawns)
    if client_id.is_tui() {
        hub.terminal_dims = (rows, cols);
    }

    // Get connected PTY indices BEFORE mutating the client
    // (can't borrow client and state at the same time)
    let connected_ptys: Vec<(usize, usize)> = match &client_id {
        ClientId::Tui => {
            if let Some(tui) = hub.clients.get_tui() {
                tui.connected_pty().into_iter().collect()
            } else {
                vec![]
            }
        }
        ClientId::Browser(_) => {
            if let Some(client) = hub.clients.get(&client_id) {
                if let Some(browser) = client.as_any().and_then(|a| a.downcast_ref::<BrowserClient>()) {
                    browser.connected_ptys().collect()
                } else {
                    vec![]
                }
            } else {
                vec![]
            }
        }
    };

    // Update client's internal dimensions (this no longer calls resize_pty internally)
    if let Some(client) = hub.clients.get_mut(&client_id) {
        client.set_dims(cols, rows);
    }

    // Resize all connected PTYs directly
    for (agent_idx, pty_idx) in connected_ptys {
        let pty_handle = {
            let state = hub.state.read().unwrap();
            state.get_agent_handle(agent_idx)
                .and_then(|agent| agent.get_pty(pty_idx).cloned())
        };

        if let Some(pty) = pty_handle {
            if let Some(client) = hub.clients.get(&client_id) {
                if let Err(e) = client.resize_pty_with_handle(&pty, rows, cols) {
                    log::debug!(
                        "Failed to resize PTY ({}, {}) for {}: {}",
                        agent_idx,
                        pty_idx,
                        client_id,
                        e
                    );
                }
            }
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

    // Enter tokio runtime context for spawn_command_processor() which uses tokio::spawn()
    let _runtime_guard = hub.tokio_runtime.enter();

    // Spawn agent - release lock before continuing
    let spawn_result = {
        let mut state = hub.state.write().unwrap();
        lifecycle::spawn_agent(&mut state, &config, dims)
    };

    match spawn_result {
        Ok(result) => {
            log::info!("Client {} created agent: {}", client_id, result.agent_id);

            // Sync handle cache for thread-safe agent access
            hub.sync_handle_cache();

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
            handle_select_agent_for_client(hub, client_id, agent_id.clone());

            // Broadcast AgentCreated event to all subscribers (including TUI)
            if let Some(info) = hub.state.read().unwrap().get_agent_info(&agent_id) {
                hub.broadcast(crate::hub::HubEvent::agent_created(agent_id, info));
            }
        }
        Err(e) => {
            hub.send_error_to(&client_id, format!("Failed to spawn agent: {}", e));
        }
    }
}

/// Handle deleting an agent for a specific client.
///
/// When an agent is deleted:
/// 1. All clients connected to that agent's PTYs are disconnected
/// 2. The agent and optionally its worktree are deleted
pub fn handle_delete_agent_for_client(
    hub: &mut Hub,
    client_id: ClientId,
    request: DeleteAgentRequest,
) {
    // Get agent index before deletion
    let agent_index = hub
        .state
        .read()
        .unwrap()
        .agents
        .keys()
        .position(|k| k == &request.agent_id);

    // Disconnect all clients from this agent's PTYs
    if let Some(idx) = agent_index {
        for (_client_id, client) in hub.clients.iter_mut() {
            // Disconnect from CLI PTY (index 0) and Server PTY (index 1)
            client.disconnect_from_pty(idx, 0);
            client.disconnect_from_pty(idx, 1);
        }
    }

    // Delete the agent - release lock before continuing
    let close_result = {
        let mut state = hub.state.write().unwrap();
        lifecycle::close_agent(&mut state, &request.agent_id, request.delete_worktree)
    };

    match close_result {
        Ok(_was_deleted) => {
            log::info!("Client {} deleted agent: {}", client_id, request.agent_id);

            // Sync handle cache for thread-safe agent access
            hub.sync_handle_cache();

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
        // Get crypto service - required for BrowserClient
        let Some(crypto_service) = hub.browser.crypto_service.clone() else {
            log::error!("Cannot create BrowserClient: crypto service not initialized");
            return;
        };

        // Build config from Hub's direct access (avoid hub_handle commands that would deadlock)
        let config = BrowserClientConfig {
            crypto_service,
            server_url: hub.config.server_url.clone(),
            api_key: hub.config.get_api_key().to_string(),
            server_hub_id: hub.server_hub_id().to_string(),
        };

        let hub_handle = hub.handle();
        let runtime_handle = hub.tokio_runtime.handle().clone();
        let browser_client = BrowserClient::new(hub_handle, identity.clone(), runtime_handle, config);
        hub.clients.register(Box::new(browser_client));
        log::info!("Registered BrowserClient for {}", identity);
    }
}

/// Handle client disconnected event.
///
/// When a client disconnects:
/// 1. Disconnect from all connected PTYs (via Client trait)
/// 2. Unregister the client from the registry
pub fn handle_client_disconnected(hub: &mut Hub, client_id: ClientId) {
    log::info!("Client disconnecting: {}", client_id);

    // Unregister the client - this drops BrowserClient which cleans up its channels
    hub.clients.unregister(&client_id);

    log::info!("Client disconnected: {}", client_id);
}
